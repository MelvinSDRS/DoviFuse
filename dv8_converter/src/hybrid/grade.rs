//! Brightness grade comparison between the DV source and the HDR target.
//!
//! A hybrid is only valid when both sources share the same HDR grade: a
//! 4000-nit DV master's RPU injected into a 1000-nit HDR10 trim produces
//! wrong tone mapping on every DV display. Two layers of defense:
//!
//! - Static gate (preflight): mastering-display max luminance mismatch is a
//!   hard failure, MaxCLL disagreement a warning (or failure in metadata-only
//!   mode). WEB-DL DV sources often carry no static metadata at all, so this
//!   alone is not enough.
//! - Measured check (this module): decode sampled windows from both files
//!   (shifted by the sync offset), fine-align per window by cross-correlating
//!   average-luma series, then compare per-frame average brightness in PQ
//!   space and the p99 peak brightness in nits.

use std::path::Path;

use crate::cli::GradeCheckMode;
use crate::exec::AppResult;
use crate::ffmpeg::{
    cropdetect_window, measure_luma_window, sample_windows, CropRect, FrameLuma, SampleWindow,
};
use crate::logger::Logger;
use crate::mediainfo::HybridMediaInfo;
use crate::pq::{code_limited_to_pq, pq_to_nits};
use crate::runtime::Runtime;

/// Mean |delta PQ| of YAVG above which a window is graded differently.
const WINDOW_DELTA_PQ_FAIL: f64 = 0.015;
/// p99 YMAX nits ratio above which the peak brightness differs.
const PEAK_RATIO_FAIL: f64 = 1.5;
/// Bad windows needed to fail the gate (one can be a fluke: dissolve, recap).
const MAX_BAD_WINDOWS: usize = 1;
/// Overlapping frames required for a window comparison to count.
const MIN_OVERLAP_FRAMES: usize = 24;
/// Seconds of slack decoded around each DV window to absorb seek imprecision.
const DV_WINDOW_PAD_S: f64 = 2.0;
/// Skip the peak-ratio check when both p99 values are this dark (ratio of
/// near-black values is meaningless).
const PEAK_MIN_NITS: f64 = 50.0;
/// cropdetect threshold used to exclude letterbox bars from measurement
/// (bars in one source but not the other would skew YAVG).
const GRADE_CROP_LIMIT: f64 = 0.08;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Verdict {
    Pass,
    Warn,
    Fail,
}

/// Static (metadata-only) grade gate, used as preflight check 14.
pub(crate) fn static_grade_verdict(
    dv: &HybridMediaInfo,
    hdr: &HybridMediaInfo,
    mode: GradeCheckMode,
    skip: bool,
) -> (Verdict, String) {
    // In metadata-only mode this gate is the ONLY grade check; missing
    // metadata on either side means the grades cannot be verified at all,
    // which must not pass silently.
    let metadata_only = mode == GradeCheckMode::Metadata && !skip;
    let unverified_suffix = if skip {
        " - grades are UNVERIFIED (--skip-grade-check)"
    } else if mode == GradeCheckMode::Metadata {
        ""
    } else {
        " - relying on measured grade check"
    };

    let hdr_has_any = hdr.max_cll.is_some()
        || hdr.max_fall.is_some()
        || hdr.mastering_min_nits.is_some()
        || hdr.mastering_max_nits.is_some();
    if !hdr_has_any {
        if metadata_only {
            return (
                Verdict::Fail,
                "HDR target has no static brightness metadata to compare - metadata-only grade check impossible (use --grade-check sampled, or --skip-grade-check)"
                    .to_string(),
            );
        }
        return (
            Verdict::Warn,
            format!("HDR target brightness metadata missing - L6 override will be skipped{unverified_suffix}"),
        );
    }

    let dv_has_any = dv.max_cll.is_some()
        || dv.max_fall.is_some()
        || dv.mastering_min_nits.is_some()
        || dv.mastering_max_nits.is_some();
    if !dv_has_any {
        if metadata_only {
            return (
                Verdict::Fail,
                "DV source has no static brightness metadata to compare - metadata-only grade check impossible (use --grade-check sampled, or --skip-grade-check)"
                    .to_string(),
            );
        }
        return (
            Verdict::Warn,
            format!("DV source has no static brightness metadata (common for WEB-DL){unverified_suffix}"),
        );
    }

    let rel_diff = |a: f64, b: f64| (a - b).abs() / a.max(b).max(1e-9);

    if let (Some(d), Some(h)) = (dv.mastering_max_nits, hdr.mastering_max_nits) {
        if rel_diff(d, h) > 0.05 {
            let msg = format!(
                "Mastering display max luminance differs: {d:.0} vs {h:.0} nits - this is a compatibility warning, not proof of a different grade"
            );
            return if metadata_only {
                (Verdict::Fail, msg)
            } else {
                (Verdict::Warn, format!("{msg}{unverified_suffix}"))
            };
        }
    }

    if let (Some(d), Some(h)) = (dv.max_cll, hdr.max_cll) {
        if rel_diff(f64::from(d), f64::from(h)) > 0.20 {
            let msg = format!("MaxCLL differs by >20%: {d} vs {h} nits");
            return if mode == GradeCheckMode::Metadata && !skip {
                (
                    Verdict::Fail,
                    format!("{msg} - failing in metadata-only mode"),
                )
            } else {
                (Verdict::Warn, format!("{msg} - measured check will decide"))
            };
        }
    }

    let same_cll = dv.max_cll == hdr.max_cll;
    let same_fall = dv.max_fall == hdr.max_fall;
    let same_min = match (dv.mastering_min_nits, hdr.mastering_min_nits) {
        (Some(a), Some(b)) => (a - b).abs() < 0.0001,
        (None, None) => true,
        _ => false,
    };
    if same_cll && same_fall && same_min {
        (Verdict::Pass, "Brightness metadata matches".to_string())
    } else {
        (
            Verdict::Warn,
            "Brightness metadata differs slightly - RPU L6 will be updated to match HDR target"
                .to_string(),
        )
    }
}

/// Whether a cropdetect rectangle can plausibly be letterbox bars. On a dark
/// window (candle scene, fade, starfield) cropdetect collapses to the
/// bounding box of lit content; measuring YAVG over such a rect on one source
/// and near-full-frame on the other fabricates a grade mismatch. Real bars
/// only ever shrink one axis strongly: require each axis >= 50% of the canvas
/// and the area >= 40% (windowboxed 4:3-in-scope is ~42%).
pub(crate) fn plausible_crop(crop: &CropRect, canvas_w: u32, canvas_h: u32) -> bool {
    if crop.x + crop.w > canvas_w || crop.y + crop.h > canvas_h {
        return false;
    }
    let (w, h) = (u64::from(crop.w), u64::from(crop.h));
    let (cw, ch) = (u64::from(canvas_w), u64::from(canvas_h));
    w * 2 >= cw && h * 2 >= ch && w * h * 5 >= cw * ch * 2
}

/// Find the lag (dv index minus hdr index) minimizing mean |delta| between
/// two PQ series, requiring enough overlap. Returns (lag, mean_abs_delta).
pub(crate) fn best_lag_delta(
    dv: &[f64],
    hdr: &[f64],
    lags: std::ops::RangeInclusive<i64>,
) -> Option<(i64, f64)> {
    let mut best: Option<(i64, f64)> = None;

    for lag in lags {
        let mut sum = 0.0;
        let mut n = 0usize;
        for (i, &h) in hdr.iter().enumerate() {
            let j = i as i64 + lag;
            if j < 0 {
                continue;
            }
            let Some(&d) = dv.get(j as usize) else {
                break;
            };
            sum += (d - h).abs();
            n += 1;
        }
        if n < MIN_OVERLAP_FRAMES {
            continue;
        }
        let mean = sum / n as f64;
        if best.is_none_or(|(_, b)| mean < b) {
            best = Some((lag, mean));
        }
    }

    best
}

/// p99 of a value list (99th percentile, no interpolation).
pub(crate) fn p99(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f64> = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[(v.len() - 1).min(v.len() * 99 / 100)]
}

pub(crate) struct GradeOutcome {
    pub(crate) windows_measured: usize,
    pub(crate) windows_bad: usize,
    pub(crate) worst_delta_pq: f64,
    pub(crate) p99_dv_nits: f64,
    pub(crate) p99_hdr_nits: f64,
    pub(crate) peak_ratio: f64,
    pub(crate) pass: bool,
}

fn yavg_pq(series: &[FrameLuma], bit_depth: u32) -> Vec<f64> {
    series
        .iter()
        .map(|f| code_limited_to_pq(f.yavg, bit_depth))
        .collect()
}

/// Measured grade comparison. `offset_frames` is the verified sync offset
/// (dv_frame = hdr_frame + offset).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_grade_check(
    rt: &Runtime,
    logger: &Logger,
    dv_source: &Path,
    hdr_target: &Path,
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    offset_frames: i64,
    fps: f64,
    mode: GradeCheckMode,
    window_count: usize,
) -> AppResult<GradeOutcome> {
    let dv_bd = dv_info.bit_depth.unwrap_or(10);
    let hdr_bd = hdr_info.bit_depth.unwrap_or(10);
    let duration_s = hdr_info
        .duration_ms
        .map(|ms| ms / 1000.0)
        .or_else(|| (fps > 0.0).then(|| hdr_info.frame_count as f64 / fps))
        .ok_or_else(|| "Cannot determine HDR target duration for grade check".to_string())?;
    let offset_s = offset_frames as f64 / fps;

    // (hdr series, dv series, dv lag search range) per window
    let mut window_pairs: Vec<(
        Vec<FrameLuma>,
        Vec<FrameLuma>,
        std::ops::RangeInclusive<i64>,
    )> = Vec::new();

    // Sanity-gate cropdetect rects against the source's canvas; skip the
    // check when the resolution is unknown.
    let crop_ok = |crop: &Option<CropRect>, info: &HybridMediaInfo| -> bool {
        match (crop, info.width, info.height) {
            (Some(c), Some(w), Some(h)) => plausible_crop(c, w, h),
            _ => true,
        }
    };

    match mode {
        GradeCheckMode::Metadata => {
            return Err("run_grade_check called in metadata-only mode".to_string())
        }
        GradeCheckMode::Sampled => {
            let windows = sample_windows(duration_s, window_count, 5.0);
            let pad_frames = (DV_WINDOW_PAD_S * fps).ceil() as i64;
            for w in &windows {
                // DV window: same content position, shifted by the sync
                // offset, padded both sides for the lag search.
                let dv_start = (w.start_s + offset_s - DV_WINDOW_PAD_S).max(0.0);
                let dv_w = SampleWindow {
                    start_s: dv_start,
                    dur_s: w.dur_s + 2.0 * DV_WINDOW_PAD_S,
                };
                let hdr_crop = cropdetect_window(rt, logger, hdr_target, w, GRADE_CROP_LIMIT)?;
                let dv_crop = cropdetect_window(rt, logger, dv_source, &dv_w, GRADE_CROP_LIMIT)?;
                if !crop_ok(&hdr_crop, hdr_info) || !crop_ok(&dv_crop, dv_info) {
                    logger.warn(&format!(
                        "Grade window at {:.0}s: cropdetect returned an implausible rectangle (dark scene?), window skipped",
                        w.start_s
                    ));
                    continue;
                }
                let hdr_series = measure_luma_window(rt, logger, hdr_target, w, hdr_crop.as_ref())?;
                let dv_series =
                    measure_luma_window(rt, logger, dv_source, &dv_w, dv_crop.as_ref())?;
                window_pairs.push((hdr_series, dv_series, 0..=2 * pad_frames));
            }
        }
        GradeCheckMode::Full => {
            let full = SampleWindow {
                start_s: 0.0,
                dur_s: duration_s + 2.0,
            };
            // One representative crop per source (mid-file window).
            let mid = SampleWindow {
                start_s: duration_s * 0.5,
                dur_s: 5.0,
            };
            let mut hdr_crop = cropdetect_window(rt, logger, hdr_target, &mid, GRADE_CROP_LIMIT)?;
            let mut dv_crop = cropdetect_window(rt, logger, dv_source, &mid, GRADE_CROP_LIMIT)?;
            if !crop_ok(&hdr_crop, hdr_info) || !crop_ok(&dv_crop, dv_info) {
                logger.warn(
                    "Mid-file cropdetect returned an implausible rectangle (dark scene?) - measuring both sources uncropped",
                );
                hdr_crop = None;
                dv_crop = None;
            }
            logger.log("Measuring full runtime of both sources (two full decodes)...");
            let hdr_series = measure_luma_window(rt, logger, hdr_target, &full, hdr_crop.as_ref())?;
            let dv_series = measure_luma_window(rt, logger, dv_source, &full, dv_crop.as_ref())?;

            // Apply the global offset by slicing, then evaluate in chunks so
            // a localized regrade can't hide in a whole-film average.
            let (hdr_series, dv_series): (Vec<_>, Vec<_>) = if offset_frames >= 0 {
                (
                    hdr_series,
                    dv_series.into_iter().skip(offset_frames as usize).collect(),
                )
            } else {
                (
                    hdr_series
                        .into_iter()
                        .skip((-offset_frames) as usize)
                        .collect(),
                    dv_series,
                )
            };
            let n = hdr_series.len().min(dv_series.len());
            let chunks = window_count.max(1);
            let chunk_len = (n / chunks).max(1);
            for c in 0..chunks {
                let lo = c * chunk_len;
                let hi = if c + 1 == chunks {
                    n
                } else {
                    ((c + 1) * chunk_len).min(n)
                };
                if hi <= lo {
                    break;
                }
                window_pairs.push((
                    hdr_series[lo..hi].to_vec(),
                    dv_series[lo..hi].to_vec(),
                    -3..=3,
                ));
            }
        }
    }

    let mut windows_measured = 0usize;
    let mut windows_bad = 0usize;
    let mut worst_delta_pq: f64 = 0.0;
    let mut dv_peaks_nits: Vec<f64> = Vec::new();
    let mut hdr_peaks_nits: Vec<f64> = Vec::new();

    for (idx, (hdr_series, dv_series, lags)) in window_pairs.iter().enumerate() {
        let hdr_pq = yavg_pq(hdr_series, hdr_bd);
        let dv_pq = yavg_pq(dv_series, dv_bd);

        let Some((lag, mean_delta)) = best_lag_delta(&dv_pq, &hdr_pq, lags.clone()) else {
            logger.warn(&format!(
                "Grade window {}: not enough overlapping frames, skipped",
                idx + 1
            ));
            continue;
        };

        windows_measured += 1;
        worst_delta_pq = worst_delta_pq.max(mean_delta);
        let bad = mean_delta > WINDOW_DELTA_PQ_FAIL;
        if bad {
            windows_bad += 1;
        }
        logger.log(&format!(
            "Grade window {}: lag {lag:+}, mean |dPQ(YAVG)| {mean_delta:.4} [{}]",
            idx + 1,
            if bad { "MISMATCH" } else { "ok" }
        ));

        // Peaks only over the aligned overlap so both see the same content.
        for (i, h) in hdr_series.iter().enumerate() {
            let j = i as i64 + lag;
            if j < 0 {
                continue;
            }
            let Some(d) = dv_series.get(j as usize) else {
                break;
            };
            hdr_peaks_nits.push(pq_to_nits(code_limited_to_pq(h.ymax, hdr_bd)));
            dv_peaks_nits.push(pq_to_nits(code_limited_to_pq(d.ymax, dv_bd)));
        }
    }

    if windows_measured == 0 {
        return Err("Grade check could not measure any window".to_string());
    }

    let p99_dv_nits = p99(&dv_peaks_nits);
    let p99_hdr_nits = p99(&hdr_peaks_nits);
    let peak_ratio = if p99_dv_nits.max(p99_hdr_nits) < PEAK_MIN_NITS {
        1.0
    } else {
        let lo = p99_dv_nits.min(p99_hdr_nits).max(1e-6);
        p99_dv_nits.max(p99_hdr_nits) / lo
    };

    let pass = windows_bad < windows_measured
        && windows_bad <= MAX_BAD_WINDOWS
        && peak_ratio <= PEAK_RATIO_FAIL;

    Ok(GradeOutcome {
        windows_measured,
        windows_bad,
        worst_delta_pq,
        p99_dv_nits,
        p99_hdr_nits,
        peak_ratio,
        pass,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::GradeCheckMode as M;

    fn info(
        max_cll: Option<u16>,
        max_fall: Option<u16>,
        min_nits: Option<f64>,
        max_nits: Option<f64>,
    ) -> HybridMediaInfo {
        HybridMediaInfo {
            max_cll,
            max_fall,
            mastering_min_nits: min_nits,
            mastering_max_nits: max_nits,
            ..Default::default()
        }
    }

    #[test]
    fn static_gate_identical_passes() {
        let a = info(Some(1000), Some(400), Some(0.005), Some(1000.0));
        let (v, _) = static_grade_verdict(&a, &a.clone(), M::Sampled, false);
        assert_eq!(v, Verdict::Pass);
    }

    #[test]
    fn static_gate_mastering_display_difference_requires_measurement() {
        let dv = info(Some(4000), Some(400), Some(0.005), Some(4000.0));
        let hdr = info(Some(1000), Some(400), Some(0.005), Some(1000.0));
        let (v, msg) = static_grade_verdict(&dv, &hdr, M::Sampled, false);
        assert_eq!(v, Verdict::Warn, "{msg}");
        // --skip-grade-check demotes to WARN
        let (v, _) = static_grade_verdict(&dv, &hdr, M::Sampled, true);
        assert_eq!(v, Verdict::Warn);
    }

    #[test]
    fn static_gate_maxcll_diff_warns_sampled_fails_metadata() {
        let dv = info(Some(700), None, Some(0.005), Some(1000.0));
        let hdr = info(Some(1000), None, Some(0.005), Some(1000.0));
        let (v, _) = static_grade_verdict(&dv, &hdr, M::Sampled, false);
        assert_eq!(v, Verdict::Warn);
        let (v, _) = static_grade_verdict(&dv, &hdr, M::Metadata, false);
        assert_eq!(v, Verdict::Fail);
    }

    #[test]
    fn static_gate_missing_metadata_warns() {
        let dv = info(None, None, None, None);
        let hdr = info(Some(1000), Some(400), Some(0.005), Some(1000.0));
        let (v, msg) = static_grade_verdict(&dv, &hdr, M::Sampled, false);
        assert_eq!(v, Verdict::Warn);
        assert!(msg.contains("measured grade check"), "{msg}");
        let (v, _) = static_grade_verdict(&hdr, &dv, M::Sampled, false);
        assert_eq!(v, Verdict::Warn);
    }

    #[test]
    fn static_gate_missing_metadata_fails_metadata_only_mode() {
        // Metadata mode has no measured fallback: nothing to compare must
        // not pass as a WARN claiming a measured check will run.
        let dv = info(None, None, None, None);
        let hdr = info(Some(1000), Some(400), Some(0.005), Some(1000.0));
        let (v, msg) = static_grade_verdict(&dv, &hdr, M::Metadata, false);
        assert_eq!(v, Verdict::Fail, "{msg}");
        let (v, _) = static_grade_verdict(&hdr, &dv, M::Metadata, false);
        assert_eq!(v, Verdict::Fail);
        // --skip-grade-check demotes to an honest WARN
        let (v, msg) = static_grade_verdict(&dv, &hdr, M::Metadata, true);
        assert_eq!(v, Verdict::Warn);
        assert!(msg.contains("UNVERIFIED"), "{msg}");
    }

    #[test]
    fn plausible_crop_accepts_bars_rejects_bounding_boxes() {
        // Scope letterbox on UHD: fine.
        let scope = CropRect {
            w: 3840,
            h: 1600,
            x: 0,
            y: 280,
        };
        assert!(plausible_crop(&scope, 3840, 2160));
        // Windowboxed (bars on all four edges, ~56% area): still plausible.
        let windowboxed = CropRect {
            w: 2880,
            h: 1600,
            x: 480,
            y: 280,
        };
        assert!(plausible_crop(&windowboxed, 3840, 2160));
        // Candle-scene bounding box: both axes collapsed.
        let dark = CropRect {
            w: 1200,
            h: 800,
            x: 1300,
            y: 700,
        };
        assert!(!plausible_crop(&dark, 3840, 2160));
        // Out of canvas bounds.
        let oob = CropRect {
            w: 3840,
            h: 2160,
            x: 16,
            y: 0,
        };
        assert!(!plausible_crop(&oob, 3840, 2160));
    }

    #[test]
    fn lag_recovered_on_identical_series() {
        // dv is hdr delayed by 7 samples
        let hdr: Vec<f64> = (0..200).map(|i| ((i * 37) % 100) as f64 / 100.0).collect();
        let mut dv = vec![0.5; 7];
        dv.extend(&hdr);
        let (lag, delta) = best_lag_delta(&dv, &hdr, 0..=20).unwrap();
        assert_eq!(lag, 7);
        assert!(delta < 1e-12);
    }

    #[test]
    fn brightness_shift_detected() {
        let hdr: Vec<f64> = (0..200)
            .map(|i| 0.3 + ((i * 13) % 50) as f64 / 500.0)
            .collect();
        let dv: Vec<f64> = hdr.iter().map(|v| v + 0.05).collect(); // regraded
        let (_, delta) = best_lag_delta(&dv, &hdr, -5..=5).unwrap();
        assert!(delta > WINDOW_DELTA_PQ_FAIL);
    }

    #[test]
    fn overlap_too_small_returns_none() {
        let a = vec![0.5; 10];
        assert!(best_lag_delta(&a, &a.clone(), 0..=0).is_none());
    }

    #[test]
    fn p99_basic() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(p99(&v), 100.0);
        let v: Vec<f64> = (1..=1000).map(f64::from).collect();
        assert_eq!(p99(&v), 991.0);
        assert_eq!(p99(&[]), 0.0);
    }
}
