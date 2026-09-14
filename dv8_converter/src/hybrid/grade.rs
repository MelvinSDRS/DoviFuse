//! Brightness grade comparison between the DV source and the HDR target.
//!
//! A hybrid needs a compatible HDR base-layer grade: a 4000-nit DV master's
//! RPU injected into a 1000-nit HDR10 trim can produce wrong tone mapping.
//! This module provides bounded screening evidence; it cannot establish
//! calibrated creative-grade equivalence. Two layers of defense:
//!
//! - Static gate (preflight): mastering-display and MaxCLL disagreement
//!   warn in measured modes and fail in metadata-only mode. WEB-DL DV sources often carry no static metadata at all, so this
//!   alone is not enough.
//! - Measured check (this module): decode sampled windows from both files
//!   (shifted by the sync offset), fine-align per window by cross-correlating
//!   average-luma series, then compare per-frame average brightness in PQ
//!   space and the p99 PQ(Y) peak surrogate converted to nominal nits. These
//!   code-derived values are screening metrics, not measured luminance.

use std::path::Path;

use crate::cli::GradeCheckMode;
use crate::exec::AppResult;
use crate::ffmpeg::{
    cropdetect_window, measure_luma_window, plausible_crop as ffmpeg_plausible_crop,
    sample_windows, CropRect, FrameLuma, SampleWindow,
};
use crate::logger::Logger;
use crate::mediainfo::HybridMediaInfo;
use crate::pq::{code_limited_to_pq, pq_to_nits};
use crate::runtime::Runtime;
use serde::Serialize;

/// Mean |delta PQ| of YAVG above which a window is graded differently.
const WINDOW_DELTA_PQ_FAIL: f64 = 0.015;
/// p99 YMAX nits ratio above which the peak brightness differs.
const PEAK_RATIO_FAIL: f64 = 1.5;
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
/// Every scored interval is bounded to five seconds, including full mode.
const GRADE_INTERVAL_S: f64 = 5.0;
/// Coarse normalized chroma delta that is large enough to screen a likely
/// trim mismatch while remaining a screen rather than a creative-grade test.
const CHROMA_DELTA_FAIL: f64 = 0.05;

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
    ffmpeg_plausible_crop(crop, canvas_w, canvas_h)
}

/// Find the lag (dv index minus hdr index) minimizing mean |delta| between
/// two PQ series, requiring enough overlap. Returns (lag, mean_abs_delta).
#[cfg(test)]
pub(crate) fn best_lag_delta(
    dv: &[f64],
    hdr: &[f64],
    lags: std::ops::RangeInclusive<i64>,
) -> Option<(i64, f64)> {
    best_lag_delta_with_min_overlap(dv, hdr, lags, MIN_OVERLAP_FRAMES)
}

fn best_lag_delta_with_min_overlap(
    dv: &[f64],
    hdr: &[f64],
    lags: std::ops::RangeInclusive<i64>,
    min_overlap: usize,
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
        if n < min_overlap {
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
    let mut v: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[(v.len() - 1).min(v.len() * 99 / 100)]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct GradeFrameInterval {
    pub(crate) start_frame: u64,
    /// Exclusive end frame.
    pub(crate) end_frame: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) enum GradeWindowStatus {
    Measured,
    Skipped,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GradeWindowEvidence {
    pub(crate) index: usize,
    pub(crate) target: GradeFrameInterval,
    pub(crate) donor: GradeFrameInterval,
    pub(crate) measured_target: Option<GradeFrameInterval>,
    pub(crate) measured_donor: Option<GradeFrameInterval>,
    pub(crate) status: GradeWindowStatus,
    pub(crate) reason: Option<String>,
    pub(crate) measured_overlap_frames: usize,
    pub(crate) lag: Option<i64>,
    pub(crate) mean_delta_pq: Option<f64>,
    pub(crate) mean_chroma_delta: Option<f64>,
    pub(crate) bad: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct GradeOutcome {
    pub(crate) windows_measured: usize,
    pub(crate) windows_bad: usize,
    pub(crate) worst_delta_pq: f64,
    #[serde(rename = "p99_dv_pq_y_surrogate")]
    pub(crate) p99_dv_nits: f64,
    #[serde(rename = "p99_hdr_pq_y_surrogate")]
    pub(crate) p99_hdr_nits: f64,
    pub(crate) peak_ratio: f64,
    pub(crate) windows_requested: usize,
    pub(crate) windows_skipped: usize,
    pub(crate) windows: Vec<GradeWindowEvidence>,
    /// Complete means every requested interval was measured. It does not
    /// claim that sampled mode covers the entire feature runtime.
    pub(crate) coverage_complete: bool,
    pub(crate) color_screened: bool,
    pub(crate) pass: bool,
}

#[derive(Clone, Debug)]
struct RequestedWindow {
    index: usize,
    target: GradeFrameInterval,
    donor: GradeFrameInterval,
    target_decode: SampleWindow,
    donor_decode: SampleWindow,
    target_decode_start_frame: u64,
    donor_decode_start_frame: u64,
    lag_range: std::ops::RangeInclusive<i64>,
}

struct WindowMeasurement {
    evidence: GradeWindowEvidence,
    hdr_peaks_nits: Vec<f64>,
    dv_peaks_nits: Vec<f64>,
    chroma_screened: bool,
}

fn yavg_pq(series: &[FrameLuma], bit_depth: u32) -> Vec<f64> {
    series
        .iter()
        .map(|f| code_limited_to_pq(f.yavg, bit_depth))
        .collect()
}

fn crop_is_usable(crop: &Option<CropRect>, info: &HybridMediaInfo) -> bool {
    match (crop, info.width, info.height) {
        (Some(c), Some(w), Some(h)) => plausible_crop(c, w, h),
        (Some(_), None, _) | (Some(_), _, None) => true,
        (None, _, _) => false,
    }
}

fn sample_request(
    index: usize,
    window: &SampleWindow,
    fps: f64,
    offset_frames: i64,
    pad_frames: i64,
    target_frame_count: u64,
) -> RequestedWindow {
    let target_start = (window.start_s * fps).round().max(0.0) as u64;
    let target_len = (window.dur_s * fps).round().max(1.0) as u64;
    let target_end = if target_frame_count > 0 {
        target_start
            .saturating_add(target_len)
            .min(target_frame_count)
    } else {
        target_start.saturating_add(target_len)
    };
    let donor_start_i = target_start as i128 + offset_frames as i128;
    let donor_end_i = target_end as i128 + offset_frames as i128;
    let donor_start = donor_start_i.max(0) as u64;
    let donor_end = donor_end_i.max(0) as u64;
    let donor_decode_start = donor_start.saturating_sub(pad_frames.max(0) as u64);
    let donor_decode_end = donor_end.saturating_add(pad_frames.max(0) as u64);
    let expected_lag = donor_start as i64 - donor_decode_start as i64;
    let search_pad = pad_frames.max(0);

    RequestedWindow {
        index,
        target: GradeFrameInterval {
            start_frame: target_start,
            end_frame: target_end,
        },
        donor: GradeFrameInterval {
            start_frame: donor_start,
            end_frame: donor_end,
        },
        target_decode: SampleWindow {
            start_s: target_start as f64 / fps,
            dur_s: target_end.saturating_sub(target_start) as f64 / fps,
        },
        donor_decode: SampleWindow {
            start_s: donor_decode_start as f64 / fps,
            dur_s: donor_decode_end.saturating_sub(donor_decode_start) as f64 / fps,
        },
        target_decode_start_frame: target_start,
        donor_decode_start_frame: donor_decode_start,
        lag_range: expected_lag - search_pad..=expected_lag + search_pad,
    }
}

fn skipped_window(request: &RequestedWindow, reason: impl Into<String>) -> GradeWindowEvidence {
    GradeWindowEvidence {
        index: request.index,
        target: request.target,
        donor: request.donor,
        measured_target: None,
        measured_donor: None,
        status: GradeWindowStatus::Skipped,
        reason: Some(reason.into()),
        measured_overlap_frames: 0,
        lag: None,
        mean_delta_pq: None,
        mean_chroma_delta: None,
        bad: false,
    }
}

fn aligned_pair_count(dv_len: usize, hdr_len: usize, lag: i64) -> usize {
    (0..hdr_len)
        .filter_map(|i| {
            let j = i as i64 + lag;
            (j >= 0).then_some(j as usize)
        })
        .take_while(|&j| j < dv_len)
        .count()
}

fn chroma_delta(dv: &FrameLuma, hdr: &FrameLuma, bit_depth: u32) -> Option<f64> {
    let (du, dv_delta) = (dv.uavg?, dv.vavg?);
    if !du.is_finite() || !dv_delta.is_finite() || !hdr.uavg?.is_finite() || !hdr.vavg?.is_finite()
    {
        return None;
    }
    let limited_chroma_span = if bit_depth >= 8 {
        224.0 * 2f64.powi((bit_depth.min(16) - 8) as i32)
    } else {
        224.0 / 2f64.powi((8 - bit_depth) as i32)
    };
    Some(
        ((du - hdr.uavg.unwrap())
            .abs()
            .max((dv_delta - hdr.vavg.unwrap()).abs()))
            / limited_chroma_span,
    )
}

fn score_window(
    request: &RequestedWindow,
    hdr_series: &[FrameLuma],
    dv_series: &[FrameLuma],
    hdr_bd: u32,
    dv_bd: u32,
    min_overlap: usize,
) -> Option<WindowMeasurement> {
    let hdr_pq = yavg_pq(hdr_series, hdr_bd);
    let dv_pq = yavg_pq(dv_series, dv_bd);
    let (mut lag, mut mean_delta) =
        best_lag_delta_with_min_overlap(&dv_pq, &hdr_pq, request.lag_range.clone(), min_overlap)?;
    let expected_lag = request.donor.start_frame as i64 - request.donor_decode_start_frame as i64;
    if request.lag_range.contains(&expected_lag) {
        if let Some((preferred, delta)) = best_lag_delta_with_min_overlap(
            &dv_pq,
            &hdr_pq,
            expected_lag..=expected_lag,
            min_overlap,
        ) {
            if delta <= mean_delta + 1e-12 {
                lag = preferred;
                mean_delta = delta;
            }
        }
    }
    let overlap = aligned_pair_count(dv_series.len(), hdr_series.len(), lag);
    if overlap < min_overlap {
        return None;
    }

    let mut hdr_peaks_nits = Vec::with_capacity(overlap);
    let mut dv_peaks_nits = Vec::with_capacity(overlap);
    let mut chroma_sum = 0.0;
    let mut chroma_count = 0usize;
    for (i, hdr) in hdr_series.iter().enumerate() {
        let j = i as i64 + lag;
        if j < 0 {
            continue;
        }
        let Some(dv) = dv_series.get(j as usize) else {
            break;
        };
        hdr_peaks_nits.push(pq_to_nits(code_limited_to_pq(hdr.ymax, hdr_bd)));
        dv_peaks_nits.push(pq_to_nits(code_limited_to_pq(dv.ymax, dv_bd)));
        if let Some(delta) = chroma_delta(dv, hdr, dv_bd.min(hdr_bd)) {
            chroma_sum += delta;
            chroma_count += 1;
        }
    }
    let hdr_first = if lag < 0 { lag.unsigned_abs() } else { 0 };
    let dv_first = if lag > 0 { lag as u64 } else { 0 };
    let measured_target = Some(GradeFrameInterval {
        start_frame: request.target_decode_start_frame.saturating_add(hdr_first),
        end_frame: request
            .target_decode_start_frame
            .saturating_add(hdr_first)
            .saturating_add(overlap as u64),
    });
    let measured_donor = Some(GradeFrameInterval {
        start_frame: request.donor_decode_start_frame.saturating_add(dv_first),
        end_frame: request
            .donor_decode_start_frame
            .saturating_add(dv_first)
            .saturating_add(overlap as u64),
    });
    let clipped = measured_target != Some(request.target) || measured_donor != Some(request.donor);
    let mean_chroma_delta = (chroma_count == overlap).then_some(chroma_sum / overlap as f64);
    let chroma_bad = mean_chroma_delta.is_some_and(|delta| delta > CHROMA_DELTA_FAIL);
    let bad = mean_delta > WINDOW_DELTA_PQ_FAIL || chroma_bad;
    Some(WindowMeasurement {
        evidence: GradeWindowEvidence {
            index: request.index,
            target: request.target,
            donor: request.donor,
            measured_target,
            measured_donor,
            status: GradeWindowStatus::Measured,
            reason: clipped.then(|| "measured overlap is clipped to decoded frames".to_string()),
            measured_overlap_frames: overlap,
            lag: Some(lag),
            mean_delta_pq: Some(mean_delta),
            mean_chroma_delta,
            bad,
        },
        hdr_peaks_nits,
        dv_peaks_nits,
        chroma_screened: mean_chroma_delta.is_some(),
    })
}

fn validate_full_frames(
    source: &str,
    series: &[FrameLuma],
    required_frames: u64,
) -> AppResult<Vec<FrameLuma>> {
    let required = usize::try_from(required_frames)
        .map_err(|_| format!("{source} frame count does not fit in memory"))?;
    if series.len() < required {
        return Err(format!(
            "Full grade coverage incomplete: {source} decoded {} frames, expected {required}",
            series.len()
        ));
    }
    for (index, frame) in series.iter().take(required).enumerate() {
        if frame.frame != index as u64 {
            return Err(format!(
                "Full grade coverage incomplete: {source} frame sequence missing frame {index} (found {})",
                frame.frame
            ));
        }
    }
    Ok(series[..required].to_vec())
}

fn log_grade_failure(
    logger: &Logger,
    mode: GradeCheckMode,
    windows_requested: usize,
    windows: &[GradeWindowEvidence],
    reason: impl Into<String>,
) -> AppResult<GradeOutcome> {
    let reason = reason.into();
    let windows_measured = windows
        .iter()
        .filter(|w| matches!(w.status, GradeWindowStatus::Measured))
        .count();
    logger.measurement(
        "brightness_screen",
        serde_json::json!({
            "mode": format!("{mode:?}"),
            "windows_requested": windows_requested,
            "windows_measured": windows_measured,
            "windows_skipped": windows.len().saturating_sub(windows_measured),
            "windows": windows,
            "coverage_complete": false,
            "color_screened": false,
            "pass": false,
            "error": reason,
            "peak_metric": "PQ(Y) code-derived peak surrogate, not measured luminance"
        }),
    );
    Err(reason)
}

/// Measured grade comparison. `offset_frames` is the verified sync offset
/// (dv_frame = hdr_frame + offset).
fn format_window_log(window: &GradeWindowEvidence) -> String {
    let status = if window.bad { "MISMATCH" } else { "ok" };
    format!(
        "Grade window {}: target {}..{}, donor {}..{}, lag {:+}, mean |dPQ(YAVG)| {:.4}, chroma delta {:?} [{}]",
        window.index,
        window.target.start_frame,
        window.target.end_frame,
        window.donor.start_frame,
        window.donor.end_frame,
        window.lag.unwrap_or(0),
        window.mean_delta_pq.unwrap_or(0.0),
        window.mean_chroma_delta,
        status
    )
}

/// Measured grade comparison. offset_frames is the verified sync offset
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
    if !fps.is_finite() || fps <= 0.0 {
        return log_grade_failure(
            logger,
            mode,
            0,
            &[],
            "Cannot determine a positive finite frame rate for grade check",
        );
    }
    let duration_s = match hdr_info
        .duration_ms
        .map(|ms| ms / 1000.0)
        .or_else(|| (fps > 0.0).then(|| hdr_info.frame_count as f64 / fps))
    {
        Some(duration) if duration.is_finite() && duration > 0.0 => duration,
        _ => {
            return log_grade_failure(
                logger,
                mode,
                0,
                &[],
                "Cannot determine HDR target duration for grade check",
            )
        }
    };

    let mut windows: Vec<GradeWindowEvidence> = Vec::new();
    let requested: usize;
    let mut peak_pairs: Vec<(f64, f64)> = Vec::new();
    let mut worst_delta_pq: f64 = 0.0;
    let mut color_screened_windows = 0usize;

    match mode {
        GradeCheckMode::Metadata => {
            return log_grade_failure(
                logger,
                mode,
                0,
                &[],
                "run_grade_check called in metadata-only mode",
            )
        }
        GradeCheckMode::Sampled => {
            let sampled = sample_windows(duration_s, window_count, GRADE_INTERVAL_S);
            requested = sampled.len();
            let pad_frames = (DV_WINDOW_PAD_S * fps).ceil() as i64;
            for (index, original) in sampled.iter().enumerate() {
                let request = sample_request(
                    index + 1,
                    original,
                    fps,
                    offset_frames,
                    pad_frames,
                    hdr_info.frame_count,
                );
                let signed_donor_start = request.target.start_frame as i128 + offset_frames as i128;
                let signed_donor_end = request.target.end_frame as i128 + offset_frames as i128;
                if request.target.end_frame <= request.target.start_frame
                    || request.donor.end_frame <= request.donor.start_frame
                    || signed_donor_start < 0
                    || signed_donor_end < 0
                    || (dv_info.frame_count > 0 && signed_donor_end > dv_info.frame_count as i128)
                {
                    windows.push(skipped_window(
                        &request,
                        "requested target/donor frame interval has no overlap",
                    ));
                    continue;
                }

                let hdr_crop = match cropdetect_window(
                    rt,
                    logger,
                    hdr_target,
                    &request.target_decode,
                    GRADE_CROP_LIMIT,
                ) {
                    Ok(crop) => crop,
                    Err(error) => {
                        windows.push(skipped_window(
                            &request,
                            format!("target cropdetect failed: {error}"),
                        ));
                        return log_grade_failure(
                            logger,
                            mode,
                            requested,
                            &windows,
                            format!("target cropdetect failed: {error}"),
                        );
                    }
                };
                let dv_crop = match cropdetect_window(
                    rt,
                    logger,
                    dv_source,
                    &request.donor_decode,
                    GRADE_CROP_LIMIT,
                ) {
                    Ok(crop) => crop,
                    Err(error) => {
                        windows.push(skipped_window(
                            &request,
                            format!("donor cropdetect failed: {error}"),
                        ));
                        return log_grade_failure(
                            logger,
                            mode,
                            requested,
                            &windows,
                            format!("donor cropdetect failed: {error}"),
                        );
                    }
                };
                if !crop_is_usable(&hdr_crop, hdr_info) || !crop_is_usable(&dv_crop, dv_info) {
                    windows.push(skipped_window(
                        &request,
                        "cropdetect returned an implausible rectangle",
                    ));
                    continue;
                }

                let mut hdr_series = match measure_luma_window(
                    rt,
                    logger,
                    hdr_target,
                    &request.target_decode,
                    hdr_crop.as_ref(),
                ) {
                    Ok(series) => series,
                    Err(error) => {
                        windows.push(skipped_window(
                            &request,
                            format!("target measurement failed: {error}"),
                        ));
                        return log_grade_failure(
                            logger,
                            mode,
                            requested,
                            &windows,
                            format!("target measurement failed: {error}"),
                        );
                    }
                };
                let dv_series = match measure_luma_window(
                    rt,
                    logger,
                    dv_source,
                    &request.donor_decode,
                    dv_crop.as_ref(),
                ) {
                    Ok(series) => series,
                    Err(error) => {
                        windows.push(skipped_window(
                            &request,
                            format!("donor measurement failed: {error}"),
                        ));
                        return log_grade_failure(
                            logger,
                            mode,
                            requested,
                            &windows,
                            format!("donor measurement failed: {error}"),
                        );
                    }
                };
                // ffmpeg's output -t can evaluate one extra upstream filter frame.
                // Score exactly the requested target interval; a short read remains short.
                hdr_series.truncate(
                    usize::try_from(request.target.end_frame - request.target.start_frame)
                        .unwrap_or(usize::MAX),
                );
                let Some(measurement) = score_window(
                    &request,
                    &hdr_series,
                    &dv_series,
                    hdr_bd,
                    dv_bd,
                    MIN_OVERLAP_FRAMES,
                ) else {
                    windows.push(skipped_window(
                        &request,
                        "measured target/donor interval has insufficient overlap",
                    ));
                    continue;
                };
                if measurement.evidence.measured_target != Some(request.target)
                    || measurement.evidence.measured_donor != Some(request.donor)
                {
                    let mut evidence = measurement.evidence;
                    evidence.status = GradeWindowStatus::Skipped;
                    evidence.reason = Some(format!(
                        "measured target {:?} / donor {:?} differs from requested coverage",
                        evidence.measured_target, evidence.measured_donor
                    ));
                    windows.push(evidence);
                    continue;
                }
                logger.log(&format_window_log(&measurement.evidence));
                worst_delta_pq =
                    worst_delta_pq.max(measurement.evidence.mean_delta_pq.unwrap_or(0.0));
                if measurement.chroma_screened {
                    color_screened_windows += 1;
                }
                peak_pairs.extend(
                    measurement
                        .hdr_peaks_nits
                        .into_iter()
                        .zip(measurement.dv_peaks_nits),
                );
                windows.push(measurement.evidence);
            }
        }
        GradeCheckMode::Full => {
            let target_count = hdr_info.frame_count;
            if target_count == 0 || dv_info.frame_count == 0 {
                return log_grade_failure(
                    logger,
                    mode,
                    0,
                    &[],
                    "Full grade check requires declared target and donor frame counts",
                );
            }
            let offset_abs = offset_frames.unsigned_abs();
            if offset_frames < 0 && offset_abs >= target_count {
                return log_grade_failure(
                    logger,
                    mode,
                    0,
                    &[],
                    format!("Full grade check has no aligned overlap at offset {offset_frames}"),
                );
            }
            let donor_decode_count = match (target_count as i128 + offset_frames as i128).try_into()
            {
                Ok(count) => count,
                Err(_) => {
                    return log_grade_failure(
                        logger,
                        mode,
                        0,
                        &[],
                        "Full grade donor frame count overflow",
                    )
                }
            };
            if donor_decode_count > dv_info.frame_count {
                return log_grade_failure(
                    logger,
                    mode,
                    0,
                    &[],
                    format!(
                        "Full grade check donor overlap missing: need frame {}, donor has {} frames",
                        donor_decode_count, dv_info.frame_count
                    ),
                );
            }

            let target_duration = target_count as f64 / fps;
            let donor_duration = donor_decode_count as f64 / fps;
            let target_full = SampleWindow {
                start_s: 0.0,
                dur_s: target_duration,
            };
            let donor_full = SampleWindow {
                start_s: 0.0,
                dur_s: donor_duration,
            };
            let mid_target = SampleWindow {
                start_s: target_duration * 0.5,
                dur_s: GRADE_INTERVAL_S.min(target_duration.max(0.5)),
            };
            let mid_donor = SampleWindow {
                start_s: ((target_count / 2) as i64 + offset_frames).max(0) as f64 / fps,
                dur_s: mid_target.dur_s,
            };
            let hdr_crop =
                match cropdetect_window(rt, logger, hdr_target, &mid_target, GRADE_CROP_LIMIT) {
                    Ok(crop) => crop,
                    Err(error) => {
                        return log_grade_failure(
                            logger,
                            mode,
                            0,
                            &[],
                            format!("target cropdetect failed: {error}"),
                        )
                    }
                };
            let dv_crop =
                match cropdetect_window(rt, logger, dv_source, &mid_donor, GRADE_CROP_LIMIT) {
                    Ok(crop) => crop,
                    Err(error) => {
                        return log_grade_failure(
                            logger,
                            mode,
                            0,
                            &[],
                            format!("donor cropdetect failed: {error}"),
                        )
                    }
                };
            if !crop_is_usable(&hdr_crop, hdr_info) || !crop_is_usable(&dv_crop, dv_info) {
                return log_grade_failure(
                    logger,
                    mode,
                    0,
                    &[],
                    "Full grade check crop coverage unavailable or implausible",
                );
            }

            logger.log(&format!(
                "Measuring full grade coverage: target {} frames, donor through frame {}",
                target_count, donor_decode_count
            ));
            let hdr_series = match measure_luma_window(
                rt,
                logger,
                hdr_target,
                &target_full,
                hdr_crop.as_ref(),
            ) {
                Ok(series) => validate_full_frames("target", &series, target_count),
                Err(error) => Err(error),
            };
            let hdr_series = match hdr_series {
                Ok(series) => series,
                Err(error) => return log_grade_failure(logger, mode, 0, &[], error),
            };
            let dv_series =
                match measure_luma_window(rt, logger, dv_source, &donor_full, dv_crop.as_ref()) {
                    Ok(series) => validate_full_frames("donor", &series, donor_decode_count),
                    Err(error) => Err(error),
                };
            let dv_series = match dv_series {
                Ok(series) => series,
                Err(error) => return log_grade_failure(logger, mode, 0, &[], error),
            };

            let overlap = target_count - if offset_frames < 0 { offset_abs } else { 0 };
            let target_start = offset_frames.saturating_neg().max(0) as u64;
            let donor_start = offset_frames.max(0) as u64;
            let interval_frames = (fps * GRADE_INTERVAL_S).floor().max(1.0) as usize;
            let overlap_usize = match usize::try_from(overlap) {
                Ok(value) => value,
                Err(_) => {
                    return log_grade_failure(
                        logger,
                        mode,
                        0,
                        &[],
                        "Full grade overlap does not fit in memory",
                    )
                }
            };
            requested = overlap_usize.div_ceil(interval_frames);
            for chunk_start in (0..overlap_usize).step_by(interval_frames) {
                let chunk_len = (overlap_usize - chunk_start).min(interval_frames);
                let target_lo = match usize::try_from(target_start) {
                    Ok(value) => value.saturating_add(chunk_start),
                    Err(_) => {
                        return log_grade_failure(
                            logger,
                            mode,
                            requested,
                            &windows,
                            "Full grade target interval does not fit in memory",
                        )
                    }
                };
                let donor_lo = match usize::try_from(donor_start) {
                    Ok(value) => value.saturating_add(chunk_start),
                    Err(_) => {
                        return log_grade_failure(
                            logger,
                            mode,
                            requested,
                            &windows,
                            "Full grade donor interval does not fit in memory",
                        )
                    }
                };
                let target_hi = target_lo.saturating_add(chunk_len);
                let donor_hi = donor_lo.saturating_add(chunk_len);
                let request = RequestedWindow {
                    index: windows.len() + 1,
                    target: GradeFrameInterval {
                        start_frame: target_start + chunk_start as u64,
                        end_frame: target_start + (chunk_start + chunk_len) as u64,
                    },
                    donor: GradeFrameInterval {
                        start_frame: donor_start + chunk_start as u64,
                        end_frame: donor_start + (chunk_start + chunk_len) as u64,
                    },
                    target_decode: target_full,
                    donor_decode: donor_full,
                    target_decode_start_frame: target_lo as u64,
                    donor_decode_start_frame: donor_lo as u64,
                    lag_range: 0..=0,
                };
                if target_hi > hdr_series.len() || donor_hi > dv_series.len() {
                    windows.push(skipped_window(
                        &request,
                        "full aligned interval is outside decoded coverage",
                    ));
                    continue;
                }
                let Some(measurement) = score_window(
                    &request,
                    &hdr_series[target_lo..target_hi],
                    &dv_series[donor_lo..donor_hi],
                    hdr_bd,
                    dv_bd,
                    chunk_len,
                ) else {
                    windows.push(skipped_window(
                        &request,
                        "full aligned interval has insufficient overlap",
                    ));
                    continue;
                };
                logger.log(&format_window_log(&measurement.evidence));
                worst_delta_pq =
                    worst_delta_pq.max(measurement.evidence.mean_delta_pq.unwrap_or(0.0));
                if measurement.chroma_screened {
                    color_screened_windows += 1;
                }
                peak_pairs.extend(
                    measurement
                        .hdr_peaks_nits
                        .into_iter()
                        .zip(measurement.dv_peaks_nits),
                );
                windows.push(measurement.evidence);
            }
            if windows
                .iter()
                .any(|window| matches!(window.status, GradeWindowStatus::Skipped))
            {
                return log_grade_failure(
                    logger,
                    mode,
                    requested,
                    &windows,
                    "Full grade check could not measure every aligned interval",
                );
            }
        }
    }

    let windows_measured = windows
        .iter()
        .filter(|w| matches!(w.status, GradeWindowStatus::Measured))
        .count();
    let windows_skipped = windows.len().saturating_sub(windows_measured);
    if windows_measured == 0 {
        return log_grade_failure(
            logger,
            mode,
            requested,
            &windows,
            "Grade check could not measure any window",
        );
    }
    let windows_bad = windows.iter().filter(|w| w.bad).count();
    let p99_dv_nits = p99(&peak_pairs.iter().map(|(_, dv)| *dv).collect::<Vec<_>>());
    let p99_hdr_nits = p99(&peak_pairs.iter().map(|(hdr, _)| *hdr).collect::<Vec<_>>());
    let peak_ratio = if p99_dv_nits.max(p99_hdr_nits) < PEAK_MIN_NITS {
        1.0
    } else {
        let lo = p99_dv_nits.min(p99_hdr_nits).max(1e-6);
        p99_dv_nits.max(p99_hdr_nits) / lo
    };
    let pass = windows_bad == 0 && peak_ratio <= PEAK_RATIO_FAIL;
    Ok(GradeOutcome {
        windows_measured,
        windows_bad,
        worst_delta_pq,
        p99_dv_nits,
        p99_hdr_nits,
        peak_ratio,
        windows_requested: requested,
        windows_skipped,
        windows,
        coverage_complete: windows_skipped == 0,
        color_screened: color_screened_windows == windows_measured,
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

    fn frame_series(count: usize, yavg: f64, uavg: f64, vavg: f64) -> Vec<FrameLuma> {
        (0..count)
            .map(|frame| FrameLuma {
                frame: frame as u64,
                yavg,
                ymax: yavg,
                uavg: Some(uavg),
                vavg: Some(vavg),
            })
            .collect()
    }

    fn test_window() -> RequestedWindow {
        RequestedWindow {
            index: 1,
            target: GradeFrameInterval {
                start_frame: 0,
                end_frame: 48,
            },
            donor: GradeFrameInterval {
                start_frame: 0,
                end_frame: 48,
            },
            target_decode: SampleWindow {
                start_s: 0.0,
                dur_s: 2.0,
            },
            donor_decode: SampleWindow {
                start_s: 0.0,
                dur_s: 2.0,
            },
            target_decode_start_frame: 0,
            donor_decode_start_frame: 0,
            lag_range: 0..=0,
        }
    }

    #[test]
    fn short_luma_mismatch_is_a_bad_interval() {
        let request = test_window();
        let hdr = frame_series(48, 500.0, 512.0, 512.0);
        let dv = frame_series(48, 700.0, 512.0, 512.0);
        let measurement = score_window(&request, &hdr, &dv, 10, 10, MIN_OVERLAP_FRAMES)
            .expect("24-frame minimum overlap should measure");
        assert!(measurement.evidence.bad);
        assert!(measurement.evidence.mean_delta_pq.unwrap() > WINDOW_DELTA_PQ_FAIL);
    }

    #[test]
    fn constant_sample_prefers_the_known_offset_and_short_full_tail_is_measured() {
        let request = sample_request(
            1,
            &SampleWindow {
                start_s: 10.0,
                dur_s: 5.0,
            },
            24.0,
            0,
            48,
            1000,
        );
        let hdr = frame_series(120, 500.0, 512.0, 512.0);
        let dv = frame_series(216, 500.0, 512.0, 512.0);
        let measured = score_window(&request, &hdr, &dv, 10, 10, 24).unwrap();
        assert_eq!(measured.evidence.measured_donor, Some(request.donor));
        let short = frame_series(10, 500.0, 512.0, 512.0);
        assert_eq!(
            score_window(&test_window(), &short, &short, 10, 10, 1)
                .unwrap()
                .evidence
                .measured_overlap_frames,
            10
        );
    }

    #[test]
    fn missing_overlap_is_explicitly_unmeasurable() {
        let request = test_window();
        let hdr = frame_series(10, 500.0, 512.0, 512.0);
        let dv = frame_series(10, 500.0, 512.0, 512.0);
        assert!(score_window(&request, &hdr, &dv, 10, 10, MIN_OVERLAP_FRAMES).is_none());
    }

    #[test]
    fn sampled_request_preserves_offset_beyond_two_seconds() {
        let request = sample_request(
            1,
            &SampleWindow {
                start_s: 12.5,
                dur_s: 5.0,
            },
            24.0,
            120,
            48,
            1_000,
        );
        assert_eq!(
            request.target,
            GradeFrameInterval {
                start_frame: 300,
                end_frame: 420
            }
        );
        assert_eq!(
            request.donor,
            GradeFrameInterval {
                start_frame: 420,
                end_frame: 540
            }
        );
        assert_eq!(request.donor_decode_start_frame, 372);
        assert_eq!(request.lag_range, 0..=96);
    }

    #[test]
    fn chroma_change_is_screened_even_when_y_is_unchanged() {
        let request = test_window();
        let hdr = frame_series(48, 500.0, 512.0, 512.0);
        let dv = frame_series(48, 500.0, 650.0, 400.0);
        let measurement = score_window(&request, &hdr, &dv, 10, 10, MIN_OVERLAP_FRAMES)
            .expect("chroma screen should measure");
        assert!(measurement.chroma_screened);
        assert!(measurement.evidence.mean_delta_pq.unwrap() <= WINDOW_DELTA_PQ_FAIL);
        assert!(measurement.evidence.bad);
    }

    #[test]
    fn unchanged_chroma_and_y_pass_the_interval_screen() {
        let request = test_window();
        let hdr = frame_series(48, 500.0, 512.0, 512.0);
        let dv = frame_series(48, 500.0, 512.0, 512.0);
        let measurement = score_window(&request, &hdr, &dv, 10, 10, MIN_OVERLAP_FRAMES)
            .expect("identical interval should measure");
        assert!(measurement.chroma_screened);
        assert!(!measurement.evidence.bad);
    }
}
