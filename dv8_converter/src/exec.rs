use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::cancellation;
use crate::logger::Logger;

pub(crate) type AppResult<T> = Result<T, String>;

pub(crate) struct CleanupGuard {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    logger: Logger,
    enabled: bool,
}

impl CleanupGuard {
    pub(crate) fn new(logger: Logger) -> Self {
        Self {
            files: Vec::new(),
            dirs: Vec::new(),
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

    pub(crate) fn add_dir<P: AsRef<Path>>(&mut self, p: P) {
        self.dirs.push(p.as_ref().to_path_buf());
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
        for d in self.dirs.iter().rev() {
            let _ = fs::remove_dir_all(d);
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
    let out = run_captured_process(logger, program, args)?;

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
    let out = run_captured_process(logger, program, args)?;

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
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute {}: {e}", program.display()))?;
    let stdout_reader = stream_tool_output(
        child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture stdout".to_string())?,
        logger.clone(),
    );
    let stderr_reader = stream_tool_output(
        child
            .stderr
            .take()
            .ok_or_else(|| "Failed to capture stderr".to_string())?,
        logger.clone(),
    );
    let status = loop {
        if cancellation::requested() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err("Conversion cancelled".to_string());
        }
        match child
            .try_wait()
            .map_err(|e| format!("Failed waiting for {}: {e}", program.display()))?
        {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(100)),
        }
    };
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();

    if !status.success() {
        return Err(format!("Command failed ({status}): {cmd}"));
    }

    Ok(())
}

fn stream_tool_output<R: Read + Send + 'static>(
    reader: R,
    logger: Logger,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => logger.tool_output(&String::from_utf8_lossy(&line)),
            }
        }
    })
}

fn run_captured_process(
    logger: &Logger,
    program: &Path,
    args: &[OsString],
) -> AppResult<std::process::Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute {}: {e}", program.display()))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture stdout".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture stderr".to_string())?;
    let stdout_reader = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let tool_logger = logger.clone();
    let stderr_reader = thread::spawn(move || {
        let mut v = Vec::new();
        let mut reader = BufReader::new(&mut stderr);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    tool_logger.tool_output(&String::from_utf8_lossy(&line));
                    v.extend_from_slice(&line);
                }
            }
        }
        v
    });

    let status = loop {
        if cancellation::requested() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err("Conversion cancelled".to_string());
        }
        match child
            .try_wait()
            .map_err(|e| format!("Failed waiting for {}: {e}", program.display()))?
        {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(100)),
        }
    };

    Ok(std::process::Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}
