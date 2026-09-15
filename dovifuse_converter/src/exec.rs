use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
            if let Err(e) = fs::remove_file(f) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    self.logger.warn(&format!(
                        "Could not remove temporary file {}: {e}",
                        f.display()
                    ));
                }
            }
        }
        for d in self.dirs.iter().rev() {
            if let Err(e) = fs::remove_dir_all(d) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    self.logger.warn(&format!(
                        "Could not remove scratch directory {}: {e}",
                        d.display()
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_output_is_an_error_not_truncated_validation_input() {
        let reader = read_output(std::io::Cursor::new(vec![b'x'; 65_537]), None, true, 65_536);
        assert!(reader
            .join()
            .unwrap()
            .unwrap_err()
            .contains("capture budget"));
    }

    #[test]
    fn output_preserves_unterminated_lines_and_chunk_boundaries() {
        let mut bytes = vec![b'x'; 65_537];
        bytes.extend_from_slice(b"\nlast line");
        let reader = read_output(std::io::Cursor::new(bytes.clone()), None, true, bytes.len());
        assert_eq!(reader.join().unwrap().unwrap(), bytes);
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
    let out = run_process(Some(logger), program, args, false, None, CAPTURE_LIMIT)?;
    if !out.status.success() {
        return Err(format!("Command failed ({}): {cmd}", out.status));
    }
    Ok(())
}

// A corrupt or excessively verbose tool must fail explicitly rather than exhaust
// memory or silently truncate metadata used by validation. Limits are per pipe.
const CAPTURE_LIMIT: usize = 256 * 1024 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Tool discovery and report version collection use the same cancellation and
/// process-tree handling as conversion, with a short deadline and smaller budget.
pub(crate) fn probe(program: &Path, flag: &str) -> AppResult<Output> {
    run_process(
        None,
        program,
        &[flag.into()],
        true,
        Some(Duration::from_secs(10)),
        1024 * 1024,
    )
}

fn run_captured_process(logger: &Logger, program: &Path, args: &[OsString]) -> AppResult<Output> {
    run_process(Some(logger), program, args, true, None, CAPTURE_LIMIT)
}

struct ToolChild(Child);
impl ToolChild {
    fn terminate(&mut self) {
        #[cfg(unix)]
        unsafe {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // Spawn always creates a private group. Descendants inherit it;
            // never signal the converter's or caller's process group.
            kill(-(self.0.id() as i32), 9);
        }
        let _ = self.0.kill();
    }
}
impl Drop for ToolChild {
    fn drop(&mut self) {
        self.terminate();
        // Never block teardown on uninterruptible filesystem I/O.
        let _ = self.0.try_wait();
    }
}

fn read_output<R: Read + Send + 'static>(
    reader: R,
    logger: Option<Logger>,
    capture: bool,
    limit: usize,
) -> thread::JoinHandle<AppResult<Vec<u8>>> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut output = Vec::new();
        loop {
            // Bound an unterminated line too, even in streaming-only commands.
            let mut chunk = Vec::new();
            let n = reader
                .by_ref()
                .take(64 * 1024)
                .read_until(b'\n', &mut chunk)
                .map_err(|e| format!("Cannot read tool output: {e}"))?;
            if n == 0 {
                break;
            }
            if capture {
                if output.len().saturating_add(n) > limit {
                    return Err(format!(
                        "Tool output exceeds {limit} byte capture budget; validation unavailable"
                    ));
                }
                output.extend_from_slice(&chunk);
            }
            if let Some(logger) = &logger {
                logger.tool_output(&String::from_utf8_lossy(&chunk));
            }
        }
        Ok(output)
    })
}

fn run_process(
    logger: Option<&Logger>,
    program: &Path,
    args: &[OsString],
    capture: bool,
    timeout: Option<Duration>,
    limit: usize,
) -> AppResult<Output> {
    if cancellation::requested() {
        return Err("Conversion cancelled".into());
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = ToolChild(
        command
            .spawn()
            .map_err(|e| format!("Failed to execute {}: {e}", program.display()))?,
    );
    let stdout = child.0.stdout.take().ok_or("Failed to capture stdout")?;
    let stderr = child.0.stderr.take().ok_or("Failed to capture stderr")?;
    let mut readers = [
        Some(read_output(
            stdout,
            if capture { None } else { logger.cloned() },
            capture,
            limit,
        )),
        Some(read_output(stderr, logger.cloned(), capture, limit)),
    ];
    let mut output = [Vec::new(), Vec::new()];
    let started = Instant::now();
    let mut exited = None;
    let mut status = None;
    let result = loop {
        if cancellation::requested() {
            break Err("Conversion cancelled".to_string());
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            break Err(format!("Tool probe timed out: {}", program.display()));
        }
        let mut error = None;
        for (index, reader) in readers.iter_mut().enumerate() {
            if reader.as_ref().is_some_and(|r| r.is_finished()) {
                match reader.take().unwrap().join() {
                    Ok(Ok(bytes)) => output[index] = bytes,
                    Ok(Err(e)) => error = Some(e),
                    Err(_) => error = Some("Tool output reader panicked".into()),
                }
            }
        }
        if let Some(error) = error {
            break Err(error);
        }
        if status.is_none() {
            match child.0.try_wait() {
                Ok(Some(value)) => {
                    status = Some(value);
                    exited = Some(Instant::now());
                }
                Ok(None) => (),
                Err(e) => break Err(format!("Failed waiting for {}: {e}", program.display())),
            }
        }
        if status.is_some() && readers.iter().all(Option::is_none) {
            break Ok(());
        }
        if exited.is_some_and(|t| t.elapsed() >= DRAIN_TIMEOUT) {
            break Err(
                "Tool exited but output pipes remained open; terminating descendants".into(),
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    // Also stop abandoned descendants after a successful parent exit. Closing
    // pipes is part of completion; cancellation must remain live during drain.
    child.terminate();
    let drain_started = Instant::now();
    while readers.iter().flatten().any(|r| !r.is_finished())
        && drain_started.elapsed() < DRAIN_TIMEOUT
    {
        thread::sleep(Duration::from_millis(20));
    }
    for reader in readers.into_iter().flatten() {
        if reader.is_finished() {
            let _ = reader.join();
        }
        // A detached reader cannot hold up a failed job on OS-blocked I/O.
    }
    result?;
    Ok(Output {
        status: status.unwrap(),
        stdout: std::mem::take(&mut output[0]),
        stderr: std::mem::take(&mut output[1]),
    })
}
