//! Durable, opt-in job reports. Filesystem identities are not content hashes.
use crate::{cli::CliArgs, exec::AppResult, runtime::Runtime};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Default)]
pub(crate) struct Recorder(Arc<Mutex<Vec<Value>>>);
impl Recorder {
    pub(crate) fn record(&self, event: Value) {
        let mut events = self.0.lock().unwrap();
        // Directory batches may contain many files; never silently drop checks.
        // Crossing the report budget marks the report failed and stops collecting.
        match events.len().cmp(&16_384) {
            std::cmp::Ordering::Less => events.push(event),
            std::cmp::Ordering::Equal => events.push(json!({"event":"check_result","key":"report_capacity","status":"fail","label":"Report capacity","detail":"Report event budget exceeded; complete results unavailable"})),
            std::cmp::Ordering::Greater => (),
        }
    }
    pub(crate) fn events(&self) -> Vec<Value> {
        self.0.lock().unwrap().clone()
    }
}

pub(crate) fn identity(path: &Path) -> Value {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match std::fs::metadata(path) {
        Ok(m) => {
            let mut v = json!({"path":resolved,"kind":if m.is_dir(){"directory"}else{"file"},"bytes":m.len(),
                "modified_unix_ns":m.modified().ok().and_then(|t|t.duration_since(UNIX_EPOCH).ok()).map(|t|t.as_nanos().to_string()),
                "identity_method":"filesystem-stat; not a content hash"});
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                v["device"] = json!(m.dev());
                v["inode"] = json!(m.ino());
                v["ctime_seconds"] = json!(m.ctime());
                v["ctime_nanoseconds"] = json!(m.ctime_nsec());
            }
            v
        }
        Err(e) => json!({"path":resolved,"unavailable":e.to_string()}),
    }
}

pub(crate) fn validation(events: &[Value], execution: &str, dry: bool) -> &'static str {
    if execution == "failed"
        || events
            .iter()
            .any(|e| e["event"] == "check_result" && e["status"] == "fail")
    {
        return "fail";
    }
    if execution != "completed" || dry {
        return "inconclusive";
    }
    let checks: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "check_result")
        .collect();
    if checks.is_empty()
        || checks
            .iter()
            .any(|e| !matches!(e["status"].as_str(), Some("pass" | "warn" | "skipped")))
    {
        return "inconclusive";
    }
    if checks.iter().any(|e| e["status"] != "pass")
        || events.iter().any(|e| e["level"] == "warning")
    {
        return "warn";
    }
    "pass"
}

pub(crate) struct JobReport {
    file: File,
    pub(crate) path: PathBuf,
    pub(crate) recorder: Recorder,
    data: Value,
    inputs: Vec<PathBuf>,
    started: Instant,
}
impl JobReport {
    pub(crate) fn begin(cli: &CliArgs) -> AppResult<Option<Self>> {
        let Some(path) = cli.report.as_ref() else {
            return Ok(None);
        };
        let path = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path);
        let inputs: Vec<_> = [&cli.input_path, &cli.dv_source, &cli.hdr_target]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Cannot reserve new report {}: {e}", path.display()))?;
        let mut result = Self {
            file,
            path,
            recorder: Recorder::default(),
            inputs,
            started: Instant::now(),
            data: json!({"schema_version":1,"operation":cli.operation(),"execution":"running","validation":"inconclusive",
                "started_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().to_string(),
                "arguments":cli.original_args,"dry_run":cli.dry_run,"converter_version":env!("CARGO_PKG_VERSION"),
                "coverage":{"scope":"Only explicitly recorded checks; no implicit full-film picture or donor-grade validation","frames_measured":null},
                "limits":["Filesystem identities do not prove byte-for-byte integrity.","Process completion does not imply full Dolby Vision validation."]}),
        };
        result.data["inputs_before"] = json!(result
            .inputs
            .iter()
            .map(|p| identity(p))
            .collect::<Vec<_>>());
        result.write()?;
        Ok(Some(result))
    }
    pub(crate) fn input_change_error(&self) -> Option<String> {
        if self.data["operation"] == "standard" {
            return None;
        }
        let now = json!(self.inputs.iter().map(|p| identity(p)).collect::<Vec<_>>());
        (now != self.data["inputs_before"]).then(|| "Input filesystem identity changed during analysis; checked results cannot authorize repair".into())
    }

    pub(crate) fn tools(&mut self, rt: &Runtime) {
        self.data["tools"] = json!([
            ("dovi_tool", Some(&rt.dovi_tool), "--version"),
            ("mkvmerge", Some(&rt.mkvmerge), "--version"),
            ("mkvextract", Some(&rt.mkvextract), "--version"),
            ("mediainfo", Some(&rt.mediainfo), "--version"),
            ("ffmpeg", rt.ffmpeg.as_ref(), "-version"),
            ("ffprobe", rt.ffprobe.as_ref(), "-version")
        ]
        .into_iter()
        .map(|(name, path, flag)| {
            let version = path
                .and_then(|p| crate::exec::probe(p, flag).ok())
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .take(2)
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            json!({"name":name,"path":path,"version":version})
        })
        .collect::<Vec<_>>());
    }
    fn write(&mut self) -> AppResult<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = self.file.metadata().map_err(|e| e.to_string())?;
            let named = std::fs::symlink_metadata(&self.path)
                .map_err(|e| format!("Report path disappeared: {e}"))?;
            if held.dev() != named.dev() || held.ino() != named.ino() || !named.is_file() {
                return Err("Report path was replaced during processing".into());
            }
        }
        let bytes = serde_json::to_vec_pretty(&self.data).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .and_then(|_| self.file.write_all(&bytes))
            .and_then(|_| self.file.set_len(bytes.len() as u64))
            .and_then(|_| crate::fsutil::sync_file(&self.file))
            .map_err(|e| format!("Cannot save report {}: {e}", self.path.display()))
    }
    pub(crate) fn finish(&mut self, result: &AppResult<()>, cancelled: bool) -> AppResult<Value> {
        let events = self.recorder.events();
        if events.iter().any(|e| e["key"] == "report_capacity") {
            return Err("Report capacity exceeded; no complete report available".into());
        }
        let execution = if cancelled {
            "cancelled"
        } else if result.is_err() {
            "failed"
        } else {
            "completed"
        };
        self.data["execution"] = json!(execution);
        self.data["validation"] =
            json!(validation(&events, execution, self.data["dry_run"] == true));
        self.data["error"] = json!(result.as_ref().err());
        self.data["elapsed_seconds"] = json!(self.started.elapsed().as_secs_f64());
        self.data["inputs_after"] =
            json!(self.inputs.iter().map(|p| identity(p)).collect::<Vec<_>>());
        self.data["outputs"] = json!(events
            .iter()
            .filter(|e| e["event"] == "completed")
            .filter_map(|e| e["output"].as_str())
            .map(|p| identity(Path::new(p)))
            .collect::<Vec<_>>());
        self.data["checks"] = json!(events
            .iter()
            .filter(|e| e["event"] == "check_result")
            .collect::<Vec<_>>());
        self.data["measurements"] = json!(events
            .iter()
            .filter(|e| e["event"] == "measurement")
            .collect::<Vec<_>>());
        self.data["warnings"] = json!(events
            .iter()
            .filter(|e| e["level"] == "warning")
            .collect::<Vec<_>>());
        self.write()?;
        Ok(
            json!({"version":1,"event":"job_finalized","execution":execution,"status":self.data["validation"],"report":self.path}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outcome_never_promotes_missing_unknown_or_cancelled_checks() {
        let pass = json!({"event":"check_result","status":"pass"});
        assert_eq!(validation(&[], "completed", false), "inconclusive");
        assert_eq!(
            validation(std::slice::from_ref(&pass), "completed", false),
            "pass"
        );
        assert_eq!(
            validation(std::slice::from_ref(&pass), "cancelled", false),
            "inconclusive"
        );
        assert_eq!(
            validation(std::slice::from_ref(&pass), "completed", true),
            "inconclusive"
        );
        assert_eq!(
            validation(
                &[
                    pass.clone(),
                    json!({"event":"check_result","status":"future"})
                ],
                "completed",
                false
            ),
            "inconclusive"
        );
        assert_eq!(
            validation(
                &[pass, json!({"event":"check_result","status":"fail"})],
                "completed",
                false
            ),
            "fail"
        );
    }
}
