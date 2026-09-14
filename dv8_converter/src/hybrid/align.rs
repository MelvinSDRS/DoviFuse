use crate::exec::AppResult;

#[derive(Clone)]
pub(crate) struct DuplicateOp {
    pub(crate) source: u64,
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

#[derive(Default, Clone)]
pub(crate) struct AlignmentStrategy {
    pub(crate) action: String,
    pub(crate) description: String,
    pub(crate) remove_ranges: Vec<String>,
    pub(crate) duplicates: Vec<DuplicateOp>,
    pub(crate) high_risk: bool,
    /// Start-of-file offset (dv_frame = hdr_frame + start_offset), used to
    /// shift measurement windows in the grade check.
    pub(crate) start_offset: i64,
}

/// Exact alignment from a scene-cut-verified offset: dv_frame = hdr_frame + offset.
///
/// dovi_tool editor semantics this must mirror: `remove` ranges are in the
/// ORIGINAL RPU index space and are applied first (list is compacted after);
/// `duplicate` ops then apply in post-remove index space, offset-descending,
/// so multiple duplicates don't invalidate each other's indices.
pub(crate) fn alignment_from_offset(
    offset: i64,
    dv_frames: u64,
    hdr_frames: u64,
) -> AppResult<AlignmentStrategy> {
    if dv_frames == 0 || hdr_frames == 0 {
        return Err("Cannot align: zero frame count".to_string());
    }
    if offset >= 0 && offset as u64 >= dv_frames {
        return Err(format!(
            "Sync offset {offset} consumes the entire DV RPU ({dv_frames} frames)"
        ));
    }
    let start_pad = offset.unsigned_abs();
    if offset < 0 && start_pad >= hdr_frames {
        return Err(format!(
            "Sync offset {offset} exceeds HDR target length ({hdr_frames} frames)"
        ));
    }

    let mut s = AlignmentStrategy {
        action: "scene_sync".to_string(),
        start_offset: offset,
        ..Default::default()
    };
    let mut parts: Vec<String> = Vec::new();

    // Frames available after the start adjustment (before end adjustment).
    let after_start = if offset >= 0 {
        dv_frames
            .checked_sub(offset as u64)
            .ok_or_else(|| format!("Sync offset {offset} consumes the entire DV RPU"))?
    } else {
        dv_frames
            .checked_add(start_pad)
            .ok_or_else(|| "Sync offset overflows the DV frame count".to_string())?
    };

    match offset.cmp(&0) {
        std::cmp::Ordering::Greater => {
            s.remove_ranges.push(format!("0-{}", offset - 1));
            parts.push(format!("trim {offset} RPU frames from start"));
        }
        std::cmp::Ordering::Less => {
            s.duplicates.push(DuplicateOp {
                source: 0,
                offset: 0,
                length: start_pad,
            });
            parts.push(format!("pad {start_pad} duplicated frames at start"));
        }
        std::cmp::Ordering::Equal => {}
    }

    match after_start.cmp(&hdr_frames) {
        std::cmp::Ordering::Greater => {
            // End trim in ORIGINAL index space. Kept original indices are
            // [max(offset,0), hdr_frames + offset): with a start pad of k
            // frames only hdr - k originals are needed, hence the same
            // hdr + offset bound.
            let excess = after_start - hdr_frames;
            let start = if offset >= 0 {
                hdr_frames
                    .checked_add(offset as u64)
                    .ok_or_else(|| "Sync offset overflows the HDR frame count".to_string())?
            } else {
                hdr_frames.checked_sub(start_pad).ok_or_else(|| {
                    "Sync offset exceeds the HDR frame count after end trim".to_string()
                })?
            };
            s.remove_ranges.push(format!("{start}-{}", dv_frames - 1));
            parts.push(format!("trim {excess} frames from end"));
        }
        std::cmp::Ordering::Less => {
            // End pad in post-remove index space (start-pad duplicates apply
            // at a lower offset, hence after this one under offset-descending
            // order).
            let missing = hdr_frames - after_start;
            let kept = if offset >= 0 {
                dv_frames
                    .checked_sub(offset as u64)
                    .ok_or_else(|| "Sync offset consumes the DV RPU".to_string())?
            } else {
                dv_frames
            };
            s.duplicates.push(DuplicateOp {
                source: kept - 1,
                offset: kept,
                length: missing,
            });
            parts.push(format!("pad {missing} duplicated frames at end"));
        }
        std::cmp::Ordering::Equal => {}
    }

    if parts.is_empty() {
        parts.push("no adjustment needed".to_string());
    }
    if !s.duplicates.is_empty() {
        s.high_risk = true;
    }
    s.description = format!(
        "Scene-sync offset {offset:+}: {} ({dv_frames} -> {hdr_frames} frames)",
        parts.join(", ")
    );

    Ok(s)
}

/// Equal-frame-count fallback when no scene-cut evidence is available. A
/// differing count cannot establish an offset, so the caller must provide
/// `--offset` and pass it through `alignment_from_offset` explicitly.
pub(crate) fn compute_alignment_framecount(
    dv_frames: u64,
    hdr_frames: u64,
    _fps: f64,
) -> AppResult<AlignmentStrategy> {
    if dv_frames == 0 || hdr_frames == 0 {
        return Err("Cannot align: zero frame count".to_string());
    }
    if dv_frames != hdr_frames {
        return Err(format!(
            "Frame counts differ (DV {dv_frames}, HDR {hdr_frames}); provide an explicit --offset"
        ));
    }

    Ok(AlignmentStrategy {
        action: "none".to_string(),
        description: format!(
            "Frame counts match ({dv_frames}); offset 0 is unverified without scene-cut evidence"
        ),
        ..Default::default()
    })
}

/// Require explicit operator approval before applying duplicated edge
/// metadata. `--force` is intentionally not part of this decision.
pub(crate) fn require_padding_review(
    strategy: &AlignmentStrategy,
    allow_padding: bool,
) -> AppResult<()> {
    if strategy.duplicates.is_empty() || allow_padding {
        return Ok(());
    }

    Err(format!(
        "Alignment requires duplicated edge metadata: {}. Review it and re-run with --allow-padding",
        strategy.description
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate the editor: apply removes (original index space) then
    /// duplicates (post-remove space, offset-descending) and return the
    /// resulting sequence of original frame indices (u64::MAX-tagged for pads
    /// keeps it simple: pads copy an existing element).
    fn simulate_editor(s: &AlignmentStrategy, dv_frames: u64) -> Vec<u64> {
        let mut data: Vec<u64> = (0..dv_frames).collect();
        let mut drop = vec![false; dv_frames as usize];
        for range in &s.remove_ranges {
            let (a, b) = range.split_once('-').unwrap();
            let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
            assert!(b < dv_frames as usize, "remove out of range: {range}");
            for d in drop.iter_mut().take(b + 1).skip(a) {
                *d = true;
            }
        }
        data = data
            .into_iter()
            .zip(&drop)
            .filter(|(_, &d)| !d)
            .map(|(v, _)| v)
            .collect();
        let mut dups = s.duplicates.clone();
        dups.sort_by(|a, b| b.offset.cmp(&a.offset));
        for d in &dups {
            assert!(
                (d.source as usize) < data.len() && d.offset as usize <= data.len(),
                "duplicate op out of range"
            );
            let v = data[d.source as usize];
            for _ in 0..d.length {
                data.insert(d.offset as usize, v);
            }
        }
        data
    }

    #[test]
    fn offset_positive_with_end_trim() {
        // DV has 25 extra frames at start and 5 extra at end.
        let s = alignment_from_offset(25, 1030, 1000).unwrap();
        let out = simulate_editor(&s, 1030);
        assert_eq!(out.len(), 1000);
        assert_eq!(out[0], 25);
        assert_eq!(*out.last().unwrap(), 1024);
    }

    #[test]
    fn offset_positive_with_end_pad() {
        // DV has 25 extra at start but ends 10 frames short.
        let s = alignment_from_offset(25, 1015, 1000).unwrap();
        let out = simulate_editor(&s, 1015);
        assert_eq!(out.len(), 1000);
        assert_eq!(out[0], 25);
        assert_eq!(out[989], 1014);
        assert_eq!(*out.last().unwrap(), 1014); // padded from last kept frame
    }

    #[test]
    fn offset_negative_with_end_trim() {
        // HDR has 40 extra frames at start; DV runs long at the end.
        let s = alignment_from_offset(-40, 1000, 1000).unwrap();
        let out = simulate_editor(&s, 1000);
        assert_eq!(out.len(), 1000);
        assert_eq!(out[0], 0);
        assert_eq!(out[39], 0); // 40-frame start pad copies frame 0
        assert_eq!(out[40], 0);
        assert_eq!(out[41], 1);
        assert_eq!(*out.last().unwrap(), 959);
    }

    #[test]
    fn offset_negative_with_end_pad() {
        // HDR has 40 extra at start AND 60 extra at end.
        let s = alignment_from_offset(-40, 1000, 1100).unwrap();
        let out = simulate_editor(&s, 1000);
        assert_eq!(out.len(), 1100);
        assert_eq!(out[40], 0);
        assert_eq!(out[1039], 999);
        assert_eq!(*out.last().unwrap(), 999);
    }

    #[test]
    fn offset_zero_exact_match() {
        let s = alignment_from_offset(0, 1000, 1000).unwrap();
        assert!(s.remove_ranges.is_empty() && s.duplicates.is_empty());
        assert_eq!(s.action, "scene_sync");
    }

    #[test]
    fn offset_degenerate_rejected() {
        assert!(alignment_from_offset(1000, 1000, 500).is_err());
        assert!(alignment_from_offset(-500, 1000, 500).is_err());
        assert!(alignment_from_offset(0, 0, 500).is_err());
    }

    #[test]
    fn framecount_equal_counts_are_unverified_offset_zero() {
        let s = compute_alignment_framecount(1000, 1000, 23.976).unwrap();
        assert_eq!(s.action, "none");
        assert_eq!(s.start_offset, 0);
        assert!(s.description.contains("unverified"));
        assert!(s.remove_ranges.is_empty() && s.duplicates.is_empty());
    }

    #[test]
    fn framecount_unequal_counts_require_explicit_offset() {
        assert!(compute_alignment_framecount(1010, 1000, 23.976).is_err());
        assert!(compute_alignment_framecount(1000, 1010, 23.976).is_err());
    }

    #[test]
    fn framecount_zero_frames_rejected() {
        // A zero count must never reach the bucket heuristics: a short file
        // would get a remove-everything config, a long one an unverified
        // "leave as-is".
        assert!(compute_alignment_framecount(0, 1000, 23.976).is_err());
        assert!(compute_alignment_framecount(1000, 0, 23.976).is_err());
    }

    #[test]
    fn duplicated_edges_are_high_risk_and_require_review() {
        let strategy = alignment_from_offset(-40, 1000, 1100).unwrap();
        assert!(!strategy.duplicates.is_empty());
        assert!(strategy.high_risk);
        let error = require_padding_review(&strategy, false).unwrap_err();
        assert!(error.contains(&strategy.description));
        assert!(error.contains("--allow-padding"));
        assert!(require_padding_review(&strategy, true).is_ok());
    }

    #[test]
    fn no_padding_needs_no_review() {
        let strategy = alignment_from_offset(0, 1000, 1000).unwrap();
        assert!(require_padding_review(&strategy, false).is_ok());
    }

    #[test]
    fn offset_overflow_and_extreme_negative_are_rejected() {
        assert!(alignment_from_offset(i64::MIN, u64::MAX, u64::MAX).is_err());
        assert!(alignment_from_offset(i64::MAX, i64::MAX as u64, u64::MAX).is_err());
    }
}
