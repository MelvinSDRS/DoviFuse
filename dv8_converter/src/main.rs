use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[0;34m";
const NC: &str = "\x1b[0m";

const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

type AppResult<T> = Result<T, String>;

#[derive(Clone)]
struct Logger {
    log_file: PathBuf,
    debug: bool,
}

impl Logger {
    fn new(log_file: PathBuf, debug: bool) -> Self {
        Self { log_file, debug }
    }

    fn rotate_log(&self) {
        if let Ok(meta) = fs::metadata(&self.log_file) {
            if meta.len() > LOG_MAX_BYTES {
                let old = self.log_file.with_extension("txt.old");
                let _ = fs::rename(&self.log_file, old);
            }
        }
    }

    fn now_string(&self) -> String {
        if let Ok(out) = Command::new("date").arg("+%F %T").output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }

        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("unix:{secs}")
    }

    fn log(&self, msg: &str) {
        let line = format!("{} - {}\n", self.now_string(), msg);
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn dbg(&self, msg: &str) {
        if self.debug {
            self.log(&format!("DEBUG: {msg}"));
        }
    }

    fn step(&self, msg: &str) {
        println!("\n{BLUE}=== {msg} ==={NC}\n");
    }

    fn ok(&self, msg: &str) {
        println!("{GREEN}{msg}{NC}");
    }

    fn warn(&self, msg: &str) {
        println!("{YELLOW}{msg}{NC}");
    }

    fn err(&self, msg: &str) {
        eprintln!("{RED}{msg}{NC}");
    }

    fn preflight_status(&self, label: &str, msg: &str) {
        match label {
            "PASS" => self.ok(&format!("[PASS] {msg}")),
            "WARN" => self.warn(&format!("[WARN] {msg}")),
            "FAIL" => self.err(&format!("[FAIL] {msg}")),
            _ => println!("[{label}] {msg}"),
        }
    }
}

#[derive(Clone)]
struct Runtime {
    script_dir: PathBuf,
    output_dir: PathBuf,
    json_file: PathBuf,
    mkvextract: PathBuf,
    mkvmerge: PathBuf,
    mediainfo: PathBuf,
    dovi_tool: PathBuf,
    save_el_rpu: bool,
    dry_run: bool,
}

struct CliArgs {
    save_el_rpu: bool,
    debug: bool,
    dry_run: bool,
    hybrid_mode: bool,
    custom_output: Option<PathBuf>,
    input_path: Option<PathBuf>,
    dv_source: Option<PathBuf>,
    hdr_target: Option<PathBuf>,
    original_args: Vec<String>,
}

#[derive(Default, Clone)]
struct HybridMediaInfo {
    codec: String,
    codec_id: String,
    frame_count: u64,
    frame_rate: Option<f64>,
    frame_rate_num: Option<f64>,
    frame_rate_den: Option<f64>,
    duration_ms: Option<f64>,
    hdr_format: String,
    width: Option<u32>,
    height: Option<u32>,
    bit_depth: Option<u32>,
    colour_primaries: String,
    transfer_characteristics: String,
    frame_rate_mode: String,
    max_cll: Option<u16>,
    max_fall: Option<u16>,
    mastering_min_nits: Option<f64>,
    mastering_max_nits: Option<f64>,
}

#[derive(Clone)]
struct DuplicateOp {
    source: u64,
    offset: u64,
    length: u64,
}

#[derive(Default, Clone)]
struct AlignmentStrategy {
    action: String,
    description: String,
    remove_ranges: Vec<String>,
    duplicates: Vec<DuplicateOp>,
    high_risk: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct Level6Meta {
    max_display_mastering_luminance: u16,
    min_display_mastering_luminance: u16,
    max_content_light_level: u16,
    max_frame_average_light_level: u16,
}

struct CleanupGuard {
    files: Vec<PathBuf>,
    logger: Logger,
    enabled: bool,
}

impl CleanupGuard {
    fn new(logger: Logger) -> Self {
        Self {
            files: Vec::new(),
            logger,
            enabled: true,
        }
    }

    fn add<P: AsRef<Path>>(&mut self, p: P) {
        self.files.push(p.as_ref().to_path_buf());
    }

    fn clear(&mut self) {
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

fn usage() {
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

fn parse_args() -> AppResult<CliArgs> {
    let original_args: Vec<String> = env::args().skip(1).collect();

    let mut save_el_rpu = true;
    let mut debug = false;
    let mut dry_run = false;
    let mut hybrid_mode = false;
    let mut custom_output: Option<PathBuf> = None;

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
        original_args,
    })
}

fn is_executable(path: &Path) -> bool {
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

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for p in env::split_paths(&path) {
        let candidate = p.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn resolve_executable_tool(candidates: &[PathBuf]) -> Option<PathBuf> {
    for c in candidates {
        if !is_executable(c) {
            continue;
        }

        if let Ok(status) = Command::new(c)
            .arg("--version")
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

fn resolve_script_dir() -> PathBuf {
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

fn build_runtime(cli: &CliArgs) -> AppResult<(Runtime, Logger)> {
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

    let mkvextract = resolve_executable_tool(
        &[
            which("mkvextract").unwrap_or_default(),
            script_dir.join("tools/mkvextract"),
        ]
        .into_iter()
        .filter(|p| !p.as_os_str().is_empty())
        .collect::<Vec<_>>(),
    )
    .ok_or_else(|| "Missing tool: mkvextract".to_string())?;

    let mkvmerge = resolve_executable_tool(
        &[
            which("mkvmerge").unwrap_or_default(),
            script_dir.join("tools/mkvmerge"),
        ]
        .into_iter()
        .filter(|p| !p.as_os_str().is_empty())
        .collect::<Vec<_>>(),
    )
    .ok_or_else(|| "Missing tool: mkvmerge".to_string())?;

    let mediainfo = resolve_executable_tool(
        &[
            which("mediainfo").unwrap_or_default(),
            script_dir.join("tools/mediainfo"),
        ]
        .into_iter()
        .filter(|p| !p.as_os_str().is_empty())
        .collect::<Vec<_>>(),
    )
    .ok_or_else(|| "Missing tool: mediainfo".to_string())?;

    let mut dovi_candidates: Vec<PathBuf> = Vec::new();
    dovi_candidates.push(script_dir.join("tools/dovi_tool"));
    if let Some(p) = which("dovi_tool") {
        dovi_candidates.push(p);
    }
    dovi_candidates.push(script_dir.join("dovi_tool/target/release/dovi_tool"));

    let dovi_tool = resolve_executable_tool(&dovi_candidates)
        .ok_or_else(|| "Missing tool: dovi_tool".to_string())?;

    let logger = Logger::new(log_file.clone(), cli.debug);
    logger.rotate_log();

    if cli.save_el_rpu && !cli.dry_run {
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
        save_el_rpu: cli.save_el_rpu,
        dry_run: cli.dry_run,
    };

    Ok((rt, logger))
}

fn command_string(program: &Path, args: &[OsString]) -> String {
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

fn run_capture(logger: &Logger, program: &Path, args: &[OsString]) -> AppResult<String> {
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

fn run_status(
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

fn parse_int(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u64>().ok()
    }
}

fn parse_u16(s: &str) -> Option<u16> {
    parse_int(s).and_then(|v| u16::try_from(v).ok())
}

fn parse_float(s: &str) -> Option<f64> {
    let cleaned = s.replace(',', "").trim().to_string();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

fn parse_mastering_luminance(raw: &str) -> (Option<f64>, Option<f64>) {
    let mut nums: Vec<f64> = Vec::new();
    let mut buf = String::new();

    for ch in raw.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            buf.push(ch);
        } else if !buf.is_empty() {
            if let Ok(v) = buf.parse::<f64>() {
                nums.push(v);
            }
            buf.clear();
        }
    }

    if !buf.is_empty() {
        if let Ok(v) = buf.parse::<f64>() {
            nums.push(v);
        }
    }

    if nums.len() < 2 {
        return (None, None);
    }

    let mut min = nums[0];
    let mut max = nums[0];

    for n in nums {
        if n < min {
            min = n;
        }
        if n > max {
            max = n;
        }
    }

    (Some(min), Some(max))
}

fn fps_from_info(info: &HybridMediaInfo) -> Option<f64> {
    if let (Some(num), Some(den)) = (info.frame_rate_num, info.frame_rate_den) {
        if den > 0.0 {
            return Some(num / den);
        }
    }
    info.frame_rate
}

fn get_hevc_track_id(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<u32> {
    let args = vec![
        OsString::from("--Output=Video;%ID%\\n"),
        file.as_os_str().to_os_string(),
    ];
    let out = run_capture(logger, &rt.mediainfo, &args)?;
    let first = out.lines().find(|l| !l.trim().is_empty()).unwrap_or("0");
    let id = first.trim().parse::<u32>().unwrap_or(0);
    if id > 0 {
        Ok(id - 1)
    } else {
        Ok(0)
    }
}

fn available_kb(path: &Path, logger: &Logger) -> AppResult<u64> {
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

fn check_disk_space(file: &Path, logger: &Logger) -> AppResult<()> {
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

fn check_disk_space_hybrid(file: &Path, output_dir: &Path, logger: &Logger) -> AppResult<()> {
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

fn is_dv7_file(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<bool> {
    let base = file
        .file_name()
        .map(|b| b.to_string_lossy().to_string())
        .unwrap_or_default();

    if base.starts_with("._") {
        logger.log(&format!("{} skipped: hidden", file.display()));
        return Ok(false);
    }

    let args = vec![file.as_os_str().to_os_string()];
    let info = run_capture(logger, &rt.mediainfo, &args)?;
    let lower = info.to_lowercase();

    let has_profile7 = (lower.contains("dolby")
        && lower.contains("vision")
        && (lower.contains("profile 7") || lower.contains("profile: 7")))
        || lower.contains("dvhe.07");

    if has_profile7 {
        logger.log(&format!("{} DV7 detected", file.display()));
        Ok(true)
    } else {
        logger.log(&format!("{} not DV7", file.display()));
        Ok(false)
    }
}

fn replace_case_insensitive_all(input: &str, pattern: &str, replacement: &str) -> (String, bool) {
    let input_lower = input.to_lowercase();
    let pattern_lower = pattern.to_lowercase();

    let mut out = String::new();
    let mut start = 0usize;
    let mut changed = false;

    while let Some(rel_pos) = input_lower[start..].find(&pattern_lower) {
        let pos = start + rel_pos;
        out.push_str(&input[start..pos]);
        out.push_str(replacement);
        start = pos + pattern.len();
        changed = true;
    }

    out.push_str(&input[start..]);
    (out, changed)
}

fn make_dv8_name(name: &str) -> String {
    let mut result = name.to_string();
    let mut changed_any = false;

    for pat in [
        "Dolby.Vision",
        "Dolby-Vision",
        "Dolby_Vision",
        "DolbyVision",
        "DoVi",
        "DOVI",
        "Dovi",
        "DV7",
        "dv7",
    ] {
        let (updated, changed) = replace_case_insensitive_all(&result, pat, "DV8");
        result = updated;
        changed_any |= changed;
    }

    if !changed_any {
        result = format!("{name}.DV8");
    }

    while result.contains("..") {
        result = result.replace("..", ".");
    }

    result
}

fn move_to_dir(src: &Path, dir: &Path) -> AppResult<()> {
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

fn process_file(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<()> {
    let input_dir = file
        .parent()
        .ok_or_else(|| format!("Invalid path: {}", file.display()))?;
    let mkv_base = file
        .file_stem()
        .ok_or_else(|| format!("Invalid filename: {}", file.display()))?
        .to_string_lossy()
        .to_string();

    let bl_el_rpu_hevc = input_dir.join(format!("{mkv_base}.BL_EL_RPU.hevc"));
    let dv7_el_rpu_hevc = input_dir.join(format!("{mkv_base}.DV7.EL_RPU.hevc"));
    let dv8_bl_rpu_hevc = input_dir.join(format!("{mkv_base}.DV8.BL_RPU.hevc"));
    let dv8_rpu_bin = input_dir.join(format!("{mkv_base}.DV8.RPU.bin"));

    let mut cleanup = CleanupGuard::new(logger.clone());
    cleanup.add(&bl_el_rpu_hevc);
    cleanup.add(&dv7_el_rpu_hevc);
    cleanup.add(&dv8_bl_rpu_hevc);
    cleanup.add(&dv8_rpu_bin);

    // let out_base = make_dv8_name(&mkv_base);
    let out_base = mkv_base.clone();
    let final_file = input_dir.join(format!("{out_base}.mkv"));
    let out_file = input_dir.join(format!("{out_base}.DV8_TMP.mkv"));

    if out_file.exists() {
        logger.warn(&format!(
            "Output file already exists, skipping: {}",
            out_file.display()
        ));
        logger.log(&format!(
            "{} skipped: output already exists: {}",
            file.display(),
            out_file.display()
        ));
        cleanup.clear();
        return Ok(());
    }

    check_disk_space(file, logger)?;

    if rt.dry_run {
        logger.ok(&format!("[DRY RUN] Would convert: {}", file.display()));
        logger.ok(&format!("[DRY RUN] Output: {}", out_file.display()));
        if rt.save_el_rpu {
            logger.ok(&format!(
                "[DRY RUN] Archive EL+RPU to: {}",
                rt.output_dir.display()
            ));
        }
        cleanup.clear();
        return Ok(());
    }

    let track_id = get_hevc_track_id(file, rt, logger)?;
    logger.dbg(&format!("Using video track ID: {track_id}"));

    logger.step("1 | Extract BL+EL+RPU");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvextract,
        &[
            OsString::from("tracks"),
            file.as_os_str().to_os_string(),
            OsString::from(format!("{}:{}", track_id, bl_el_rpu_hevc.display())),
        ],
    )?;

    logger.step("2 | Demux EL+RPU");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("demux"),
            OsString::from("--el-only"),
            bl_el_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("-e"),
            dv7_el_rpu_hevc.as_os_str().to_os_string(),
        ],
    )?;

    if rt.save_el_rpu {
        move_to_dir(&dv7_el_rpu_hevc, &rt.output_dir)?;
    } else {
        let _ = fs::remove_file(&dv7_el_rpu_hevc);
    }

    logger.step("3 | Convert to DV8");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("--edit-config"),
            rt.json_file.as_os_str().to_os_string(),
            OsString::from("convert"),
            OsString::from("--discard"),
            bl_el_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("-o"),
            dv8_bl_rpu_hevc.as_os_str().to_os_string(),
        ],
    )?;

    logger.step("4 | Extract RPU (optional)");
    let _ = run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("extract-rpu"),
            dv8_bl_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("-o"),
            dv8_rpu_bin.as_os_str().to_os_string(),
        ],
    );

    logger.step("5 | Prepare output name");
    logger.dbg(&format!("Output temp name: {}", out_file.display()));
    logger.dbg(&format!("Final output name: {}", final_file.display()));

    logger.step("6 | Remux final MKV");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvmerge,
        &[
            OsString::from("-o"),
            out_file.as_os_str().to_os_string(),
            OsString::from("-D"),
            file.as_os_str().to_os_string(),
            dv8_bl_rpu_hevc.as_os_str().to_os_string(),
            OsString::from("--track-order"),
            OsString::from("1:0"),
        ],
    )?;

    let orig_size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    let out_size = fs::metadata(&out_file).map(|m| m.len()).unwrap_or(0);

    if out_size == 0 {
        let _ = fs::remove_file(&out_file);
        return Err(format!(
            "Output file is empty - keeping original: {}",
            file.display()
        ));
    }

    if orig_size > 0 && (out_size * 100 / orig_size) < 50 {
        return Err(format!(
            "Output is much smaller than original ({} MB vs {} MB) - keeping original",
            out_size / 1_048_576,
            orig_size / 1_048_576
        ));
    }

    let _ = fs::remove_file(&bl_el_rpu_hevc);
    let _ = fs::remove_file(&dv8_bl_rpu_hevc);
    let _ = fs::remove_file(&dv8_rpu_bin);
    cleanup.clear();

    fs::remove_file(file)
        .map_err(|e| format!("Failed to delete original {}: {e}", file.display()))?;
    fs::rename(&out_file, &final_file).map_err(|e| {
        format!(
            "Failed to rename output {} to {}: {e}",
            out_file.display(),
            final_file.display()
        )
    })?;

    logger.log(&format!(
        "{} processed successfully -> {}",
        file.display(),
        final_file.display()
    ));
    logger.ok(&format!("Done: {}", final_file.display()));

    Ok(())
}

fn collect_mkv_files(dir: &Path, out: &mut Vec<PathBuf>) -> AppResult<()> {
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

fn process_directory(dir: &Path, rt: &Runtime, logger: &Logger) -> AppResult<()> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_mkv_files(dir, &mut files)?;
    files.sort();

    logger.step(&format!("Scan folder {}", dir.display()));

    let mut count = 0usize;
    let mut failures = 0usize;

    for f in files {
        if is_dv7_file(&f, rt, logger)? {
            match process_file(&f, rt, logger) {
                Ok(_) => count += 1,
                Err(e) => {
                    failures += 1;
                    logger.err(&format!("Failed: {} ({e})", f.display()));
                }
            }
        }
    }

    logger.ok(&format!("{count} file(s) converted."));
    if failures > 0 {
        logger.warn(&format!("{failures} file(s) failed."));
    }

    Ok(())
}

fn hybrid_get_media_info(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<HybridMediaInfo> {
    let template = "--Output=Video;%Format%|%CodecID%|%FrameCount%|%FrameRate%|%FrameRate_Num%|%FrameRate_Den%|%Duration%|%HDR_Format%|%Width%|%Height%|%BitDepth%|%colour_primaries%|%transfer_characteristics%|%FrameRate_Mode%|%ScanType%|%MaxCLL%|%MaxFALL%|%MasteringDisplay_Luminance%";

    let args = vec![OsString::from(template), file.as_os_str().to_os_string()];
    let out = run_capture(logger, &rt.mediainfo, &args)?;
    let line = out
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| format!("No mediainfo video output for {}", file.display()))?;

    let parts: Vec<&str> = line.split('|').collect();
    let get = |idx: usize| -> String {
        parts
            .get(idx)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };

    let mastering = get(17);
    let (mastering_min_nits, mastering_max_nits) = parse_mastering_luminance(&mastering);
    let _ = get(14);

    Ok(HybridMediaInfo {
        codec: get(0),
        codec_id: get(1),
        frame_count: parse_int(&get(2)).unwrap_or(0),
        frame_rate: parse_float(&get(3)),
        frame_rate_num: parse_float(&get(4)),
        frame_rate_den: parse_float(&get(5)),
        duration_ms: parse_float(&get(6)),
        hdr_format: get(7),
        width: parse_int(&get(8)).and_then(|v| u32::try_from(v).ok()),
        height: parse_int(&get(9)).and_then(|v| u32::try_from(v).ok()),
        bit_depth: parse_int(&get(10)).and_then(|v| u32::try_from(v).ok()),
        colour_primaries: get(11),
        transfer_characteristics: get(12),
        frame_rate_mode: get(13),
        max_cll: parse_u16(&get(15)),
        max_fall: parse_u16(&get(16)),
        mastering_min_nits,
        mastering_max_nits,
    })
}

fn hybrid_detect_dv_profile(file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<Option<u8>> {
    let out = run_capture(logger, &rt.mediainfo, &[file.as_os_str().to_os_string()])?;
    let lower = out.to_lowercase();

    if lower.contains("dvhe.05") || lower.contains("profile 5") || lower.contains("profile: 5") {
        return Ok(Some(5));
    }
    if lower.contains("dvhe.07") || lower.contains("profile 7") || lower.contains("profile: 7") {
        return Ok(Some(7));
    }
    if lower.contains("dvhe.08")
        || lower.contains("profile 8")
        || lower.contains("profile: 8")
        || lower.contains("profile 8.1")
    {
        return Ok(Some(8));
    }

    Ok(None)
}

fn compare_brightness_signature(dv: &HybridMediaInfo, hdr: &HybridMediaInfo) -> Option<bool> {
    let dv_values = (
        dv.max_cll,
        dv.max_fall,
        dv.mastering_min_nits,
        dv.mastering_max_nits,
    );
    let hdr_values = (
        hdr.max_cll,
        hdr.max_fall,
        hdr.mastering_min_nits,
        hdr.mastering_max_nits,
    );

    let hdr_has_any = hdr_values.0.is_some()
        || hdr_values.1.is_some()
        || hdr_values.2.is_some()
        || hdr_values.3.is_some();
    if !hdr_has_any {
        return None;
    }

    let dv_has_any = dv_values.0.is_some()
        || dv_values.1.is_some()
        || dv_values.2.is_some()
        || dv_values.3.is_some();

    if !dv_has_any {
        return Some(false);
    }

    let same_cll = dv_values.0 == hdr_values.0;
    let same_fall = dv_values.1 == hdr_values.1;

    let same_min = match (dv_values.2, hdr_values.2) {
        (Some(a), Some(b)) => (a - b).abs() < 0.0001,
        (None, None) => true,
        _ => false,
    };

    let same_max = match (dv_values.3, hdr_values.3) {
        (Some(a), Some(b)) => (a - b).abs() < 0.001,
        (None, None) => true,
        _ => false,
    };

    Some(same_cll && same_fall && same_min && same_max)
}

fn hybrid_preflight_checks(
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    dv_profile: Option<u8>,
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

    if dv_profile == Some(5) {
        logger.preflight_status(
            "WARN",
            "9. DV Profile 5 source detected - mode 3 will be used",
        );
    } else {
        logger.preflight_status(
            "PASS",
            &format!(
                "9. DV profile check (detected: {})",
                dv_profile
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ),
        );
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

    match compare_brightness_signature(dv_info, hdr_info) {
        Some(true) => logger.preflight_status("PASS", "14. Brightness metadata matches"),
        Some(false) => logger.preflight_status(
            "WARN",
            "14. Brightness metadata differs - RPU L6 will be updated to match HDR target",
        ),
        None => logger.preflight_status(
            "WARN",
            "14. HDR target brightness metadata missing - L6 override will be skipped",
        ),
    }

    if has_fail {
        logger.err("Preflight failed. Hybrid conversion aborted.");
    }

    has_fail
}

fn hybrid_get_rpu_frame_count(rpu_file: &Path, rt: &Runtime, logger: &Logger) -> AppResult<u64> {
    let out = run_capture(
        logger,
        &rt.dovi_tool,
        &[
            OsString::from("info"),
            OsString::from("-i"),
            rpu_file.as_os_str().to_os_string(),
            OsString::from("--summary"),
        ],
    )?;

    for line in out.lines() {
        if let Some(idx) = line.find("Frames:") {
            let value = line[idx + "Frames:".len()..].trim();
            if let Some(n) = parse_int(value) {
                return Ok(n);
            }
        }
    }

    Err(format!(
        "Could not parse frame count from dovi_tool info for {}",
        rpu_file.display()
    ))
}

fn parse_chapter_timestamp_ms(ts: &str) -> Option<i64> {
    let mut parts = ts.split(':');
    let h = parts.next()?.parse::<i64>().ok()?;
    let m = parts.next()?.parse::<i64>().ok()?;
    let sec_ms = parts.next()?;

    let mut sec_parts = sec_ms.split('.');
    let s = sec_parts.next()?.parse::<i64>().ok()?;
    let ms_str = sec_parts.next().unwrap_or("0");

    let mut ms_norm = ms_str.to_string();
    if ms_norm.len() > 3 {
        ms_norm.truncate(3);
    }
    while ms_norm.len() < 3 {
        ms_norm.push('0');
    }

    let ms = ms_norm.parse::<i64>().ok()?;
    Some((((h * 60 + m) * 60 + s) * 1000) + ms)
}

fn hybrid_extract_chapters(file: &Path, rt: &Runtime, logger: &Logger) -> Vec<i64> {
    let out = run_capture(
        logger,
        &rt.mkvextract,
        &[
            OsString::from("chapters"),
            file.as_os_str().to_os_string(),
            OsString::from("--simple"),
        ],
    );

    let Ok(out) = out else {
        return Vec::new();
    };

    let mut chapters = Vec::new();
    for line in out.lines() {
        if !line.starts_with("CHAPTER") || line.contains("NAME") {
            continue;
        }
        if let Some((_, ts)) = line.split_once('=') {
            if let Some(ms) = parse_chapter_timestamp_ms(ts.trim()) {
                chapters.push(ms);
            }
        }
    }

    chapters
}

fn hybrid_chapter_offset(dv_chapters: &[i64], hdr_chapters: &[i64]) -> Option<i32> {
    if dv_chapters.len() < 3 || hdr_chapters.len() < 3 {
        return None;
    }

    let dv_intervals: Vec<i64> = dv_chapters.windows(2).map(|w| w[1] - w[0]).collect();
    let hdr_intervals: Vec<i64> = hdr_chapters.windows(2).map(|w| w[1] - w[0]).collect();

    if dv_intervals.len() < 2 || hdr_intervals.len() < 2 {
        return None;
    }

    let mut best_offset = 0i32;
    let mut best_matches = 0usize;

    let min_offset = -((dv_intervals.len() as i32) - 1);
    let max_offset = (hdr_intervals.len() as i32) - 1;

    for offset in min_offset..=max_offset {
        let mut matches = 0usize;
        for (i, &dv_v) in dv_intervals.iter().enumerate() {
            let j = i as i32 + offset;
            if j < 0 || j >= hdr_intervals.len() as i32 {
                continue;
            }
            let hdr_v = hdr_intervals[j as usize];
            if (dv_v - hdr_v).abs() <= 500 {
                matches += 1;
            }
        }

        if matches > best_matches {
            best_matches = matches;
            best_offset = offset;
        }
    }

    if best_matches >= 2 {
        Some(best_offset)
    } else {
        None
    }
}

fn hybrid_compute_alignment(
    dv_frames: u64,
    hdr_frames: u64,
    _dv_dur_ms: Option<f64>,
    _hdr_dur_ms: Option<f64>,
    fps: f64,
) -> AlignmentStrategy {
    let mut strategy = AlignmentStrategy::default();

    let abs_diff = dv_frames.abs_diff(hdr_frames);
    strategy.action = "none".to_string();
    strategy.description = format!("No alignment needed (frame counts match: {dv_frames})");

    if abs_diff == 0 {
        return strategy;
    }

    let small = (fps * 2.0).round() as u64;
    let medium = (fps * 60.0).round() as u64;
    let large = (fps * 300.0).round() as u64;

    if abs_diff <= small {
        if dv_frames > hdr_frames {
            let start = hdr_frames;
            let end = dv_frames - 1;
            strategy.action = "remove_end".to_string();
            strategy.description =
                format!("Small diff ({abs_diff} frames): trim DV RPU from end ({start}-{end})");
            strategy.remove_ranges.push(format!("{start}-{end}"));
        } else {
            strategy.action = "duplicate_end".to_string();
            strategy.description =
                format!("Small diff ({abs_diff} frames): duplicate last RPU metadata at end");
            strategy.duplicates.push(DuplicateOp {
                source: dv_frames.saturating_sub(1),
                offset: dv_frames,
                length: abs_diff,
            });
        }
        return strategy;
    }

    if abs_diff <= medium {
        if dv_frames > hdr_frames {
            let end = abs_diff.saturating_sub(1);
            strategy.action = "remove_start".to_string();
            strategy.description =
                format!("Medium diff ({abs_diff} frames): trim DV RPU from start (0-{end})");
            strategy.remove_ranges.push(format!("0-{end}"));
        } else {
            strategy.action = "duplicate_start".to_string();
            strategy.description =
                format!("Medium diff ({abs_diff} frames): duplicate first RPU metadata at start");
            strategy.duplicates.push(DuplicateOp {
                source: 0,
                offset: 0,
                length: abs_diff,
            });
        }
        return strategy;
    }

    if abs_diff <= large {
        strategy.high_risk = true;
        if dv_frames > hdr_frames {
            let end = abs_diff.saturating_sub(1);
            strategy.action = "remove_start".to_string();
            strategy.description = format!(
                "Large diff ({abs_diff} frames): HIGH RISK, trim DV RPU from start (0-{end})"
            );
            strategy.remove_ranges.push(format!("0-{end}"));
        } else {
            strategy.action = "duplicate_start".to_string();
            strategy.description = format!(
                "Large diff ({abs_diff} frames): HIGH RISK, duplicate first RPU metadata at start"
            );
            strategy.duplicates.push(DuplicateOp {
                source: 0,
                offset: 0,
                length: abs_diff,
            });
        }
        return strategy;
    }

    strategy.high_risk = true;
    strategy.description = format!(
        "Frame diff ({abs_diff}) exceeds 5-minute heuristic at {fps:.3} fps; leaving as-is"
    );
    strategy
}

fn l6_from_media_info(info: &HybridMediaInfo) -> Option<Level6Meta> {
    let max_cll = info.max_cll?;
    let max_fall = info.max_fall?;
    let min_nits = info.mastering_min_nits?;
    let max_nits = info.mastering_max_nits?;

    let mut min_display = if min_nits <= 1.0 {
        (min_nits * 10000.0).round() as u16
    } else {
        min_nits.round() as u16
    };
    if min_display == 0 {
        min_display = 1;
    }

    let max_display = max_nits.round().clamp(1.0, 10000.0) as u16;

    Some(Level6Meta {
        max_display_mastering_luminance: max_display,
        min_display_mastering_luminance: min_display.min(10000),
        max_content_light_level: max_cll.min(10000),
        max_frame_average_light_level: max_fall.min(10000),
    })
}

fn build_active_area_json(dv_info: &HybridMediaInfo, hdr_info: &HybridMediaInfo) -> Option<String> {
    let (Some(dw), Some(dh), Some(hw), Some(hh)) = (
        dv_info.width,
        dv_info.height,
        hdr_info.width,
        hdr_info.height,
    ) else {
        return None;
    };

    if dw == hw && dh == hh {
        return None;
    }

    let target_bigger_or_equal = hw >= dw && hh >= dh;
    let target_smaller_or_equal = hw <= dw && hh <= dh;

    if target_bigger_or_equal && (hw > dw || hh > dh) {
        let left = (hw - dw) / 2;
        let right = (hw - dw) / 2;
        let top = (hh - dh) / 2;
        let bottom = (hh - dh) / 2;

        return Some(format!(
            "{{\n    \"presets\": [{{\"id\": 1, \"left\": {left}, \"right\": {right}, \"top\": {top}, \"bottom\": {bottom}}}],\n    \"edits\": {{\"all\": 1}}\n  }}"
        ));
    }

    if target_smaller_or_equal && (hw < dw || hh < dh) {
        return Some("{\"crop\": true}".to_string());
    }

    Some("{\"crop\": true}".to_string())
}

fn hybrid_build_editor_json(
    strategy: &AlignmentStrategy,
    dv_profile: Option<u8>,
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    json_output_path: &Path,
) -> AppResult<()> {
    let mode = if dv_profile == Some(5) { 3 } else { 2 };

    let mut fields: Vec<String> = Vec::new();
    fields.push(format!("  \"mode\": {mode}"));
    fields.push("  \"remove_cmv4\": false".to_string());
    fields.push("  \"remove_mapping\": true".to_string());

    if !strategy.remove_ranges.is_empty() {
        let values = strategy
            .remove_ranges
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(format!("  \"remove\": [{values}]"));
    }

    if !strategy.duplicates.is_empty() {
        let mut dup_json = String::from("  \"duplicate\": [\n");
        for (idx, d) in strategy.duplicates.iter().enumerate() {
            let comma = if idx + 1 == strategy.duplicates.len() {
                ""
            } else {
                ","
            };
            dup_json.push_str(&format!(
                "    {{\"source\": {}, \"offset\": {}, \"length\": {}}}{}\n",
                d.source, d.offset, d.length, comma
            ));
        }
        dup_json.push_str("  ]");
        fields.push(dup_json);
    }

    if let Some(active_area_json) = build_active_area_json(dv_info, hdr_info) {
        fields.push(format!("  \"active_area\": {active_area_json}"));
    }

    let dv_l6 = l6_from_media_info(dv_info);
    let hdr_l6 = l6_from_media_info(hdr_info);
    if let Some(target_l6) = hdr_l6 {
        let should_override = dv_l6.as_ref().map(|src| src != &target_l6).unwrap_or(true);

        if should_override {
            fields.push(format!(
                "  \"level6\": {{\"max_display_mastering_luminance\": {}, \"min_display_mastering_luminance\": {}, \"max_content_light_level\": {}, \"max_frame_average_light_level\": {}}}",
                target_l6.max_display_mastering_luminance,
                target_l6.min_display_mastering_luminance,
                target_l6.max_content_light_level,
                target_l6.max_frame_average_light_level
            ));
        }
    }

    let json = format!("{{\n{}\n}}\n", fields.join(",\n"));
    fs::write(json_output_path, json).map_err(|e| {
        format!(
            "Failed to write editor JSON {}: {e}",
            json_output_path.display()
        )
    })?;

    Ok(())
}

fn hybrid_validate_output(
    out_file: &Path,
    hdr_target: &Path,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    let out_meta = fs::metadata(out_file).map_err(|e| {
        format!(
            "Validation failed: output missing {}: {e}",
            out_file.display()
        )
    })?;
    if out_meta.len() == 0 {
        return Err(format!(
            "Validation failed: output is empty: {}",
            out_file.display()
        ));
    }

    let hdr_size = fs::metadata(hdr_target).map(|m| m.len()).unwrap_or(0);
    if hdr_size > 0 && out_meta.len() * 100 / hdr_size < 80 {
        return Err(format!(
            "Validation failed: output too small ({} MB vs {} MB)",
            out_meta.len() / 1_048_576,
            hdr_size / 1_048_576
        ));
    }

    let out_info = hybrid_get_media_info(out_file, rt, logger)?;
    let hdr_info = hybrid_get_media_info(hdr_target, rt, logger)?;

    let codec = format!("{} {}", out_info.codec, out_info.codec_id).to_lowercase();
    if !(codec.contains("hevc")
        || codec.contains("h.265")
        || codec.contains("h265")
        || codec.contains("hev1")
        || codec.contains("dvhe"))
    {
        return Err("Validation failed: output codec is not HEVC".to_string());
    }

    if out_info.frame_count > 0
        && hdr_info.frame_count > 0
        && out_info.frame_count != hdr_info.frame_count
    {
        return Err(format!(
            "Validation failed: output frame count {} does not match HDR target {}",
            out_info.frame_count, hdr_info.frame_count
        ));
    }

    let profile = hybrid_detect_dv_profile(out_file, rt, logger)?;
    if profile != Some(8) {
        return Err(format!(
            "Validation failed: expected DV profile 8 in output, detected {:?}",
            profile
        ));
    }

    let full_out = run_capture(
        logger,
        &rt.mediainfo,
        &[out_file.as_os_str().to_os_string()],
    )?;
    let lower = full_out.to_lowercase();
    let dv_seen = lower.contains("dolby") && lower.contains("vision") || lower.contains("dvhe.");
    if !dv_seen {
        logger.warn(
            "Validation warning: mediainfo did not detect Dolby Vision metadata (can be false negative)",
        );
    }

    Ok(())
}

fn process_hybrid(
    dv_source: &Path,
    hdr_target: &Path,
    custom_output: Option<&Path>,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    logger.step("Hybrid mode");

    if !dv_source.exists() {
        return Err(format!("DV source file not found: {}", dv_source.display()));
    }
    if !hdr_target.exists() {
        return Err(format!(
            "HDR target file not found: {}",
            hdr_target.display()
        ));
    }

    if fs::canonicalize(dv_source).ok() == fs::canonicalize(hdr_target).ok() {
        return Err("DV source and HDR target must be different files".to_string());
    }

    let output_path = if let Some(custom) = custom_output {
        custom.to_path_buf()
    } else {
        let dir = hdr_target
            .parent()
            .ok_or_else(|| format!("Invalid HDR target path: {}", hdr_target.display()))?;
        let stem = hdr_target
            .file_stem()
            .ok_or_else(|| format!("Invalid HDR target filename: {}", hdr_target.display()))?
            .to_string_lossy()
            .to_string();
        dir.join(format!("{stem}.DV8.Hybrid.mkv"))
    };

    if output_path.exists() {
        logger.warn(&format!(
            "Output file already exists, skipping: {}",
            output_path.display()
        ));
        return Ok(());
    }

    let tmp_dir = hdr_target
        .parent()
        .ok_or_else(|| format!("Invalid HDR target path: {}", hdr_target.display()))?;
    let base = hdr_target
        .file_stem()
        .ok_or_else(|| format!("Invalid HDR target filename: {}", hdr_target.display()))?
        .to_string_lossy()
        .to_string();

    let hybrid_rpu = tmp_dir.join(format!("{base}.hybrid.rpu.bin"));
    let hybrid_aligned_rpu = tmp_dir.join(format!("{base}.hybrid.aligned.rpu.bin"));
    let hybrid_hevc = tmp_dir.join(format!("{base}.hybrid.hevc"));
    let hybrid_injected_hevc = tmp_dir.join(format!("{base}.hybrid.injected.hevc"));
    let hybrid_editor_json = tmp_dir.join(format!("{base}.hybrid.editor.json"));

    let mut cleanup = CleanupGuard::new(logger.clone());
    cleanup.add(&hybrid_rpu);
    cleanup.add(&hybrid_aligned_rpu);
    cleanup.add(&hybrid_hevc);
    cleanup.add(&hybrid_injected_hevc);
    cleanup.add(&hybrid_editor_json);

    logger.step("0 | Determine output path");
    logger.ok(&format!("Hybrid output: {}", output_path.display()));

    logger.step("1 | Gather media info");
    let dv_info = hybrid_get_media_info(dv_source, rt, logger)?;
    let hdr_info = hybrid_get_media_info(hdr_target, rt, logger)?;

    logger.step("2 | Detect DV profile");
    let dv_profile = hybrid_detect_dv_profile(dv_source, rt, logger)?;
    logger.ok(&format!(
        "Detected DV profile: {}",
        dv_profile
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    ));

    logger.step("3 | Preflight checks");
    let has_fail = hybrid_preflight_checks(&dv_info, &hdr_info, dv_profile, logger);
    if has_fail {
        return Err("Hybrid preflight failed".to_string());
    }

    logger.step("4 | Disk space check");
    let out_dir = output_path
        .parent()
        .ok_or_else(|| format!("Invalid output path: {}", output_path.display()))?;
    check_disk_space_hybrid(hdr_target, out_dir, logger)?;

    if rt.dry_run {
        logger.ok("[DRY RUN] Preflight completed. Mutating steps were skipped.");
        logger.ok(&format!(
            "[DRY RUN] Would extract RPU from {}",
            dv_source.display()
        ));
        logger.ok("[DRY RUN] Would compute/apply alignment editor config");
        logger.ok(&format!(
            "[DRY RUN] Would extract HEVC track from {}",
            hdr_target.display()
        ));
        logger.ok("[DRY RUN] Would inject RPU and remux final MKV");
        logger.ok("[DRY RUN] Would validate output and delete both originals on success");
        cleanup.clear();
        return Ok(());
    }

    logger.step("5 | Extract RPU from DV source");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("extract-rpu"),
            OsString::from("-i"),
            dv_source.as_os_str().to_os_string(),
            OsString::from("-o"),
            hybrid_rpu.as_os_str().to_os_string(),
        ],
    )?;

    let dv_rpu_frames = hybrid_get_rpu_frame_count(&hybrid_rpu, rt, logger)?;
    if dv_rpu_frames == 0 {
        return Err("RPU extraction yielded 0 frames".to_string());
    }

    logger.step("6 | Compute alignment strategy");
    let fps = fps_from_info(&hdr_info)
        .or_else(|| fps_from_info(&dv_info))
        .unwrap_or(23.976);

    let strategy = hybrid_compute_alignment(
        dv_rpu_frames,
        hdr_info.frame_count,
        dv_info.duration_ms,
        hdr_info.duration_ms,
        fps,
    );

    logger.ok(&format!("Alignment strategy: {}", strategy.description));
    if strategy.high_risk {
        logger.warn("Alignment marked HIGH RISK due to large frame difference");
    }

    let abs_diff = dv_rpu_frames.abs_diff(hdr_info.frame_count);
    if abs_diff > (fps * 2.0).round() as u64 {
        let dv_chapters = hybrid_extract_chapters(dv_source, rt, logger);
        let hdr_chapters = hybrid_extract_chapters(hdr_target, rt, logger);
        if !dv_chapters.is_empty() && !hdr_chapters.is_empty() {
            if let Some(ch_off) = hybrid_chapter_offset(&dv_chapters, &hdr_chapters) {
                logger.warn(&format!(
                    "Chapter alignment signal: best interval offset = {ch_off} (secondary heuristic)"
                ));
            }
        }
    }

    logger.step("7 | Apply editor (mode conversion + alignment)");
    hybrid_build_editor_json(
        &strategy,
        dv_profile,
        &dv_info,
        &hdr_info,
        &hybrid_editor_json,
    )?;

    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("editor"),
            OsString::from("-i"),
            hybrid_rpu.as_os_str().to_os_string(),
            OsString::from("-j"),
            hybrid_editor_json.as_os_str().to_os_string(),
            OsString::from("-o"),
            hybrid_aligned_rpu.as_os_str().to_os_string(),
        ],
    )?;

    let aligned_frames = hybrid_get_rpu_frame_count(&hybrid_aligned_rpu, rt, logger)?;
    if aligned_frames != hdr_info.frame_count {
        logger.warn(&format!(
            "Aligned RPU frames ({aligned_frames}) do not match HDR target frames ({}) - inject-rpu will auto-handle residual mismatch",
            hdr_info.frame_count
        ));
    }

    logger.step("8 | Extract HEVC from HDR target");
    let track_id = get_hevc_track_id(hdr_target, rt, logger)?;
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvextract,
        &[
            OsString::from("tracks"),
            hdr_target.as_os_str().to_os_string(),
            OsString::from(format!("{}:{}", track_id, hybrid_hevc.display())),
        ],
    )?;

    logger.step("9 | Inject RPU into HEVC");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("inject-rpu"),
            OsString::from("-i"),
            hybrid_hevc.as_os_str().to_os_string(),
            OsString::from("-r"),
            hybrid_aligned_rpu.as_os_str().to_os_string(),
            OsString::from("-o"),
            hybrid_injected_hevc.as_os_str().to_os_string(),
        ],
    )?;

    logger.step("10 | Remux final MKV");
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.mkvmerge,
        &[
            OsString::from("-o"),
            output_path.as_os_str().to_os_string(),
            OsString::from("-D"),
            hdr_target.as_os_str().to_os_string(),
            hybrid_injected_hevc.as_os_str().to_os_string(),
            OsString::from("--track-order"),
            OsString::from("1:0"),
        ],
    )?;

    logger.step("11 | Validate output");
    hybrid_validate_output(&output_path, hdr_target, rt, logger)?;

    logger.step("12 | Cleanup and delete originals");
    let _ = fs::remove_file(&hybrid_rpu);
    let _ = fs::remove_file(&hybrid_aligned_rpu);
    let _ = fs::remove_file(&hybrid_hevc);
    let _ = fs::remove_file(&hybrid_injected_hevc);
    let _ = fs::remove_file(&hybrid_editor_json);
    cleanup.clear();

    fs::remove_file(dv_source)
        .map_err(|e| format!("Failed to delete DV source {}: {e}", dv_source.display()))?;
    fs::remove_file(hdr_target)
        .map_err(|e| format!("Failed to delete HDR target {}: {e}", hdr_target.display()))?;

    logger.log(&format!(
        "Hybrid processed successfully: {} + {} -> {}",
        dv_source.display(),
        hdr_target.display(),
        output_path.display()
    ));
    logger.ok(&format!("Hybrid done: {}", output_path.display()));

    Ok(())
}

fn run(cli: CliArgs) -> AppResult<()> {
    let (rt, logger) = build_runtime(&cli)?;

    logger.log(&format!(
        "Running script with arguments: {}",
        cli.original_args.join(" ")
    ));
    logger.log(&format!("scriptDir: {}", rt.script_dir.display()));
    logger.log(&format!("doviToolPath: {}", rt.dovi_tool.display()));
    logger.log(&format!("mkvextractPath: {}", rt.mkvextract.display()));
    logger.log(&format!("mkvmergePath: {}", rt.mkvmerge.display()));
    logger.log(&format!("mediainfoPath: {}", rt.mediainfo.display()));
    logger.log(&format!("jsonFilePath: {}", rt.json_file.display()));
    logger.log(&format!("output_dir: {}", rt.output_dir.display()));
    logger.log(&format!("save_el_rpu: {}", rt.save_el_rpu));
    logger.log(&format!("DRY_RUN: {}", rt.dry_run));

    if cli.hybrid_mode {
        let dv_source = cli
            .dv_source
            .as_ref()
            .ok_or_else(|| "Missing DV source path".to_string())?;
        let hdr_target = cli
            .hdr_target
            .as_ref()
            .ok_or_else(|| "Missing HDR target path".to_string())?;

        process_hybrid(
            dv_source,
            hdr_target,
            cli.custom_output.as_deref(),
            &rt,
            &logger,
        )?;
        return Ok(());
    }

    let input_path = cli
        .input_path
        .as_ref()
        .ok_or_else(|| "No input path provided".to_string())?;

    if input_path.is_dir() {
        process_directory(input_path, &rt, &logger)?;
    } else if input_path.is_file() {
        if is_dv7_file(input_path, &rt, &logger)? {
            process_file(input_path, &rt, &logger)?;
        } else {
            return Err(format!("Not a DV7 file: {}", input_path.display()));
        }
    } else {
        return Err(format!("Path not found: {}", input_path.display()));
    }

    Ok(())
}

fn main() {
    let cli = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{RED}{e}{NC}");
            usage();
            std::process::exit(1);
        }
    };

    if let Err(e) = run(cli) {
        eprintln!("{RED}{e}{NC}");
        std::process::exit(1);
    }
}
