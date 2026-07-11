use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::exec::{run_capture, run_status, AppResult};
use crate::logger::Logger;
use crate::mediainfo::{hybrid_detect_dv_profile, hybrid_get_media_info, parse_int};
use crate::runtime::Runtime;

use super::scenes::{
    correlate_scene_cuts, export_dv_scene_cuts, parse_scene_list, CorrelationReport,
};

pub(crate) fn hybrid_get_rpu_frame_count(
    rpu_file: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<u64> {
    let out = run_capture(
        logger,
        &rt.dovi_tool,
        &[
            OsString::from("info"),
            OsString::from("-i"),
            rpu_file.as_os_str().to_os_string(),
            OsString::from("--summary"),
        ],
    )?;

    for line in out.lines() {
        if let Some(idx) = line.find("Frames:") {
            let value = line[idx + "Frames:".len()..].trim();
            if let Some(n) = parse_int(value) {
                return Ok(n);
            }
        }
    }

    Err(format!(
        "Could not parse frame count from dovi_tool info for {}",
        rpu_file.display()
    ))
}

pub(crate) fn hybrid_validate_output(
    out_file: &Path,
    hdr_target: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    let out_meta = fs::metadata(out_file).map_err(|e| {
        format!(
            "Validation failed: output missing {}: {e}",
            out_file.display()
        )
    })?;
    if out_meta.len() == 0 {
        return Err(format!(
            "Validation failed: output is empty: {}",
            out_file.display()
        ));
    }

    let hdr_size = fs::metadata(hdr_target).map(|m| m.len()).unwrap_or(0);
    if hdr_size > 0 && out_meta.len() * 100 / hdr_size < 80 {
        return Err(format!(
            "Validation failed: output too small ({} MB vs {} MB)",
            out_meta.len() / 1_048_576,
            hdr_size / 1_048_576
        ));
    }

    let out_info = hybrid_get_media_info(out_file, rt, logger)?;
    let hdr_info = hybrid_get_media_info(hdr_target, rt, logger)?;

    let codec = format!("{} {}", out_info.codec, out_info.codec_id).to_lowercase();
    if !(codec.contains("hevc")
        || codec.contains("h.265")
        || codec.contains("h265")
        || codec.contains("hev1")
        || codec.contains("dvhe"))
    {
        return Err("Validation failed: output codec is not HEVC".to_string());
    }

    if out_info.frame_count > 0
        && hdr_info.frame_count > 0
        && out_info.frame_count != hdr_info.frame_count
    {
        return Err(format!(
            "Validation failed: output frame count {} does not match HDR target {}",
            out_info.frame_count, hdr_info.frame_count
        ));
    } else if out_info.frame_count == 0 || hdr_info.frame_count == 0 {
        logger.warn(
            "mediainfo frame count unavailable - relying on the RPU frame count verification",
        );
    }

    let profile = hybrid_detect_dv_profile(out_file, rt, logger)?;
    if profile != Some(8) {
        return Err(format!(
            "Validation failed: expected DV profile 8 in output, detected {:?}",
            profile
        ));
    }

    let full_out = run_capture(
        logger,
        &rt.mediainfo,
        &[out_file.as_os_str().to_os_string()],
    )?;
    let lower = full_out.to_lowercase();
    let dv_seen = lower.contains("dolby") && lower.contains("vision") || lower.contains("dvhe.");
    if !dv_seen {
        logger.warn(
            "Validation warning: mediainfo did not detect Dolby Vision metadata (can be false negative)",
        );
    }

    Ok(())
}

pub(crate) enum SyncVerdict {
    /// Offset 0 confirmed across the whole runtime.
    Verified(String),
    /// Correlation could not establish a confident offset (e.g. periodic
    /// content, too few cuts). Not proof of a problem - warn only.
    Unverifiable(String),
    /// A confident non-zero offset: the injected metadata is out of sync.
    Mismatch(String),
}

/// Pure verdict from correlating the OUTPUT's RPU cuts against the HDR
/// target's detected cuts. Correlation acceptance already requires every
/// measurable tercile (start/middle/end) to agree with the global offset,
/// so a verified offset 0 covers the whole runtime.
pub(crate) fn judge_output_sync(report: &CorrelationReport) -> SyncVerdict {
    match &report.accepted {
        Some(sync) if sync.offset == 0 => SyncVerdict::Verified(format!(
            "Post-inject sync verified: offset 0 in all measurable terciles ({} matches, {:.0}% of RPU cuts, dominance {:.1})",
            sync.matches,
            sync.match_ratio * 100.0,
            sync.dominance
        )),
        Some(sync) => SyncVerdict::Mismatch(format!(
            "Post-inject sync FAILED: output RPU is offset {:+} frames from the video ({} matches, dominance {:.1})",
            sync.offset, sync.matches, sync.dominance
        )),
        None => SyncVerdict::Unverifiable(format!(
            "Post-inject sync could not be verified: {} (RPU cuts: {}, HDR cuts: {}, top offsets: {:?})",
            report
                .rejection
                .as_deref()
                .unwrap_or("unknown"),
            report.dv_count,
            report.hdr_count,
            report.top_offsets
        )),
    }
}

/// Post-inject verification (the tutorial's start/middle/end spot-check,
/// automated): re-extract the RPU from the finished output, require its
/// frame count to equal the HDR target's, then correlate its scene cuts
/// against the HDR cut list persisted during alignment.
///
/// `max_offset` is the same search window used for alignment - wide enough
/// that a genuinely misaligned RPU is FOUND at its nonzero offset (hard
/// fail) instead of falling outside the window and reading as merely
/// unverifiable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn hybrid_verify_output_sync(
    out_file: &Path,
    hdr_frames: u64,
    hdr_scenes_txt: &Path,
    verify_rpu: &Path,
    verify_scenes_txt: &Path,
    max_offset: i64,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("extract-rpu"),
            OsString::from("-i"),
            out_file.as_os_str().to_os_string(),
            OsString::from("-o"),
            verify_rpu.as_os_str().to_os_string(),
        ],
    )?;

    let rpu_frames = hybrid_get_rpu_frame_count(verify_rpu, rt, logger)?;
    if hdr_frames > 0 {
        if rpu_frames != hdr_frames {
            return Err(format!(
                "Output RPU frame count {rpu_frames} does not match HDR target {hdr_frames}"
            ));
        }
        logger.ok(&format!(
            "Output RPU frame count matches HDR target: {rpu_frames}"
        ));
    } else {
        logger.warn("HDR target frame count unknown - RPU frame count check skipped");
    }

    let hdr_cuts = match fs::read_to_string(hdr_scenes_txt) {
        Ok(content) => parse_scene_list(&content),
        Err(_) => Vec::new(),
    };
    if hdr_cuts.is_empty() {
        logger.warn(
            "No HDR scene-cut list available (--sync framecount?) - scene verification skipped",
        );
        return Ok(());
    }

    let out_cuts = export_dv_scene_cuts(verify_rpu, verify_scenes_txt, rt, logger)?;
    let report = correlate_scene_cuts(&out_cuts, &hdr_cuts, max_offset, 1);

    match judge_output_sync(&report) {
        SyncVerdict::Verified(msg) => {
            logger.ok(&msg);
            Ok(())
        }
        SyncVerdict::Unverifiable(msg) => {
            logger.warn(&msg);
            logger.warn("Spot-check playback at the start, middle and end manually");
            Ok(())
        }
        SyncVerdict::Mismatch(msg) => Err(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::scenes::SyncResult;

    fn report_with(accepted: Option<SyncResult>, rejection: Option<&str>) -> CorrelationReport {
        CorrelationReport {
            accepted,
            rejection: rejection.map(|s| s.to_string()),
            top_offsets: vec![(0, 11)],
            dv_count: 12,
            hdr_count: 12,
        }
    }

    fn sync(offset: i64) -> SyncResult {
        SyncResult {
            offset,
            matches: 11,
            match_ratio: 1.0,
            dominance: f64::INFINITY,
            tercile_offsets: [Some(offset), Some(offset), Some(offset)],
        }
    }

    #[test]
    fn offset_zero_verifies() {
        match judge_output_sync(&report_with(Some(sync(0)), None)) {
            SyncVerdict::Verified(msg) => assert!(msg.contains("offset 0")),
            _ => panic!("expected Verified"),
        }
    }

    #[test]
    fn nonzero_offset_is_mismatch() {
        match judge_output_sync(&report_with(Some(sync(2)), None)) {
            SyncVerdict::Mismatch(msg) => assert!(msg.contains("+2")),
            _ => panic!("expected Mismatch"),
        }
    }

    #[test]
    fn rejection_is_unverifiable() {
        match judge_output_sync(&report_with(None, Some("ambiguous: dominance 1.1"))) {
            SyncVerdict::Unverifiable(msg) => assert!(msg.contains("dominance 1.1")),
            _ => panic!("expected Unverifiable"),
        }
    }
}
