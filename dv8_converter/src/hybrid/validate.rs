use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::exec::{run_capture, AppResult};
use crate::logger::Logger;
use crate::mediainfo::{hybrid_detect_dv_profile, hybrid_get_media_info, parse_int};
use crate::runtime::Runtime;

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
