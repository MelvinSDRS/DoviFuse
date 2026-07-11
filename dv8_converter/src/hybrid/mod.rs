pub(crate) mod align;
pub(crate) mod editor;
pub(crate) mod preflight;
pub(crate) mod validate;

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::cli::HybridOptions;
use crate::exec::{run_status, AppResult, CleanupGuard};
use crate::fsutil::check_disk_space_hybrid;
use crate::logger::Logger;
use crate::mediainfo::{
    fps_from_info, get_hevc_track_id, hybrid_detect_dv_profile, hybrid_get_media_info,
};
use crate::runtime::Runtime;

use align::compute_alignment_framecount;
use editor::hybrid_build_editor_json;
use preflight::hybrid_preflight_checks;
use validate::{hybrid_get_rpu_frame_count, hybrid_validate_output};

pub(crate) fn process_hybrid(
    dv_source: &Path,
    hdr_target: &Path,
    custom_output: Option<&Path>,
    opts: &HybridOptions,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    logger.step("Hybrid mode");

    if !dv_source.exists() {
        return Err(format!("DV source file not found: {}", dv_source.display()));
    }
    if !hdr_target.exists() {
        return Err(format!(
            "HDR target file not found: {}",
            hdr_target.display()
        ));
    }

    if fs::canonicalize(dv_source).ok() == fs::canonicalize(hdr_target).ok() {
        return Err("DV source and HDR target must be different files".to_string());
    }

    let output_path = if let Some(custom) = custom_output {
        custom.to_path_buf()
    } else {
        let dir = hdr_target
            .parent()
            .ok_or_else(|| format!("Invalid HDR target path: {}", hdr_target.display()))?;
        let stem = hdr_target
            .file_stem()
            .ok_or_else(|| format!("Invalid HDR target filename: {}", hdr_target.display()))?
            .to_string_lossy()
            .to_string();
        dir.join(format!("{stem}.DV8.Hybrid.mkv"))
    };

    if output_path.exists() {
        logger.warn(&format!(
            "Output file already exists, skipping: {}",
            output_path.display()
        ));
        return Ok(());
    }

    let tmp_dir = hdr_target
        .parent()
        .ok_or_else(|| format!("Invalid HDR target path: {}", hdr_target.display()))?;
    let base = hdr_target
        .file_stem()
        .ok_or_else(|| format!("Invalid HDR target filename: {}", hdr_target.display()))?
        .to_string_lossy()
        .to_string();

    let hybrid_rpu = tmp_dir.join(format!("{base}.hybrid.rpu.bin"));
    let hybrid_aligned_rpu = tmp_dir.join(format!("{base}.hybrid.aligned.rpu.bin"));
    let hybrid_hevc = tmp_dir.join(format!("{base}.hybrid.hevc"));
    let hybrid_injected_hevc = tmp_dir.join(format!("{base}.hybrid.injected.hevc"));
    let hybrid_editor_json = tmp_dir.join(format!("{base}.hybrid.editor.json"));

    let mut cleanup = CleanupGuard::new(logger.clone());
    cleanup.add(&hybrid_rpu);
    cleanup.add(&hybrid_aligned_rpu);
    cleanup.add(&hybrid_hevc);
    cleanup.add(&hybrid_injected_hevc);
    cleanup.add(&hybrid_editor_json);

    logger.step("0 | Determine output path");
    logger.ok(&format!("Hybrid output: {}", output_path.display()));

    logger.step("1 | Gather media info");
    let dv_info = hybrid_get_media_info(dv_source, rt, logger)?;
    let hdr_info = hybrid_get_media_info(hdr_target, rt, logger)?;

    logger.step("2 | Detect DV profile");
    let dv_profile = hybrid_detect_dv_profile(dv_source, rt, logger)?;
    logger.ok(&format!(
        "Detected DV profile: {}",
        dv_profile
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ));

    logger.step("3 | Preflight checks");
    let has_fail = hybrid_preflight_checks(&dv_info, &hdr_info, dv_profile, logger);
    if has_fail {
        return Err("Hybrid preflight failed".to_string());
    }

    logger.step("4 | Disk space check");
    let out_dir = output_path
        .parent()
        .ok_or_else(|| format!("Invalid output path: {}", output_path.display()))?;
    check_disk_space_hybrid(hdr_target, out_dir, logger)?;

    if rt.dry_run {
        logger.ok("[DRY RUN] Preflight completed. Mutating steps were skipped.");
        logger.ok(&format!(
            "[DRY RUN] Would extract RPU from {}",
            dv_source.display()
        ));
        logger.ok("[DRY RUN] Would compute/apply alignment editor config");
        logger.ok(&format!(
            "[DRY RUN] Would extract HEVC track from {}",
            hdr_target.display()
        ));
        logger.ok("[DRY RUN] Would inject RPU and remux final MKV");
        if opts.delete_sources {
            logger.ok("[DRY RUN] Would validate output and delete both originals on success (--delete-sources)");
        } else {
            logger.ok("[DRY RUN] Would validate output and keep both originals");
        }
        cleanup.clear();
        return Ok(());
    }

    logger.step("5 | Extract RPU from DV source");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("extract-rpu"),
            OsString::from("-i"),
            dv_source.as_os_str().to_os_string(),
            OsString::from("-o"),
            hybrid_rpu.as_os_str().to_os_string(),
        ],
    )?;

    let dv_rpu_frames = hybrid_get_rpu_frame_count(&hybrid_rpu, rt, logger)?;
    if dv_rpu_frames == 0 {
        return Err("RPU extraction yielded 0 frames".to_string());
    }

    logger.step("6 | Compute alignment strategy");
    let fps = fps_from_info(&hdr_info)
        .or_else(|| fps_from_info(&dv_info))
        .unwrap_or(23.976);

    let strategy = compute_alignment_framecount(dv_rpu_frames, hdr_info.frame_count, fps);

    logger.ok(&format!("Alignment strategy: {}", strategy.description));
    if strategy.high_risk {
        logger.warn("Alignment marked HIGH RISK due to large frame difference");
    }

    logger.step("7 | Apply editor (mode conversion + alignment)");
    hybrid_build_editor_json(
        &strategy,
        dv_profile,
        &dv_info,
        &hdr_info,
        &hybrid_editor_json,
    )?;

    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("editor"),
            OsString::from("-i"),
            hybrid_rpu.as_os_str().to_os_string(),
            OsString::from("-j"),
            hybrid_editor_json.as_os_str().to_os_string(),
            OsString::from("-o"),
            hybrid_aligned_rpu.as_os_str().to_os_string(),
        ],
    )?;

    let aligned_frames = hybrid_get_rpu_frame_count(&hybrid_aligned_rpu, rt, logger)?;
    if aligned_frames != hdr_info.frame_count {
        logger.warn(&format!(
            "Aligned RPU frames ({aligned_frames}) do not match HDR target frames ({}) - inject-rpu will auto-handle residual mismatch",
            hdr_info.frame_count
        ));
    }

    logger.step("8 | Extract HEVC from HDR target");
    let track_id = get_hevc_track_id(hdr_target, rt, logger)?;
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvextract,
        &[
            OsString::from("tracks"),
            hdr_target.as_os_str().to_os_string(),
            OsString::from(format!("{}:{}", track_id, hybrid_hevc.display())),
        ],
    )?;

    logger.step("9 | Inject RPU into HEVC");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("inject-rpu"),
            OsString::from("-i"),
            hybrid_hevc.as_os_str().to_os_string(),
            OsString::from("-r"),
            hybrid_aligned_rpu.as_os_str().to_os_string(),
            OsString::from("-o"),
            hybrid_injected_hevc.as_os_str().to_os_string(),
        ],
    )?;

    logger.step("10 | Remux final MKV");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvmerge,
        &[
            OsString::from("-o"),
            output_path.as_os_str().to_os_string(),
            OsString::from("-D"),
            hdr_target.as_os_str().to_os_string(),
            hybrid_injected_hevc.as_os_str().to_os_string(),
            OsString::from("--track-order"),
            OsString::from("1:0"),
        ],
    )?;

    logger.step("11 | Validate output");
    if let Err(e) = hybrid_validate_output(&output_path, hdr_target, rt, logger) {
        let failed_path = output_path.with_extension("FAILED.mkv");
        let _ = fs::rename(&output_path, &failed_path);
        logger.log(&format!(
            "Hybrid validation failed: {} + {} -> kept at {}",
            dv_source.display(),
            hdr_target.display(),
            failed_path.display()
        ));
        return Err(format!(
            "{e}\n  Output kept for inspection: {}",
            failed_path.display()
        ));
    }

    logger.step("12 | Cleanup");
    let _ = fs::remove_file(&hybrid_rpu);
    let _ = fs::remove_file(&hybrid_aligned_rpu);
    let _ = fs::remove_file(&hybrid_hevc);
    let _ = fs::remove_file(&hybrid_injected_hevc);
    let _ = fs::remove_file(&hybrid_editor_json);
    cleanup.clear();

    if opts.delete_sources {
        fs::remove_file(dv_source)
            .map_err(|e| format!("Failed to delete DV source {}: {e}", dv_source.display()))?;
        fs::remove_file(hdr_target)
            .map_err(|e| format!("Failed to delete HDR target {}: {e}", hdr_target.display()))?;
        logger.ok("Deleted both source files (--delete-sources)");
    } else {
        logger.ok("Keeping both source files (use --delete-sources to remove them)");
    }

    logger.log(&format!(
        "Hybrid processed successfully: {} + {} -> {}",
        dv_source.display(),
        hdr_target.display(),
        output_path.display()
    ));
    logger.ok(&format!("Hybrid done: {}", output_path.display()));

    Ok(())
}
