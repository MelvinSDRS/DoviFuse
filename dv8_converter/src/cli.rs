use std::env;
use std::path::PathBuf;

use crate::exec::AppResult;

pub(crate) struct CliArgs {
    pub(crate) save_el_rpu: bool,
    pub(crate) archive_dir: Option<PathBuf>,
    pub(crate) debug: bool,
    pub(crate) dry_run: bool,
    pub(crate) hybrid_mode: bool,
    pub(crate) checker_mode: bool,
    pub(crate) report: Option<PathBuf>,
    pub(crate) repair_sync_offset: Option<i64>,
    pub(crate) custom_output: Option<PathBuf>,
    pub(crate) input_path: Option<PathBuf>,
    pub(crate) dv_source: Option<PathBuf>,
    pub(crate) hdr_target: Option<PathBuf>,
    pub(crate) hybrid: HybridOptions,
    pub(crate) tmp_dir: Option<PathBuf>,
    pub(crate) hwaccel: HwAccelMode,
    pub(crate) progress: ProgressMode,
    pub(crate) original_args: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HwAccelMode {
    Auto,
    VideoToolbox,
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ProgressMode {
    Human,
    JsonLines,
}

fn parse_hwaccel(value: &str) -> AppResult<HwAccelMode> {
    match value {
        "auto" => Ok(HwAccelMode::Auto),
        "videotoolbox" => Ok(HwAccelMode::VideoToolbox),
        "off" => Ok(HwAccelMode::Off),
        other => Err(format!(
            "Invalid --hwaccel mode '{other}' (expected auto|videotoolbox|off)"
        )),
    }
}

fn parse_progress(value: &str) -> AppResult<ProgressMode> {
    match value {
        "human" => Ok(ProgressMode::Human),
        "jsonl" => Ok(ProgressMode::JsonLines),
        other => Err(format!(
            "Invalid --progress mode '{other}' (expected human|jsonl)"
        )),
    }
}

fn parse_repair_offset(value: &str) -> AppResult<i64> {
    let offset = value.parse::<i64>().map_err(|_| {
        format!("Invalid --repair-sync offset '{value}' (expected non-zero frames)")
    })?;
    if offset == 0 || offset.unsigned_abs() > 1_000_000 {
        return Err("--repair-sync offset must be non-zero and at most 1000000 frames".to_string());
    }
    Ok(offset)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GradeCheckMode {
    /// Static metadata comparison only (no pixel measurement).
    Metadata,
    /// Measure brightness over sampled windows (default).
    Sampled,
    /// Measure brightness over the entire runtime.
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LetterboxMode {
    /// cropdetect the HDR target and set L5 from the measurement (default).
    Measured,
    /// Legacy: derive L5 from the resolution difference between sources.
    Resolution,
    /// Leave the RPU's L5 metadata untouched.
    Off,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SyncMode {
    /// Scene-cut correlation between the DV RPU and the HDR target (default).
    Scenes,
    /// Legacy frame-count-difference heuristic.
    Framecount,
}

/// Options that only apply to --hybrid runs.
#[derive(Clone)]
pub(crate) struct HybridOptions {
    pub(crate) delete_sources: bool,
    pub(crate) sync: SyncMode,
    pub(crate) force: bool,
    pub(crate) scene_threshold: f64,
    pub(crate) max_offset: Option<u64>,
    pub(crate) grade_check: GradeCheckMode,
    pub(crate) skip_grade_check: bool,
    pub(crate) grade_windows: usize,
    pub(crate) letterbox: LetterboxMode,
    /// Trusted offset measured by a completed checker run. Repair mode still
    /// scans scene cuts so the corrected output can be verified afterward.
    pub(crate) known_offset: Option<i64>,
}

impl Default for HybridOptions {
    fn default() -> Self {
        Self {
            delete_sources: false,
            sync: SyncMode::Scenes,
            force: false,
            scene_threshold: 8.0,
            max_offset: None,
            grade_check: GradeCheckMode::Sampled,
            skip_grade_check: false,
            grade_windows: 6,
            letterbox: LetterboxMode::Measured,
            known_offset: None,
        }
    }
}

pub(crate) fn usage() {
    println!(
        "Usage: DV7toDV8.sh [OPTIONS] <file.mkv|directory>\n\
\n\
Convert Dolby Vision Profile 7 MKV files to Profile 8.\n\
\n\
Options:\n\
  -n          Do NOT save the DV7 EL+RPU file (default: archived to NAS)\n\
  --archive-dir <path>  Standard-mode EL+RPU destination (overrides DV8_EL_RPU_DIR)\n\
  -d, --debug Enable debug logging (verbose + command logging)\n\
  --dry-run   Show what would be done without modifying any files\n\
  --tmp-dir <path>  Store large intermediate files in this directory\n\
  --hwaccel <mode>  Decode acceleration: auto, videotoolbox, off\n\
  --progress <mode> Progress output: human (default) or jsonl\n\
  --report <path>  New JSON job report\n\
  --check     Read-only validation of an existing DV8 MKV\n\
  --repair-sync <frames>  Create and verify a repaired copy using an offset\n\
                    reported by --check (the original is never modified)\n\
  --hybrid    Hybrid mode: inject DV metadata from one file into another\n\
  -o <path>   Custom output path for hybrid or repair mode\n\
  -h, --help  Show this help message\n\
\n\
Hybrid-only options:\n\
  --delete-sources  Request input deletion (withheld while active-area coverage is unverified)\n\
                    (default: keep both originals)\n\
  --sync <mode>     Alignment mode: scenes (scene-cut correlation, default)\n\
                    or framecount (legacy frame-count heuristic)\n\
  --force           Fall back to the framecount heuristic when scene-cut\n\
                    correlation fails instead of aborting\n\
  --scene-threshold <f>  scdet scene-change threshold (default: 8.0)\n\
  --max-offset <n>  Max frame offset searched during correlation\n\
                    (default: 5 minutes worth of frames)\n\
  --grade-check <mode>   Brightness grade comparison: metadata (static\n\
                    only), sampled (measure windows, default), full\n\
                    (measure the entire runtime)\n\
  --skip-grade-check     Skip the grade gate entirely (mismatched grades\n\
                    will NOT abort the conversion)\n\
  --grade-windows <n>    Sample windows for the sampled grade check\n\
                    (default: 6)\n\
  --letterbox <mode>     L5 active-area handling: measured (cropdetect the\n\
                    HDR target, default), resolution (derive from the\n\
                    resolution difference), off (keep RPU L5 as-is)\n\
\n\
Examples:\n\
  DV7toDV8.sh /path/to/movie.mkv\n\
  DV7toDV8.sh /path/to/folder/\n\
  DV7toDV8.sh -n /path/to/movie.mkv\n\
  DV7toDV8.sh --check /path/to/downloaded.dv8.mkv\n\
\n\
Hybrid mode (inject DV metadata from one file into another):\n\
  DV7toDV8.sh --hybrid <dv_source.mkv> <hdr_target.mkv>\n\
  DV7toDV8.sh --hybrid -o output.mkv <dv_source.mkv> <hdr_target.mkv>"
    );
}

pub(crate) fn parse_args() -> AppResult<CliArgs> {
    parse_args_from(env::args().skip(1).collect())
}

fn parse_args_from(original_args: Vec<String>) -> AppResult<CliArgs> {
    let mut save_el_rpu = true;
    let mut archive_dir = None;
    let mut debug = false;
    let mut dry_run = false;
    let mut hybrid_mode = false;
    let mut checker_mode = false;
    let mut report = None;
    let mut repair_sync_offset: Option<i64> = None;
    let mut custom_output: Option<PathBuf> = None;
    let mut tmp_dir = env::var_os("DV8_TMP_DIR").map(PathBuf::from);
    let mut hwaccel = HwAccelMode::Auto;
    let mut progress = ProgressMode::Human;
    let mut hybrid = HybridOptions::default();
    let mut hybrid_only_flags: Vec<String> = Vec::new();

    let mut positional: Vec<String> = Vec::new();
    let args: Vec<String> = std::iter::once("dv8_converter".to_string())
        .chain(original_args.iter().cloned())
        .collect();

    let mut i = 1usize;
    let mut parse_flags = true;

    while i < args.len() {
        let arg = &args[i];

        if parse_flags && arg.starts_with('-') {
            match arg.as_str() {
                "-n" => save_el_rpu = false,
                "--archive-dir" => {
                    i += 1;
                    let value = args
                        .get(i)
                        .filter(|v| !v.is_empty())
                        .ok_or("Missing value for --archive-dir")?;
                    archive_dir = Some(PathBuf::from(value));
                }
                "-d" | "--debug" => debug = true,
                "--dry-run" => dry_run = true,
                "--tmp-dir" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --tmp-dir".to_string());
                    }
                    tmp_dir = Some(PathBuf::from(&args[i]));
                }
                "--hwaccel" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --hwaccel".to_string());
                    }
                    hwaccel = parse_hwaccel(&args[i])?;
                }
                "--progress" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --progress".to_string());
                    }
                    progress = parse_progress(&args[i])?;
                }
                "--hybrid" => hybrid_mode = true,
                "--check" => checker_mode = true,
                "--report" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --report".to_string());
                    }
                    report = Some(PathBuf::from(&args[i]));
                }
                "--repair-sync" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --repair-sync".to_string());
                    }
                    repair_sync_offset = Some(parse_repair_offset(&args[i])?);
                }
                "--delete-sources" => {
                    hybrid.delete_sources = true;
                    hybrid_only_flags.push(arg.clone());
                }
                "--force" => {
                    hybrid.force = true;
                    hybrid_only_flags.push(arg.clone());
                }
                "--sync" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --sync".to_string());
                    }
                    hybrid.sync = match args[i].as_str() {
                        "scenes" => SyncMode::Scenes,
                        "framecount" => SyncMode::Framecount,
                        other => {
                            return Err(format!(
                                "Invalid --sync mode '{other}' (expected scenes|framecount)"
                            ))
                        }
                    };
                    hybrid_only_flags.push(arg.clone());
                }
                "--scene-threshold" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --scene-threshold".to_string());
                    }
                    hybrid.scene_threshold = args[i]
                        .parse::<f64>()
                        .ok()
                        .filter(|v| *v > 0.0 && *v <= 100.0)
                        .ok_or_else(|| {
                            format!("Invalid --scene-threshold '{}' (expected 0-100)", args[i])
                        })?;
                    hybrid_only_flags.push(arg.clone());
                }
                "--max-offset" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --max-offset".to_string());
                    }
                    hybrid.max_offset = Some(
                        args[i]
                            .parse::<u64>()
                            .ok()
                            .filter(|v| *v <= 1_000_000)
                            .ok_or_else(|| "--max-offset must be 0..1000000 frames".to_string())?,
                    );
                    hybrid_only_flags.push(arg.clone());
                }
                "--grade-check" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --grade-check".to_string());
                    }
                    hybrid.grade_check = match args[i].as_str() {
                        "metadata" => GradeCheckMode::Metadata,
                        "sampled" => GradeCheckMode::Sampled,
                        "full" => GradeCheckMode::Full,
                        other => {
                            return Err(format!(
                            "Invalid --grade-check mode '{other}' (expected metadata|sampled|full)"
                        ))
                        }
                    };
                    hybrid_only_flags.push(arg.clone());
                }
                "--letterbox" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --letterbox".to_string());
                    }
                    hybrid.letterbox = match args[i].as_str() {
                        "measured" => LetterboxMode::Measured,
                        "resolution" => LetterboxMode::Resolution,
                        "off" => LetterboxMode::Off,
                        other => {
                            return Err(format!(
                            "Invalid --letterbox mode '{other}' (expected measured|resolution|off)"
                        ))
                        }
                    };
                    hybrid_only_flags.push(arg.clone());
                }
                "--skip-grade-check" => {
                    hybrid.skip_grade_check = true;
                    hybrid_only_flags.push(arg.clone());
                }
                "--grade-windows" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for --grade-windows".to_string());
                    }
                    hybrid.grade_windows = args[i]
                        .parse::<usize>()
                        .ok()
                        .filter(|v| (1..=32).contains(v))
                        .ok_or_else(|| {
                            format!("Invalid --grade-windows '{}' (expected 1-32)", args[i])
                        })?;
                    hybrid_only_flags.push(arg.clone());
                }
                "-o" => {
                    i += 1;
                    if i >= args.len() {
                        return Err("Missing value for -o".to_string());
                    }
                    custom_output = Some(PathBuf::from(&args[i]));
                }
                "-h" | "--help" => {
                    usage();
                    std::process::exit(0);
                }
                "--" => parse_flags = false,
                _ => return Err(format!("Unknown flag {arg}")),
            }
        } else {
            positional.push(arg.clone());
        }

        i += 1;
    }

    let selected_modes = usize::from(hybrid_mode)
        + usize::from(checker_mode)
        + usize::from(repair_sync_offset.is_some());
    if selected_modes > 1 {
        return Err("--hybrid, --check, and --repair-sync are mutually exclusive".to_string());
    }

    if archive_dir.is_some() && (!save_el_rpu || selected_modes != 0) {
        return Err(
            "--archive-dir requires standard conversion with archiving enabled (no -n)".into(),
        );
    }

    if hybrid_mode {
        if positional.len() != 2 {
            return Err(
                "Hybrid mode requires exactly 2 positional args: <dv_source.mkv> <hdr_target.mkv>"
                    .to_string(),
            );
        }
    } else {
        if custom_output.is_some() && repair_sync_offset.is_none() {
            return Err("-o is only valid with --hybrid or --repair-sync".to_string());
        }
        if let Some(flag) = hybrid_only_flags.first() {
            return Err(format!("{flag} is only valid with --hybrid"));
        }
        if positional.len() != 1 {
            return Err("No file/folder specified".to_string());
        }
    }

    let (input_path, dv_source, hdr_target) = if hybrid_mode {
        (
            None,
            Some(PathBuf::from(&positional[0])),
            Some(PathBuf::from(&positional[1])),
        )
    } else {
        (Some(PathBuf::from(&positional[0])), None, None)
    };

    Ok(CliArgs {
        save_el_rpu,
        archive_dir,
        debug,
        dry_run,
        hybrid_mode,
        checker_mode,
        report,
        repair_sync_offset,
        custom_output,
        input_path,
        dv_source,
        hdr_target,
        hybrid,
        tmp_dir,
        hwaccel,
        progress,
        original_args,
    })
}

impl CliArgs {
    pub(crate) fn operation(&self) -> &'static str {
        if self.repair_sync_offset.is_some() {
            "repair"
        } else if self.checker_mode {
            "checker"
        } else if self.hybrid_mode {
            "hybrid"
        } else {
            "standard"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experimental_p5_commands_are_not_available_on_main() {
        for flag in ["--analyze-pair", "--probe-color-backend"] {
            assert!(parse_args_from(vec![flag.into(), "in.mkv".into()]).is_err());
        }
    }

    #[test]
    fn parses_new_modes() {
        assert_eq!(parse_hwaccel("auto").unwrap(), HwAccelMode::Auto);
        assert_eq!(
            parse_hwaccel("videotoolbox").unwrap(),
            HwAccelMode::VideoToolbox
        );
        assert_eq!(parse_progress("jsonl").unwrap(), ProgressMode::JsonLines);
        assert_eq!(parse_repair_offset("-1").unwrap(), -1);
        assert_eq!(parse_repair_offset("12").unwrap(), 12);
        assert!(parse_repair_offset("0").is_err());
        assert!(parse_repair_offset("nope").is_err());
        assert!(parse_hwaccel("cuda").is_err());
        assert!(parse_progress("xml").is_err());
    }

    #[test]
    fn archive_destination_is_explicit_and_standard_only() {
        fn parse(args: &[&str]) -> AppResult<CliArgs> {
            parse_args_from(args.iter().map(|s| s.to_string()).collect())
        }
        let cli = parse(&["--archive-dir", "/tmp/EL archive", "movie.mkv"]).unwrap();
        assert!(cli.save_el_rpu);
        assert_eq!(cli.archive_dir, Some(PathBuf::from("/tmp/EL archive")));
        assert!(parse(&["movie.mkv"]).unwrap().save_el_rpu);
        assert!(!parse(&["-n", "movie.mkv"]).unwrap().save_el_rpu);
        for args in [
            vec!["--archive-dir"],
            vec!["--archive-dir", "", "movie.mkv"],
            vec!["-n", "--archive-dir", "/tmp/archive", "movie.mkv"],
            vec!["--check", "--archive-dir", "/tmp/archive", "movie.mkv"],
            vec![
                "--hybrid",
                "--archive-dir",
                "/tmp/archive",
                "dv.mkv",
                "hdr.mkv",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
