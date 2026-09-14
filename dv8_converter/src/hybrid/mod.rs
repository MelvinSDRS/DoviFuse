pub(crate) mod active_area;
pub(crate) mod align;
mod donor;
pub(crate) mod editor;
pub(crate) mod grade;
pub(crate) mod l5;
pub(crate) mod letterbox;
mod mapping;
pub(crate) mod preflight;
pub(crate) mod scenes;
pub(crate) mod validate;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::{GradeCheckMode, HybridOptions, LetterboxMode, SyncMode};
use crate::exec::{run_status, AppResult, CleanupGuard};
use crate::ffmpeg::detect_scene_cuts;
use crate::fsutil::{check_disk_space_hybrid, create_job_dir};
use crate::logger::Logger;
use crate::mediainfo::{
    fps_from_info, get_hevc_track_id, hybrid_detect_dv_profile, hybrid_get_media_info,
};
use crate::runtime::Runtime;

use align::{
    alignment_from_offset, compute_alignment_framecount, require_padding_review, AlignmentStrategy,
};
use editor::hybrid_build_editor_json;
use grade::run_grade_check;
use letterbox::{decide_active_area, dv_rpu_l5_presets, measure_letterbox, ActiveAreaChoice};
use preflight::hybrid_preflight_checks;
use scenes::{assess_temporal_alignment, correlate_scene_cuts, export_dv_scene_cuts};
use validate::{hybrid_get_rpu_frame_count, hybrid_validate_output, hybrid_verify_output_sync};

/// Complement of measured half-open frame intervals on the target timeline.
fn unmeasured_ranges(covered: &[(u64, u64)], frames: u64) -> Vec<(u64, u64)> {
    let mut covered = covered.to_vec();
    covered.sort_unstable();
    let mut cursor = 0;
    let mut gaps = Vec::new();
    for (start, end) in covered {
        let (start, end) = (start.min(frames), end.min(frames));
        if end <= start {
            continue;
        }
        if start > cursor {
            gaps.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < frames {
        gaps.push((cursor, frames));
    }
    gaps
}

/// Correlation search window in frames: --max-offset, or 5 minutes' worth.
fn correlation_max_offset(opts: &HybridOptions, fps: f64) -> i64 {
    opts.max_offset
        .unwrap_or_else(|| (fps * 300.0).round() as u64) as i64
}

/// Compute the RPU alignment strategy per the configured sync mode.
///
/// Scenes mode: correlate the RPU's scene-cut list against an ffmpeg scdet
/// scan of the HDR target. Explicit offsets never override local contradictions.
/// Frame counts alone cannot locate an edit. Both cut lists are persisted next to the
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
    if opts.sync == SyncMode::Framecount && opts.explicit_offset.is_none() {
        compute_alignment_framecount(dv_rpu_frames, hdr_frames, fps)?;
    }

    let dv_cuts = export_dv_scene_cuts(hybrid_rpu, dv_scenes_txt, rt, logger)?;
    logger.ok(&format!("DV RPU scene cuts: {}", dv_cuts.len()));

    logger.log("Scanning HDR target for scene cuts (full decode, this can take a while)...");
    let hdr_cuts = detect_scene_cuts(rt, logger, hdr_target, opts.scene_threshold)?;
    // The post-inject verification re-reads this file; a failed write would
    // make it compare against stale cuts or silently skip.
    fs::write(
        hdr_scenes_txt,
        hdr_cuts
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .map_err(|e| {
        format!(
            "Failed to write HDR scene list {}: {e}",
            hdr_scenes_txt.display()
        )
    })?;
    logger.ok(&format!("HDR target scene cuts: {}", hdr_cuts.len()));

    let max_offset = correlation_max_offset(opts, fps);
    let report = correlate_scene_cuts(&dv_cuts, &hdr_cuts, max_offset, 1);

    if let Some(expected_offset) = opts.known_offset {
        match report.accepted {
            Some(sync) if sync.offset == expected_offset => logger.ok(&format!(
                "Checker offset reconfirmed at {expected_offset:+} frames before repair"
            )),
            Some(sync) => {
                return Err(format!(
                    "Repair stopped: checker reported offset {expected_offset:+}, but the fresh scan found {:+}",
                    sync.offset
                ))
            }
            None => {
                return Err(format!(
                    "Repair stopped: the checker offset {expected_offset:+} could not be reconfirmed ({})",
                    report.rejection.as_deref().unwrap_or("unknown reason")
                ))
            }
        }
    }

    let candidate_offset = if opts.sync == SyncMode::Framecount {
        opts.explicit_offset.unwrap_or(0)
    } else {
        opts.explicit_offset
            .or_else(|| report.accepted.map(|sync| sync.offset))
            .unwrap_or(0)
    };
    let evidence = assess_temporal_alignment(
        &dv_cuts,
        &hdr_cuts,
        dv_rpu_frames,
        hdr_frames,
        candidate_offset,
        max_offset,
        1,
    );
    logger.measurement(
        "temporal_alignment",
        serde_json::to_value(&evidence).unwrap(),
    );
    logger.log(&format!("Temporal evidence: {} matched anchors; {} unverified intervals over the full timelines (use --report to retain frame ranges)", evidence.matched_anchors.len(), evidence.unverified_intervals.len()));
    if let Some(limit) = &evidence.analysis_limit {
        return Err(format!("Temporal inspection incomplete: {limit}. Inspect the source timelines before proceeding."));
    }
    if !evidence.contradictions.is_empty() {
        return Err(format!("Temporal alignment rejected: local scene offsets contradict the proposed offset {candidate_offset:+}. Conflicting intervals: {:?}. Inspect decoded pictures in these ranges; --force and --offset cannot approve different edits.", evidence.contradictions));
    }
    if opts.sync == SyncMode::Framecount && opts.explicit_offset.is_none() {
        if let Some(sync) = report.accepted {
            if sync.offset != 0 {
                return Err(format!("Framecount offset 0 contradicts measured scene offset {:+}; inspect the sources and supply --offset", sync.offset));
            }
        }
        return compute_alignment_framecount(dv_rpu_frames, hdr_frames, fps);
    }
    if let Some(offset) = opts.explicit_offset {
        if let Some(sync) = report.accepted {
            if sync.offset.abs_diff(offset) > 1 {
                return Err(format!(
                    "Explicit offset {offset:+} contradicts measured scene offset {:+}",
                    sync.offset
                ));
            }
        }
        logger.warn("Using an explicit offset; picture alignment and intervals between anchors remain unverified");
        return alignment_from_offset(offset, dv_rpu_frames, hdr_frames);
    }

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
            let reason = report.rejection.unwrap_or_else(|| "unknown".to_string());
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
                logger
                    .warn("--force: assuming offset 0 only for equal frame counts (picture alignment NOT verified)");
                compute_alignment_framecount(dv_rpu_frames, hdr_frames, fps)
            } else {
                Err(format!(
                    "Scene-cut sync failed: {reason}. Inspect the scene lists and supply --offset <frames>, or use --sync framecount only for equal frame counts."
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
    process_hybrid_impl(
        dv_source,
        hdr_target,
        custom_output,
        opts,
        rt,
        logger,
        false,
        true,
    )
}

pub(crate) fn repair_sync(
    file: &Path,
    offset: i64,
    allow_padding: bool,
    custom_output: Option<&Path>,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<PathBuf> {
    let output = match custom_output {
        Some(path) => path.to_path_buf(),
        None => repair_output_path(file)?,
    };
    if output.exists() {
        return Err(format!(
            "Repair output already exists: {}",
            output.display()
        ));
    }

    let opts = HybridOptions {
        skip_grade_check: true,
        letterbox: LetterboxMode::Off,
        known_offset: Some(offset),
        allow_padding,
        ..HybridOptions::default()
    };
    process_hybrid_impl(file, file, Some(&output), &opts, rt, logger, true, false)?;
    Ok(output)
}

fn repair_output_path(file: &Path) -> AppResult<PathBuf> {
    let dir = file
        .parent()
        .ok_or_else(|| format!("Invalid repair input path: {}", file.display()))?;
    let stem = file
        .file_stem()
        .ok_or_else(|| format!("Invalid repair input filename: {}", file.display()))?
        .to_string_lossy();

    let first = dir.join(format!("{stem}.DV8.Fixed.mkv"));
    if !first.exists() {
        return Ok(first);
    }
    for index in 2..=9999 {
        let candidate = dir.join(format!("{stem}.DV8.Fixed.{index}.mkv"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Could not choose an unused repair output beside {}",
        file.display()
    ))
}

#[allow(clippy::too_many_arguments)]
fn process_hybrid_impl(
    dv_source: &Path,
    hdr_target: &Path,
    custom_output: Option<&Path>,
    opts: &HybridOptions,
    rt: &Runtime,
    logger: &Logger,
    allow_same_input: bool,
    emit_completed: bool,
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

    if !allow_same_input && fs::canonicalize(dv_source).ok() == fs::canonicalize(hdr_target).ok() {
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
        return Err(format!(
            "Output file already exists: {}",
            output_path.display()
        ));
    }

    let target_dir = hdr_target
        .parent()
        .ok_or_else(|| format!("Invalid HDR target path: {}", hdr_target.display()))?;
    let scratch_root = rt.tmp_dir.as_deref().unwrap_or(target_dir);
    let tmp_dir_owned = if !rt.dry_run {
        create_job_dir(scratch_root, "dv8-hybrid")?
    } else {
        scratch_root.to_path_buf()
    };
    let tmp_dir = tmp_dir_owned.as_path();
    let base = hdr_target
        .file_stem()
        .ok_or_else(|| format!("Invalid HDR target filename: {}", hdr_target.display()))?
        .to_string_lossy()
        .to_string();

    let hybrid_rpu = tmp_dir.join(format!("{base}.hybrid.rpu.bin"));
    let hybrid_aligned_rpu = tmp_dir.join(format!("{base}.hybrid.aligned.rpu.bin"));
    let hybrid_l5_rpu = tmp_dir.join(format!("{base}.hybrid.target-l5.rpu.bin"));
    let hybrid_hevc = tmp_dir.join(format!("{base}.hybrid.hevc"));
    let hybrid_injected_hevc = tmp_dir.join(format!("{base}.hybrid.injected.hevc"));
    let hybrid_editor_json = tmp_dir.join(format!("{base}.hybrid.editor.json"));
    let dv_scenes_txt = tmp_dir.join(format!("{base}.hybrid.dv_scenes.txt"));
    let hdr_scenes_txt = tmp_dir.join(format!("{base}.hybrid.hdr_scenes.txt"));
    let hybrid_l5_json = tmp_dir.join(format!("{base}.hybrid.l5.json"));
    let mapping_json = tmp_dir.join(format!("{base}.hybrid.mapping.json"));
    let verify_rpu = tmp_dir.join(format!("{base}.hybrid.verify.rpu.bin"));
    // Like the scene lists, verify_scenes stays out of the CleanupGuard so a
    // sync-verification failure leaves it behind for inspection.
    let verify_scenes_txt = tmp_dir.join(format!("{base}.hybrid.verify_scenes.txt"));

    // These paths are deterministic per target, and the CleanupGuard deletes
    // them on any failure. Refuse to adopt files this run did not create:
    // they belong to a concurrent conversion of the same target or to a
    // hard-killed run the user should inspect. (The scene lists are exempt -
    // they are kept on failure by design and simply overwritten on retry.)
    let intermediates = [
        &hybrid_rpu,
        &hybrid_aligned_rpu,
        &hybrid_l5_rpu,
        &hybrid_hevc,
        &hybrid_injected_hevc,
        &hybrid_editor_json,
        &hybrid_l5_json,
        &mapping_json,
        &verify_rpu,
    ];
    if let Some(existing) = intermediates.iter().find(|p| p.exists()) {
        return Err(format!(
            "Intermediate file already exists: {} - another conversion of this target may be running, or a previous run was killed. Remove the stale {base}.hybrid.* files to proceed.",
            existing.display()
        ));
    }

    let mut cleanup = CleanupGuard::new(logger.clone());
    if !rt.dry_run {
        cleanup.add_dir(tmp_dir);
    }
    if !rt.dry_run {
        for path in intermediates {
            cleanup.add(path);
        }
        crate::fsutil::reserve_output(&output_path)?;
        cleanup.add(&output_path);
    }

    logger.step("0 | Determine output path");
    logger.ok(&format!("Hybrid output: {}", output_path.display()));

    logger.step("1 | Gather media info");
    get_hevc_track_id(dv_source, rt, logger)?;
    get_hevc_track_id(hdr_target, rt, logger)?;
    let dv_info = hybrid_get_media_info(dv_source, rt, logger)?;
    let hdr_info = hybrid_get_media_info(hdr_target, rt, logger)?;
    let target_profile = hybrid_detect_dv_profile(hdr_target, rt, logger)?;
    if (!allow_same_input && target_profile.is_some())
        || (allow_same_input && target_profile != Some(8))
    {
        return Err(
            "Hybrid requires an HDR10-only target; sync repair requires Profile 8".to_string(),
        );
    }

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
    check_disk_space_hybrid(hdr_target, scratch_root, out_dir, logger)?;

    rt.require_ffmpeg()?;

    if rt.dry_run {
        logger.ok("[DRY RUN] Preflight completed. Mutating steps were skipped.");
        logger.ok("[DRY RUN] Extracted RPU eligibility remains unchecked; it is required before editing in a real run.");
        logger.ok("[DRY RUN] Mapping policy remains unchecked; a real hybrid must establish supported mapping across every donor RPU before editing.");
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
        if emit_completed {
            logger.completed(&output_path);
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

    let dv_rpu_frames = donor::validate_rpu(&hybrid_rpu, dv_profile, rt, logger)?;
    let mapping_policy = if allow_same_input {
        // This path only moves metadata over the same source pictures. It
        // does not authorize transferring a transform to a different target.
        logger.measurement(
            "mapping_policy",
            serde_json::json!({
                "policy": "preserve for same-input sync repair",
                "mapping_removed": false,
                "identity_required": false,
            }),
        );
        mapping::MappingPolicy::PreserveForSyncRepair
    } else {
        mapping::inspect(
            &hybrid_rpu,
            &mapping_json,
            dv_profile,
            dv_rpu_frames,
            rt,
            logger,
        )?
    };

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

    logger.measurement("alignment", serde_json::json!({"action":strategy.action,"offset_frames":strategy.start_offset,"method":format!("{:?}",opts.sync),"description":strategy.description,"high_risk":strategy.high_risk,"padding_reviewed":opts.allow_padding,"remove_ranges":strategy.remove_ranges,"duplicates":strategy.duplicates.iter().map(|d|serde_json::json!({"source":d.source,"offset":d.offset,"length":d.length})).collect::<Vec<_>>(),"picture_alignment_verified":false}));
    require_padding_review(&strategy, opts.allow_padding)?;
    logger.check_result("picture_alignment", "Picture alignment", "inconclusive",
        "The offset is based on metadata scene flags or frame counts. Full-film decoded-picture alignment has not been validated.");
    logger.ok(&format!("Alignment strategy: {}", strategy.description));
    if strategy.high_risk {
        logger.warn("Alignment repeats edge metadata; the padded pictures remain unverified");
    }

    logger.step("7 | Brightness/chroma screening");
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
        let mut screen = serde_json::to_value(&outcome).expect("brightness screen is serializable");
        let covered: Vec<_> = outcome
            .windows
            .iter()
            .filter(|window| matches!(window.status, grade::GradeWindowStatus::Measured))
            .filter_map(|window| window.measured_target)
            .map(|interval| (interval.start_frame, interval.end_frame))
            .collect();
        let gaps = unmeasured_ranges(&covered, hdr_info.frame_count);
        screen["target_unmeasured_intervals"] = serde_json::json!(gaps
            .iter()
            .map(|(start, end)| serde_json::json!({"start_frame": start,"end_frame": end}))
            .collect::<Vec<_>>());
        screen["whole_target_screened"] = serde_json::json!(gaps.is_empty());
        screen["target_frames_covered"] = serde_json::json!(
            hdr_info.frame_count - gaps.iter().map(|(start, end)| end - start).sum::<u64>()
        );
        screen["mode"] = serde_json::json!(format!("{:?}", opts.grade_check));
        screen["method"] = serde_json::json!("Limited-range BT.2020 PQ Y averages and U/V averages; mismatch screening, not calibrated RGB grade equivalence");
        screen["peak_metric"] = serde_json::json!(
            "PQ(Y) code-derived brightness surrogate, not measured luminance or MaxCLL"
        );
        logger.measurement("brightness_screen", screen);
        if !gaps.is_empty() {
            logger.warn(&format!("Brightness/chroma coverage leaves {} unmeasured target intervals; see the job report for exact frame ranges", gaps.len()));
        }
        logger.log(&format!(
            "Brightness/chroma screen: {} of {} requested intervals measured, {} skipped, {} mismatched; worst mean |dPQ(YAVG)| {:.4}; PQ(Y) peak surrogate {:.0} (DV) vs {:.0} (HDR), ratio {:.2}. These values are not measured luminance.",
            outcome.windows_measured, outcome.windows_requested, outcome.windows_skipped,
            outcome.windows_bad, outcome.worst_delta_pq, outcome.p99_dv_nits,
            outcome.p99_hdr_nits, outcome.peak_ratio
        ));
        if !outcome.pass {
            return Err(format!(
                "Brightness/chroma screen failed: {} of {} intervals mismatched, peak surrogate ratio {:.2}. Grade compatibility has not been established; hybrid creation stopped.",
                outcome.windows_bad, outcome.windows_measured, outcome.peak_ratio
            ));
        }
        logger.ok("No brightness/chroma mismatch detected within the measured coverage; creative-grade equivalence remains unverified");
    }
    logger.check_result("picture_grade", "Picture grade compatibility", "inconclusive",
        "Calibrated RGB grade compatibility has not been established. Brightness, average chroma and static-metadata screens cannot verify the creative grade.");

    logger.step("8 | Letterbox L5 (active area)");
    let active_area = match opts.letterbox {
        LetterboxMode::Off => {
            logger.ok("Letterbox handling disabled (--letterbox off) - RPU L5 kept as-is");
            ActiveAreaChoice::Keep
        }
        LetterboxMode::Measured => {
            rt.require_ffmpeg()?;
            let duration_s = hdr_info
                .duration_ms
                .map(|ms| ms / 1000.0)
                .unwrap_or_else(|| hdr_info.frame_count as f64 / fps.max(1.0));
            let windows = crate::ffmpeg::sample_windows(duration_s, opts.grade_windows, 5.0);
            let (canvas_w, canvas_h) = match (hdr_info.width, hdr_info.height) {
                (Some(w), Some(h)) => (w, h),
                _ => {
                    return Err(
                        "HDR target resolution unavailable for letterbox measurement".to_string(),
                    )
                }
            };
            let measured = measure_letterbox(rt, logger, hdr_target, &windows, canvas_w, canvas_h)?;
            let dv_presets = dv_rpu_l5_presets(&hybrid_rpu, &hybrid_l5_json, rt, logger)?;
            let canvas_match = dv_info.width == hdr_info.width
                && dv_info.height == hdr_info.height
                && dv_info.width.is_some();
            let (choice, logs) = decide_active_area(measured, &dv_presets, canvas_match)?;
            for (warn, msg) in &logs {
                if *warn {
                    logger.warn(msg);
                } else {
                    logger.ok(msg);
                }
            }
            choice
        }
    };
    logger.check_result(
        "active_area_picture", "Active-picture coverage", "inconclusive",
        "Full-timeline target-picture/L5 correspondence has not been validated. Sources will be retained; no automatic active-area repair is available.",
    );

    logger.step("9 | Apply editor (mode conversion + alignment)");
    hybrid_build_editor_json(
        &strategy,
        mapping_policy,
        &dv_info,
        &hdr_info,
        if matches!(active_area, ActiveAreaChoice::Measured(_)) {
            &ActiveAreaChoice::Keep
        } else {
            &active_area
        },
        &hybrid_editor_json,
    )?;

    if allow_same_input {
        // A sync repair must move the original metadata, preserving mapping and trims.
        let mut config = editor::build_editor_config(
            &strategy,
            mapping_policy,
            &dv_info,
            &hdr_info,
            &ActiveAreaChoice::Keep,
        );
        config.level6 = None;
        fs::write(
            &hybrid_editor_json,
            serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }

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
        return Err(format!("Aligned RPU has {aligned_frames} frames; expected {}. Refusing implicit truncation or padding.", hdr_info.frame_count));
    }

    let canvas = (
        hdr_info.width.ok_or("Missing target width")?,
        hdr_info.height.ok_or("Missing target height")?,
    );
    if let ActiveAreaChoice::Measured(bars) = active_area {
        let timeline =
            active_area::Timeline::constant(hdr_info.frame_count, bars, canvas.0, canvas.1)?;
        l5::apply_after_alignment(
            &timeline,
            &hybrid_aligned_rpu,
            &hybrid_l5_rpu,
            &hybrid_editor_json,
            &hybrid_l5_json,
            canvas,
            rt,
            logger,
        )?;
    }
    let expected_l5 = l5::export_timeline(
        &hybrid_aligned_rpu,
        &hybrid_l5_json,
        hdr_info.frame_count,
        canvas,
        rt,
        logger,
    )?;

    logger.step("10 | Extract HEVC from HDR target");
    let track_id = get_hevc_track_id(hdr_target, rt, logger)?;
    let remux_source = crate::remux::RemuxSource::read(hdr_target, rt, logger)?;
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

    logger.step("11 | Inject RPU into HEVC");
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

    let timestamps = tmp_dir.join("video.timestamps.txt");
    run_status(
        logger,
        false,
        true,
        &rt.mkvextract,
        &[
            hdr_target.as_os_str().to_os_string(),
            "timestamps_v2".into(),
            format!("{track_id}:{}", timestamps.display()).into(),
        ],
    )?;
    logger.step("12 | Remux final MKV");
    let remux_result = run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvmerge,
        &remux_source.arguments(hdr_target, &hybrid_injected_hevc, &timestamps, &output_path)?,
    );
    if let Err(e) = remux_result {
        // A failed mkvmerge (disk full, ...) can leave a partial file at the
        // output path, which a future run would skip as "already exists".
        let _ = fs::remove_file(&output_path);
        return Err(e);
    }

    // Rename a bad output to a fresh .FAILED[.n].mkv (kept for inspection)
    // and build the final error. Shared by validation and sync verification
    // below. If the rename fails, the bad file is still at output_path where
    // the next run would skip it as "already exists" - report that honestly.
    let fail_output = |e: String| -> String {
        let mut failed_path = output_path.with_extension("FAILED.mkv");
        let mut n = 1;
        while failed_path.exists() {
            n += 1;
            failed_path = output_path.with_extension(format!("FAILED.{n}.mkv"));
        }
        let kept_path = match fs::rename(&output_path, &failed_path) {
            Ok(()) => failed_path,
            Err(rename_err) => {
                logger.warn(&format!(
                    "Could not rename failed output to {}: {rename_err}",
                    failed_path.display()
                ));
                output_path.clone()
            }
        };
        for (source, suffix) in [
            (&dv_scenes_txt, "dv_scenes.txt"),
            (&hdr_scenes_txt, "hdr_scenes.txt"),
            (&verify_scenes_txt, "verify_scenes.txt"),
        ] {
            if source.exists() {
                let diagnostic = kept_path.with_extension(suffix);
                let _ = fs::copy(source, diagnostic);
            }
        }
        logger.log(&format!(
            "Hybrid validation failed: {} + {} -> kept at {}",
            dv_source.display(),
            hdr_target.display(),
            kept_path.display()
        ));
        format!("{e}\n  Output kept for inspection: {}", kept_path.display())
    };

    logger.step("13 | Validate output");
    remux_source
        .verify(&output_path, rt, logger)
        .map_err(fail_output)?;
    if let Err(e) = hybrid_validate_output(&output_path, hdr_target, rt, logger) {
        return Err(fail_output(e));
    }

    let scan = crate::ffmpeg::scan_video(rt, logger, &output_path, opts.scene_threshold)
        .map_err(fail_output)?;
    if scan.frames != hdr_info.frame_count {
        return Err(fail_output(
            "Decoded output count differs from HDR target".to_string(),
        ));
    }
    logger.step("14 | Post-inject sync verification");
    if let Err(e) = hybrid_verify_output_sync(
        &output_path,
        hdr_info.frame_count,
        &scan.cuts,
        &verify_rpu,
        &verify_scenes_txt,
        correlation_max_offset(opts, fps),
        rt,
        logger,
    ) {
        return Err(fail_output(e));
    }

    let actual_l5 = l5::export_timeline(
        &verify_rpu,
        &hybrid_l5_json,
        hdr_info.frame_count,
        canvas,
        rt,
        logger,
    )
    .map_err(fail_output)?;
    if expected_l5 != actual_l5 {
        return Err(fail_output(
            "Output L5 frame intervals differ from the aligned RPU".into(),
        ));
    }
    logger.ok("Re-extracted output L5 matches every expected frame interval (metadata preservation, not picture-area validation)");
    if !allow_same_input {
        mapping::inspect(
            &verify_rpu,
            &mapping_json,
            Some(8),
            hdr_info.frame_count,
            rt,
            logger,
        )
        .map_err(|e| fail_output(format!("Output mapping verification failed: {e}")))?;
        logger.check_result(
            "output_mapping", "Output mapping", "pass",
            "Every re-extracted output RPU has the supported identity mapping. This verifies mapping transport, not picture-grade compatibility.",
        );
    }

    logger.step("15 | Cleanup");
    let _ = fs::remove_file(&hybrid_rpu);
    let _ = fs::remove_file(&hybrid_aligned_rpu);
    let _ = fs::remove_file(&hybrid_hevc);
    let _ = fs::remove_file(&hybrid_injected_hevc);
    let _ = fs::remove_file(&hybrid_editor_json);
    let _ = fs::remove_file(&dv_scenes_txt);
    let _ = fs::remove_file(&hdr_scenes_txt);
    let _ = fs::remove_file(&hybrid_l5_json);
    let _ = fs::remove_file(&mapping_json);
    let _ = fs::remove_file(&verify_rpu);
    let _ = fs::remove_file(&verify_scenes_txt);
    if !rt.dry_run {
        let _ = fs::remove_dir_all(tmp_dir);
    }
    cleanup.clear();

    if opts.delete_sources {
        logger.warn("Keeping both source files: active-area picture validation does not yet cover the full timeline; --delete-sources withheld");
    }
    logger.ok("Keeping both source files");

    logger.log(&format!(
        "Hybrid processed successfully: {} + {} -> {}",
        dv_source.display(),
        hdr_target.display(),
        output_path.display()
    ));
    logger.ok(&format!("Hybrid done: {}", output_path.display()));
    if emit_completed {
        logger.completed(&output_path);
    }

    Ok(())
}

#[cfg(test)]
mod repair_tests {
    use super::*;

    #[test]
    fn screen_coverage_exposes_head_tail_and_internal_gaps_without_double_counting() {
        assert_eq!(
            unmeasured_ranges(&[(30, 50), (10, 40), (50, 60), (80, 150)], 100),
            vec![(0, 10), (60, 80)]
        );
        assert_eq!(unmeasured_ranges(&[], 100), vec![(0, 100)]);
        assert!(unmeasured_ranges(&[(0, 100)], 100).is_empty());
    }

    #[test]
    fn repair_output_is_non_destructive_and_numbered() {
        let root =
            std::env::temp_dir().join(format!("dv8-repair-name-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let input = root.join("Movie.mkv");
        fs::write(&input, b"input").unwrap();

        let first = repair_output_path(&input).unwrap();
        assert_eq!(first, root.join("Movie.DV8.Fixed.mkv"));
        fs::write(&first, b"existing").unwrap();
        assert_eq!(
            repair_output_path(&input).unwrap(),
            root.join("Movie.DV8.Fixed.2.mkv")
        );

        fs::remove_dir_all(root).unwrap();
    }
}
