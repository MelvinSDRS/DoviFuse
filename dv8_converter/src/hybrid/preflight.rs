use crate::cli::HybridOptions;
use crate::logger::Logger;
use crate::mediainfo::{fps_from_info, HybridMediaInfo};

use super::grade::{static_grade_verdict, Verdict};

pub(crate) fn hybrid_preflight_checks(
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    dv_profile: Option<u8>,
    opts: &HybridOptions,
    logger: &Logger,
) -> bool {
    logger.step("Preflight checks");

    let mut has_fail = false;

    let dv_hdr_lower = dv_info.hdr_format.to_lowercase();
    let dv_codec_lower = format!("{} {}", dv_info.codec, dv_info.codec_id).to_lowercase();
    let hdr_codec_lower = format!("{} {}", hdr_info.codec, hdr_info.codec_id).to_lowercase();

    let dv_has_dv = dv_hdr_lower.contains("dolby")
        || dv_hdr_lower.contains("vision")
        || dv_info.codec_id.to_lowercase().contains("dvhe")
        || dv_profile.is_some();
    if dv_has_dv {
        logger.preflight_status("PASS", "1. DV source has Dolby Vision");
    } else {
        logger.preflight_status("FAIL", "1. DV source has Dolby Vision");
        has_fail = true;
    }

    let hdr_is_av1 = hdr_codec_lower.contains("av1");
    let hdr_is_hevc = hdr_codec_lower.contains("hevc")
        || hdr_codec_lower.contains("h.265")
        || hdr_codec_lower.contains("h265")
        || hdr_codec_lower.contains("hev1");
    if hdr_is_av1 || !hdr_is_hevc {
        logger.preflight_status("FAIL", "2. HDR target codec is HEVC (not AV1)");
        has_fail = true;
    } else {
        logger.preflight_status("PASS", "2. HDR target codec is HEVC (not AV1)");
    }

    let dv_is_hevc = dv_codec_lower.contains("hevc")
        || dv_codec_lower.contains("h.265")
        || dv_codec_lower.contains("h265")
        || dv_codec_lower.contains("hev1")
        || dv_codec_lower.contains("dvhe");
    if dv_is_hevc {
        logger.preflight_status("PASS", "3. DV source codec is HEVC");
    } else {
        logger.preflight_status("FAIL", "3. DV source codec is HEVC");
        has_fail = true;
    }

    let dv_vfr = dv_info.frame_rate_mode.to_lowercase().contains("variable")
        || dv_info.frame_rate_mode.to_lowercase().contains("vfr");
    let hdr_vfr = hdr_info.frame_rate_mode.to_lowercase().contains("variable")
        || hdr_info.frame_rate_mode.to_lowercase().contains("vfr");

    if dv_vfr || hdr_vfr {
        logger.preflight_status("FAIL", "4. Neither file is VFR");
        has_fail = true;
    } else {
        logger.preflight_status("PASS", "4. Neither file is VFR");
    }

    match (fps_from_info(dv_info), fps_from_info(hdr_info)) {
        (Some(dv_fps), Some(hdr_fps)) => {
            let diff = (dv_fps - hdr_fps).abs();
            if diff > 0.5 {
                logger.preflight_status(
                    "FAIL",
                    &format!("5. Frame rate match (diff {:.3} fps > 0.5 fps)", diff),
                );
                has_fail = true;
            } else if diff > 0.01 {
                logger.preflight_status(
                    "WARN",
                    &format!(
                        "5. Frame rate slight mismatch (diff {:.3} fps > 0.01 fps)",
                        diff
                    ),
                );
            } else {
                logger.preflight_status("PASS", "5. Frame rate match");
            }
        }
        _ => {
            logger.preflight_status("FAIL", "5. Frame rate match (missing metadata)");
            has_fail = true;
        }
    }

    match (dv_info.duration_ms, hdr_info.duration_ms) {
        (Some(dv_ms), Some(hdr_ms)) => {
            let diff_s = (dv_ms - hdr_ms).abs() / 1000.0;
            if diff_s > 300.0 {
                logger.preflight_status(
                    "FAIL",
                    &format!("6. Duration sanity (diff {:.2}s > 300s)", diff_s),
                );
                has_fail = true;
            } else if diff_s > 2.0 {
                logger.preflight_status(
                    "WARN",
                    &format!("6. Duration differs (diff {:.2}s > 2s)", diff_s),
                );
            } else {
                logger.preflight_status("PASS", "6. Duration sanity");
            }
        }
        _ => logger.preflight_status(
            "WARN",
            "6. Duration sanity could not be verified (missing metadata)",
        ),
    }

    if dv_info.frame_count > 0 && hdr_info.frame_count > 0 {
        logger.preflight_status("PASS", "7. Frame counts are available");
    } else {
        logger.preflight_status("FAIL", "7. Frame counts are available (non-zero)");
        has_fail = true;
    }

    match (
        dv_info.width,
        dv_info.height,
        hdr_info.width,
        hdr_info.height,
    ) {
        (Some(dw), Some(dh), Some(hw), Some(hh)) if dw == hw && dh == hh => {
            logger.preflight_status("PASS", "8. Resolution match (no L5 adjustment needed)");
        }
        (Some(dw), Some(dh), Some(hw), Some(hh)) => {
            let horiz = hw.abs_diff(dw) / 2;
            let vert = hh.abs_diff(dh) / 2;
            logger.preflight_status(
                "WARN",
                &format!(
                    "8. Resolution differs - L5 active area auto-adjust (left/right: {}px, top/bottom: {}px)",
                    horiz, vert
                ),
            );
        }
        _ => logger.preflight_status(
            "WARN",
            "8. Resolution could not be compared (missing metadata)",
        ),
    }

    match dv_profile {
        Some(5) => logger.preflight_status(
            "WARN",
            "9. DV Profile 5 source detected - mode 3 will be used",
        ),
        Some(p) => logger.preflight_status("PASS", &format!("9. DV profile check (detected: {p})")),
        // An undetected profile would silently get mode 2; if the source is
        // actually P5 that skips the IPT-PQ-c2 conversion and produces a
        // broken P8-labelled output.
        None if opts.force => logger.preflight_status(
            "WARN",
            "9. DV profile could not be detected - assuming P7/P8 (editor mode 2) due to --force",
        ),
        None => {
            logger.preflight_status(
                "FAIL",
                "9. DV profile could not be detected - a P5 source would be converted incorrectly (re-run with --force to assume P7/P8)",
            );
            has_fail = true;
        }
    }

    let hdr_meta_lower = hdr_info.hdr_format.to_lowercase();
    let hdr_has_hdr = hdr_meta_lower.contains("hdr")
        || hdr_meta_lower.contains("2086")
        || hdr_meta_lower.contains("pq")
        || hdr_info.max_cll.is_some()
        || hdr_info.max_fall.is_some();

    if hdr_has_hdr {
        logger.preflight_status("PASS", "10. HDR target has HDR metadata");
    } else {
        logger.preflight_status("WARN", "10. HDR target appears to have no HDR metadata");
    }

    match (dv_info.bit_depth, hdr_info.bit_depth) {
        (Some(d), Some(h)) if d == h => {
            logger.preflight_status("PASS", "11. Bit depth match");
        }
        _ => logger.preflight_status("WARN", "11. Bit depth differs"),
    }

    let primaries = hdr_info.colour_primaries.to_lowercase();
    if primaries.contains("2020") {
        logger.preflight_status("PASS", "12. Color primaries BT.2020");
    } else {
        logger.preflight_status("WARN", "12. Color primaries are not BT.2020");
    }

    let transfer = hdr_info.transfer_characteristics.to_lowercase();
    if transfer.contains("2084") || transfer.contains("pq") {
        logger.preflight_status("PASS", "13. Transfer characteristics PQ");
    } else {
        logger.preflight_status("WARN", "13. Transfer characteristics are not PQ");
    }

    let (verdict, msg) =
        static_grade_verdict(dv_info, hdr_info, opts.grade_check, opts.skip_grade_check);
    match verdict {
        Verdict::Pass => logger.preflight_status("PASS", &format!("14. {msg}")),
        Verdict::Warn => logger.preflight_status("WARN", &format!("14. {msg}")),
        Verdict::Fail => {
            logger.preflight_status("FAIL", &format!("14. {msg}"));
            has_fail = true;
        }
    }

    if has_fail {
        logger.err("Preflight failed. Hybrid conversion aborted.");
    }

    has_fail
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn logger() -> Logger {
        Logger::new(PathBuf::from("/dev/null"), false)
    }

    /// A DV/HDR pair that passes every check (profile passed separately).
    fn good_pair() -> (HybridMediaInfo, HybridMediaInfo) {
        let dv = HybridMediaInfo {
            codec: "HEVC".to_string(),
            codec_id: "V_MPEGH/ISO/HEVC".to_string(),
            frame_count: 100_000,
            frame_rate: Some(23.976),
            duration_ms: Some(4_170_000.0),
            hdr_format: "Dolby Vision".to_string(),
            width: Some(3840),
            height: Some(2160),
            bit_depth: Some(10),
            colour_primaries: "BT.2020".to_string(),
            transfer_characteristics: "PQ".to_string(),
            frame_rate_mode: "Constant".to_string(),
            max_cll: Some(1000),
            max_fall: Some(400),
            mastering_min_nits: Some(0.005),
            mastering_max_nits: Some(1000.0),
            ..Default::default()
        };
        let hdr = HybridMediaInfo {
            hdr_format: "SMPTE ST 2086".to_string(),
            ..dv.clone()
        };
        (dv, hdr)
    }

    #[test]
    fn good_pair_with_known_profile_passes() {
        let (dv, hdr) = good_pair();
        let opts = HybridOptions::default();
        assert!(!hybrid_preflight_checks(&dv, &hdr, Some(8), &opts, &logger()));
        assert!(!hybrid_preflight_checks(&dv, &hdr, Some(7), &opts, &logger()));
        // P5 is a WARN (mode 3), not a failure.
        assert!(!hybrid_preflight_checks(&dv, &hdr, Some(5), &opts, &logger()));
    }

    #[test]
    fn unknown_profile_fails_without_force() {
        // dv_has_dv must not depend on the profile here, hence the explicit
        // "Dolby Vision" hdr_format in good_pair().
        let (dv, hdr) = good_pair();
        let mut opts = HybridOptions::default();
        assert!(hybrid_preflight_checks(&dv, &hdr, None, &opts, &logger()));
        opts.force = true;
        assert!(!hybrid_preflight_checks(&dv, &hdr, None, &opts, &logger()));
    }

    #[test]
    fn zero_frame_count_fails() {
        let (dv, mut hdr) = good_pair();
        hdr.frame_count = 0;
        let opts = HybridOptions::default();
        assert!(hybrid_preflight_checks(&dv, &hdr, Some(8), &opts, &logger()));
    }

    #[test]
    fn vfr_fails() {
        let (mut dv, hdr) = good_pair();
        dv.frame_rate_mode = "Variable".to_string();
        let opts = HybridOptions::default();
        assert!(hybrid_preflight_checks(&dv, &hdr, Some(8), &opts, &logger()));
    }
}
