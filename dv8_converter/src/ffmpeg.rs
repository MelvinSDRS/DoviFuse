//! ffmpeg invocation wrappers and pure parsers for their outputs.

use std::ffi::OsString;
use std::path::Path;

use crate::exec::{run_capture_all, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SampleWindow {
    pub(crate) start_s: f64,
    pub(crate) dur_s: f64,
}

/// Evenly spaced measurement windows within [10%, 85%] of the runtime,
/// skipping studio logos at the head and credits at the tail.
pub(crate) fn sample_windows(duration_s: f64, count: usize, window_s: f64) -> Vec<SampleWindow> {
    if duration_s <= 0.0 || count == 0 || window_s <= 0.0 {
        return Vec::new();
    }

    let lo = duration_s * 0.10;
    let hi = (duration_s * 0.85 - window_s).max(lo);
    let span = hi - lo;

    if span <= 0.0 {
        // Clip too short for spread windows: one window at the start of range.
        let dur = window_s.min(duration_s - lo).max(0.5);
        return vec![SampleWindow {
            start_s: lo,
            dur_s: dur,
        }];
    }

    // Collapse the window count on short clips so windows don't all overlap.
    let n = count.min((span / window_s) as usize + 1).max(1);
    (0..n)
        .map(|i| {
            let t = if n == 1 {
                0.5
            } else {
                i as f64 / (n - 1) as f64
            };
            SampleWindow {
                start_s: lo + span * t,
                dur_s: window_s,
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameLuma {
    pub(crate) yavg: f64,
    pub(crate) ymax: f64,
}

/// Parse `metadata=mode=print` output of the signalstats filter:
/// ```text
/// frame:0    pts:0       pts_time:0
/// lavfi.signalstats.YAVG=501.544
/// lavfi.signalstats.YMAX=900
/// ```
pub(crate) fn parse_signalstats(output: &str) -> Vec<FrameLuma> {
    let mut frames = Vec::new();
    let mut cur_frame: Option<u64> = None;
    let mut yavg: Option<f64> = None;
    let mut ymax: Option<f64> = None;

    // The frame number gates the flush (a stats pair without a preceding
    // frame: line is malformed) but is not stored - measurements are used
    // positionally.
    let mut flush = |frame: Option<u64>, yavg: &mut Option<f64>, ymax: &mut Option<f64>| {
        if let (Some(_), Some(a), Some(m)) = (frame, yavg.take(), ymax.take()) {
            frames.push(FrameLuma { yavg: a, ymax: m });
        }
    };

    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("frame:") {
            flush(cur_frame, &mut yavg, &mut ymax);
            cur_frame = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok());
        } else if let Some(v) = line.strip_prefix("lavfi.signalstats.YAVG=") {
            yavg = v.parse::<f64>().ok();
        } else if let Some(v) = line.strip_prefix("lavfi.signalstats.YMAX=") {
            ymax = v.parse::<f64>().ok();
        }
    }
    flush(cur_frame, &mut yavg, &mut ymax);

    frames
}

/// Parse `metadata=mode=print:key=lavfi.scd.time` output of the scdet filter.
/// Returns the exact frame indices tagged as scene changes (frames are not
/// dropped by scdet, so `frame:N` equals the source frame number under CFR).
pub(crate) fn parse_scdet_frames(output: &str) -> Vec<u64> {
    let mut cur_frame: Option<u64> = None;
    let mut cuts = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("frame:") {
            cur_frame = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok());
        } else if line.starts_with("lavfi.scd.time=") {
            if let Some(f) = cur_frame.take() {
                cuts.push(f);
            }
        }
    }

    cuts
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CropRect {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) x: u32,
    pub(crate) y: u32,
}

/// Parse cropdetect stderr; returns the LAST `crop=W:H:X:Y` occurrence
/// (cropdetect accumulates with reset=0, so the last line is the verdict).
pub(crate) fn parse_cropdetect(stderr: &str) -> Option<CropRect> {
    let mut last: Option<CropRect> = None;

    for line in stderr.lines() {
        let Some(idx) = line.rfind("crop=") else {
            continue;
        };
        let spec = line[idx + "crop=".len()..]
            .split(|c: char| !(c.is_ascii_digit() || c == ':'))
            .next()
            .unwrap_or("");
        let parts: Vec<u32> = spec.split(':').filter_map(|p| p.parse().ok()).collect();
        if parts.len() == 4 {
            last = Some(CropRect {
                w: parts[0],
                h: parts[1],
                x: parts[2],
                y: parts[3],
            });
        }
    }

    last
}

/// Decode a window of `file` and return per-frame luma statistics
/// (code values at the source bit depth). `crop` restricts the measurement
/// to that rectangle — pass the detected active area so letterbox bars
/// don't drag the average down.
pub(crate) fn measure_luma_window(
    rt: &Runtime,
    logger: &Logger,
    file: &Path,
    w: &SampleWindow,
    crop: Option<&CropRect>,
) -> AppResult<Vec<FrameLuma>> {
    let (ffmpeg, _) = rt.require_ffmpeg()?;
    let filter = match crop {
        Some(c) => format!(
            "crop={}:{}:{}:{},signalstats,metadata=mode=print:file=-",
            c.w, c.h, c.x, c.y
        ),
        None => "signalstats,metadata=mode=print:file=-".to_string(),
    };
    let args = vec![
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-ss"),
        OsString::from(format!("{:.3}", w.start_s)),
        OsString::from("-i"),
        file.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-t"),
        OsString::from(format!("{:.3}", w.dur_s)),
        OsString::from("-vf"),
        OsString::from(filter),
        OsString::from("-fps_mode"),
        OsString::from("passthrough"),
        OsString::from("-an"),
        OsString::from("-sn"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ];

    let (stdout, _) = run_capture_all(logger, ffmpeg, &args, false)?;
    Ok(parse_signalstats(&stdout))
}

/// One full downscaled decode of `file`, returning frame indices of detected
/// scene cuts. Expensive (full decode) but run only once per hybrid job.
pub(crate) fn detect_scene_cuts(
    rt: &Runtime,
    logger: &Logger,
    file: &Path,
    threshold: f64,
) -> AppResult<Vec<u64>> {
    let (ffmpeg, _) = rt.require_ffmpeg()?;
    let filter = format!(
        "scale=480:-2:flags=fast_bilinear,scdet=threshold={threshold},metadata=mode=print:key=lavfi.scd.time:file=-"
    );
    let args = vec![
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-v"),
        OsString::from("error"),
        OsString::from("-i"),
        file.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-vf"),
        OsString::from(filter),
        OsString::from("-fps_mode"),
        OsString::from("passthrough"),
        OsString::from("-an"),
        OsString::from("-sn"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ];

    let (stdout, _) = run_capture_all(logger, ffmpeg, &args, false)?;
    Ok(parse_scdet_frames(&stdout))
}

/// Run cropdetect over one window; returns the accumulated crop rectangle.
pub(crate) fn cropdetect_window(
    rt: &Runtime,
    logger: &Logger,
    file: &Path,
    w: &SampleWindow,
    limit: f64,
) -> AppResult<Option<CropRect>> {
    let (ffmpeg, _) = rt.require_ffmpeg()?;
    let args = vec![
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        // cropdetect logs its verdicts at info level on stderr
        OsString::from("-v"),
        OsString::from("info"),
        OsString::from("-ss"),
        OsString::from(format!("{:.3}", w.start_s)),
        OsString::from("-i"),
        file.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from("0:v:0"),
        OsString::from("-t"),
        OsString::from(format!("{:.3}", w.dur_s)),
        OsString::from("-vf"),
        OsString::from(format!("cropdetect=limit={limit}:round=2:reset=0")),
        OsString::from("-an"),
        OsString::from("-sn"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from("-"),
    ];

    let (_, stderr) = run_capture_all(logger, ffmpeg, &args, false)?;
    Ok(parse_cropdetect(&stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_spread_within_bounds() {
        let ws = sample_windows(7200.0, 6, 5.0);
        assert_eq!(ws.len(), 6);
        assert!((ws[0].start_s - 720.0).abs() < 1e-9);
        assert!(ws[5].start_s + ws[5].dur_s <= 7200.0 * 0.85 + 1e-6);
        for pair in ws.windows(2) {
            assert!(pair[1].start_s > pair[0].start_s);
        }
    }

    #[test]
    fn windows_short_clip_single() {
        let ws = sample_windows(10.0, 6, 5.0);
        assert_eq!(ws.len(), 1);
        assert!(ws[0].start_s >= 1.0 - 1e-9);
    }

    #[test]
    fn windows_degenerate() {
        assert!(sample_windows(0.0, 6, 5.0).is_empty());
        assert!(sample_windows(100.0, 0, 5.0).is_empty());
    }

    #[test]
    fn parse_signalstats_real_output() {
        let fixture = "\
frame:0    pts:0       pts_time:0
lavfi.signalstats.YMIN=69
lavfi.signalstats.YLOW=164
lavfi.signalstats.YAVG=501.544
lavfi.signalstats.YHIGH=840
lavfi.signalstats.YMAX=900
lavfi.signalstats.UMIN=17
frame:1    pts:41      pts_time:0.041
lavfi.signalstats.YAVG=502.1
lavfi.signalstats.YMAX=910
";
        let frames = parse_signalstats(fixture);
        assert_eq!(frames.len(), 2);
        assert!((frames[0].yavg - 501.544).abs() < 1e-9);
        assert!((frames[0].ymax - 900.0).abs() < 1e-9);
        assert!((frames[1].yavg - 502.1).abs() < 1e-9);
        assert!((frames[1].ymax - 910.0).abs() < 1e-9);
    }

    #[test]
    fn parse_scdet_real_output() {
        let fixture = "\
frame:96   pts:4000    pts_time:4
lavfi.scd.time=4
frame:192  pts:8000    pts_time:8
lavfi.scd.time=8
frame:1056 pts:44000   pts_time:44
lavfi.scd.time=44
";
        assert_eq!(parse_scdet_frames(fixture), vec![96, 192, 1056]);
    }

    #[test]
    fn parse_cropdetect_takes_last() {
        let fixture = "\
[Parsed_cropdetect_0 @ 0x55] x1:0 x2:639 y1:0 y2:359 w:640 h:352 x:0 y:4 pts:123 t:0.1 crop=640:352:0:4
[Parsed_cropdetect_0 @ 0x55] x1:0 x2:639 y1:46 y2:313 w:640 h:264 x:0 y:48 pts:456 t:0.2 crop=640:264:0:48
";
        assert_eq!(
            parse_cropdetect(fixture),
            Some(CropRect {
                w: 640,
                h: 264,
                x: 0,
                y: 48
            })
        );
        assert_eq!(parse_cropdetect("no crops here"), None);
    }
}
