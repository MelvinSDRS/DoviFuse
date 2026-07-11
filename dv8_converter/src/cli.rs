use std::env;
use std::path::PathBuf;

use crate::exec::AppResult;

pub(crate) struct CliArgs {
    pub(crate) save_el_rpu: bool,
    pub(crate) debug: bool,
    pub(crate) dry_run: bool,
    pub(crate) hybrid_mode: bool,
    pub(crate) custom_output: Option<PathBuf>,
    pub(crate) input_path: Option<PathBuf>,
    pub(crate) dv_source: Option<PathBuf>,
    pub(crate) hdr_target: Option<PathBuf>,
    pub(crate) hybrid: HybridOptions,
    pub(crate) original_args: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GradeCheckMode {
    /// Static metadata comparison only (no pixel measurement).
    Metadata,
    /// Measure brightness over sampled windows (default).
    Sampled,
    /// Measure brightness over the entire runtime.
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
  -d, --debug Enable debug logging (verbose + command logging)\n\
  --dry-run   Show what would be done without modifying any files\n\
  --hybrid    Hybrid mode: inject DV metadata from one file into another\n\
  -o <path>   Custom output path for hybrid mode\n\
  -h, --help  Show this help message\n\
\n\
Hybrid-only options:\n\
  --delete-sources  Delete both input files after successful validation\n\
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
\n\
Examples:\n\
  DV7toDV8.sh /path/to/movie.mkv\n\
  DV7toDV8.sh /path/to/folder/\n\
  DV7toDV8.sh -n /path/to/movie.mkv\n\
\n\
Hybrid mode (inject DV metadata from one file into another):\n\
  DV7toDV8.sh --hybrid <dv_source.mkv> <hdr_target.mkv>\n\
  DV7toDV8.sh --hybrid -o output.mkv <dv_source.mkv> <hdr_target.mkv>"
    );
}

pub(crate) fn parse_args() -> AppResult<CliArgs> {
    let original_args: Vec<String> = env::args().skip(1).collect();

    let mut save_el_rpu = true;
    let mut debug = false;
    let mut dry_run = false;
    let mut hybrid_mode = false;
    let mut custom_output: Option<PathBuf> = None;
    let mut hybrid = HybridOptions::default();
    let mut hybrid_only_flags: Vec<String> = Vec::new();

    let mut positional: Vec<String> = Vec::new();
    let args: Vec<String> = env::args().collect();

    let mut i = 1usize;
    let mut parse_flags = true;

    while i < args.len() {
        let arg = &args[i];

        if parse_flags && arg.starts_with('-') {
            match arg.as_str() {
                "-n" => save_el_rpu = false,
                "-d" | "--debug" => debug = true,
                "--dry-run" => dry_run = true,
                "--hybrid" => hybrid_mode = true,
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
                    hybrid.max_offset = Some(args[i].parse::<u64>().map_err(|_| {
                        format!("Invalid --max-offset '{}' (expected frames)", args[i])
                    })?);
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

    if hybrid_mode {
        if positional.len() != 2 {
            return Err(
                "Hybrid mode requires exactly 2 positional args: <dv_source.mkv> <hdr_target.mkv>"
                    .to_string(),
            );
        }
    } else {
        if custom_output.is_some() {
            return Err("-o is only valid with --hybrid".to_string());
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
        debug,
        dry_run,
        hybrid_mode,
        custom_output,
        input_path,
        dv_source,
        hdr_target,
        hybrid,
        original_args,
    })
}
