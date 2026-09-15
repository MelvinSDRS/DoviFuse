//! Scene-cut based synchronization between the DV RPU and the HDR target.
//!
//! The DV source's cuts come free from the RPU (`dovi_tool export -d scenes`,
//! one frame index per line where scene_refresh_flag == 1). The HDR target's
//! cuts come from one downscaled ffmpeg scdet pass. Cross-correlating the two
//! lists by offset voting yields the global frame offset plus confidence
//! metrics; it tolerates the heavy partial overlap between Dolby's cut list
//! and ffmpeg's detector.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::exec::{run_status, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;
use serde::Serialize;

/// Minimum scene cuts a tercile needs before its offset vote is trusted.
const TERCILE_MIN_CUTS: usize = 3;
/// Candidate offset clusters to score exactly (diagnostics show the same set).
const TOP_CANDIDATES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SyncResult {
    /// dv_frame = hdr_frame + offset. Positive: DV has extra leading frames.
    pub(crate) offset: i64,
    pub(crate) matches: usize,
    pub(crate) match_ratio: f64,
    pub(crate) dominance: f64,
    pub(crate) tercile_offsets: [Option<i64>; 3],
}

pub(crate) struct CorrelationReport {
    pub(crate) accepted: Option<SyncResult>,
    pub(crate) rejection: Option<String>,
    /// Top candidate offsets with their one-to-one match counts.
    pub(crate) top_offsets: Vec<(i64, usize)>,
    pub(crate) dv_count: usize,
    pub(crate) hdr_count: usize,
}

/// Export the DV scene-cut list from an RPU file and parse it.
pub(crate) fn export_dv_scene_cuts(
    rpu_file: &Path,
    scenes_txt: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<Vec<u64>> {
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("export"),
            OsString::from("-i"),
            rpu_file.as_os_str().to_os_string(),
            OsString::from("-d"),
            OsString::from(format!("scenes={}", scenes_txt.display())),
        ],
    )?;

    let content = fs::read_to_string(scenes_txt)
        .map_err(|e| format!("Failed to read scene list {}: {e}", scenes_txt.display()))?;
    Ok(parse_scene_list(&content))
}

/// Parse a scene list: one frame index per line, blank lines ignored.
pub(crate) fn parse_scene_list(content: &str) -> Vec<u64> {
    let mut cuts: Vec<u64> = content
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

/// One-to-one greedy matching: count DV cuts that pair with an HDR cut
/// within `tolerance` frames once shifted by `offset`.
fn count_matches(dv_cuts: &[u64], hdr_cuts: &[u64], offset: i64, tolerance: i64) -> usize {
    let mut matches = 0usize;
    let mut j = 0usize;

    for &d in dv_cuts {
        let shifted = d as i64 - offset;
        while j < hdr_cuts.len() && (hdr_cuts[j] as i64) < shifted - tolerance {
            j += 1;
        }
        if j < hdr_cuts.len() && (hdr_cuts[j] as i64 - shifted).abs() <= tolerance {
            matches += 1;
            j += 1;
        }
    }

    matches
}

/// Correlate the two scene-cut lists via offset voting.
///
/// Acceptance requires: matches >= max(10, 5% of DV cuts), the best offset
/// dominating the runner-up cluster by >= 1.5x, and every measurable tercile
/// of the runtime agreeing with the global offset (catches different edits
/// that happen to line up at the start).
pub(crate) fn correlate_scene_cuts(
    dv_cuts: &[u64],
    hdr_cuts: &[u64],
    max_offset: i64,
    tolerance: i64,
) -> CorrelationReport {
    // scdet cannot tag frame 0 (no previous frame), but the RPU almost always
    // marks it as a scene start — drop it so it can't cast a bogus vote.
    let dv_cuts: Vec<u64> = dv_cuts.iter().copied().filter(|&c| c != 0).collect();
    let hdr_cuts: Vec<u64> = {
        let mut v = hdr_cuts.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    };

    let mut report = CorrelationReport {
        accepted: None,
        rejection: None,
        top_offsets: Vec::new(),
        dv_count: dv_cuts.len(),
        hdr_count: hdr_cuts.len(),
    };

    if dv_cuts.is_empty() || hdr_cuts.is_empty() {
        report.rejection = Some("one of the scene-cut lists is empty".to_string());
        return report;
    }

    // Offset voting over a sliding window of HDR cuts per DV cut.
    let mut votes: HashMap<i64, usize> = HashMap::new();
    let mut lo = 0usize;
    for &d in &dv_cuts {
        let d = d as i64;
        while lo < hdr_cuts.len() && (hdr_cuts[lo] as i64) < d - max_offset {
            lo += 1;
        }
        for &h in &hdr_cuts[lo..] {
            let h = h as i64;
            if h > d + max_offset {
                break;
            }
            *votes.entry(d - h).or_insert(0) += 1;
        }
    }

    if votes.is_empty() {
        report.rejection = Some(format!(
            "no cut pairs within max offset ({max_offset} frames)"
        ));
        return report;
    }

    // Cluster candidates: walk offsets by vote count, skipping any offset
    // adjacent (within 2*tolerance) to an already-taken candidate.
    let mut by_votes: Vec<(i64, usize)> = votes.iter().map(|(&o, &c)| (o, c)).collect();
    by_votes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.abs().cmp(&b.0.abs())));
    let mut candidates: Vec<i64> = Vec::new();
    for &(o, _) in &by_votes {
        if candidates.iter().all(|&c| (c - o).abs() > 2 * tolerance) {
            candidates.push(o);
        }
        if candidates.len() >= TOP_CANDIDATES {
            break;
        }
    }

    // Detection jitter smears a real offset's votes across o-1/o/o+1, so a
    // cluster's raw-vote representative can be off by one; refine each
    // candidate by exact match count over its immediate neighborhood.
    let candidates: Vec<i64> = candidates
        .iter()
        .map(|&o| {
            (o - tolerance..=o + tolerance)
                .max_by_key(|&x| {
                    // Ties broken by raw exact votes: a clean run at offset o
                    // also fully matches at o±tolerance, but only o has votes.
                    (
                        count_matches(&dv_cuts, &hdr_cuts, x, tolerance),
                        votes.get(&x).copied().unwrap_or(0),
                    )
                })
                .unwrap_or(o)
        })
        .collect();

    let mut scored: Vec<(i64, usize)> = candidates
        .iter()
        .map(|&o| (o, count_matches(&dv_cuts, &hdr_cuts, o, tolerance)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.abs().cmp(&b.0.abs())));
    report.top_offsets = scored.clone();

    let (best_offset, best_matches) = scored[0];
    let runner_up = scored.get(1).map(|&(_, m)| m).unwrap_or(0);
    let dominance = if runner_up == 0 {
        f64::INFINITY
    } else {
        best_matches as f64 / runner_up as f64
    };
    let match_ratio = best_matches as f64 / dv_cuts.len() as f64;

    // Per-tercile agreement over the DV timeline.
    let span = *dv_cuts.last().unwrap() as i64 + 1;
    let mut tercile_offsets: [Option<i64>; 3] = [None; 3];
    for (k, slot) in tercile_offsets.iter_mut().enumerate() {
        let lo_f = span * k as i64 / 3;
        let hi_f = span * (k as i64 + 1) / 3;
        let part: Vec<u64> = dv_cuts
            .iter()
            .copied()
            .filter(|&c| (c as i64) >= lo_f && (c as i64) < hi_f)
            .collect();
        if part.len() < TERCILE_MIN_CUTS {
            continue;
        }
        let best = candidates
            .iter()
            .map(|&o| (o, count_matches(&part, &hdr_cuts, o, tolerance)))
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.abs().cmp(&a.0.abs())));
        if let Some((o, m)) = best {
            if m >= 2 {
                *slot = Some(o);
            }
        }
    }

    let min_matches = 10usize.max((dv_cuts.len() as f64 * 0.05).ceil() as usize);
    let result = SyncResult {
        offset: best_offset,
        matches: best_matches,
        match_ratio,
        dominance,
        tercile_offsets,
    };

    if best_matches < min_matches {
        report.rejection = Some(format!(
            "only {best_matches} matching cuts at best offset {best_offset} (need >= {min_matches})"
        ));
    } else if dominance < 1.5 {
        report.rejection = Some(format!(
            "ambiguous offset: best {best_offset} ({best_matches} matches) vs runner-up ({runner_up} matches), dominance {dominance:.2} < 1.5"
        ));
    } else if let Some(bad) = tercile_offsets
        .iter()
        .enumerate()
        .find(|(_, t)| t.is_some() && **t != Some(best_offset))
    {
        report.rejection = Some(format!(
            "tercile {} disagrees: offset {:?} vs global {best_offset} (sources are likely different edits)",
            bad.0 + 1,
            bad.1.unwrap()
        ));
    } else if tercile_offsets.iter().flatten().count() == 0 {
        report.rejection =
            Some("no tercile has enough scene cuts to confirm the offset".to_string());
    } else {
        report.accepted = Some(result);
        return report;
    }

    report
}

const LOCAL_WINDOW_CUTS: usize = 20;
const LOCAL_WINDOW_STEP: usize = 10;
const LOCAL_MIN_CUTS: usize = 10;
const LOCAL_TOP_CANDIDATES: usize = 64;
const LOCAL_WORK_BUDGET: usize = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum TemporalEvidenceStatus {
    Consistent,
    Contradiction,
    InsufficientEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct MatchedAnchor {
    pub(crate) dv_frame: u64,
    pub(crate) hdr_frame: u64,
    pub(crate) residual: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum EvidenceStream {
    Dv,
    Hdr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum CoverageGapKind {
    NoMatchedAnchors,
    Leading,
    BetweenAnchors,
    Trailing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct UnverifiedInterval {
    pub(crate) stream: EvidenceStream,
    pub(crate) start_frame: u64,
    pub(crate) end_frame: u64,
    pub(crate) kind: CoverageGapKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct LocalOffsetContradiction {
    pub(crate) dv_start: u64,
    pub(crate) dv_end: u64,
    pub(crate) expected_offset: i64,
    pub(crate) alternate_offset: i64,
    pub(crate) expected_matches: usize,
    pub(crate) alternate_matches: usize,
    pub(crate) window_cuts: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct TemporalEvidence {
    pub(crate) status: TemporalEvidenceStatus,
    pub(crate) dv_frames: u64,
    pub(crate) hdr_frames: u64,
    pub(crate) estimated_offset: i64,
    pub(crate) max_offset: i64,
    pub(crate) tolerance: i64,
    pub(crate) matched_anchors: Vec<MatchedAnchor>,
    pub(crate) unverified_intervals: Vec<UnverifiedInterval>,
    pub(crate) contradictions: Vec<LocalOffsetContradiction>,
    /// Set when local alternate-offset inspection exhausted its bounded work
    /// budget; a limited assessment cannot be reported as consistent.
    pub(crate) analysis_limit: Option<String>,
}

/// Assess temporal evidence across the complete timelines.
///
/// Scene anchors establish local correspondence only.  The returned gaps
/// cover the full DV and HDR frame ranges, including leading, between-anchor,
/// and trailing regions.  A strong alternate offset in a local window is a
/// contradiction; sparse or unmatched anchors alone remain insufficient
/// evidence and are never treated as a contradiction.
pub(crate) fn assess_temporal_alignment(
    dv_cuts: &[u64],
    hdr_cuts: &[u64],
    dv_frames: u64,
    hdr_frames: u64,
    estimated_offset: i64,
    max_offset: i64,
    tolerance: i64,
) -> TemporalEvidence {
    let tolerance = tolerance.max(0);
    let dv = normalize_cuts(dv_cuts, dv_frames);
    let hdr = normalize_cuts(hdr_cuts, hdr_frames);
    let anchors = matched_anchors(&dv, &hdr, estimated_offset, tolerance);
    let (contradictions, analysis_limit) =
        local_offset_contradictions(&dv, &hdr, estimated_offset, max_offset.max(0), tolerance);
    let mut gaps = timeline_gaps(
        EvidenceStream::Dv,
        dv_frames,
        anchors.iter().map(|a| a.dv_frame),
    );
    gaps.extend(timeline_gaps(
        EvidenceStream::Hdr,
        hdr_frames,
        anchors.iter().map(|a| a.hdr_frame),
    ));

    let minimum_anchors = 10usize.max((dv.len() as f64 * 0.05).ceil() as usize);
    let status = if !contradictions.is_empty() {
        TemporalEvidenceStatus::Contradiction
    } else if analysis_limit.is_some()
        || dv_frames == 0
        || hdr_frames == 0
        || anchors.len() < minimum_anchors
    {
        TemporalEvidenceStatus::InsufficientEvidence
    } else {
        TemporalEvidenceStatus::Consistent
    };

    TemporalEvidence {
        status,
        dv_frames,
        hdr_frames,
        estimated_offset,
        max_offset,
        tolerance,
        matched_anchors: anchors,
        unverified_intervals: gaps,
        contradictions,
        analysis_limit,
    }
}

fn normalize_cuts(cuts: &[u64], frames: u64) -> Vec<u64> {
    let mut normalized: Vec<u64> = cuts
        .iter()
        .copied()
        .filter(|&cut| frames == 0 || cut < frames)
        .filter(|&cut| cut != 0)
        .collect();
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

fn matched_anchors(
    dv_cuts: &[u64],
    hdr_cuts: &[u64],
    offset: i64,
    tolerance: i64,
) -> Vec<MatchedAnchor> {
    let mut anchors = Vec::new();
    let mut hdr_index = 0usize;
    for &dv_frame in dv_cuts {
        let target = dv_frame as i128 - offset as i128;
        while hdr_index < hdr_cuts.len()
            && (hdr_cuts[hdr_index] as i128) < target - tolerance as i128
        {
            hdr_index += 1;
        }
        if hdr_index >= hdr_cuts.len() {
            break;
        }
        let hdr_frame = hdr_cuts[hdr_index];
        let residual = dv_frame as i128 - hdr_frame as i128 - offset as i128;
        if residual.abs() <= tolerance as i128 {
            anchors.push(MatchedAnchor {
                dv_frame,
                hdr_frame,
                residual: residual as i64,
            });
            hdr_index += 1;
        }
    }
    anchors
}

fn local_offset_contradictions(
    dv_cuts: &[u64],
    hdr_cuts: &[u64],
    expected_offset: i64,
    max_offset: i64,
    tolerance: i64,
) -> (Vec<LocalOffsetContradiction>, Option<String>) {
    if dv_cuts.len() < LOCAL_MIN_CUTS || hdr_cuts.is_empty() {
        return (Vec::new(), None);
    }

    let window_size = LOCAL_WINDOW_CUTS.min(dv_cuts.len());
    let final_start = dv_cuts.len() - window_size;

    let mut findings = Vec::new();
    let mut work_remaining = LOCAL_WORK_BUDGET;
    let mut analysis_limited = false;
    let mut start = 0usize;
    loop {
        let window = &dv_cuts[start..start + window_size];
        let expected_matches = match matched_anchor_count_bounded(
            window,
            hdr_cuts,
            expected_offset,
            tolerance,
            &mut work_remaining,
        ) {
            Ok(matches) => matches,
            Err(()) => {
                analysis_limited = true;
                break;
            }
        };
        let alternate = match best_local_alternate(
            window,
            hdr_cuts,
            expected_offset,
            max_offset,
            tolerance,
            &mut work_remaining,
        ) {
            Ok(alternate) => alternate,
            Err(()) => {
                analysis_limited = true;
                break;
            }
        };
        let Some((alternate_offset, alternate_matches)) = alternate else {
            if start == final_start {
                break;
            }
            start = (start + LOCAL_WINDOW_STEP).min(final_start);
            continue;
        };
        let strong_minimum = LOCAL_MIN_CUTS.min(window.len());
        let beats_expected = alternate_matches >= expected_matches.saturating_add(3)
            && alternate_matches.saturating_mul(2) >= expected_matches.max(1).saturating_mul(3);
        if alternate_matches >= strong_minimum && beats_expected {
            findings.push(LocalOffsetContradiction {
                dv_start: window[0],
                dv_end: *window.last().unwrap(),
                expected_offset,
                alternate_offset,
                expected_matches,
                alternate_matches,
                window_cuts: window.len(),
            });
        }
        if start == final_start {
            break;
        }
        start = (start + LOCAL_WINDOW_STEP).min(final_start);
    }

    findings.sort_by_key(|finding| (finding.dv_start, finding.dv_end));
    let mut merged: Vec<LocalOffsetContradiction> = Vec::new();
    for finding in findings {
        if let Some(previous) = merged.last_mut() {
            if previous.expected_offset == finding.expected_offset
                && previous.alternate_offset == finding.alternate_offset
                && finding.dv_start <= previous.dv_end.saturating_add(1)
            {
                previous.dv_end = previous.dv_end.max(finding.dv_end);
                previous.expected_matches = previous.expected_matches.max(finding.expected_matches);
                previous.alternate_matches =
                    previous.alternate_matches.max(finding.alternate_matches);
                previous.window_cuts = previous.window_cuts.max(finding.window_cuts);
                continue;
            }
        }
        merged.push(finding);
    }
    let analysis_limit = analysis_limited.then(|| {
        format!(
            "local offset inspection stopped after {LOCAL_WORK_BUDGET} bounded comparisons; temporal evidence is incomplete"
        )
    });
    (merged, analysis_limit)
}

fn best_local_alternate(
    window: &[u64],
    hdr_cuts: &[u64],
    expected_offset: i64,
    max_offset: i64,
    tolerance: i64,
    work_remaining: &mut usize,
) -> Result<Option<(i64, usize)>, ()> {
    let mut votes: HashMap<i64, usize> = HashMap::new();
    for &dv_frame in window {
        let (start, end) = hdr_candidate_range(hdr_cuts, dv_frame, max_offset);
        for &hdr_frame in &hdr_cuts[start..end] {
            consume_local_work(work_remaining)?;
            let delta = dv_frame as i128 - hdr_frame as i128;
            if let Ok(offset) = i64::try_from(delta) {
                *votes.entry(offset).or_insert(0) += 1;
            }
        }
    }

    // Keep only the strongest raw-vote candidates.  Scoring every distinct
    // offset would make dense, highly permissive inputs expensive, while a
    // single raw winner can miss tolerance-supported support around it.
    let mut candidates = Vec::with_capacity(LOCAL_TOP_CANDIDATES);
    for (&offset, &count) in &votes {
        candidates.push((offset, count));
        candidates.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.abs().cmp(&b.0.abs())));
        if candidates.len() > LOCAL_TOP_CANDIDATES {
            candidates.pop();
        }
    }

    let mut scored_offsets = Vec::new();
    let mut best: Option<(i64, usize, usize)> = None;
    let radius = tolerance.min(4);
    for (candidate, _) in candidates {
        if scored_offsets
            .iter()
            .any(|&seen: &i64| (candidate as i128 - seen as i128).abs() <= 2 * tolerance as i128)
        {
            continue;
        }
        scored_offsets.push(candidate);
        for delta in -radius..=radius {
            let offset = candidate.saturating_add(delta);
            if (offset as i128 - expected_offset as i128).abs() <= 2 * tolerance as i128 {
                continue;
            }
            let support =
                matched_anchor_count_bounded(window, hdr_cuts, offset, tolerance, work_remaining)?;
            if support == 0 {
                continue;
            }
            let exact_votes = votes.get(&offset).copied().unwrap_or(0);
            if best.is_none_or(|(best_offset, best_support, best_votes)| {
                support > best_support
                    || (support == best_support
                        && (exact_votes > best_votes
                            || (exact_votes == best_votes && offset.abs() < best_offset.abs())))
            }) {
                best = Some((offset, support, exact_votes));
            }
        }
    }
    Ok(best.map(|(offset, support, _)| (offset, support)))
}

fn hdr_candidate_range(hdr_cuts: &[u64], dv_frame: u64, max_offset: i64) -> (usize, usize) {
    let radius = max_offset as u64;
    let lower = dv_frame.saturating_sub(radius);
    let upper = dv_frame.saturating_add(radius);
    let start = hdr_cuts.partition_point(|&hdr_frame| hdr_frame < lower);
    let end = hdr_cuts.partition_point(|&hdr_frame| hdr_frame <= upper);
    (start, end)
}

fn matched_anchor_count_bounded(
    dv_cuts: &[u64],
    hdr_cuts: &[u64],
    offset: i64,
    tolerance: i64,
    work_remaining: &mut usize,
) -> Result<usize, ()> {
    let Some(&first_dv) = dv_cuts.first() else {
        return Ok(0);
    };
    let first_target = first_dv as i128 - offset as i128;
    let mut hdr_index = hdr_cuts
        .partition_point(|&hdr_frame| (hdr_frame as i128) < first_target - tolerance as i128);
    let mut matches = 0usize;
    for &dv_frame in dv_cuts {
        let target = dv_frame as i128 - offset as i128;
        while hdr_index < hdr_cuts.len()
            && (hdr_cuts[hdr_index] as i128) < target - tolerance as i128
        {
            consume_local_work(work_remaining)?;
            hdr_index += 1;
        }
        if hdr_index >= hdr_cuts.len() {
            break;
        }
        consume_local_work(work_remaining)?;
        let residual = dv_frame as i128 - hdr_cuts[hdr_index] as i128 - offset as i128;
        if residual.abs() <= tolerance as i128 {
            matches += 1;
            hdr_index += 1;
        }
    }
    Ok(matches)
}

fn consume_local_work(work_remaining: &mut usize) -> Result<(), ()> {
    if *work_remaining == 0 {
        return Err(());
    }
    *work_remaining -= 1;
    Ok(())
}

fn timeline_gaps<I>(stream: EvidenceStream, frames: u64, anchors: I) -> Vec<UnverifiedInterval>
where
    I: Iterator<Item = u64>,
{
    if frames == 0 {
        return Vec::new();
    }
    let mut anchors: Vec<u64> = anchors.filter(|&frame| frame < frames).collect();
    anchors.sort_unstable();
    anchors.dedup();
    if anchors.is_empty() {
        return vec![UnverifiedInterval {
            stream,
            start_frame: 0,
            end_frame: frames - 1,
            kind: CoverageGapKind::NoMatchedAnchors,
        }];
    }

    let mut gaps = Vec::new();
    if anchors[0] > 0 {
        gaps.push(UnverifiedInterval {
            stream,
            start_frame: 0,
            end_frame: anchors[0] - 1,
            kind: CoverageGapKind::Leading,
        });
    }
    for pair in anchors.windows(2) {
        if pair[1] > pair[0].saturating_add(1) {
            gaps.push(UnverifiedInterval {
                stream,
                start_frame: pair[0] + 1,
                end_frame: pair[1] - 1,
                kind: CoverageGapKind::BetweenAnchors,
            });
        }
    }
    if anchors.last().copied().unwrap() < frames - 1 {
        gaps.push(UnverifiedInterval {
            stream,
            start_frame: anchors.last().copied().unwrap() + 1,
            end_frame: frames - 1,
            kind: CoverageGapKind::Trailing,
        });
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cuts_with_offset(base: &[u64], offset: i64) -> Vec<u64> {
        base.iter()
            .filter_map(|&c| {
                let v = c as i64 + offset;
                (v >= 0).then_some(v as u64)
            })
            .collect()
    }

    /// Pseudo-random but deterministic cut list: irregular spacing.
    fn synthetic_cuts(n: usize) -> Vec<u64> {
        let mut cuts = Vec::with_capacity(n);
        let mut f = 40u64;
        for i in 0..n {
            // spacing 48..~340 frames, varies deterministically
            f += 48 + (i as u64 * 7919) % 293;
            cuts.push(f);
        }
        cuts
    }

    #[test]
    fn parse_scene_list_basic() {
        assert_eq!(parse_scene_list("0\n96\n192\n\n96\n"), vec![0, 96, 192]);
        assert!(parse_scene_list("garbage\n").is_empty());
    }

    #[test]
    fn exact_offset_recovered_with_partial_overlap_and_noise() {
        let dv = synthetic_cuts(120);
        // HDR sees 60% of DV cuts, jittered by ±1, plus false positives.
        let mut hdr: Vec<u64> = cuts_with_offset(&dv, -25)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 5 != 0 && i % 7 != 0)
            .map(|(i, c)| c.wrapping_add((i as u64 % 3).wrapping_sub(1)))
            .collect();
        // 30% extra false-positive cuts between real ones
        let fps: Vec<u64> = dv.iter().step_by(3).map(|&c| c + 17).collect();
        hdr.extend(cuts_with_offset(&fps, -25));
        hdr.sort_unstable();

        let report = correlate_scene_cuts(&dv, &hdr, 7200, 1);
        let sync = report.accepted.expect("should accept");
        assert_eq!(sync.offset, 25);
        assert!(sync.matches >= 60, "matches = {}", sync.matches);
        assert_eq!(sync.tercile_offsets, [Some(25), Some(25), Some(25)]);

        let evidence = assess_temporal_alignment(
            &dv,
            &hdr,
            dv.last().copied().unwrap() + 100,
            hdr.last().copied().unwrap() + 100,
            sync.offset,
            7200,
            1,
        );
        assert!(
            evidence.contradictions.is_empty(),
            "unexpected local contradiction: {:?}",
            evidence.contradictions
        );
    }

    #[test]
    fn negative_offset_recovered() {
        let dv = synthetic_cuts(80);
        let hdr = cuts_with_offset(&dv, 40); // HDR leads → DV needs padding
        let report = correlate_scene_cuts(&dv, &hdr, 7200, 1);
        assert_eq!(report.accepted.expect("should accept").offset, -40);
    }

    #[test]
    fn periodic_cuts_rejected_as_ambiguous() {
        // Perfectly periodic cuts: every offset multiple of the period ties.
        let dv: Vec<u64> = (1..200).map(|i| i * 96).collect();
        let hdr = dv.clone();
        let report = correlate_scene_cuts(&dv, &hdr, 7200, 1);
        assert!(report.accepted.is_none());
        assert!(report.rejection.unwrap().contains("ambiguous"));
    }

    #[test]
    fn different_edit_rejected_by_tercile() {
        // First two thirds align at offset 0, last third shifts by 200
        // (e.g. an extended cut with an extra mid-film scene).
        let dv = synthetic_cuts(120);
        let span = *dv.last().unwrap();
        let hdr: Vec<u64> = dv
            .iter()
            .map(|&c| if c > span * 2 / 3 { c + 200 } else { c })
            .collect();
        let report = correlate_scene_cuts(&dv, &hdr, 7200, 1);
        assert!(report.accepted.is_none());
        let why = report.rejection.unwrap();
        assert!(
            why.contains("tercile") || why.contains("ambiguous"),
            "{why}"
        );
    }

    #[test]
    fn too_few_matches_rejected() {
        let dv = vec![100, 300, 500];
        let hdr = vec![100, 300, 500];
        let report = correlate_scene_cuts(&dv, &hdr, 7200, 1);
        assert!(report.accepted.is_none());
        assert!(report.rejection.unwrap().contains("matching cuts"));
    }

    #[test]
    fn frame_zero_dropped_from_dv() {
        let mut dv = synthetic_cuts(60);
        dv.insert(0, 0);
        let hdr = cuts_with_offset(&synthetic_cuts(60), 0);
        let report = correlate_scene_cuts(&dv, &hdr, 7200, 1);
        assert_eq!(report.dv_count, 60);
        assert_eq!(report.accepted.expect("should accept").offset, 0);
    }

    #[test]
    fn temporal_evidence_detects_a_twenty_cut_local_shift() {
        let dv = synthetic_cuts(300);
        let mut hdr = dv.clone();
        for frame in &mut hdr[120..140] {
            *frame += 48;
        }
        hdr.sort_unstable();

        let evidence = assess_temporal_alignment(
            &dv,
            &hdr,
            dv.last().copied().unwrap() + 200,
            hdr.last().copied().unwrap() + 200,
            0,
            7200,
            1,
        );
        assert_eq!(evidence.status, TemporalEvidenceStatus::Contradiction);
        assert!(
            evidence
                .contradictions
                .iter()
                .any(|finding| finding.alternate_offset.abs() == 48
                    && finding.alternate_matches >= 10),
            "{evidence:?}"
        );
    }

    #[test]
    fn temporal_evidence_accepts_a_global_offset_with_anchor_gaps() {
        let dv = synthetic_cuts(80);
        let hdr = cuts_with_offset(&dv, -25);
        let evidence = assess_temporal_alignment(
            &dv,
            &hdr,
            dv.last().copied().unwrap() + 100,
            hdr.last().copied().unwrap() + 100,
            25,
            7200,
            1,
        );
        assert_eq!(evidence.status, TemporalEvidenceStatus::Consistent);
        assert_eq!(evidence.matched_anchors.len(), 80);
        assert!(evidence.contradictions.is_empty());
        assert!(evidence
            .unverified_intervals
            .iter()
            .any(|gap| gap.kind == CoverageGapKind::Leading));
    }

    #[test]
    fn temporal_evidence_marks_sparse_cuts_insufficient_without_contradiction() {
        let cuts = vec![100, 400];
        let evidence = assess_temporal_alignment(&cuts, &cuts, 1000, 1000, 0, 7200, 1);
        assert_eq!(
            evidence.status,
            TemporalEvidenceStatus::InsufficientEvidence
        );
        assert!(evidence.contradictions.is_empty());
        assert!(evidence
            .unverified_intervals
            .iter()
            .any(|gap| gap.kind == CoverageGapKind::NoMatchedAnchors
                || gap.kind == CoverageGapKind::Leading));
    }

    #[test]
    fn temporal_evidence_reports_added_tail_to_full_hdr_timeline() {
        let cuts = vec![100, 400, 800];
        let evidence = assess_temporal_alignment(&cuts, &cuts, 1000, 1500, 0, 7200, 1);
        assert!(evidence.unverified_intervals.iter().any(|gap| {
            gap.stream == EvidenceStream::Hdr
                && gap.kind == CoverageGapKind::Trailing
                && gap.start_frame == 801
                && gap.end_frame == 1499
        }));
    }

    #[test]
    fn six_noisy_local_cuts_are_not_called_a_contradiction() {
        let dv = synthetic_cuts(80);
        let mut hdr = dv.clone();
        for frame in &mut hdr[30..36] {
            *frame += 5;
        }
        hdr.sort_unstable();
        let evidence = assess_temporal_alignment(
            &dv,
            &hdr,
            dv.last().copied().unwrap() + 100,
            hdr.last().copied().unwrap() + 100,
            0,
            7200,
            1,
        );
        assert!(evidence.contradictions.is_empty());
    }

    #[test]
    fn temporal_evidence_completes_a_normal_two_thousand_cut_input() {
        let dv = synthetic_cuts(2_000);
        let frames = dv.last().copied().unwrap() + 100;
        let evidence = assess_temporal_alignment(&dv, &dv, frames, frames, 0, 7200, 1);
        assert_eq!(evidence.status, TemporalEvidenceStatus::Consistent);
        assert!(
            evidence.analysis_limit.is_none(),
            "unexpected boundedness limit: {:?}",
            evidence.analysis_limit
        );
    }

    #[test]
    fn temporal_evidence_marks_dense_input_incomplete_at_work_limit() {
        let dv = synthetic_cuts(2_000);
        let hdr: Vec<u64> = (1..=500_000).collect();
        let evidence = assess_temporal_alignment(
            &dv,
            &hdr,
            dv.last().copied().unwrap() + 100,
            500_100,
            0,
            7200,
            1,
        );
        assert!(
            evidence.analysis_limit.is_some(),
            "dense input unexpectedly completed: status={:?}",
            evidence.status
        );
        assert_ne!(evidence.status, TemporalEvidenceStatus::Consistent);
    }
}
