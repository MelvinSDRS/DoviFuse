mod cancellation;
mod checker;
mod cli;
mod enhancement;
mod exec;
mod ffmpeg;
mod fsutil;
mod hybrid;
mod logger;
mod mediainfo;
mod pq;
mod remux;
mod report;
mod runtime;
mod standard;

use std::path::PathBuf;

use cli::{parse_args, usage, CliArgs};
use exec::AppResult;
use logger::{NC, RED};
use runtime::build_runtime;

/// Resolve to an absolute path so `.parent()` never yields "" for paths
/// given as bare filenames (df/mkvextract choke on empty dirs). Falls back
/// to cwd-joining for paths that don't exist yet (e.g. -o outputs).
fn absolutize(p: &PathBuf) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    if p.is_relative() {
        if let Ok(cwd) = std::env::current_dir() {
            return cwd.join(p);
        }
    }
    p.clone()
}

fn run(cli: &CliArgs, rt: runtime::Runtime, logger: logger::Logger) -> AppResult<()> {
    logger.job_started(cli.operation());

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

    if let Some(offset) = cli.repair_sync_offset {
        let input = cli
            .input_path
            .as_ref()
            .map(absolutize)
            .ok_or_else(|| "No repair input provided".to_string())?;
        let custom_output = cli.custom_output.as_ref().map(absolutize);
        let output = hybrid::repair_sync(
            &input,
            offset,
            cli.hybrid.allow_padding,
            custom_output.as_deref(),
            &rt,
            &logger,
        )?;
        if rt.dry_run {
            logger.completed(&output);
        } else {
            checker::check_file_with_phase_offset(&output, &rt, &logger, 15)?;
        }
        return Ok(());
    }

    if cli.checker_mode {
        let input = cli
            .input_path
            .as_ref()
            .map(absolutize)
            .ok_or_else(|| "No checker input provided".to_string())?;
        checker::check_file(&input, &rt, &logger)?;
        return Ok(());
    }

    if cli.hybrid_mode {
        let dv_source = cli
            .dv_source
            .as_ref()
            .map(absolutize)
            .ok_or_else(|| "Missing DV source path".to_string())?;
        let hdr_target = cli
            .hdr_target
            .as_ref()
            .map(absolutize)
            .ok_or_else(|| "Missing HDR target path".to_string())?;

        let custom_output = cli.custom_output.as_ref().map(absolutize);
        hybrid::process_hybrid(
            &dv_source,
            &hdr_target,
            custom_output.as_deref(),
            &cli.hybrid,
            &rt,
            &logger,
        )?;
        return Ok(());
    }

    let input_path = cli
        .input_path
        .as_ref()
        .map(absolutize)
        .ok_or_else(|| "No input path provided".to_string())?;
    let input_path = &input_path;

    if input_path.is_dir() {
        standard::process_directory(input_path, &rt, &logger)?;
    } else if input_path.is_file() {
        if standard::is_dv7_file(input_path, &rt, &logger)? {
            standard::process_file(input_path, &rt, &logger)?;
        } else {
            return Err(format!("Not a DV7 file: {}", input_path.display()));
        }
    } else {
        return Err(format!("Path not found: {}", input_path.display()));
    }

    Ok(())
}

fn main() {
    cancellation::install();
    let cli = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{RED}{e}{NC}");
            usage();
            std::process::exit(1);
        }
    };

    let json_progress = cli.progress == cli::ProgressMode::JsonLines;
    let result = execute(&cli);
    if let Err(e) = result {
        if json_progress {
            let event = if cancellation::requested() {
                "cancelled"
            } else {
                "failed"
            };
            println!(
                "{}",
                serde_json::json!({"version":1,"event":event,"error":e})
            );
        }
        eprintln!("{RED}{e}{NC}");
        std::process::exit(1);
    }
}

fn execute(cli: &CliArgs) -> AppResult<()> {
    let mut report = report::JobReport::begin(cli)?;
    let mut result = match build_runtime(cli) {
        Ok((rt, mut logger)) => {
            if let Some(report) = report.as_mut() {
                report.tools(&rt);
                logger.attach_report(report.recorder.clone());
            }
            run(cli, rt, logger)
        }
        Err(e) => Err(e),
    };
    if let Some(report) = report.as_mut() {
        if let Some(error) = report.input_change_error() {
            result = Err(error);
        }
        let final_event = report.finish(&result, cancellation::requested())?;
        if cli.progress == cli::ProgressMode::JsonLines {
            if result.is_ok() && !cancellation::requested() {
                for event in report
                    .recorder
                    .events()
                    .iter()
                    .filter(|e| e["event"] == "completed")
                {
                    println!("{event}");
                }
            }
            println!("{final_event}");
        }
    }
    if cancellation::requested() {
        return Err("Operation cancelled".into());
    }
    result
}
