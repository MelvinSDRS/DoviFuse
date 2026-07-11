pub(crate) mod align;
pub(crate) mod editor;
pub(crate) mod grade;
pub(crate) mod preflight;
pub(crate) mod scenes;
pub(crate) mod validate;

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::cli::{GradeCheckMode, HybridOptions, SyncMode};
use crate::exec::{run_status, AppResult, CleanupGuard};
use crate::ffmpeg::detect_scene_cuts;
use crate::fsutil::check_disk_space_hybrid;
use crate::logger::Logger;
use crate::mediainfo::{
    fps_from_info, get_hevc_track_id, hybrid_detect_dv_profile, hybrid_get_media_info,
};
use crate::runtime::Runtime;

use align::{alignment_from_offset, compute_alignment_framecount, AlignmentStrategy};
use editor::hybrid_build_editor_json;
use grade::run_grade_check;
use preflight::hybrid_preflight_checks;
use scenes::{correlate_scene_cuts, export_dv_scene_cuts};
use validate::{hybrid_get_rpu_frame_count, hybrid_validate_output};

/// Compute the RPU alignment strategy per the configured sync mode.
///
/// Scenes mode: correlate the RPU's scene-cut list against an ffmpeg scdet
/// scan of the HDR target. Rejection aborts unless --force, which falls back
/// to the frame-count heuristic. Both cut lists are persisted next to the
/// target while processing (kept on failure for inspection).
#[allow(clippy::too_many_arguments)]
fn compute_alignment(
    opts: &HybridOptions,
    dv_rpu_frames: u64,
    hybrid_rpu: &Path,
    hdr_target: &Path,
    hdr_frames: u64,
    fps: f64,
    dv_scenes_txt: &Path,
    hdr_scenes_txt: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<AlignmentStrategy> {
    if opts.sync == SyncMode::Framecount {
        return Ok(compute_alignment_framecount(
            dv_rpu_frames,
            hdr_frames,
            fps,
        ));
    }

    let dv_cuts = export_dv_scene_cuts(hybrid_rpu, dv_scenes_txt, rt, logger)?;
    logger.ok(&format!("DV RPU scene cuts: {}", dv_cuts.len()));

    logger.log("Scanning HDR target for scene cuts (full decode, this can take a while)...");
    let hdr_cuts = detect_scene_cuts(rt, logger, hdr_target, opts.scene_threshold)?;
    let _ = fs::write(
        hdr_scenes_txt,
        hdr_cuts
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    logger.ok(&format!("HDR target scene cuts: {}", hdr_cuts.len()));

    let max_offset = opts
        .max_offset
        .unwrap_or_else(|| (fps * 300.0).round() as u64) as i64;
    let report = correlate_scene_cuts(&dv_cuts, &hdr_cuts, max_offset, 1);

    match report.accepted {
        Some(sync) => {
            logger.ok(&format!(
                "Scene-cut sync: offset {:+} frames ({} matches, {:.0}% of DV cuts, dominance {:.1})",
                sync.offset,
                sync.matches,
                sync.match_ratio * 100.0,
                sync.dominance
            ));
            alignment_from_offset(sync.offset, dv_rpu_frames, hdr_frames)
        }
        None => {
            let reason = report
                .rejection
                .unwrap_or_else(|| "unknown".to_string());
            logger.warn(&format!("Scene-cut correlation rejected: {reason}"));
            logger.warn(&format!(
                "  DV cuts: {}, HDR cuts: {}, top offsets: {:?}",
                report.dv_count, report.hdr_count, report.top_offsets
            ));
            logger.warn(&format!(
                "  Scene lists kept for inspection: {} / {}",
                dv_scenes_txt.display(),
                hdr_scenes_txt.display()
            ));
            if opts.force {
                logger.warn(
                    "--force: falling back to the frame-count heuristic (sync NOT verified)",
                );
                Ok(compute_alignment_framecount(
                    dv_rpu_frames,
                    hdr_frames,
                    fps,
                ))
            } else {
                Err(format!(
                    "Scene-cut sync failed: {reason}. Re-run with --force to use the frame-count heuristic, or --sync framecount."
                ))
            }
        }
    }
}

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
    let dv_scenes_txt = tmp_dir.join(format!("{base}.hybrid.dv_scenes.txt"));
    let hdr_scenes_txt = tmp_dir.join(format!("{base}.hybrid.hdr_scenes.txt"));

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
    let has_fail = hybrid_preflight_checks(&dv_info, &hdr_info, dv_profile, opts, logger);
    if has_fail {
        return Err("Hybrid preflight failed".to_string());
    }

    logger.step("4 | Disk space check");
    let out_dir = output_path
        .parent()
        .ok_or_else(|| format!("Invalid output path: {}", output_path.display()))?;
    check_disk_space_hybrid(hdr_target, out_dir, logger)?;

    let grade_measures = !opts.skip_grade_check && opts.grade_check != GradeCheckMode::Metadata;
    if opts.sync == SyncMode::Scenes || grade_measures {
        rt.require_ffmpeg()?;
    }

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

    let strategy = compute_alignment(
        opts,
        dv_rpu_frames,
        &hybrid_rpu,
        hdr_target,
        hdr_info.frame_count,
        fps,
        &dv_scenes_txt,
        &hdr_scenes_txt,
        rt,
        logger,
    )?;
    let exact_alignment = strategy.action == "scene_sync";

    logger.ok(&format!("Alignment strategy: {}", strategy.description));
    if strategy.high_risk {
        logger.warn("Alignment marked HIGH RISK due to large frame difference");
    }

    logger.step("7 | Grade check (brightness comparison)");
    if opts.skip_grade_check {
        logger.warn("Grade check skipped (--skip-grade-check)");
    } else if opts.grade_check == GradeCheckMode::Metadata {
        logger.ok("Metadata-only grade check: static gate already enforced in preflight");
    } else {
        let outcome = run_grade_check(
            rt,
            logger,
            dv_source,
            hdr_target,
            &dv_info,
            &hdr_info,
            strategy.start_offset,
            fps,
            opts.grade_check,
            opts.grade_windows,
        )?;
        logger.log(&format!(
            "Grade summary: {} windows measured, {} mismatched, worst mean |dPQ| {:.4}, p99 peak {:.0} nits (DV) vs {:.0} nits (HDR), ratio {:.2}",
            outcome.windows_measured,
            outcome.windows_bad,
            outcome.worst_delta_pq,
            outcome.p99_dv_nits,
            outcome.p99_hdr_nits,
            outcome.peak_ratio
        ));
        if !outcome.pass {
            return Err(format!(
                "Grade check FAILED: the two sources appear to use different HDR grades ({} of {} windows mismatched, peak ratio {:.2}). A hybrid from these would tone-map incorrectly. Use --skip-grade-check only if you are certain the grades match.",
                outcome.windows_bad, outcome.windows_measured, outcome.peak_ratio
            ));
        }
        logger.ok("Grade check passed: brightness profiles match");
    }

    logger.step("8 | Apply editor (mode conversion + alignment)");
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
        if exact_alignment {
            return Err(format!(
                "Aligned RPU frames ({aligned_frames}) do not match HDR target frames ({}) despite scene-sync alignment",
                hdr_info.frame_count
            ));
        }
        logger.warn(&format!(
            "Aligned RPU frames ({aligned_frames}) do not match HDR target frames ({}) - inject-rpu will auto-handle residual mismatch",
            hdr_info.frame_count
        ));
    }

    logger.step("9 | Extract HEVC from HDR target");
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

    logger.step("10 | Inject RPU into HEVC");
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

    logger.step("11 | Remux final MKV");
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

    logger.step("12 | Validate output");
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

    logger.step("13 | Cleanup");
    let _ = fs::remove_file(&hybrid_rpu);
    let _ = fs::remove_file(&hybrid_aligned_rpu);
    let _ = fs::remove_file(&hybrid_hevc);
    let _ = fs::remove_file(&hybrid_injected_hevc);
    let _ = fs::remove_file(&hybrid_editor_json);
    let _ = fs::remove_file(&dv_scenes_txt);
    let _ = fs::remove_file(&hdr_scenes_txt);
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
