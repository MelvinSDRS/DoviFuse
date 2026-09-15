use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::exec::{run_status, AppResult, CleanupGuard};
use crate::ffmpeg::scan_video;
use crate::fsutil::create_job_dir;
use crate::hybrid::scenes::{
    assess_temporal_alignment, correlate_scene_cuts, export_dv_scene_cuts, CorrelationReport,
};
use crate::hybrid::validate::hybrid_get_rpu_frame_count;
use crate::logger::Logger;
use crate::mediainfo::{
    fps_from_info, get_hevc_track_id, hybrid_detect_dv_profile, hybrid_get_hdr_compatibility,
    hybrid_get_media_info, is_hdr10_compatible,
};
use crate::runtime::Runtime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

fn report(logger: &Logger, key: &str, label: &str, status: CheckStatus, detail: &str) {
    logger.check_result(key, label, status.as_str(), detail);
}

fn report_fixable(
    logger: &Logger,
    key: &str,
    label: &str,
    status: CheckStatus,
    detail: &str,
    fix_action: &str,
    fix_value: i64,
) {
    logger.check_result_with_fix(
        key,
        label,
        status.as_str(),
        detail,
        Some(fix_action),
        Some(fix_value),
    );
}

fn step(logger: &Logger, phase_offset: usize, index: usize, label: &str) {
    logger.step(&format!("{} | {label}", phase_offset + index));
}

/// Record a failed check and stop only when later checks cannot meaningfully run.
fn stop(logger: &Logger, key: &str, label: &str, detail: impl Into<String>) -> String {
    let detail = detail.into();
    report(logger, key, label, CheckStatus::Fail, &detail);
    detail
}

/// The converter's own post-inject gate remains strict, but the standalone
/// checker treats a consistent one-frame boundary difference as detector
/// jitter and reports it as a warning.
fn checker_sync_result(report: &CorrelationReport) -> (CheckStatus, String) {
    match report.accepted {
        Some(sync) => {
            let status = match sync.offset.unsigned_abs() {
                0 => CheckStatus::Pass,
                1 => CheckStatus::Warn,
                _ => CheckStatus::Fail,
            };
            let frame_word = if sync.offset.unsigned_abs() == 1 {
                "frame"
            } else {
                "frames"
            };
            let detail = if sync.offset == 0 {
                format!(
                    "RPU and video scene changes align at offset 0 ({} matches, {:.0}% of RPU cuts, dominance {:.1})",
                    sync.matches,
                    sync.match_ratio * 100.0,
                    sync.dominance
                )
            } else {
                format!(
                    "RPU is consistently offset {:+} {frame_word} from the video ({} matches, {:.0}% of RPU cuts, dominance {:.1})",
                    sync.offset,
                    sync.matches,
                    sync.match_ratio * 100.0,
                    sync.dominance
                )
            };
            (status, detail)
        }
        None => (
            CheckStatus::Warn,
            format!(
                "Metadata sync could not be verified: {} (RPU cuts: {}, video cuts: {}, top offsets: {:?})",
                report.rejection.as_deref().unwrap_or("unknown reason"),
                report.dv_count,
                report.hdr_count,
                report.top_offsets
            ),
        ),
    }
}

pub(crate) fn check_file(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<()> {
    check_file_with_phase_offset(file, rt, logger, 0)
}

pub(crate) fn check_file_with_phase_offset(
    file: &Path,
    rt: &Runtime,
    logger: &Logger,
    phase_offset: usize,
) -> AppResult<()> {
    let initial_failures = logger.check_failure_count();
    if !file.is_file() {
        return Err(stop(
            logger,
            "file",
            "Input file",
            format!("File not found: {}", file.display()),
        ));
    }
    if file
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_lowercase)
        .as_deref()
        != Some("mkv")
    {
        return Err(stop(
            logger,
            "container",
            "Matroska container",
            "The input is not an MKV file",
        ));
    }

    let scratch_root = rt.tmp_dir.clone().unwrap_or_else(std::env::temp_dir);
    let scratch = create_job_dir(&scratch_root, "dovifuse-check")?;
    let rpu = scratch.join("check.rpu.bin");
    let rpu_scenes = scratch.join("rpu-scenes.txt");
    let mut cleanup = CleanupGuard::new(logger.clone());
    cleanup.add_dir(&scratch);

    step(
        logger,
        phase_offset,
        1,
        "Inspect container and video stream",
    );
    let size = fs::metadata(file).map_err(|error| error.to_string())?.len();
    if size == 0 {
        return Err(stop(
            logger,
            "container",
            "Matroska container",
            "The file is empty",
        ));
    }
    get_hevc_track_id(file, rt, logger).map_err(|error| {
        stop(
            logger,
            "container",
            "Matroska / HEVC structure",
            format!("Unreadable container or missing HEVC video track: {error}"),
        )
    })?;
    report(
        logger,
        "container",
        "Matroska / HEVC structure",
        CheckStatus::Pass,
        "Container is readable and contains an HEVC video track",
    );
    let info = hybrid_get_media_info(file, rt, logger).map_err(|error| {
        stop(
            logger,
            "metadata",
            "Video metadata",
            format!("Could not read video metadata: {error}"),
        )
    })?;

    step(
        logger,
        phase_offset,
        2,
        "Validate Dolby Vision and HDR metadata",
    );
    let detected_profile = match hybrid_detect_dv_profile(file, rt, logger) {
        Ok(profile @ Some(8)) => {
            report(
                logger,
                "profile",
                "Dolby Vision profile",
                CheckStatus::Pass,
                "Profile 8 detected",
            );
            profile
        }
        Ok(profile) => {
            report(
                logger,
                "profile",
                "Dolby Vision profile",
                CheckStatus::Fail,
                &format!(
                    "Expected Profile 8, detected {}",
                    profile
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "none".to_string())
                ),
            );
            profile
        }
        Err(error) => {
            report(
                logger,
                "profile",
                "Dolby Vision profile",
                CheckStatus::Fail,
                &format!("Could not inspect the Dolby Vision profile: {error}"),
            );
            None
        }
    };

    let hdr10_repair_safe = match hybrid_get_hdr_compatibility(file, rt, logger) {
        Ok(hdr_compatibility) => {
            let hdr_detail = format!(
                "{}-bit {}, {} — {}",
                info.bit_depth.unwrap_or(0),
                info.colour_primaries.trim(),
                info.transfer_characteristics.trim(),
                hdr_compatibility
            );
            let hdr10_compatible = is_hdr10_compatible(&info.hdr_format, &hdr_compatibility);
            let ten_bit = crate::mediainfo::has_hdr10_base(&info);
            let (status, detail) = match (hdr10_compatible, ten_bit) {
                (true, true) => (CheckStatus::Pass, hdr_detail),
                (false, true) => (
                    CheckStatus::Fail,
                    format!("HDR10 compatibility was not reported — {hdr_detail}"),
                ),
                (true, false) => (
                    CheckStatus::Fail,
                    format!("The base layer must be 10-bit BT.2020 PQ — {hdr_detail}"),
                ),
                (false, false) => (
                    CheckStatus::Fail,
                    format!(
                        "HDR10 compatibility was not reported and the base layer is not 10-bit — {hdr_detail}"
                    ),
                ),
            };
            report(logger, "hdr10", "HDR10 base layer", status, &detail);
            status == CheckStatus::Pass
        }
        Err(error) => {
            report(
                logger,
                "hdr10",
                "HDR10 base layer",
                CheckStatus::Fail,
                &format!("Could not inspect HDR10 compatibility: {error}"),
            );
            false
        }
    };

    step(
        logger,
        phase_offset,
        3,
        "Extract and parse Dolby Vision RPU",
    );
    let rpu_frames = match run_status(
        logger,
        false,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("extract-rpu"),
            OsString::from("-i"),
            file.as_os_str().to_os_string(),
            OsString::from("-o"),
            rpu.as_os_str().to_os_string(),
        ],
    ) {
        Ok(()) => match hybrid_get_rpu_frame_count(&rpu, rt, logger) {
            Ok(0) => {
                report(
                    logger,
                    "rpu",
                    "Dolby Vision RPU",
                    CheckStatus::Fail,
                    "The RPU contains no frames",
                );
                None
            }
            Ok(frames) => {
                report(
                    logger,
                    "rpu",
                    "Dolby Vision RPU",
                    CheckStatus::Pass,
                    &format!("Parsed {frames} RPU frames"),
                );
                Some(frames)
            }
            Err(error) => {
                report(logger, "rpu", "Dolby Vision RPU", CheckStatus::Fail, &error);
                None
            }
        },
        Err(error) => {
            report(
                logger,
                "rpu",
                "Dolby Vision RPU",
                CheckStatus::Fail,
                &format!("RPU extraction failed: {error}"),
            );
            None
        }
    };

    step(
        logger,
        phase_offset,
        4,
        "Compare video and RPU frame counts",
    );
    match rpu_frames {
        None => report(
            logger,
            "frames",
            "Frame count",
            CheckStatus::Skipped,
            "Not run because the RPU could not be parsed",
        ),
        Some(_) if info.frame_count == 0 => report(
            logger,
            "frames",
            "Frame count",
            CheckStatus::Warn,
            "Video frame count is unavailable; RPU count could not be compared",
        ),
        Some(rpu_frames) if info.frame_count != rpu_frames => report(
            logger,
            "frames",
            "Frame count",
            CheckStatus::Fail,
            &format!(
                "Video has {} frames but the RPU has {rpu_frames}",
                info.frame_count
            ),
        ),
        Some(rpu_frames) => report(
            logger,
            "frames",
            "Frame count",
            CheckStatus::Pass,
            &format!("Video and RPU both contain {rpu_frames} frames"),
        ),
    }

    step(
        logger,
        phase_offset,
        5,
        "Fully decode video and detect scene changes",
    );
    rt.require_ffmpeg()
        .map_err(|error| stop(logger, "decode", "Full video decode", error))?;
    let video_cuts = match scan_video(rt, logger, file, 8.0) {
        Ok(scan) => {
            if let Some(rpu_frames) = rpu_frames {
                let status = if rpu_frames == scan.frames {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Fail
                };
                report(
                    logger,
                    "decoded_frames",
                    "Decoded frame count",
                    status,
                    &format!(
                        "Decoded {} video frames; RPU contains {rpu_frames}",
                        scan.frames
                    ),
                );
            }
            report(
                logger,
                "decode",
                "Full video decode",
                CheckStatus::Pass,
                &format!(
                    "Entire video decoded successfully; {} scene changes detected",
                    scan.cuts.len()
                ),
            );
            Some(scan.cuts)
        }
        Err(error) => {
            report(
                logger,
                "decode",
                "Full video decode",
                CheckStatus::Fail,
                &format!("Decode failed: {error}"),
            );
            None
        }
    };

    step(logger, phase_offset, 6, "Verify Dolby Vision metadata sync");
    match (rpu_frames, video_cuts.as_deref()) {
        (Some(_), Some(video_cuts)) => match export_dv_scene_cuts(&rpu, &rpu_scenes, rt, logger) {
            Ok(rpu_cuts) => {
                let fps = fps_from_info(&info).unwrap_or(23.976);
                let max_offset = (fps * 300.0).round() as i64;
                let sync_report = correlate_scene_cuts(&rpu_cuts, video_cuts, max_offset, 1);
                logger.measurement("metadata_sync", serde_json::json!({"accepted_offset_frames":sync_report.accepted.as_ref().map(|s|s.offset),"source":"RPU flags versus decoded scene cuts; not donor picture alignment"}));
                let evidence = assess_temporal_alignment(
                    &rpu_cuts,
                    video_cuts,
                    rpu_frames.unwrap(),
                    info.frame_count,
                    sync_report.accepted.map(|sync| sync.offset).unwrap_or(0),
                    max_offset,
                    1,
                );
                logger.measurement(
                    "temporal_alignment",
                    serde_json::to_value(&evidence).unwrap(),
                );
                let local_conflict = !evidence.contradictions.is_empty();
                let (status, detail) = if local_conflict {
                    (CheckStatus::Fail, format!("Local scene offsets disagree: a single offset cannot repair this timeline. Inspect decoded pictures in these intervals: {:?}", evidence.contradictions))
                } else {
                    let (status, detail) = checker_sync_result(&sync_report);
                    if let Some(limit) = &evidence.analysis_limit {
                        let status = if matches!(status, CheckStatus::Fail) {
                            status
                        } else {
                            CheckStatus::Warn
                        };
                        (
                            status,
                            format!("{detail}. Temporal inspection incomplete: {limit}"),
                        )
                    } else {
                        (status, detail)
                    }
                };
                let fixable_offset = sync_report
                    .accepted
                    .map(|sync| sync.offset)
                    .filter(|offset| *offset != 0);
                if evidence.analysis_limit.is_none()
                    && !local_conflict
                    && detected_profile == Some(8)
                    && hdr10_repair_safe
                    && rpu_frames == Some(info.frame_count)
                    && info.frame_count > 0
                    && logger.check_failure_count() == initial_failures
                {
                    if let Some(offset) = fixable_offset {
                        report_fixable(
                            logger,
                            "sync",
                            "Metadata sync",
                            status,
                            &detail,
                            "sync-offset",
                            offset,
                        );
                    } else {
                        report(logger, "sync", "Metadata sync", status, &detail);
                    }
                } else {
                    report(logger, "sync", "Metadata sync", status, &detail);
                }
            }
            Err(error) => report(
                logger,
                "sync",
                "Metadata sync",
                CheckStatus::Fail,
                &format!("Could not parse RPU scene changes: {error}"),
            ),
        },
        (None, Some(_)) => report(
            logger,
            "sync",
            "Metadata sync",
            CheckStatus::Skipped,
            "Not run because the RPU could not be parsed",
        ),
        (Some(_), None) => report(
            logger,
            "sync",
            "Metadata sync",
            CheckStatus::Skipped,
            "Not run because the video could not be fully decoded",
        ),
        (None, None) => report(
            logger,
            "sync",
            "Metadata sync",
            CheckStatus::Skipped,
            "Not run because neither RPU scene changes nor video scene changes were available",
        ),
    }

    let failures = logger.check_failure_count() - initial_failures;
    if failures > 0 {
        return Err(format!(
            "Checker found {failures} failed check(s); see the complete report"
        ));
    }
    logger.completed(&file.to_path_buf());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::scenes::SyncResult;

    fn accepted_report(offset: i64) -> CorrelationReport {
        CorrelationReport {
            accepted: Some(SyncResult {
                offset,
                matches: 100,
                match_ratio: 0.8,
                dominance: 10.0,
                tercile_offsets: [Some(offset); 3],
            }),
            rejection: None,
            top_offsets: vec![(offset, 100)],
            dv_count: 125,
            hdr_count: 120,
        }
    }

    #[test]
    fn checker_sync_accepts_zero_offset() {
        let (status, detail) = checker_sync_result(&accepted_report(0));
        assert_eq!(status, CheckStatus::Pass);
        assert!(detail.contains("offset 0"));
    }

    #[test]
    fn checker_sync_warns_for_one_frame_in_either_direction() {
        for offset in [-1, 1] {
            let (status, detail) = checker_sync_result(&accepted_report(offset));
            assert_eq!(status, CheckStatus::Warn);
            assert!(detail.contains(&format!("offset {offset:+}")));
        }
    }

    #[test]
    fn checker_sync_fails_for_larger_offset() {
        let (status, detail) = checker_sync_result(&accepted_report(-2));
        assert_eq!(status, CheckStatus::Fail);
        assert!(detail.contains("offset -2"));
    }

    #[test]
    fn checker_sync_warns_when_correlation_is_unverifiable() {
        let report = CorrelationReport {
            accepted: None,
            rejection: Some("not enough scene changes".to_string()),
            top_offsets: Vec::new(),
            dv_count: 2,
            hdr_count: 1,
        };
        let (status, detail) = checker_sync_result(&report);
        assert_eq!(status, CheckStatus::Warn);
        assert!(detail.contains("not enough scene changes"));
    }
}
