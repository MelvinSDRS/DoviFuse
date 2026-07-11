use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const RED: &str = "\x1b[0;31m";
pub(crate) const GREEN: &str = "\x1b[0;32m";
pub(crate) const YELLOW: &str = "\x1b[1;33m";
pub(crate) const BLUE: &str = "\x1b[0;34m";
pub(crate) const NC: &str = "\x1b[0m";

const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Logger {
    log_file: PathBuf,
    debug: bool,
    utc_offset_secs: i64,
}

/// Query the local UTC offset once at startup (e.g. "+0200" -> 7200).
fn local_utc_offset_secs() -> i64 {
    if let Ok(out) = Command::new("date").arg("+%z").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1i64 } else { 1i64 };
                let h = s[1..3].parse::<i64>().unwrap_or(0);
                let m = s[3..5].parse::<i64>().unwrap_or(0);
                return sign * (h * 3600 + m * 60);
            }
        }
    }
    0
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl Logger {
    pub(crate) fn new(log_file: PathBuf, debug: bool) -> Self {
        Self {
            log_file,
            debug,
            utc_offset_secs: local_utc_offset_secs(),
        }
    }

    pub(crate) fn rotate_log(&self) {
        if let Ok(meta) = fs::metadata(&self.log_file) {
            if meta.len() > LOG_MAX_BYTES {
                let old = self.log_file.with_extension("txt.old");
                let _ = fs::rename(&self.log_file, old);
            }
        }
    }

    fn now_string(&self) -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let local = secs + self.utc_offset_secs;
        let (days, rem) = (local.div_euclid(86_400), local.rem_euclid(86_400));
        let (y, mo, d) = civil_from_days(days);
        format!(
            "{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    pub(crate) fn log(&self, msg: &str) {
        let line = format!("{} - {}\n", self.now_string(), msg);
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }

    pub(crate) fn dbg(&self, msg: &str) {
        if self.debug {
            self.log(&format!("DEBUG: {msg}"));
        }
    }

    pub(crate) fn step(&self, msg: &str) {
        println!("\n{BLUE}=== {msg} ==={NC}\n");
    }

    pub(crate) fn ok(&self, msg: &str) {
        println!("{GREEN}{msg}{NC}");
    }

    pub(crate) fn warn(&self, msg: &str) {
        println!("{YELLOW}{msg}{NC}");
    }

    pub(crate) fn err(&self, msg: &str) {
        eprintln!("{RED}{msg}{NC}");
    }

    pub(crate) fn preflight_status(&self, label: &str, msg: &str) {
        match label {
            "PASS" => self.ok(&format!("[PASS] {msg}")),
            "WARN" => self.warn(&format!("[WARN] {msg}")),
            "FAIL" => self.err(&format!("[FAIL] {msg}")),
            _ => println!("[{label}] {msg}"),
        }
    }
}
