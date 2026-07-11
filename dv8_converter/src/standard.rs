use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::exec::{run_capture, run_status, AppResult, CleanupGuard};
use crate::fsutil::{check_disk_space, collect_mkv_files, move_to_dir};
use crate::logger::Logger;
use crate::mediainfo::get_hevc_track_id;
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

    let args = vec![file.as_os_str().to_os_string()];
    let info = run_capture(logger, &rt.mediainfo, &args)?;
    let lower = info.to_lowercase();

    let has_profile7 = (lower.contains("dolby")
        && lower.contains("vision")
        && (lower.contains("profile 7") || lower.contains("profile: 7")))
        || lower.contains("dvhe.07");

    if has_profile7 {
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

    let bl_el_rpu_hevc = input_dir.join(format!("{mkv_base}.BL_EL_RPU.hevc"));
    let dv7_el_rpu_hevc = input_dir.join(format!("{mkv_base}.DV7.EL_RPU.hevc"));
    let dv8_bl_rpu_hevc = input_dir.join(format!("{mkv_base}.DV8.BL_RPU.hevc"));
    let dv8_rpu_bin = input_dir.join(format!("{mkv_base}.DV8.RPU.bin"));

    let mut cleanup = CleanupGuard::new(logger.clone());
    cleanup.add(&bl_el_rpu_hevc);
    cleanup.add(&dv7_el_rpu_hevc);
    cleanup.add(&dv8_bl_rpu_hevc);
    cleanup.add(&dv8_rpu_bin);

    // let out_base = make_dv8_name(&mkv_base);
    let out_base = mkv_base.clone();
    let final_file = input_dir.join(format!("{out_base}.mkv"));
    let out_file = input_dir.join(format!("{out_base}.DV8_TMP.mkv"));

    if out_file.exists() {
        logger.warn(&format!(
            "Output file already exists, skipping: {}",
            out_file.display()
        ));
        logger.log(&format!(
            "{} skipped: output already exists: {}",
            file.display(),
            out_file.display()
        ));
        cleanup.clear();
        return Ok(());
    }

    check_disk_space(file, logger)?;

    if rt.dry_run {
        logger.ok(&format!("[DRY RUN] Would convert: {}", file.display()));
        logger.ok(&format!("[DRY RUN] Output: {}", out_file.display()));
        if rt.save_el_rpu {
            logger.ok(&format!(
                "[DRY RUN] Archive EL+RPU to: {}",
                rt.output_dir.display()
            ));
        }
        cleanup.clear();
        return Ok(());
    }

    let track_id = get_hevc_track_id(file, rt, logger)?;
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

    logger.step("4 | Extract RPU (optional)");
    let _ = run_status(
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
    );

    logger.step("5 | Prepare output name");
    logger.dbg(&format!("Output temp name: {}", out_file.display()));
    logger.dbg(&format!("Final output name: {}", final_file.display()));

    logger.step("6 | Remux final MKV");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvmerge,
        &[
            OsString::from("-o"),
            out_file.as_os_str().to_os_string(),
            OsString::from("-D"),
            file.as_os_str().to_os_string(),
            dv8_bl_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("--track-order"),
            OsString::from("1:0"),
        ],
    )?;

    let orig_size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let out_size = fs::metadata(&out_file).map(|m| m.len()).unwrap_or(0);

    if out_size == 0 {
        let _ = fs::remove_file(&out_file);
        return Err(format!(
            "Output file is empty - keeping original: {}",
            file.display()
        ));
    }

    if orig_size > 0 && (out_size * 100 / orig_size) < 50 {
        return Err(format!(
            "Output is much smaller than original ({} MB vs {} MB) - keeping original",
            out_size / 1_048_576,
            orig_size / 1_048_576
        ));
    }

    let _ = fs::remove_file(&bl_el_rpu_hevc);
    let _ = fs::remove_file(&dv8_bl_rpu_hevc);
    let _ = fs::remove_file(&dv8_rpu_bin);
    cleanup.clear();

    fs::remove_file(file)
        .map_err(|e| format!("Failed to delete original {}: {e}", file.display()))?;
    fs::rename(&out_file, &final_file).map_err(|e| {
        format!(
            "Failed to rename output {} to {}: {e}",
            out_file.display(),
            final_file.display()
        )
    })?;

    logger.log(&format!(
        "{} processed successfully -> {}",
        file.display(),
        final_file.display()
    ));
    logger.ok(&format!("Done: {}", final_file.display()));

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
        logger.warn(&format!("{failures} file(s) failed."));
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
