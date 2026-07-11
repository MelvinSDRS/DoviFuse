use std::ffi::OsString;
use std::path::Path;

use crate::exec::{run_capture, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;

#[derive(Default, Clone)]
pub(crate) struct HybridMediaInfo {
    pub(crate) codec: String,
    pub(crate) codec_id: String,
    pub(crate) frame_count: u64,
    pub(crate) frame_rate: Option<f64>,
    pub(crate) frame_rate_num: Option<f64>,
    pub(crate) frame_rate_den: Option<f64>,
    pub(crate) duration_ms: Option<f64>,
    pub(crate) hdr_format: String,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) bit_depth: Option<u32>,
    pub(crate) colour_primaries: String,
    pub(crate) transfer_characteristics: String,
    pub(crate) frame_rate_mode: String,
    pub(crate) max_cll: Option<u16>,
    pub(crate) max_fall: Option<u16>,
    pub(crate) mastering_min_nits: Option<f64>,
    pub(crate) mastering_max_nits: Option<f64>,
}

pub(crate) fn parse_int(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u64>().ok()
    }
}

pub(crate) fn parse_u16(s: &str) -> Option<u16> {
    parse_int(s).and_then(|v| u16::try_from(v).ok())
}

pub(crate) fn parse_float(s: &str) -> Option<f64> {
    let cleaned = s.replace(',', "").trim().to_string();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

pub(crate) fn parse_mastering_luminance(raw: &str) -> (Option<f64>, Option<f64>) {
    let mut nums: Vec<f64> = Vec::new();
    let mut buf = String::new();

    for ch in raw.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            buf.push(ch);
        } else if !buf.is_empty() {
            if let Ok(v) = buf.parse::<f64>() {
                nums.push(v);
            }
            buf.clear();
        }
    }

    if !buf.is_empty() {
        if let Ok(v) = buf.parse::<f64>() {
            nums.push(v);
        }
    }

    if nums.len() < 2 {
        return (None, None);
    }

    let mut min = nums[0];
    let mut max = nums[0];

    for n in nums {
        if n < min {
            min = n;
        }
        if n > max {
            max = n;
        }
    }

    (Some(min), Some(max))
}

pub(crate) fn fps_from_info(info: &HybridMediaInfo) -> Option<f64> {
    if let (Some(num), Some(den)) = (info.frame_rate_num, info.frame_rate_den) {
        if den > 0.0 {
            return Some(num / den);
        }
    }
    info.frame_rate
}

/// Resolve the mkvextract track ID of the first HEVC video track using
/// `mkvmerge -J` (track `id` there IS the mkvextract TID, unlike mediainfo's
/// stream ID which needs an error-prone -1 adjustment).
pub(crate) fn get_hevc_track_id(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<u64> {
    let args = vec![OsString::from("-J"), file.as_os_str().to_os_string()];
    let out = run_capture(logger, &rt.mkvmerge, &args)?;

    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| {
        format!(
            "Failed to parse mkvmerge -J output for {}: {e}",
            file.display()
        )
    })?;

    let tracks = v
        .get("tracks")
        .and_then(|t| t.as_array())
        .ok_or_else(|| format!("No tracks in mkvmerge -J output for {}", file.display()))?;

    // Prefer the first HEVC video track; fall back to the first video track.
    let mut first_video: Option<u64> = None;
    for t in tracks {
        if t.get("type").and_then(|v| v.as_str()) != Some("video") {
            continue;
        }
        let id = t.get("id").and_then(|v| v.as_u64());
        if first_video.is_none() {
            first_video = id;
        }
        let codec_id = t
            .get("properties")
            .and_then(|p| p.get("codec_id"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if codec_id.starts_with("V_MPEGH/ISO/HEVC") {
            if let Some(id) = id {
                return Ok(id);
            }
        }
    }

    first_video.ok_or_else(|| format!("No video track found in {}", file.display()))
}

pub(crate) fn hybrid_get_media_info(
    file: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<HybridMediaInfo> {
    let template = "--Output=Video;%Format%|%CodecID%|%FrameCount%|%FrameRate%|%FrameRate_Num%|%FrameRate_Den%|%Duration%|%HDR_Format%|%Width%|%Height%|%BitDepth%|%colour_primaries%|%transfer_characteristics%|%FrameRate_Mode%|%ScanType%|%MaxCLL%|%MaxFALL%|%MasteringDisplay_Luminance%";

    let args = vec![OsString::from(template), file.as_os_str().to_os_string()];
    let out = run_capture(logger, &rt.mediainfo, &args)?;
    let line = out
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| format!("No mediainfo video output for {}", file.display()))?;

    let parts: Vec<&str> = line.split('|').collect();
    let get = |idx: usize| -> String {
        parts
            .get(idx)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };

    let mastering = get(17);
    let (mastering_min_nits, mastering_max_nits) = parse_mastering_luminance(&mastering);
    let _ = get(14);

    Ok(HybridMediaInfo {
        codec: get(0),
        codec_id: get(1),
        frame_count: parse_int(&get(2)).unwrap_or(0),
        frame_rate: parse_float(&get(3)),
        frame_rate_num: parse_float(&get(4)),
        frame_rate_den: parse_float(&get(5)),
        duration_ms: parse_float(&get(6)),
        hdr_format: get(7),
        width: parse_int(&get(8)).and_then(|v| u32::try_from(v).ok()),
        height: parse_int(&get(9)).and_then(|v| u32::try_from(v).ok()),
        bit_depth: parse_int(&get(10)).and_then(|v| u32::try_from(v).ok()),
        colour_primaries: get(11),
        transfer_characteristics: get(12),
        frame_rate_mode: get(13),
        max_cll: parse_u16(&get(15)),
        max_fall: parse_u16(&get(16)),
        mastering_min_nits,
        mastering_max_nits,
    })
}

pub(crate) fn hybrid_detect_dv_profile(
    file: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<Option<u8>> {
    let out = run_capture(logger, &rt.mediainfo, &[file.as_os_str().to_os_string()])?;
    let lower = out.to_lowercase();

    if lower.contains("dvhe.05") || lower.contains("profile 5") || lower.contains("profile: 5") {
        return Ok(Some(5));
    }
    if lower.contains("dvhe.07") || lower.contains("profile 7") || lower.contains("profile: 7") {
        return Ok(Some(7));
    }
    if lower.contains("dvhe.08")
        || lower.contains("profile 8")
        || lower.contains("profile: 8")
        || lower.contains("profile 8.1")
    {
        return Ok(Some(8));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mastering_luminance_min_max() {
        let (min, max) = parse_mastering_luminance("min: 0.0050 cd/m2, max: 4000 cd/m2");
        assert_eq!(min, Some(0.0050));
        assert_eq!(max, Some(4000.0));
    }

    #[test]
    fn mastering_luminance_missing() {
        assert_eq!(parse_mastering_luminance(""), (None, None));
        assert_eq!(parse_mastering_luminance("max: 1000"), (None, None));
    }

    #[test]
    fn parse_int_strips_units() {
        assert_eq!(parse_int("129 597"), Some(129_597));
        assert_eq!(parse_int(""), None);
    }
}
