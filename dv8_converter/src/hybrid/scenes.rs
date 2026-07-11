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
}
