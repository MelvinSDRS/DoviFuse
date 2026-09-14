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
    /// MediaInfo's HDR_Format_Profile, when the video track exposes it. This
    /// is deliberately kept separate from `hybrid_detect_dv_profile`: the
    /// latter only needs the broad Dolby Vision profile family, while donor
    /// eligibility must reject an explicitly declared P8.4/HLG track.
    pub(crate) hdr_format_profile: String,
    /// MediaInfo's HDR_Format_Compatibility. Hybrid donors must explicitly
    /// advertise HDR10 compatibility; transfer metadata alone is not enough.
    pub(crate) hdr_format_compatibility: String,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) bit_depth: Option<u32>,
    pub(crate) colour_primaries: String,
    pub(crate) transfer_characteristics: String,
    pub(crate) colour_range: String,
    pub(crate) matrix_coefficients: String,
    pub(crate) frame_rate_mode: String,
    pub(crate) max_cll: Option<u16>,
    pub(crate) max_fall: Option<u16>,
    pub(crate) mastering_min_nits: Option<f64>,
    pub(crate) mastering_max_nits: Option<f64>,
    pub(crate) static_metadata_errors: Vec<String>,
}

pub(crate) fn parse_int(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u64>().ok()
    }
}

fn parse_light_level(raw: &str, name: &str, errors: &mut Vec<String>) -> Option<u16> {
    if raw.trim().is_empty() {
        return None;
    }
    let numeric = raw
        .strip_suffix("cd/m2")
        .unwrap_or(raw)
        .trim()
        .replace([',', ' '], "");
    match numeric.parse::<u16>() {
        Ok(value) => Some(value),
        Err(_) => {
            errors.push(format!("Invalid {name} value: {raw}"));
            None
        }
    }
}

pub(crate) fn parse_float(s: &str) -> Option<f64> {
    let cleaned = s.replace(',', "").trim().to_string();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok().filter(|v| v.is_finite())
}

pub(crate) fn parse_mastering_luminance(raw: &str) -> (Option<f64>, Option<f64>) {
    // Read labeled values, never the "2" from cd/m2 or the extrema of
    // unrelated numbers. Preserve the declared ordering and sign so the
    // reconciliation policy can reject invalid metadata instead of repairing it.
    fn field(raw: &str, label: &str) -> Option<f64> {
        let mut matches = raw.match_indices(label);
        let (index, _) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        let token = raw[index + label.len()..].split_whitespace().next()?;
        token
            .trim_end_matches(',')
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    }
    (field(raw, "min:"), field(raw, "max:"))
}

pub(crate) fn fps_from_info(info: &HybridMediaInfo) -> Option<f64> {
    if let (Some(num), Some(den)) = (info.frame_rate_num, info.frame_rate_den) {
        if den > 0.0 && num > 0.0 && num.is_finite() && den.is_finite() {
            return Some(num / den);
        }
    }
    info.frame_rate.filter(|v| v.is_finite() && *v > 0.0)
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

    // All decoding, RPU extraction and probing must describe the same video.
    // Multi-video remuxing needs explicit selection and preservation support.
    if tracks
        .iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("video"))
        .count()
        != 1
    {
        return Err(
            "Exactly one video track is required; multi-video inputs are unsupported".to_string(),
        );
    }
    for t in tracks {
        if t.get("type").and_then(|v| v.as_str()) != Some("video") {
            continue;
        }
        let id = t.get("id").and_then(|v| v.as_u64());
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

    Err(format!("No HEVC video track found in {}", file.display()))
}

pub(crate) fn has_hdr10_base(info: &HybridMediaInfo) -> bool {
    let transfer = info.transfer_characteristics.to_ascii_lowercase();
    info.bit_depth == Some(10)
        && info.colour_primaries.contains("2020")
        && (transfer.contains("2084") || transfer == "pq")
}

/// The donor families that the hybrid editor can consume. A MediaInfo P8
/// track with explicit HDR10/PQ metadata is treated as P8.1-compatible. The
/// profile detector cannot distinguish P8.1 from P8.4 by the Dolby Vision
/// header alone, so this type intentionally represents the broad family and
/// `hybrid_donor_eligibility` performs the stricter HDR10/PQ checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SupportedHybridDonor {
    Profile7,
    Profile8,
}

fn has_any_token(value: &str, tokens: &[&str]) -> bool {
    let lower = value.to_ascii_lowercase();
    tokens.iter().any(|token| lower.contains(token))
}

fn metadata_parts(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', '/', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

fn normalized_metadata(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && !matches!(ch, '-' | '_' | '.'))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn is_single_metadata_alias(value: &str, aliases: &[&str]) -> bool {
    let parts: Vec<_> = metadata_parts(value).collect();
    parts.len() == 1
        && aliases
            .iter()
            .any(|alias| normalized_metadata(parts[0]) == *alias)
}

fn profile_flags(text: &str) -> Result<(bool, bool), ()> {
    let lower = text.to_ascii_lowercase();
    let trimmed = lower.trim();
    let mut has7 = lower.contains("dvhe.07");
    let mut has8 = lower.contains("dvhe.08");

    // MediaInfo can expose a named profile ("Profile 8.1") separately from
    // the dvhe.08.<level> codec token. Parse named declarations strictly so
    // Profile 80 and unsupported Profile 8.x values cannot pass as P8.
    let mut rest = lower.as_str();
    while let Some(index) = rest.find("profile") {
        let after = &rest[index + "profile".len()..];
        let candidate = after.trim_start_matches([' ', ':', '_', '-']);
        let token_end = candidate
            .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
            .unwrap_or(candidate.len());
        let token = &candidate[..token_end];
        if token.is_empty() {
            return Err(());
        }
        let boundary = candidate[token_end..].chars().next();
        if boundary.is_some_and(|ch| {
            !ch.is_ascii_whitespace() && !matches!(ch, ',' | '/' | ';' | '_' | '-')
        }) {
            return Err(());
        }

        let is_p7 = token == "7"
            || (token.starts_with("7.")
                && token.len() > 2
                && token[2..].chars().all(|ch| ch.is_ascii_digit()));
        let is_p8 = token == "8" || token == "8.1";
        if !is_p7 && !is_p8 {
            return Err(());
        }
        if is_p7 {
            has7 = true;
        } else {
            has8 = true;
        }
        rest = &candidate[token_end..];
        if rest.is_empty() {
            break;
        }
    }

    // A bare profile field is also seen in some MediaInfo versions.
    if let Some(suffix) = trimmed.strip_prefix("7.") {
        if suffix.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(());
        }
        has7 = true;
    } else if trimmed == "7" {
        has7 = true;
    }
    if trimmed == "8" || trimmed == "8.1" {
        has8 = true;
    }
    Ok((has7, has8))
}

fn has_p84_evidence(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("profile 8.4")
        || lower.contains("profile: 8.4")
        || lower.contains("profile8.4")
        || lower.contains("profile 8_4")
        || lower.contains("profile: 8_4")
        || lower.contains("profile8_4")
        || lower.contains("p8.4")
        || lower.contains("p8_4")
        || lower.trim() == "8.4"
        || lower.trim() == "8_4"
}

/// Check the metadata contract required before a DV RPU is used as a hybrid
/// donor. This gate intentionally does not inspect RPU contents; the caller
/// must validate the extracted RPU separately.
pub(crate) fn hybrid_donor_eligibility(
    info: &HybridMediaInfo,
    dv_profile: Option<u8>,
) -> Result<SupportedHybridDonor, String> {
    let all_hdr_text = format!(
        "{} {} {}",
        info.hdr_format, info.hdr_format_profile, info.hdr_format_compatibility
    );

    if has_any_token(&all_hdr_text, &["hlg", "arib-std-b67", "sdr"]) {
        return Err("MediaInfo reports HLG/SDR evidence; an HDR10 donor is required".to_string());
    }
    if has_p84_evidence(&info.hdr_format) || has_p84_evidence(&info.hdr_format_profile) {
        return Err("MediaInfo reports Dolby Vision Profile 8.4; only P8.1-compatible HDR10 donors are supported".to_string());
    }

    let donor = match dv_profile {
        Some(7) => SupportedHybridDonor::Profile7,
        Some(8) => SupportedHybridDonor::Profile8,
        Some(profile) => return Err(format!("unsupported Dolby Vision donor profile {profile}")),
        None => return Err("Dolby Vision donor profile is missing or undetectable".to_string()),
    };

    // A separately detected broad profile and an explicit MediaInfo profile
    // must agree when MediaInfo provides the latter. A missing subtype is
    // allowed because some MediaInfo versions only expose dvhe.08; explicit
    // P8.4 is rejected above and the HDR10/PQ checks below identify the
    // supported P8.1-compatible base layer.
    let (hdr_declared7, hdr_declared8) = match profile_flags(&info.hdr_format) {
        Ok(flags) => flags,
        Err(()) => {
            return Err(
                "MediaInfo Dolby Vision profile is not a supported P7/P8.1 declaration".to_string(),
            )
        }
    };
    let (profile_declared7, profile_declared8) = match profile_flags(&info.hdr_format_profile) {
        Ok(flags) => flags,
        Err(()) => {
            return Err(
                "MediaInfo Dolby Vision profile is not a supported P7/P8.1 declaration".to_string(),
            )
        }
    };
    let declared7 = hdr_declared7 || profile_declared7;
    let declared8 = hdr_declared8 || profile_declared8;
    if declared7 && declared8 {
        return Err(
            "MediaInfo Dolby Vision profile contains contradictory P7 and P8 declarations"
                .to_string(),
        );
    }
    if declared7 || declared8 {
        let declared = if declared7 {
            SupportedHybridDonor::Profile7
        } else {
            SupportedHybridDonor::Profile8
        };
        if declared != donor {
            return Err(format!(
                "MediaInfo Dolby Vision profile contradicts the detected donor profile (declared {declared:?}, detected {donor:?})"
            ));
        }
    } else if !info.hdr_format_profile.trim().is_empty() {
        return Err(format!(
            "MediaInfo Dolby Vision profile is not a supported P7/P8 declaration: {}",
            info.hdr_format_profile.trim()
        ));
    }

    let compatibility = info.hdr_format_compatibility.trim();
    if compatibility.is_empty() {
        return Err("MediaInfo HDR_Format_Compatibility is missing; explicit HDR10 compatibility is required".to_string());
    }
    let compatibility_lower = compatibility.to_ascii_lowercase();
    if !compatibility_lower.contains("hdr10") {
        return Err(format!(
            "MediaInfo HDR_Format_Compatibility does not advertise HDR10: {compatibility}"
        ));
    }
    if has_any_token(compatibility, &["hlg", "arib-std-b67", "sdr"]) {
        return Err(format!(
            "MediaInfo HDR_Format_Compatibility is contradictory for an HDR10 donor: {compatibility}"
        ));
    }

    if info.bit_depth != Some(10) {
        return Err("hybrid donor must be exactly 10-bit".to_string());
    }

    if !is_single_metadata_alias(&info.colour_primaries, &["bt2020", "bt2020nc"]) {
        return Err(format!(
            "hybrid donor must use BT.2020 primaries: {}",
            info.colour_primaries.trim()
        ));
    }

    let transfer = info.transfer_characteristics.trim();
    if !is_single_metadata_alias(transfer, &["pq", "smptest2084", "st2084"]) {
        return Err(format!(
            "hybrid donor must use PQ transfer characteristics: {}",
            transfer
        ));
    }

    let range = info.colour_range.trim();
    if !is_single_metadata_alias(range, &["limited", "limitedrange", "tv", "mpeg"]) {
        return Err(format!(
            "hybrid donor must use limited/video range: {}",
            range
        ));
    }

    let matrix = info.matrix_coefficients.trim();
    if !is_single_metadata_alias(matrix, &["bt2020nonconstant", "bt2020nc"]) {
        return Err(format!(
            "hybrid donor must use the BT.2020 non-constant matrix: {}",
            matrix
        ));
    }

    Ok(donor)
}

/// Pick the video-track line the hybrid pipeline actually processes: the
/// first HEVC track, falling back to the first video track — the same
/// preference `get_hevc_track_id` uses for extraction, so probe and
/// extraction can't describe different tracks.
pub(crate) fn parse_hybrid_media_info(out: &str) -> Option<HybridMediaInfo> {
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    let line = lines
        .iter()
        .find(|l| {
            let mut it = l.split('|');
            let format = it.next().unwrap_or("").trim().to_ascii_lowercase();
            let codec_id = it.next().unwrap_or("").trim().to_ascii_lowercase();
            format.contains("hevc") || codec_id.contains("hevc") || codec_id.contains("dvhe")
        })
        .or_else(|| lines.first())?;

    let parts: Vec<&str> = line.split('|').collect();
    let get = |idx: usize| -> String {
        parts
            .get(idx)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };

    let mastering = get(17);
    let (mastering_min_nits, mastering_max_nits) = parse_mastering_luminance(&mastering);
    let mut static_metadata_errors = Vec::new();
    let max_cll = parse_light_level(&get(15), "MaxCLL", &mut static_metadata_errors);
    let max_fall = parse_light_level(&get(16), "MaxFALL", &mut static_metadata_errors);
    for (label, value) in [("min:", mastering_min_nits), ("max:", mastering_max_nits)] {
        if mastering.contains(label) && value.is_none() {
            static_metadata_errors.push(format!(
                "Invalid mastering luminance {label} in {mastering}"
            ));
        }
    }
    if !mastering.is_empty() && !mastering.contains("min:") && !mastering.contains("max:") {
        static_metadata_errors.push(format!("Unrecognized mastering luminance: {mastering}"));
    }

    Some(HybridMediaInfo {
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
        max_cll,
        max_fall,
        static_metadata_errors,
        mastering_min_nits,
        mastering_max_nits,
        // These fields were appended to preserve the established parser
        // indices for callers and old fixture lines. MediaInfo emits an
        // empty value when a field is unavailable; donor eligibility then
        // fails closed on the missing evidence.
        colour_range: get(18),
        matrix_coefficients: get(19),
        hdr_format_profile: get(20),
        hdr_format_compatibility: get(21),
    })
}

pub(crate) fn hybrid_get_media_info(
    file: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<HybridMediaInfo> {
    // The trailing \n (expanded by mediainfo) puts each video track on its
    // own line; without it multiple tracks concatenate into one unparseable
    // line and every field would describe whichever track comes first.
    let template = "--Output=Video;%Format%|%CodecID%|%FrameCount%|%FrameRate%|%FrameRate_Num%|%FrameRate_Den%|%Duration%|%HDR_Format%|%Width%|%Height%|%BitDepth%|%colour_primaries%|%transfer_characteristics%|%FrameRate_Mode%|%ScanType%|%MaxCLL%|%MaxFALL%|%MasteringDisplay_Luminance%|%colour_range%|%matrix_coefficients%|%HDR_Format_Profile%|%HDR_Format_Compatibility%\\n";

    let args = vec![OsString::from(template), file.as_os_str().to_os_string()];
    let out = run_capture(logger, &rt.mediainfo, &args)?;
    parse_hybrid_media_info(&out)
        .ok_or_else(|| format!("No mediainfo video output for {}", file.display()))
}

pub(crate) fn hybrid_detect_dv_profile(
    file: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<Option<u8>> {
    let out = run_capture(
        logger,
        &rt.mediainfo,
        &[
            "--Output=Video;%HDR_Format%|%HDR_Format_Profile%|%CodecID%".into(),
            file.as_os_str().to_os_string(),
        ],
    )?;
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

pub(crate) fn hybrid_get_hdr_compatibility(
    file: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<String> {
    let args = vec![
        OsString::from("--Output=Video;%HDR_Format_Compatibility%\\n"),
        file.as_os_str().to_os_string(),
    ];
    let out = run_capture(logger, &rt.mediainfo, &args)?;
    Ok(out
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string())
}

pub(crate) fn is_hdr10_compatible(hdr_format: &str, compatibility: &str) -> bool {
    let combined = format!("{hdr_format} {compatibility}").to_ascii_lowercase();
    combined.contains("hdr10") || combined.contains("smpte st 2084")
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
    fn mastering_luminance_uses_labels_not_unit_digits_or_extrema() {
        assert_eq!(
            parse_mastering_luminance("min: 5 cd/m2, max: 1000 cd/m2"),
            (Some(5.0), Some(1000.0))
        );
        assert_eq!(
            parse_mastering_luminance("max: 0 cd/m2, min: 1 cd/m2"),
            (Some(1.0), Some(0.0))
        );
        assert_eq!(
            parse_mastering_luminance("min: -0.005 cd/m2, max: 1000 cd/m2"),
            (Some(-0.005), Some(1000.0))
        );
        assert_eq!(
            parse_mastering_luminance("min: 0 cd/m2, max: 0 cd/m2"),
            (Some(0.0), Some(0.0))
        );
    }

    #[test]
    fn malformed_light_levels_remain_distinct_from_missing() {
        let mut errors = Vec::new();
        assert_eq!(parse_light_level("", "MaxCLL", &mut errors), None);
        assert!(errors.is_empty());
        for raw in ["65536", "-1000", "10.5", "invalid"] {
            assert_eq!(parse_light_level(raw, "MaxCLL", &mut errors), None);
        }
        assert_eq!(errors.len(), 4);
        assert_eq!(
            parse_light_level("1 000 cd/m2", "MaxCLL", &mut errors),
            Some(1000)
        );
    }

    #[test]
    fn mastering_luminance_missing() {
        assert_eq!(parse_mastering_luminance(""), (None, None));
        assert_eq!(parse_mastering_luminance("max: 1000"), (None, Some(1000.0)));
    }

    #[test]
    fn hdr10_compatibility_can_be_reported_in_a_separate_field() {
        assert!(is_hdr10_compatible(
            "Dolby Vision / SMPTE ST 2094 App 4",
            "HDR10 / HDR10+ Profile B"
        ));
        assert!(!is_hdr10_compatible("Dolby Vision", ""));
    }

    #[test]
    fn parse_int_strips_units() {
        assert_eq!(parse_int("129 597"), Some(129_597));
        assert_eq!(parse_int(""), None);
    }

    const HEVC_LINE: &str = "HEVC|V_MPEGH/ISO/HEVC|129597|23.976|24000|1001|5405400.000|Dolby Vision|3840|2160|10|BT.2020|PQ|CFR|Progressive|1016|258|min: 0.0050 cd/m2, max: 1000 cd/m2|Limited|BT.2020 non-constant|Profile 8.1|HDR10";

    #[test]
    fn hybrid_media_info_single_track() {
        let info = parse_hybrid_media_info(HEVC_LINE).unwrap();
        assert_eq!(info.codec, "HEVC");
        assert_eq!(info.frame_count, 129_597);
        assert_eq!(info.width, Some(3840));
        assert_eq!(info.max_cll, Some(1016));
        assert_eq!(info.mastering_max_nits, Some(1000.0));
        assert_eq!(info.colour_range, "Limited");
        assert_eq!(info.matrix_coefficients, "BT.2020 non-constant");
        assert_eq!(info.hdr_format_profile, "Profile 8.1");
        assert_eq!(info.hdr_format_compatibility, "HDR10");
        assert!(parse_hybrid_media_info("\n  \n").is_none());
    }

    #[test]
    fn hybrid_media_info_prefers_hevc_track() {
        // A cover-art/bonus track before the movie track must not win: the
        // extraction side (get_hevc_track_id) picks the HEVC track.
        let out = format!("V_MJPEG|V_MJPEG|1||||40.000||320|180|8|||VFR||||\n{HEVC_LINE}\n");
        let info = parse_hybrid_media_info(&out).unwrap();
        assert_eq!(info.codec, "HEVC");
        assert_eq!(info.height, Some(2160));

        // No HEVC track at all: fall back to the first video track.
        let out = "AV1|V_AV1|500|24.000|||||1920|1080|10|BT.2020|PQ|CFR|||||";
        let info = parse_hybrid_media_info(out).unwrap();
        assert_eq!(info.codec, "AV1");
    }

    fn eligible_info(profile: &str) -> HybridMediaInfo {
        HybridMediaInfo {
            hdr_format: "Dolby Vision".into(),
            hdr_format_profile: profile.into(),
            hdr_format_compatibility: "HDR10".into(),
            bit_depth: Some(10),
            colour_primaries: "BT.2020".into(),
            transfer_characteristics: "PQ".into(),
            colour_range: "Limited".into(),
            matrix_coefficients: "BT.2020 non-constant".into(),
            ..Default::default()
        }
    }

    #[test]
    fn hybrid_donor_eligibility_accepts_p7_and_p81() {
        assert_eq!(
            hybrid_donor_eligibility(&eligible_info("Profile 7"), Some(7)),
            Ok(SupportedHybridDonor::Profile7)
        );
        assert_eq!(
            hybrid_donor_eligibility(&eligible_info("Profile 8.1"), Some(8)),
            Ok(SupportedHybridDonor::Profile8)
        );
        assert_eq!(
            hybrid_donor_eligibility(&eligible_info("Profile 8.1, dvhe.08.06, BL+RPU"), Some(8)),
            Ok(SupportedHybridDonor::Profile8)
        );
    }

    #[test]
    fn hybrid_donor_eligibility_accepts_dvhe_level_four() {
        // dvhe.08.04 is Profile 8 with level 4. The final component is a
        // level, not Profile 8.4, so it must remain eligible with HDR10/PQ.
        assert_eq!(
            hybrid_donor_eligibility(&eligible_info("dvhe.08.04"), Some(8)),
            Ok(SupportedHybridDonor::Profile8)
        );
    }

    #[test]
    fn hybrid_donor_eligibility_rejects_missing_and_contradictory_metadata() {
        let mut missing = eligible_info("Profile 8.1");
        missing.hdr_format_compatibility.clear();
        assert!(hybrid_donor_eligibility(&missing, Some(8)).is_err());

        for (field, value) in [
            ("range", "Full"),
            ("matrix", "BT.2020 constant"),
            ("primaries", "BT.2020 / BT.709"),
            ("transfer", "PQ / HLG"),
            ("compatibility", "HDR10 / HLG"),
        ] {
            let mut info = eligible_info("Profile 8.1");
            match field {
                "range" => info.colour_range = value.into(),
                "matrix" => info.matrix_coefficients = value.into(),
                "primaries" => info.colour_primaries = value.into(),
                "transfer" => info.transfer_characteristics = value.into(),
                "compatibility" => info.hdr_format_compatibility = value.into(),
                _ => unreachable!(),
            }
            assert!(
                hybrid_donor_eligibility(&info, Some(8)).is_err(),
                "contradictory {field} metadata unexpectedly passed"
            );
        }
    }

    #[test]
    fn hybrid_donor_eligibility_rejects_hlg_and_explicit_p84() {
        let mut hlg = eligible_info("Profile 8.4");
        hlg.hdr_format_compatibility = "HLG".into();
        hlg.transfer_characteristics = "HLG".into();
        assert!(hybrid_donor_eligibility(&hlg, Some(8)).is_err());

        let p84 = eligible_info("Profile 8.4");
        assert!(hybrid_donor_eligibility(&p84, Some(8)).is_err());

        for profile in ["Profile 8.2", "Profile 8.9", "Profile 80", "8.2"] {
            let unsupported = eligible_info(profile);
            assert!(
                hybrid_donor_eligibility(&unsupported, Some(8)).is_err(),
                "unsupported explicit profile {profile} unexpectedly passed"
            );
        }

        let mut contradictory = eligible_info("Profile 7");
        contradictory.hdr_format = "Dolby Vision, Profile 8".into();
        assert!(hybrid_donor_eligibility(&contradictory, Some(8)).is_err());

        let mut unsupported = eligible_info("");
        unsupported.hdr_format = "Dolby Vision, Profile 5".into();
        assert!(hybrid_donor_eligibility(&unsupported, Some(8)).is_err());
    }

    #[test]
    fn malformed_bare_profile_cannot_hide_behind_a_valid_format() {
        for profile in ["7.x", "7.8.9", "7."] {
            let mut info = eligible_info(profile);
            info.hdr_format = "Dolby Vision, Profile 7".into();
            assert!(hybrid_donor_eligibility(&info, Some(7)).is_err());
        }
    }
}
