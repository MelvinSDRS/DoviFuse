use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::cli::CliArgs;
use crate::exec::AppResult;
use crate::logger::Logger;

#[derive(Clone)]
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

        if let Ok(status) = Command::new(c)
            .arg(version_flag)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if status.success() {
                return Some(c.clone());
            }
        }
    }

    None
}

pub(crate) fn resolve_script_dir() -> PathBuf {
    if let Ok(v) = env::var("DV8_SCRIPT_DIR") {
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
    /// ffmpeg + ffprobe are only required in hybrid mode.
    pub(crate) fn require_ffmpeg(&self) -> AppResult<(&Path, &Path)> {
        match (self.ffmpeg.as_deref(), self.ffprobe.as_deref()) {
            (Some(f), Some(p)) => Ok((f, p)),
            _ => Err(
                "Missing tool: ffmpeg/ffprobe (required for hybrid mode; install ffmpeg or place static builds in tools/)"
                    .to_string(),
            ),
        }
    }
}

pub(crate) fn build_runtime(cli: &CliArgs) -> AppResult<(Runtime, Logger)> {
    let script_dir = resolve_script_dir();
    let log_file = env::var("DV8_PROCESSING_LOG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| script_dir.join("processing_log.txt"));

    let output_dir = if let Ok(v) = env::var("DV8_EL_RPU_DIR") {
        PathBuf::from(v)
    } else if Path::new("/NAS").is_dir() {
        PathBuf::from("/NAS/EL_RPU/")
    } else {
        PathBuf::from("/media/NAS/EL_RPU/")
    };

    let json_file = script_dir.join("config/DV7toDV8.json");

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

    let logger = Logger::new(log_file.clone(), cli.debug);
    logger.rotate_log();

    if cli.save_el_rpu && !cli.dry_run && !cli.hybrid_mode {
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
        save_el_rpu: cli.save_el_rpu,
        dry_run: cli.dry_run,
    };

    Ok((rt, logger))
}
