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
