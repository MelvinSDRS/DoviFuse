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
use crate::ffmpeg::{cropdetect_window, SampleWindow};
use crate::logger::Logger;
use crate::runtime::Runtime;

/// cropdetect luma threshold: PQ-encoded black bars are well below 8% code.
const CROP_LIMIT: f64 = 0.08;
/// Per-edge disagreement between windows above which the film likely has a
/// variable aspect ratio (IMAX inserts).
const VARIABLE_AR_PX: u32 = 8;
/// Per-edge tolerance when comparing measured bars to the RPU's own L5.
const L5_MATCH_PX: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bars {
    pub(crate) left: u32,
    pub(crate) right: u32,
    pub(crate) top: u32,
    pub(crate) bottom: u32,
}

impl Bars {
    pub(crate) const ZERO: Bars = Bars {
        left: 0,
        right: 0,
        top: 0,
        bottom: 0,
    };

    fn max_edge_diff(&self, other: &Bars) -> u32 {
        self.left
            .abs_diff(other.left)
            .max(self.right.abs_diff(other.right))
            .max(self.top.abs_diff(other.top))
            .max(self.bottom.abs_diff(other.bottom))
    }
}

/// Convert a cropdetect rectangle into per-edge bar sizes on a canvas.
pub(crate) fn bars_from_crop(
    crop: &crate::ffmpeg::CropRect,
    canvas_w: u32,
    canvas_h: u32,
) -> Option<Bars> {
    if crop.w == 0 || crop.h == 0 || crop.x + crop.w > canvas_w || crop.y + crop.h > canvas_h {
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
            let edge = |k: &str| p.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
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
    Ok(parse_l5_presets(&content))
}

/// cropdetect over the sample windows; None when nothing usable was measured.
pub(crate) fn measure_letterbox(
    rt: &Runtime,
    logger: &Logger,
    file: &Path,
    windows: &[SampleWindow],
    canvas_w: u32,
    canvas_h: u32,
) -> AppResult<Option<(Bars, u32)>> {
    let mut all: Vec<Bars> = Vec::new();
    for w in windows {
        let Some(crop) = cropdetect_window(rt, logger, file, w, CROP_LIMIT)? else {
            continue;
        };
        match bars_from_crop(&crop, canvas_w, canvas_h) {
            Some(b) => all.push(b),
            None => logger.warn(&format!(
                "cropdetect returned a degenerate rectangle ({}x{} at {},{}) in window at {:.0}s, ignored",
                crop.w, crop.h, crop.x, crop.y, w.start_s
            )),
        }
    }
    Ok(aggregate_bars(&all))
}

/// What the editor should do about the active area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActiveAreaChoice {
    /// Leave the RPU's L5 untouched.
    Keep,
    /// Legacy: derive the offsets from the resolution difference.
    Resolution,
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
) -> (ActiveAreaChoice, Vec<(bool, String)>) {
    let mut logs: Vec<(bool, String)> = Vec::new();

    let Some((bars, disagreement)) = measured else {
        logs.push((
            true,
            "Letterbox measurement produced no usable windows - falling back to resolution-based L5"
                .to_string(),
        ));
        return (ActiveAreaChoice::Resolution, logs);
    };

    if disagreement > VARIABLE_AR_PX {
        logs.push((
            true,
            format!(
                "Sample windows disagree on letterbox by up to {disagreement}px - variable aspect ratio (IMAX)? Applying the WIDEST active area; per-scene L5 presets are not generated (TODO)"
            ),
        ));
    }

    let distinct: Vec<Bars> = {
        let mut d: Vec<Bars> = Vec::new();
        for p in dv_presets {
            if !d.contains(p) {
                d.push(*p);
            }
        }
        d
    };

    if canvas_match {
        if !distinct.is_empty() && distinct.iter().all(|p| p.max_edge_diff(&bars) <= L5_MATCH_PX) {
            logs.push((
                false,
                format!(
                    "RPU L5 already matches the measured letterbox (L{} R{} T{} B{}) - keeping it",
                    bars.left, bars.right, bars.top, bars.bottom
                ),
            ));
            return (ActiveAreaChoice::Keep, logs);
        }

        if distinct.len() > 1 {
            // The RPU carries per-scene L5 (variable AR) on the same canvas;
            // that is richer than a single measured preset.
            let widest = distinct
                .iter()
                .fold(distinct[0], |a, b| Bars {
                    left: a.left.min(b.left),
                    right: a.right.min(b.right),
                    top: a.top.min(b.top),
                    bottom: a.bottom.min(b.bottom),
                });
            logs.push((
                false,
                format!(
                    "RPU has {} distinct L5 presets (variable AR) on a matching canvas - keeping per-scene L5",
                    distinct.len()
                ),
            ));
            if widest.max_edge_diff(&bars) > VARIABLE_AR_PX {
                logs.push((
                    true,
                    format!(
                        "Measured widest area (L{} R{} T{} B{}) differs from RPU widest preset (L{} R{} T{} B{}) - verify letterbox after conversion",
                        bars.left, bars.right, bars.top, bars.bottom,
                        widest.left, widest.right, widest.top, widest.bottom
                    ),
                ));
            }
            return (ActiveAreaChoice::Keep, logs);
        }
    }

    let from = distinct
        .first()
        .map(|p| format!("L{} R{} T{} B{}", p.left, p.right, p.top, p.bottom))
        .unwrap_or_else(|| "none".to_string());
    logs.push((
        false,
        format!(
            "Applying measured letterbox L5: L{} R{} T{} B{} (RPU had: {from})",
            bars.left, bars.right, bars.top, bars.bottom
        ),
    ));
    (ActiveAreaChoice::Measured(bars), logs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::CropRect;

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
        let (choice, _) = decide_active_area(Some((bars, 2)), &[bars], true);
        assert_eq!(choice, ActiveAreaChoice::Keep);
        // within tolerance
        let close = Bars {
            top: 278,
            bottom: 282,
            ..bars
        };
        let (choice, _) = decide_active_area(Some((bars, 2)), &[close], true);
        assert_eq!(choice, ActiveAreaChoice::Keep);
    }

    #[test]
    fn decide_keeps_variable_ar_presets_on_matching_canvas() {
        let scope = Bars {
            left: 0,
            right: 0,
            top: 280,
            bottom: 280,
        };
        let (choice, logs) =
            decide_active_area(Some((Bars::ZERO, 280)), &[scope, Bars::ZERO], true);
        assert_eq!(choice, ActiveAreaChoice::Keep);
        assert!(logs.iter().any(|(warn, m)| *warn && m.contains("variable aspect")));
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
        let (choice, _) = decide_active_area(Some((measured, 0)), &[Bars::ZERO], true);
        assert_eq!(choice, ActiveAreaChoice::Measured(measured));
        // different canvas: always measured, even with multiple presets
        let (choice, _) = decide_active_area(
            Some((measured, 0)),
            &[
                Bars::ZERO,
                Bars {
                    left: 0,
                    right: 0,
                    top: 280,
                    bottom: 280,
                },
            ],
            false,
        );
        assert_eq!(choice, ActiveAreaChoice::Measured(measured));
    }

    #[test]
    fn decide_falls_back_without_measurement() {
        let (choice, logs) = decide_active_area(None, &[], true);
        assert_eq!(choice, ActiveAreaChoice::Resolution);
        assert!(logs[0].0);
    }
}
