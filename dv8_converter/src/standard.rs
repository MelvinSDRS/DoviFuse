use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::exec::{run_status, AppResult, CleanupGuard};
use crate::fsutil::{check_disk_space_hybrid, collect_mkv_files, create_job_dir, move_to_dir};
use crate::hybrid::validate::hybrid_get_rpu_frame_count;
use crate::logger::Logger;
use crate::mediainfo::{
    get_hevc_track_id, has_hdr10_base, hybrid_detect_dv_profile, hybrid_get_media_info,
};
use crate::runtime::Runtime;

pub(crate) fn is_dv7_file(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<bool> {
    let base = file
        .file_name()
        .map(|b| b.to_string_lossy().to_string())
        .unwrap_or_default();

    if base.starts_with("._") {
        logger.log(&format!("{} skipped: hidden", file.display()));
        return Ok(false);
    }

    let has_profile7 = hybrid_detect_dv_profile(file, rt, logger)? == Some(7);

    if has_profile7 {
        get_hevc_track_id(file, rt, logger)?;
        logger.log(&format!("{} DV7 detected", file.display()));
        Ok(true)
    } else {
        logger.log(&format!("{} not DV7", file.display()));
        Ok(false)
    }
}

pub(crate) fn replace_case_insensitive_all(
    input: &str,
    pattern: &str,
    replacement: &str,
) -> (String, bool) {
    let input_lower = input.to_lowercase();
    let pattern_lower = pattern.to_lowercase();

    let mut out = String::new();
    let mut start = 0usize;
    let mut changed = false;

    while let Some(rel_pos) = input_lower[start..].find(&pattern_lower) {
        let pos = start + rel_pos;
        out.push_str(&input[start..pos]);
        out.push_str(replacement);
        start = pos + pattern.len();
        changed = true;
    }

    out.push_str(&input[start..]);
    (out, changed)
}

#[allow(dead_code)]
pub(crate) fn make_dv8_name(name: &str) -> String {
    let mut result = name.to_string();
    let mut changed_any = false;

    for pat in [
        "Dolby.Vision",
        "Dolby-Vision",
        "Dolby_Vision",
        "DolbyVision",
        "DoVi",
        "DOVI",
        "Dovi",
        "DV7",
        "dv7",
    ] {
        let (updated, changed) = replace_case_insensitive_all(&result, pat, "DV8");
        result = updated;
        changed_any |= changed;
    }

    if !changed_any {
        result = format!("{name}.DV8");
    }

    while result.contains("..") {
        result = result.replace("..", ".");
    }

    result
}

pub(crate) fn process_file(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<()> {
    let input_dir = file
        .parent()
        .ok_or_else(|| format!("Invalid path: {}", file.display()))?;
    let mkv_base = file
        .file_stem()
        .ok_or_else(|| format!("Invalid filename: {}", file.display()))?
        .to_string_lossy()
        .to_string();

    let scratch_root = rt.tmp_dir.as_deref().unwrap_or(input_dir);
    let scratch_owned = if !rt.dry_run {
        create_job_dir(scratch_root, "dv8-standard")?
    } else {
        scratch_root.to_path_buf()
    };
    let scratch = scratch_owned.as_path();
    let bl_el_rpu_hevc = scratch.join(format!("{mkv_base}.BL_EL_RPU.hevc"));
    let dv7_el_rpu_hevc = scratch.join(format!("{mkv_base}.DV7.EL_RPU.hevc"));
    let dv8_bl_rpu_hevc = scratch.join(format!("{mkv_base}.DV8.BL_RPU.hevc"));
    let dv8_rpu_bin = scratch.join(format!("{mkv_base}.DV8.RPU.bin"));

    let mut cleanup = CleanupGuard::new(logger.clone());
    if !rt.dry_run {
        cleanup.add_dir(scratch);
    }

    // let out_base = make_dv8_name(&mkv_base);
    let out_base = mkv_base.clone();
    // Preserve the exact source name, including an uppercase .MKV extension.
    let final_file = file.to_path_buf();
    let out_file = input_dir.join(format!("{out_base}.DV8_TMP.mkv"));

    if out_file.exists() {
        return Err(format!(
            "Temporary output already exists: {}",
            out_file.display()
        ));
    }

    check_disk_space_hybrid(file, scratch_root, input_dir, logger)?;

    if rt.dry_run {
        logger.ok(&format!("[DRY RUN] Would convert: {}", file.display()));
        logger.ok(&format!("[DRY RUN] Output: {}", out_file.display()));
        if rt.save_el_rpu {
            logger.ok(&format!(
                "[DRY RUN] Archive EL+RPU to: {}",
                rt.output_dir.display()
            ));
        }
        logger.completed(&final_file);
        cleanup.clear();
        return Ok(());
    }

    rt.require_ffmpeg()?;
    crate::fsutil::reserve_output(&out_file)?;
    cleanup.add(&out_file);
    let track_id = get_hevc_track_id(file, rt, logger)?;
    let remux_source = crate::remux::RemuxSource::read(file, rt, logger)?;
    let source_info = hybrid_get_media_info(file, rt, logger)?;
    if source_info.frame_count == 0 || !has_hdr10_base(&source_info) {
        return Err(
            "Source must have a known frame count and a 10-bit BT.2020 PQ base layer".to_string(),
        );
    }
    let timestamps = scratch.join("video.timestamps.txt");
    run_status(
        logger,
        false,
        true,
        &rt.mkvextract,
        &[
            file.as_os_str().to_os_string(),
            "timestamps_v2".into(),
            format!("{track_id}:{}", timestamps.display()).into(),
        ],
    )?;
    logger.dbg(&format!("Using video track ID: {track_id}"));

    logger.step("1 | Extract BL+EL+RPU");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvextract,
        &[
            OsString::from("tracks"),
            file.as_os_str().to_os_string(),
            OsString::from(format!("{}:{}", track_id, bl_el_rpu_hevc.display())),
        ],
    )?;

    crate::enhancement::inspect(
        &bl_el_rpu_hevc,
        &scratch.join("source-rpu.bin"),
        source_info.frame_count,
        rt,
        logger,
    )?;

    logger.step("2 | Demux EL+RPU");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("demux"),
            OsString::from("--el-only"),
            bl_el_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("-e"),
            dv7_el_rpu_hevc.as_os_str().to_os_string(),
        ],
    )?;

    if rt.save_el_rpu {
        move_to_dir(&dv7_el_rpu_hevc, &rt.output_dir)?;
    } else {
        let _ = fs::remove_file(&dv7_el_rpu_hevc);
    }

    logger.step("3 | Convert to DV8");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("--edit-config"),
            rt.json_file.as_os_str().to_os_string(),
            OsString::from("convert"),
            OsString::from("--discard"),
            bl_el_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("-o"),
            dv8_bl_rpu_hevc.as_os_str().to_os_string(),
        ],
    )?;

    logger.step("4 | Validate converted RPU");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("extract-rpu"),
            dv8_bl_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("-o"),
            dv8_rpu_bin.as_os_str().to_os_string(),
        ],
    )?;
    let rpu_frames = hybrid_get_rpu_frame_count(&dv8_rpu_bin, rt, logger)?;
    if rpu_frames != source_info.frame_count {
        return Err(
            "Converted RPU count differs from original video; keeping original".to_string(),
        );
    }

    logger.step("5 | Prepare output name");
    logger.dbg(&format!("Output temp name: {}", out_file.display()));
    logger.dbg(&format!("Final output name: {}", final_file.display()));

    logger.step("6 | Remux final MKV");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvmerge,
        &remux_source.arguments(file, &dv8_bl_rpu_hevc, &timestamps, &out_file)?,
    )?;

    let out_size = fs::metadata(&out_file).map(|m| m.len()).unwrap_or(0);

    if out_size == 0 {
        let _ = fs::remove_file(&out_file);
        return Err(format!(
            "Output file is empty - keeping original: {}",
            file.display()
        ));
    }

    remux_source.verify(&out_file, rt, logger)?;

    let output_info = hybrid_get_media_info(&out_file, rt, logger)?;
    if hybrid_detect_dv_profile(&out_file, rt, logger)? != Some(8)
        || !has_hdr10_base(&output_info)
        || output_info.frame_count != source_info.frame_count
    {
        return Err(
            "Output profile, HDR10 base or frame count validation failed; keeping original"
                .to_string(),
        );
    }
    let decoded = crate::ffmpeg::scan_video(rt, logger, &out_file, 8.0)?;
    if decoded.frames != source_info.frame_count {
        return Err(
            "Decoded output frame count differs from original; keeping original".to_string(),
        );
    }

    logger.check_result("standard_output", "Converted output", "pass",
        "Profile 8, HDR10 base, RPU/video frame counts, full video decode and supported container headers verified before replacing the source.");

    let _ = fs::remove_file(&bl_el_rpu_hevc);
    let _ = fs::remove_file(&dv8_bl_rpu_hevc);
    let _ = fs::remove_file(&dv8_rpu_bin);

    // Both paths are in the input directory. On Unix, rename atomically
    // replaces the original; if it fails, CleanupGuard removes only the
    // temporary output and the original remains untouched.
    fs::rename(&out_file, &final_file).map_err(|e| {
        format!(
            "Failed to rename output {} to {}: {e}",
            out_file.display(),
            final_file.display()
        )
    })?;
    if !rt.dry_run {
        let _ = fs::remove_dir_all(scratch);
    }
    cleanup.clear();

    logger.log(&format!(
        "{} processed successfully -> {}",
        file.display(),
        final_file.display()
    ));
    logger.ok(&format!("Done: {}", final_file.display()));
    logger.completed(&final_file);

    Ok(())
}

pub(crate) fn process_directory(dir: &Path, rt: &Runtime, logger: &Logger) -> AppResult<()> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_mkv_files(dir, &mut files)?;
    files.sort();

    logger.step(&format!("Scan folder {}", dir.display()));

    let mut count = 0usize;
    let mut failures = 0usize;

    for f in files {
        if is_dv7_file(&f, rt, logger)? {
            match process_file(&f, rt, logger) {
                Ok(_) => count += 1,
                Err(e) => {
                    failures += 1;
                    logger.err(&format!("Failed: {} ({e})", f.display()));
                }
            }
        }
    }

    logger.ok(&format!("{count} file(s) converted."));
    if failures > 0 {
        return Err(format!("{failures} file(s) failed; {count} converted"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dv8_name_replaces_patterns() {
        assert_eq!(
            make_dv8_name("Movie.2020.DV.DoVi.Remux"),
            "Movie.2020.DV.DV8.Remux"
        );
        assert_eq!(make_dv8_name("Movie.2020.Remux"), "Movie.2020.Remux.DV8");
    }
}
