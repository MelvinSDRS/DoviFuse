use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logger::Logger;

pub(crate) type AppResult<T> = Result<T, String>;

pub(crate) struct CleanupGuard {
    files: Vec<PathBuf>,
    logger: Logger,
    enabled: bool,
}

impl CleanupGuard {
    pub(crate) fn new(logger: Logger) -> Self {
        Self {
            files: Vec::new(),
            logger,
            enabled: true,
        }
    }

    pub(crate) fn add<P: AsRef<Path>>(&mut self, p: P) {
        self.files.push(p.as_ref().to_path_buf());
    }

    pub(crate) fn clear(&mut self) {
        self.files.clear();
        self.enabled = false;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }

        if !self.files.is_empty() {
            self.logger
                .dbg(&format!("Cleaning up intermediate files: {:?}", self.files));
        }

        for f in &self.files {
            let _ = fs::remove_file(f);
        }
    }
}

pub(crate) fn command_string(program: &Path, args: &[OsString]) -> String {
    let mut parts = vec![program.display().to_string()];
    for a in args {
        let s = a.to_string_lossy();
        if s.contains(' ') || s.contains('\t') {
            parts.push(format!("'{}'", s.replace('\'', "'\\''")));
        } else {
            parts.push(s.to_string());
        }
    }
    parts.join(" ")
}

pub(crate) fn run_capture(logger: &Logger, program: &Path, args: &[OsString]) -> AppResult<String> {
    logger.dbg(&format!("Running: {}", command_string(program, args)));
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute {}: {e}", program.display()))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Command failed ({}): {}",
            out.status,
            stderr.trim()
        ));
    }

    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Like run_capture but also returns stderr and tolerates nonzero exit when
/// `allow_failure` is set (some ffmpeg probing runs end with benign errors).
pub(crate) fn run_capture_all(
    logger: &Logger,
    program: &Path,
    args: &[OsString],
    allow_failure: bool,
) -> AppResult<(String, String)> {
    logger.dbg(&format!("Running: {}", command_string(program, args)));
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute {}: {e}", program.display()))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if !out.status.success() && !allow_failure {
        return Err(format!(
            "Command failed ({}): {}",
            out.status,
            stderr.trim()
        ));
    }

    Ok((stdout, stderr))
}

pub(crate) fn run_status(
    logger: &Logger,
    dry_run: bool,
    mutating: bool,
    program: &Path,
    args: &[OsString],
) -> AppResult<()> {
    let cmd = command_string(program, args);
    if dry_run && mutating {
        logger.ok(&format!("[DRY RUN] Would run: {cmd}"));
        return Ok(());
    }

    logger.dbg(&format!("Running: {cmd}"));
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("Failed to execute {}: {e}", program.display()))?;

    if !status.success() {
        return Err(format!("Command failed ({status}): {cmd}"));
    }

    Ok(())
}
