use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::cli::{CliArgs, HwAccelMode};
use crate::exec::AppResult;
use crate::logger::Logger;

pub(crate) struct Runtime {
    pub(crate) script_dir: PathBuf,
    pub(crate) output_dir: PathBuf,
    pub(crate) json_file: PathBuf,
    pub(crate) mkvextract: PathBuf,
    pub(crate) mkvmerge: PathBuf,
    pub(crate) mediainfo: PathBuf,
    pub(crate) dovi_tool: PathBuf,
    pub(crate) ffmpeg: Option<PathBuf>,
    pub(crate) ffprobe: Option<PathBuf>,
    pub(crate) tmp_dir: Option<PathBuf>,
    pub(crate) hwaccel: HwAccelMode,
    pub(crate) save_el_rpu: bool,
    pub(crate) dry_run: bool,
}

pub(crate) fn is_executable(path: &Path) -> bool {
    if !path.exists() || !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        if let Ok(meta) = fs::metadata(path) {
            let mode = meta.permissions().mode();
            return mode & 0o111 != 0;
        }
        false
    }

    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for p in env::split_paths(&path) {
        let candidate = p.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(crate) fn resolve_executable_tool(candidates: &[PathBuf]) -> Option<PathBuf> {
    resolve_executable_tool_with(candidates, "--version")
}

fn resolve_executable_tool_with(candidates: &[PathBuf], version_flag: &str) -> Option<PathBuf> {
    for c in candidates {
        if !is_executable(c) {
            continue;
        }

        if let Ok(status) = crate::exec::probe(c, version_flag) {
            if status.status.success() {
                return Some(c.clone());
            }
        }
    }

    None
}

pub(crate) fn resolve_script_dir() -> PathBuf {
    if let Ok(v) = env::var("DOVIFUSE_SCRIPT_DIR") {
        let p = PathBuf::from(v);
        if p.exists() {
            return p;
        }
    }

    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent.ends_with("tools") {
                if let Some(root) = parent.parent() {
                    return root.to_path_buf();
                }
            }
        }
    }

    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn resolve_required(script_dir: &Path, name: &str) -> AppResult<PathBuf> {
    let candidates: Vec<PathBuf> = [
        which(name).unwrap_or_default(),
        script_dir.join("tools").join(name),
    ]
    .into_iter()
    .filter(|p| !p.as_os_str().is_empty())
    .collect();

    resolve_executable_tool(&candidates).ok_or_else(|| format!("Missing tool: {name}"))
}

/// ffmpeg/ffprobe reject `--version` (exit 8); they use single-dash `-version`.
fn resolve_optional(script_dir: &Path, name: &str) -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = [
        which(name).unwrap_or_default(),
        script_dir.join("tools").join(name),
    ]
    .into_iter()
    .filter(|p| !p.as_os_str().is_empty())
    .collect();

    resolve_executable_tool_with(&candidates, "-version")
}

impl Runtime {
    /// All modes use ffmpeg for output or input validation.
    pub(crate) fn require_ffmpeg(&self) -> AppResult<&Path> {
        self.ffmpeg.as_deref().ok_or_else(||
            "Missing tool: ffmpeg (required for video validation; install ffmpeg or place it in tools/)".to_string()
        )
    }
}

pub(crate) fn build_runtime(cli: &CliArgs) -> AppResult<(Runtime, Logger)> {
    let script_dir = resolve_script_dir();
    let log_file = env::var("DOVIFUSE_PROCESSING_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| script_dir.join("processing_log.txt"));

    let output_dir = if let Some(path) = &cli.archive_dir {
        path.clone()
    } else if let Ok(v) = env::var("DOVIFUSE_EL_RPU_DIR") {
        PathBuf::from(v)
    } else if Path::new("/NAS").is_dir() {
        PathBuf::from("/NAS/EL_RPU/")
    } else {
        PathBuf::from("/media/NAS/EL_RPU/")
    };

    let json_file = script_dir.join("config/dovifuse.json");

    let mkvextract = resolve_required(&script_dir, "mkvextract")?;
    let mkvmerge = resolve_required(&script_dir, "mkvmerge")?;
    let mediainfo = resolve_required(&script_dir, "mediainfo")?;

    let mut dovi_candidates: Vec<PathBuf> = Vec::new();
    dovi_candidates.push(script_dir.join("tools/dovi_tool"));
    if let Some(p) = which("dovi_tool") {
        dovi_candidates.push(p);
    }
    dovi_candidates.push(script_dir.join("dovi_tool/target/release/dovi_tool"));

    let dovi_tool = resolve_executable_tool(&dovi_candidates)
        .ok_or_else(|| "Missing tool: dovi_tool".to_string())?;

    let ffmpeg = resolve_optional(&script_dir, "ffmpeg");
    let ffprobe = resolve_optional(&script_dir, "ffprobe");
    let logger = Logger::new(
        log_file.clone(),
        cli.debug,
        cli.progress,
        if cli.repair_sync_offset.is_some() {
            21
        } else if cli.checker_mode {
            6
        } else if cli.hybrid_mode {
            15
        } else {
            6
        },
    );
    logger.rotate_log();

    if cli.save_el_rpu
        && !cli.dry_run
        && !cli.hybrid_mode
        && !cli.checker_mode
        && cli.repair_sync_offset.is_none()
    {
        fs::create_dir_all(&output_dir)
            .map_err(|e| format!("Cannot create archive dir {}: {e}", output_dir.display()))?;
    }

    let rt = Runtime {
        script_dir,
        output_dir,
        json_file,
        mkvextract,
        mkvmerge,
        mediainfo,
        dovi_tool,
        ffmpeg,
        ffprobe,
        tmp_dir: cli.tmp_dir.clone(),
        hwaccel: cli.hwaccel,
        save_el_rpu: cli.save_el_rpu,
        dry_run: cli.dry_run,
    };

    Ok((rt, logger))
}
