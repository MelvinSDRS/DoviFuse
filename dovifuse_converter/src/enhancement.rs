//! Inspect the complete source RPU before discarding a Profile 7 enhancement layer.
use std::path::Path;

use crate::exec::{run_capture, run_status, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;

#[derive(Debug, PartialEq, Eq)]
enum Enhancement {
    Mel,
    Fel,
    Mixed,
}

fn parse_summary(summary: &str, expected_frames: u64) -> AppResult<Enhancement> {
    let frames: Vec<_> = summary
        .lines()
        .filter_map(|s| s.trim().strip_prefix("Frames:"))
        .collect();
    if expected_frames == 0
        || frames.len() != 1
        || frames[0].trim().parse::<u64>().ok() != Some(expected_frames)
    {
        return Err("Source RPU count is missing or differs from the video frame count".into());
    }
    // Upstream RpusListSummary aggregates *all* RPU profiles and EL types.
    // Mixed profiles, unknown types and format drift must not masquerade as MEL.
    let profiles: Vec<_> = summary
        .lines()
        .map(str::trim)
        .filter(|s| s.starts_with("Profile:") || s.starts_with("Profiles:"))
        .collect();
    match profiles.as_slice() {
        ["Profile: 7 (MEL)"] => Ok(Enhancement::Mel),
        ["Profile: 7 (FEL)"] => Ok(Enhancement::Fel),
        ["Profile: 7 (FEL, MEL)"] | ["Profile: 7 (MEL, FEL)"] => Ok(Enhancement::Mixed),
        _ => {
            Err("Could not establish a complete Profile 7 MEL/FEL source; original retained".into())
        }
    }
}

pub(crate) fn inspect(
    hevc: &Path,
    rpu: &Path,
    expected_frames: u64,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    run_status(
        logger,
        false,
        true,
        &rt.dovi_tool,
        &[
            "extract-rpu".into(),
            "-i".into(),
            hevc.as_os_str().to_owned(),
            "-o".into(),
            rpu.as_os_str().to_owned(),
        ],
    )?;
    let summary = run_capture(
        logger,
        &rt.dovi_tool,
        &[
            "info".into(),
            "-i".into(),
            rpu.as_os_str().to_owned(),
            "--summary".into(),
        ],
    )?;
    let kind = parse_summary(&summary, expected_frames)?;
    let (name, detail) = match kind {
        Enhancement::Mel => ("MEL", "MEL source: conversion keeps the HDR10 base pictures and converted RPU, and discards the minimal enhancement layer. This is not a bit-exact copy of the source."),
        Enhancement::Fel => ("FEL", "FEL source: conversion discards the enhancement-layer picture residual. FEL reconstruction is not preserved; the result uses the HDR10 base pictures and converted RPU."),
        Enhancement::Mixed => ("MEL/FEL", "Mixed MEL/FEL source: conversion discards the enhancement layer, including FEL picture residuals. FEL reconstruction is not preserved."),
    };
    logger.measurement("source_enhancement_layer", serde_json::json!({
        "type": name, "rpu_frames": expected_frames, "coverage": "all source RPUs",
        "source": hevc, "fel_reconstruction_preserved": false,
        "archive_enabled": rt.save_el_rpu, "archive_directory": rt.save_el_rpu.then_some(&rt.output_dir),
    }));
    logger.check_result(
        "enhancement_layer",
        "Profile 7 enhancement layer",
        "warn",
        detail,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_summary_distinguishes_mel_fel_and_mixed() {
        for (profile, expected) in [
            ("7 (MEL)", Enhancement::Mel),
            ("7 (FEL)", Enhancement::Fel),
            ("7 (FEL, MEL)", Enhancement::Mixed),
        ] {
            assert_eq!(
                parse_summary(
                    &format!("Summary:\n  Frames: 259\n  Profile: {profile}\n"),
                    259
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn unknown_mixed_profiles_and_incomplete_sources_are_rejected() {
        for summary in [
            "Frames: 259\nProfile: 7",
            "Frames: 259\nProfiles: 7 (MEL), 8",
            "Frames: 258\nProfile: 7 (MEL)",
            "Frames: 259\nProfile: 7 (future)",
            "Frames: 259\nProfile: 7 (MEL)\nProfile: 7 (FEL)",
        ] {
            assert!(parse_summary(summary, 259).is_err(), "{summary}");
        }
    }
}
