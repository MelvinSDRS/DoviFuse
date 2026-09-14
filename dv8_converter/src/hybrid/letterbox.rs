//! Measured letterbox handling for the RPU's L5 (active area) metadata.
//!
//! The DV RPU's L5 offsets must describe the letterbox of the HDR target the
//! RPU is being injected into, not whatever canvas the WEB-DL used. Instead
//! of guessing from the resolution difference, run cropdetect over the same
//! sampled windows as the grade check and measure the actual black bars.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::exec::{run_status, AppResult};
use crate::ffmpeg::{
    cropdetect_window, measure_luma_window, plausible_crop, CropRect, FrameLuma, SampleWindow,
};
use crate::logger::Logger;
use crate::pq::code_limited_to_pq;
use crate::runtime::Runtime;
use serde::Serialize;

/// cropdetect luma threshold: PQ-encoded black bars are well below 8% code.
const CROP_LIMIT: f64 = 0.08;
/// Per-edge disagreement between windows above which the film likely has a
/// variable aspect ratio (IMAX inserts).
const VARIABLE_AR_PX: u32 = 8;
/// Per-edge tolerance when comparing measured bars to the RPU's own L5.
const L5_MATCH_PX: u32 = 4;
/// A single cropdetect observation is not enough evidence to rewrite L5.
const MIN_USABLE_WINDOWS: usize = 3;
/// Reject windows whose active picture is effectively unlit.
const MIN_ACTIVE_YMAX: f64 = 0.08;
const MIN_ACTIVE_YAVG: f64 = 0.04;
/// A single bright object or flash must not authorize an active-area edit.
const MIN_LIT_FRAME_FRACTION: f64 = 0.50;

pub(crate) use super::active_area::Bars;

#[derive(Serialize)]
struct LetterboxWindowObservation {
    start_s: f64,
    end_s: f64,
    status: &'static str,
    reason: Option<String>,
    crop: Option<LetterboxCrop>,
    bars: Option<Bars>,
    frames: usize,
    lit_frames: usize,
    max_ymax_normalized: Option<f64>,
    mean_yavg_normalized: Option<f64>,
}

#[derive(Serialize)]
struct LetterboxCrop {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
}

#[derive(Serialize)]
struct LetterboxMeasurement {
    requested_windows: usize,
    usable_windows: usize,
    skipped_windows: usize,
    unattempted_windows: usize,
    minimum_usable_windows: usize,
    spread_px: Option<u32>,
    bars: Option<Bars>,
    error: Option<String>,
    observations: Vec<LetterboxWindowObservation>,
}

fn crop_report(crop: &CropRect) -> LetterboxCrop {
    LetterboxCrop {
        width: crop.w,
        height: crop.h,
        x: crop.x,
        y: crop.y,
    }
}

fn brightness_summary(frames: &[FrameLuma]) -> (usize, Option<f64>, Option<f64>) {
    if frames.is_empty() {
        return (0, None, None);
    }
    let normalized = |code: f64| code_limited_to_pq(code, 10);
    let lit_frames = frames
        .iter()
        .filter(|frame| normalized(frame.yavg) >= MIN_ACTIVE_YAVG)
        .count();
    let max_ymax = frames
        .iter()
        .map(|frame| normalized(frame.ymax))
        .fold(0.0, f64::max);
    let mean_yavg = frames
        .iter()
        .map(|frame| normalized(frame.yavg))
        .sum::<f64>()
        / frames.len() as f64;
    (lit_frames, Some(max_ymax), Some(mean_yavg))
}

fn usable_observation(
    crop: Option<&CropRect>,
    frames: &[FrameLuma],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(Bars, usize, Option<f64>, Option<f64>), String> {
    let (lit_frames, max_ymax, mean_yavg) = brightness_summary(frames);
    let Some(max_ymax_value) = max_ymax else {
        return Err("uncropped luma measurement returned no frames".to_string());
    };
    let Some(mean_yavg_value) = mean_yavg else {
        return Err("uncropped luma measurement returned no average".to_string());
    };
    let minimum_lit_frames =
        ((frames.len() as f64 * MIN_LIT_FRAME_FRACTION).ceil() as usize).max(3);
    if max_ymax_value < MIN_ACTIVE_YMAX
        || mean_yavg_value < MIN_ACTIVE_YAVG
        || lit_frames < minimum_lit_frames
    {
        return Err(format!(
            "dark or insufficient active picture (YMAX {:.3}, YAVG {:.3}, lit {lit_frames}/{}, need {minimum_lit_frames})",
            max_ymax_value,
            mean_yavg_value,
            frames.len()
        ));
    }
    let Some(crop) = crop else {
        return Err("cropdetect returned no rectangle".to_string());
    };
    if !plausible_crop(crop, canvas_w, canvas_h) {
        return Err(format!(
            "implausible crop {}x{} at {},{} for {}x{} canvas",
            crop.w, crop.h, crop.x, crop.y, canvas_w, canvas_h
        ));
    }
    let bars = bars_from_crop(crop, canvas_w, canvas_h)
        .ok_or_else(|| "cropdetect returned a degenerate rectangle".to_string())?;
    Ok((bars, lit_frames, max_ymax, mean_yavg))
}

fn enough_usable_windows(count: usize) -> bool {
    count >= MIN_USABLE_WINDOWS
}

/// Convert a cropdetect rectangle into per-edge bar sizes on a canvas.
pub(crate) fn bars_from_crop(
    crop: &crate::ffmpeg::CropRect,
    canvas_w: u32,
    canvas_h: u32,
) -> Option<Bars> {
    if crop.w == 0
        || crop.h == 0
        || crop.x.checked_add(crop.w).is_none_or(|v| v > canvas_w)
        || crop.y.checked_add(crop.h).is_none_or(|v| v > canvas_h)
    {
        return None;
    }
    // Force even bars: L5 offsets apply to chroma-subsampled video.
    let even = |v: u32| v & !1;
    Some(Bars {
        left: even(crop.x),
        right: even(canvas_w - crop.w - crop.x),
        top: even(crop.y),
        bottom: even(canvas_h - crop.h - crop.y),
    })
}

/// Aggregate window measurements: minimum bar per edge (the widest active
/// area seen — safe for variable-AR films, where IMAX scenes open up), plus
/// the maximum per-edge disagreement across windows.
pub(crate) fn aggregate_bars(bars: &[Bars]) -> Option<(Bars, u32)> {
    let first = *bars.first()?;
    let agg = bars.iter().fold(first, |a, b| Bars {
        left: a.left.min(b.left),
        right: a.right.min(b.right),
        top: a.top.min(b.top),
        bottom: a.bottom.min(b.bottom),
    });
    let disagreement = bars.iter().map(|b| agg.max_edge_diff(b)).max().unwrap_or(0);
    Some((agg, disagreement))
}

/// Parse the presets out of a `dovi_tool export -d level5` config JSON.
pub(crate) fn parse_l5_presets(json: &str) -> Vec<Bars> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(presets) = v.get("presets").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    presets
        .iter()
        .filter_map(|p| {
            let edge = |k: &str| {
                p.get(k)
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u32::try_from(v).ok())
            };
            Some(Bars {
                left: edge("left")?,
                right: edge("right")?,
                top: edge("top")?,
                bottom: edge("bottom")?,
            })
        })
        .collect()
}

/// Export and parse the DV RPU's own L5 presets.
pub(crate) fn dv_rpu_l5_presets(
    rpu_file: &Path,
    l5_json: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<Vec<Bars>> {
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
            OsString::from(format!("level5={}", l5_json.display())),
        ],
    )?;

    let content = fs::read_to_string(l5_json)
        .map_err(|e| format!("Failed to read L5 export {}: {e}", l5_json.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("Invalid source L5 export: {e}"))?;
    let count = value["presets"]
        .as_array()
        .ok_or("Missing source L5 presets")?
        .len();
    let presets = parse_l5_presets(&content);
    if presets.is_empty() || presets.len() != count {
        return Err("Invalid or incomplete source L5 presets".into());
    }
    Ok(presets)
}

/// cropdetect and uncropped luma over sample windows; None when evidence is
/// missing or thinner than the minimum three distinct usable windows.
pub(crate) fn measure_letterbox(
    rt: &Runtime,
    logger: &Logger,
    file: &Path,
    windows: &[SampleWindow],
    canvas_w: u32,
    canvas_h: u32,
) -> AppResult<Option<(Bars, u32)>> {
    let mut all: Vec<Bars> = Vec::new();
    let mut observations = Vec::with_capacity(windows.len());
    let mut fatal_error: Option<String> = None;
    for w in windows {
        let end_s = w.start_s + w.dur_s;
        if observations
            .iter()
            .any(|observation: &LetterboxWindowObservation| {
                (observation.start_s - w.start_s).abs() < 0.001
                    && (observation.end_s - end_s).abs() < 0.001
            })
        {
            observations.push(LetterboxWindowObservation {
                start_s: w.start_s,
                end_s,
                status: "skipped",
                reason: Some("duplicate sample window".to_string()),
                crop: None,
                bars: None,
                frames: 0,
                lit_frames: 0,
                max_ymax_normalized: None,
                mean_yavg_normalized: None,
            });
            continue;
        }
        let crop = match cropdetect_window(rt, logger, file, w, CROP_LIMIT) {
            Ok(crop) => crop,
            Err(error) => {
                observations.push(LetterboxWindowObservation {
                    start_s: w.start_s,
                    end_s,
                    status: "skipped",
                    reason: Some(format!("cropdetect failed: {error}")),
                    crop: None,
                    bars: None,
                    frames: 0,
                    lit_frames: 0,
                    max_ymax_normalized: None,
                    mean_yavg_normalized: None,
                });
                fatal_error = Some(format!(
                    "cropdetect failed in window at {:.3}s: {error}",
                    w.start_s
                ));
                break;
            }
        };
        let frames = match measure_luma_window(rt, logger, file, w, None) {
            Ok(frames) => frames,
            Err(error) => {
                observations.push(LetterboxWindowObservation {
                    start_s: w.start_s,
                    end_s,
                    status: "skipped",
                    reason: Some(format!("uncropped luma measurement failed: {error}")),
                    crop: crop.as_ref().map(crop_report),
                    bars: None,
                    frames: 0,
                    lit_frames: 0,
                    max_ymax_normalized: None,
                    mean_yavg_normalized: None,
                });
                fatal_error = Some(format!(
                    "uncropped luma measurement failed in window at {:.3}s: {error}",
                    w.start_s
                ));
                break;
            }
        };
        let crop_report_value = crop.as_ref().map(crop_report);
        match usable_observation(crop.as_ref(), &frames, canvas_w, canvas_h) {
            Ok((bars, lit_frames, max_ymax_normalized, mean_yavg_normalized)) => {
                all.push(bars);
                observations.push(LetterboxWindowObservation {
                    start_s: w.start_s,
                    end_s,
                    status: "usable",
                    reason: None,
                    crop: crop_report_value,
                    bars: Some(bars),
                    frames: frames.len(),
                    lit_frames,
                    max_ymax_normalized,
                    mean_yavg_normalized,
                });
            }
            Err(reason) => {
                logger.warn(&format!(
                    "Letterbox window at {:.0}-{:.0}s skipped: {reason}",
                    w.start_s, end_s
                ));
                let (lit_frames, max_ymax_normalized, mean_yavg_normalized) =
                    brightness_summary(&frames);
                observations.push(LetterboxWindowObservation {
                    start_s: w.start_s,
                    end_s,
                    status: "skipped",
                    reason: Some(reason),
                    crop: crop_report_value,
                    bars: None,
                    frames: frames.len(),
                    lit_frames,
                    max_ymax_normalized,
                    mean_yavg_normalized,
                });
            }
        }
    }
    let aggregate = aggregate_bars(&all);
    let (bars, spread_px) = aggregate
        .map(|(bars, spread)| (Some(bars), Some(spread)))
        .unwrap_or((None, None));
    let usable_windows = all.len();
    let skipped_windows = observations.len().saturating_sub(usable_windows);
    let unattempted_windows = windows.len().saturating_sub(observations.len());
    let report = LetterboxMeasurement {
        requested_windows: windows.len(),
        usable_windows,
        skipped_windows,
        unattempted_windows,
        minimum_usable_windows: MIN_USABLE_WINDOWS,
        spread_px,
        bars,
        error: fatal_error.clone(),
        observations,
    };
    logger.measurement(
        "letterbox",
        serde_json::to_value(&report).expect("letterbox measurement is serializable"),
    );
    if let Some(error) = fatal_error {
        return Err(error);
    }
    if !enough_usable_windows(usable_windows) {
        logger.warn(&format!(
            "Active-area measurement is inconclusive: {usable_windows} usable of {} requested windows (need at least {MIN_USABLE_WINDOWS})",
            windows.len()
        ));
        return Ok(None);
    }
    Ok(aggregate_bars(&all))
}

/// What the editor should do about the active area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActiveAreaChoice {
    /// Leave the RPU's L5 untouched.
    Keep,
    /// Reset L5 and apply these measured offsets to all frames.
    Measured(Bars),
}

/// Decide the active-area edit from the measurement, the RPU's own L5
/// presets, and whether the two canvases match. Returns the choice plus
/// (is_warning, message) log lines.
pub(crate) fn decide_active_area(
    measured: Option<(Bars, u32)>,
    dv_presets: &[Bars],
    canvas_match: bool,
) -> Result<(ActiveAreaChoice, Vec<(bool, String)>), String> {
    let (bars, disagreement) = measured.ok_or(
        "Active-area measurement is inconclusive: insufficient usable windows. No resolution fallback or source deletion is allowed.",
    )?;
    if disagreement > VARIABLE_AR_PX {
        return Err(format!("Active-area measurement is inconclusive: sampled windows disagree by {disagreement}px. A frame-addressed timeline is required; refusing to flatten variable aspect ratios."));
    }
    let mut distinct = Vec::new();
    for p in dv_presets {
        if !distinct.contains(p) {
            distinct.push(*p);
        }
    }
    if distinct.len() > 1 {
        return Err("Active-area measurement is inconclusive: source RPU contains several L5 presets whose target frame intervals have not been verified. Refusing to guess or flatten them.".into());
    }
    if canvas_match
        && distinct
            .first()
            .is_some_and(|p| p.max_edge_diff(&bars) <= L5_MATCH_PX)
    {
        return Ok((ActiveAreaChoice::Keep,vec![(false,"RPU L5 agrees with sampled target bars; whole-timeline picture coverage remains unverified".into())]));
    }
    Ok((ActiveAreaChoice::Measured(bars),vec![(false,format!("Applying sampled L5: L{} R{} T{} B{}; whole-timeline picture coverage remains unverified",bars.left,bars.right,bars.top,bars.bottom))]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::{CropRect, FrameLuma};

    #[test]
    fn bars_from_crop_scope_on_uhd() {
        // 3840x2160 canvas, 3840x1600 picture centered: 280px bars
        let c = CropRect {
            w: 3840,
            h: 1600,
            x: 0,
            y: 280,
        };
        assert_eq!(
            bars_from_crop(&c, 3840, 2160),
            Some(Bars {
                left: 0,
                right: 0,
                top: 280,
                bottom: 280
            })
        );
    }

    #[test]
    fn bars_from_crop_rejects_degenerate() {
        let c = CropRect {
            w: 4000,
            h: 100,
            x: 0,
            y: 0,
        };
        assert_eq!(bars_from_crop(&c, 3840, 2160), None);
        let c = CropRect {
            w: 0,
            h: 0,
            x: 0,
            y: 0,
        };
        assert_eq!(bars_from_crop(&c, 3840, 2160), None);
    }

    #[test]
    fn aggregate_takes_min_per_edge_and_reports_spread() {
        let scope = Bars {
            left: 0,
            right: 0,
            top: 280,
            bottom: 280,
        };
        let imax = Bars {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        };
        let (agg, spread) = aggregate_bars(&[scope, imax, scope]).unwrap();
        assert_eq!(agg, imax); // widest active area wins
        assert_eq!(spread, 280);
        assert!(aggregate_bars(&[]).is_none());
    }

    #[test]
    fn parse_l5_export_format() {
        let json = r#"{
          "crop": true,
          "presets": [
            {"id": 0, "left": 0, "right": 0, "top": 280, "bottom": 280},
            {"id": 1, "left": 0, "right": 0, "top": 0, "bottom": 0}
          ],
          "edits": {"0-500": 0, "501-600": 1}
        }"#;
        let presets = parse_l5_presets(json);
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].top, 280);
        assert_eq!(presets[1], Bars::ZERO);
        assert!(parse_l5_presets("not json").is_empty());
    }

    #[test]
    fn decide_keeps_matching_l5() {
        let bars = Bars {
            left: 0,
            right: 0,
            top: 280,
            bottom: 280,
        };
        let (choice, _) = decide_active_area(Some((bars, 2)), &[bars], true).unwrap();
        assert_eq!(choice, ActiveAreaChoice::Keep);
        // within tolerance
        let close = Bars {
            top: 278,
            bottom: 282,
            ..bars
        };
        let (choice, _) = decide_active_area(Some((bars, 2)), &[close], true).unwrap();
        assert_eq!(choice, ActiveAreaChoice::Keep);
    }

    #[test]
    fn decide_rejects_variable_ar_even_on_matching_canvas() {
        let scope = Bars {
            top: 280,
            bottom: 280,
            ..Bars::ZERO
        };
        assert!(decide_active_area(Some((Bars::ZERO, 280)), &[scope, Bars::ZERO], true).is_err());
        assert!(decide_active_area(Some((Bars::ZERO, 0)), &[scope, Bars::ZERO], true).is_err());
    }

    #[test]
    fn decide_applies_measured_on_mismatch() {
        let measured = Bars {
            left: 0,
            right: 0,
            top: 60,
            bottom: 60,
        };
        // RPU says zero bars but target is letterboxed
        let (choice, _) = decide_active_area(Some((measured, 0)), &[Bars::ZERO], true).unwrap();
        assert_eq!(choice, ActiveAreaChoice::Measured(measured));
        assert!(decide_active_area(
            Some((measured, 0)),
            &[
                Bars::ZERO,
                Bars {
                    top: 280,
                    bottom: 280,
                    ..Bars::ZERO
                }
            ],
            false
        )
        .is_err());
    }

    #[test]
    fn decide_refuses_without_measurement() {
        let error = decide_active_area(None, &[], false).unwrap_err();
        assert!(error.contains("inconclusive"));
        assert!(error.contains("No resolution fallback"));
    }

    #[test]
    fn dark_observation_is_not_usable() {
        let frames = vec![
            FrameLuma {
                frame: 0,
                yavg: 64.0,
                ymax: 64.0,
                uavg: None,
                vavg: None,
            };
            120
        ];
        let crop = CropRect {
            w: 3840,
            h: 1600,
            x: 0,
            y: 280,
        };
        let error = usable_observation(Some(&crop), &frames, 3840, 2160).unwrap_err();
        assert!(error.contains("dark"));
    }

    #[test]
    fn mostly_black_window_with_one_flash_is_not_usable() {
        let mut frames = vec![
            FrameLuma {
                frame: 0,
                yavg: 64.0,
                ymax: 64.0,
                uavg: None,
                vavg: None,
            };
            120
        ];
        frames[60] = FrameLuma {
            frame: 60,
            yavg: 800.0,
            ymax: 900.0,
            uavg: None,
            vavg: None,
        };
        let crop = CropRect {
            w: 3840,
            h: 1600,
            x: 0,
            y: 280,
        };
        let error = usable_observation(Some(&crop), &frames, 3840, 2160).unwrap_err();
        assert!(error.contains("dark"));
        assert!(error.contains("lit 1/120"));
    }

    #[test]
    fn genuinely_bright_letterboxed_window_is_usable() {
        let frames = vec![
            FrameLuma {
                frame: 0,
                yavg: 500.0,
                ymax: 850.0,
                uavg: None,
                vavg: None,
            };
            120
        ];
        let crop = CropRect {
            w: 3840,
            h: 1600,
            x: 0,
            y: 280,
        };
        let (bars, lit_frames, max_ymax, mean_yavg) =
            usable_observation(Some(&crop), &frames, 3840, 2160).unwrap();
        assert_eq!(bars.top, 280);
        assert_eq!(bars.bottom, 280);
        assert_eq!(lit_frames, 120);
        assert!(max_ymax.unwrap() > MIN_ACTIVE_YMAX);
        assert!(mean_yavg.unwrap() > MIN_ACTIVE_YAVG);
    }

    #[test]
    fn implausible_crop_is_not_one_usable_window() {
        let frames = vec![
            FrameLuma {
                frame: 0,
                yavg: 500.0,
                ymax: 900.0,
                uavg: None,
                vavg: None,
            };
            120
        ];
        let crop = CropRect {
            w: 320,
            h: 180,
            x: 0,
            y: 0,
        };
        let error = usable_observation(Some(&crop), &frames, 3840, 2160).unwrap_err();
        assert!(error.contains("implausible"));
        assert!(!enough_usable_windows(1));
    }

    #[test]
    fn thin_evidence_requires_three_distinct_windows() {
        assert!(!enough_usable_windows(0));
        assert!(!enough_usable_windows(1));
        assert!(!enough_usable_windows(2));
        assert!(enough_usable_windows(3));
    }
}
