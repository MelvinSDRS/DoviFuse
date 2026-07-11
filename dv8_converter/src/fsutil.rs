use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

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

pub(crate) fn check_disk_space(file: &Path, logger: &Logger) -> AppResult<()> {
    let input_dir = file
        .parent()
        .ok_or_else(|| format!("Invalid path: {}", file.display()))?;
    let file_size_kb = fs::metadata(file)
        .map_err(|e| format!("Cannot stat {}: {e}", file.display()))?
        .len()
        / 1024;
    let avail_kb = available_kb(input_dir, logger)?;
    let needed_kb = file_size_kb.saturating_mul(3);

    if avail_kb < needed_kb {
        return Err(format!(
            "Insufficient disk space in {}\n  Available: {} MB, Estimated need: {} MB",
            input_dir.display(),
            avail_kb / 1024,
            needed_kb / 1024
        ));
    }

    logger.dbg(&format!(
        "Disk space OK: {} MB available, ~{} MB needed",
        avail_kb / 1024,
        needed_kb / 1024
    ));

    Ok(())
}

pub(crate) fn check_disk_space_hybrid(
    file: &Path,
    output_dir: &Path,
    logger: &Logger,
) -> AppResult<()> {
    let file_size_kb = fs::metadata(file)
        .map_err(|e| format!("Cannot stat {}: {e}", file.display()))?
        .len()
        / 1024;
    let avail_kb = available_kb(output_dir, logger)?;
    let needed_kb = file_size_kb.saturating_mul(2);

    if avail_kb < needed_kb {
        return Err(format!(
            "Insufficient disk space in {}\n  Available: {} MB, Estimated need: {} MB",
            output_dir.display(),
            avail_kb / 1024,
            needed_kb / 1024
        ));
    }

    Ok(())
}

pub(crate) fn move_to_dir(src: &Path, dir: &Path) -> AppResult<()> {
    let filename = src
        .file_name()
        .ok_or_else(|| format!("Invalid source path: {}", src.display()))?;
    let dst = dir.join(filename);

    match fs::rename(src, &dst) {
        Ok(_) => Ok(()),
        Err(_) => {
            fs::copy(src, &dst).map_err(|e| {
                format!("Failed to copy {} to {}: {e}", src.display(), dst.display())
            })?;
            fs::remove_file(src).map_err(|e| {
                format!("Failed to remove source {} after copy: {e}", src.display())
            })?;
            Ok(())
        }
    }
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
