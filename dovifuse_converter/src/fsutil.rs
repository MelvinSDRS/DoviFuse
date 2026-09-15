use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::exec::{run_capture, AppResult};
use crate::logger::Logger;

pub(crate) fn available_kb(path: &Path, logger: &Logger) -> AppResult<u64> {
    let args = vec![OsString::from("-k"), path.as_os_str().to_os_string()];
    let out = run_capture(logger, Path::new("df"), &args)?;

    for line in out.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 4 {
            if let Ok(v) = cols[3].parse::<u64>() {
                return Ok(v);
            }
        }
    }

    Err("Unable to parse df output".to_string())
}

pub(crate) fn check_disk_space_hybrid(
    file: &Path,
    scratch_dir: &Path,
    output_dir: &Path,
    logger: &Logger,
) -> AppResult<()> {
    let file_size_kb = fs::metadata(file)
        .map_err(|e| format!("Cannot stat {}: {e}", file.display()))?
        .len()
        / 1024;
    let scratch_avail_kb = available_kb(scratch_dir, logger)?;
    let (scratch_needed_kb, output_needed_kb) = space_requirements_kb(file_size_kb);

    #[cfg(unix)]
    let same_volume = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(scratch_dir).map_err(|e| e.to_string())?.dev()
            == fs::metadata(output_dir).map_err(|e| e.to_string())?.dev()
    };
    #[cfg(not(unix))]
    let same_volume = scratch_dir == output_dir;
    let scratch_needed_kb = if same_volume {
        scratch_needed_kb.saturating_add(output_needed_kb)
    } else {
        scratch_needed_kb
    };
    if scratch_avail_kb < scratch_needed_kb {
        return Err(format!(
            "Insufficient disk space in {}\n  Available: {} MB, Estimated need: {} MB",
            scratch_dir.display(),
            scratch_avail_kb / 1024,
            scratch_needed_kb / 1024
        ));
    }

    let output_avail_kb = available_kb(output_dir, logger)?;
    if output_avail_kb < output_needed_kb {
        return Err(format!(
            "Insufficient destination space in {}\n  Available: {} MB, Estimated need: {} MB",
            output_dir.display(),
            output_avail_kb / 1024,
            output_needed_kb / 1024
        ));
    }

    Ok(())
}

fn space_requirements_kb(file_size_kb: u64) -> (u64, u64) {
    (
        file_size_kb
            .saturating_mul(2)
            .saturating_add(5 * 1024 * 1024),
        file_size_kb.saturating_add(file_size_kb / 20),
    )
}

pub(crate) fn create_job_dir(root: &Path, prefix: &str) -> AppResult<PathBuf> {
    fs::create_dir_all(root)
        .map_err(|e| format!("Cannot create scratch root {}: {e}", root.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = root.join(format!("{prefix}-{}-{stamp}", std::process::id()));
    fs::create_dir(&dir)
        .map_err(|e| format!("Cannot create scratch directory {}: {e}", dir.display()))?;
    Ok(dir)
}

pub(crate) fn reserve_output(path: &Path) -> AppResult<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
        .map_err(|e| format!("Cannot reserve output {}: {e}", path.display()))
}

/// Request the strongest available file synchronization. Apple's F_FULLFSYNC
/// is not supported by every filesystem (notably SMB); POSIX fsync still asks
/// the filesystem/server to commit the file. Neither proves remote hardware
/// power-loss durability.
pub(crate) fn sync_file(file: &fs::File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn fsync(fd: std::os::raw::c_int) -> std::os::raw::c_int;
        }
        macos_sync_with_fallback(
            || file.sync_all(),
            || loop {
                // SAFETY: the borrowed File keeps this descriptor open.
                if unsafe { fsync(file.as_raw_fd()) } == 0 {
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

#[cfg(any(target_os = "macos", test))]
fn macos_sync_with_fallback(
    full_sync: impl FnOnce() -> std::io::Result<()>,
    filesystem_sync: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    // Darwin ENOTSUP is 45; Rust currently classifies it as Uncategorized.
    // Do not fall back on EIO, ENOSPC, permission errors, or arbitrary failures.
    match full_sync() {
        Err(error) if error.raw_os_error() == Some(45) => filesystem_sync(),
        result => result,
    }
}

pub(crate) fn move_to_dir(src: &Path, dir: &Path) -> AppResult<()> {
    move_to_dir_with_cancel(src, dir, crate::cancellation::requested)
}

fn move_to_dir_with_cancel(src: &Path, dir: &Path, cancelled: impl Fn() -> bool) -> AppResult<()> {
    let filename = src
        .file_name()
        .ok_or_else(|| format!("Invalid source path: {}", src.display()))?;
    let dst = dir.join(filename);

    // Never replace an existing archive with another release of the same name.
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dst)
        .map_err(|e| format!("Cannot reserve archive {}: {e}", dst.display()))?;
    let copied = (|| -> AppResult<()> {
        let metadata = fs::metadata(src).map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err("Archive source is not a regular file".into());
        }
        let mut source = fs::File::open(src).map_err(|e| e.to_string())?;
        let mut buffer = vec![0_u8; 4 * 1024 * 1024];
        loop {
            if cancelled() {
                return Err("Archive copy cancelled".into());
            }
            let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if cancelled() {
                return Err("Archive copy cancelled".into());
            }
            destination
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        destination
            .set_permissions(source.metadata().map_err(|e| e.to_string())?.permissions())
            .map_err(|e| e.to_string())?;
        sync_file(&destination).map_err(|e| format!("Cannot sync archive: {e}"))?;
        if cancelled() {
            return Err("Archive copy cancelled".into());
        }
        Ok(())
    })();
    drop(destination);
    if let Err(e) = copied {
        let _ = fs::remove_file(&dst);
        return Err(format!("Failed to archive {}: {e}", src.display()));
    }
    fs::remove_file(src).map_err(|e| format!("Failed to remove archived scratch file: {e}"))
}

pub(crate) fn collect_mkv_files(dir: &Path, out: &mut Vec<PathBuf>) -> AppResult<()> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read_dir {} failed: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read_dir entry error: {e}"))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| format!("file_type failed for {}: {e}", path.display()))?;

        if ft.is_dir() {
            collect_mkv_files(&path, out)?;
        } else if ft.is_file() {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext == "mkv" {
                out.push(path);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_unsupported_full_sync_requires_successful_filesystem_sync() {
        let unsupported = || Err(std::io::Error::from_raw_os_error(45));
        macos_sync_with_fallback(unsupported, || Ok(())).unwrap();
        let error =
            macos_sync_with_fallback(unsupported, || Err(std::io::Error::from_raw_os_error(5)))
                .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
    }

    #[test]
    fn macos_full_sync_errors_are_not_hidden_by_fallback() {
        for code in [5, 28, 9, 13] {
            let error = macos_sync_with_fallback(
                || Err(std::io::Error::from_raw_os_error(code)),
                || panic!("Real sync errors must not use fallback"),
            )
            .unwrap_err();
            assert_eq!(error.raw_os_error(), Some(code));
        }
        macos_sync_with_fallback(
            || Ok(()),
            || panic!("Successful full sync needs no fallback"),
        )
        .unwrap();
    }

    #[test]
    fn archive_cancel_after_copy_starts_preserves_source_and_removes_partial() {
        let root = create_job_dir(&std::env::temp_dir(), "dovifuse-archive-cancel-test").unwrap();
        let archive = root.join("archive");
        fs::create_dir(&archive).unwrap();
        let source = root.join("source.hevc");
        let bytes = vec![42; 4 * 1024 * 1024 + 1];
        fs::write(&source, &bytes).unwrap();
        let destination = archive.join("source.hevc");
        let result = move_to_dir_with_cancel(&source, &archive, || {
            fs::metadata(&destination).is_ok_and(|m| m.len() > 0)
        });
        assert!(result.unwrap_err().contains("cancelled"));
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert!(!destination.exists());
        // A clean retry must not encounter a stale reservation or partial archive.
        move_to_dir_with_cancel(&source, &archive, || false).unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_collision_preserves_both_files() {
        let root = create_job_dir(&std::env::temp_dir(), "dovifuse-archive-test").unwrap();
        let archive = root.join("archive");
        fs::create_dir(&archive).unwrap();
        let source = root.join("same.hevc");
        let destination = archive.join("same.hevc");
        fs::write(&source, b"new archive").unwrap();
        fs::write(&destination, b"existing archive").unwrap();
        assert!(move_to_dir(&source, &archive).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"new archive");
        assert_eq!(fs::read(&destination).unwrap(), b"existing archive");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scratch_and_destination_requirements_are_separate() {
        let (scratch, destination) = space_requirements_kb(50 * 1024 * 1024);
        assert_eq!(scratch, 105 * 1024 * 1024);
        assert_eq!(destination, 52 * 1024 * 1024 + 512 * 1024);
    }
}
