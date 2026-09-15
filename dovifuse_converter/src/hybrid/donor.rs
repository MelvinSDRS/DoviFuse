//! Cross-check the extracted metadata against the eligible media input.
//! Profile 8's compatibility variant is established by media preflight, not
//! by the coarse RPU profile number (which is also 8 for HLG donors).
use std::path::Path;

use crate::exec::{run_capture, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;

pub(crate) fn validate_rpu(
    rpu: &Path,
    profile: Option<u8>,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<u64> {
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
    let frames = validate_summary(&summary, profile)?;
    logger.ok("Extracted donor RPU profile agrees with eligible source across all metadata frames");
    Ok(frames)
}

fn validate_summary(summary: &str, expected: Option<u8>) -> AppResult<u64> {
    // Upstream's summary aggregates *all* RPUs. Do not inspect only frame 0:
    // a later unsupported profile must not be silently converted by mode 2.
    let allowed: &[&str] = match expected {
        Some(7) => &["7 (MEL)", "7 (FEL)", "7 (FEL, MEL)"],
        Some(8) => &["8"],
        _ => return Err("Donor RPU requires an eligible Profile 7 or 8.1 source".into()),
    };
    let mut frames = None;
    let mut profile = None;
    for line in summary.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Frames:") {
            if frames.is_some() {
                return Err("Ambiguous donor RPU frame count".into());
            }
            frames = Some(
                value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "Invalid donor RPU frame count")?,
            );
        }
        if let Some(value) = line
            .strip_prefix("Profile:")
            .or_else(|| line.strip_prefix("Profiles:"))
        {
            if profile.is_some() {
                return Err("Ambiguous donor RPU profile summary".into());
            }
            profile = Some(value.trim());
        }
    }
    if !profile.is_some_and(|p| allowed.contains(&p)) {
        return Err(format!(
            "Donor RPU profile is missing, unsupported, mixed, or contradicts media profile {:?}: {}",
            expected, profile.unwrap_or("unknown")
        ));
    }
    frames
        .filter(|n| *n > 0)
        .ok_or_else(|| "Donor RPU has no nonzero frame count".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_profiles_and_fel_mel_provenance() {
        for (profile, value) in [
            (7, "7 (MEL)"),
            (7, "7 (FEL)"),
            (7, "7 (FEL, MEL)"),
            (8, "8"),
        ] {
            assert_eq!(
                validate_summary(
                    &format!("Summary:\n  Frames: 120\n  Profile: {value}\n"),
                    Some(profile)
                ),
                Ok(120)
            );
        }
    }

    #[test]
    fn rejects_unknown_mixed_mislabeled_or_malformed_metadata() {
        for summary in [
            "Frames: 120\nProfile: 5",
            "Frames: 120\nProfiles: 5, 8",
            "Frames: 120\nProfiles: 7 (FEL), 8",
            "Frames: 120\nProfile: 7 (FEL)",
            "Frames: 120",
            "Profile: 8",
            "Frames: 0\nProfile: 8",
            "Frames: invalid\nProfile: 8",
            "Frames: 120\nProfile: 8\nProfile: 5",
            "Frames: 120\nFrames: 121\nProfile: 8",
            "Frames: 120\nProfile: 8.4",
        ] {
            assert!(validate_summary(summary, Some(8)).is_err(), "{summary}");
        }
        for profile in [None, Some(5), Some(9), Some(7)] {
            assert!(validate_summary("Frames: 120\nProfile: 8", profile).is_err());
        }
    }
}
