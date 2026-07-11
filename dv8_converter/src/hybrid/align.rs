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
    if offset >= dv_frames as i64 {
        return Err(format!(
            "Sync offset {offset} consumes the entire DV RPU ({dv_frames} frames)"
        ));
    }
    if offset < 0 && (-offset) as u64 >= hdr_frames {
        return Err(format!(
            "Sync offset {offset} exceeds HDR target length ({hdr_frames} frames)"
        ));
    }

    let mut s = AlignmentStrategy {
        action: "scene_sync".to_string(),
        ..Default::default()
    };
    let mut parts: Vec<String> = Vec::new();

    // Frames available after the start adjustment (before end adjustment).
    let after_start = (dv_frames as i64 + (-offset)) as u64;

    match offset.cmp(&0) {
        std::cmp::Ordering::Greater => {
            s.remove_ranges.push(format!("0-{}", offset - 1));
            parts.push(format!("trim {offset} RPU frames from start"));
        }
        std::cmp::Ordering::Less => {
            let k = (-offset) as u64;
            s.duplicates.push(DuplicateOp {
                source: 0,
                offset: 0,
                length: k,
            });
            parts.push(format!("pad {k} duplicated frames at start"));
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
            let start = (hdr_frames as i64 + offset) as u64;
            s.remove_ranges.push(format!("{start}-{}", dv_frames - 1));
            parts.push(format!("trim {excess} frames from end"));
        }
        std::cmp::Ordering::Less => {
            // End pad in post-remove index space (start-pad duplicates apply
            // at a lower offset, hence after this one under offset-descending
            // order).
            let missing = hdr_frames - after_start;
            let kept = dv_frames - offset.max(0) as u64;
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
    s.description = format!(
        "Scene-sync offset {offset:+}: {} ({dv_frames} -> {hdr_frames} frames)",
        parts.join(", ")
    );

    Ok(s)
}

/// Legacy frame-count-difference heuristic (no scene information). Kept as the
/// --sync=framecount / --force fallback.
pub(crate) fn compute_alignment_framecount(
    dv_frames: u64,
    hdr_frames: u64,
    fps: f64,
) -> AlignmentStrategy {
    let mut strategy = AlignmentStrategy::default();

    let abs_diff = dv_frames.abs_diff(hdr_frames);
    strategy.action = "none".to_string();
    strategy.description = format!("No alignment needed (frame counts match: {dv_frames})");

    if abs_diff == 0 {
        return strategy;
    }

    let small = (fps * 2.0).round() as u64;
    let medium = (fps * 60.0).round() as u64;
    let large = (fps * 300.0).round() as u64;

    if abs_diff <= small {
        if dv_frames > hdr_frames {
            let start = hdr_frames;
            let end = dv_frames - 1;
            strategy.action = "remove_end".to_string();
            strategy.description =
                format!("Small diff ({abs_diff} frames): trim DV RPU from end ({start}-{end})");
            strategy.remove_ranges.push(format!("{start}-{end}"));
        } else {
            strategy.action = "duplicate_end".to_string();
            strategy.description =
                format!("Small diff ({abs_diff} frames): duplicate last RPU metadata at end");
            strategy.duplicates.push(DuplicateOp {
                source: dv_frames.saturating_sub(1),
                offset: dv_frames,
                length: abs_diff,
            });
        }
        return strategy;
    }

    if abs_diff <= medium {
        if dv_frames > hdr_frames {
            let end = abs_diff.saturating_sub(1);
            strategy.action = "remove_start".to_string();
            strategy.description =
                format!("Medium diff ({abs_diff} frames): trim DV RPU from start (0-{end})");
            strategy.remove_ranges.push(format!("0-{end}"));
        } else {
            strategy.action = "duplicate_start".to_string();
            strategy.description =
                format!("Medium diff ({abs_diff} frames): duplicate first RPU metadata at start");
            strategy.duplicates.push(DuplicateOp {
                source: 0,
                offset: 0,
                length: abs_diff,
            });
        }
        return strategy;
    }

    if abs_diff <= large {
        strategy.high_risk = true;
        if dv_frames > hdr_frames {
            let end = abs_diff.saturating_sub(1);
            strategy.action = "remove_start".to_string();
            strategy.description = format!(
                "Large diff ({abs_diff} frames): HIGH RISK, trim DV RPU from start (0-{end})"
            );
            strategy.remove_ranges.push(format!("0-{end}"));
        } else {
            strategy.action = "duplicate_start".to_string();
            strategy.description = format!(
                "Large diff ({abs_diff} frames): HIGH RISK, duplicate first RPU metadata at start"
            );
            strategy.duplicates.push(DuplicateOp {
                source: 0,
                offset: 0,
                length: abs_diff,
            });
        }
        return strategy;
    }

    strategy.high_risk = true;
    strategy.description = format!(
        "Frame diff ({abs_diff}) exceeds 5-minute heuristic at {fps:.3} fps; leaving as-is"
    );
    strategy
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
    fn framecount_no_diff() {
        let s = compute_alignment_framecount(1000, 1000, 23.976);
        assert_eq!(s.action, "none");
        assert!(s.remove_ranges.is_empty() && s.duplicates.is_empty());
    }

    #[test]
    fn framecount_small_diff_trims_end() {
        let s = compute_alignment_framecount(1010, 1000, 23.976);
        assert_eq!(s.action, "remove_end");
        assert_eq!(s.remove_ranges, vec!["1000-1009".to_string()]);
    }

    #[test]
    fn framecount_small_diff_pads_end() {
        let s = compute_alignment_framecount(1000, 1010, 23.976);
        assert_eq!(s.action, "duplicate_end");
        assert_eq!(s.duplicates.len(), 1);
        assert_eq!(s.duplicates[0].source, 999);
        assert_eq!(s.duplicates[0].offset, 1000);
        assert_eq!(s.duplicates[0].length, 10);
    }

    #[test]
    fn framecount_medium_diff_trims_start() {
        let s = compute_alignment_framecount(2000, 1000, 23.976);
        assert_eq!(s.action, "remove_start");
        assert_eq!(s.remove_ranges, vec!["0-999".to_string()]);
        assert!(!s.high_risk);
    }

    #[test]
    fn framecount_huge_diff_left_as_is() {
        let s = compute_alignment_framecount(100_000, 1000, 23.976);
        assert!(s.high_risk);
        assert!(s.remove_ranges.is_empty() && s.duplicates.is_empty());
    }
}
