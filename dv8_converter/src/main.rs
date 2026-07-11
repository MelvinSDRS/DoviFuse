mod cli;
mod ffmpeg;
mod exec;
mod fsutil;
mod hybrid;
mod logger;
mod pq;
mod mediainfo;
mod runtime;
mod standard;

use std::path::PathBuf;

use cli::{parse_args, usage, CliArgs};
use exec::AppResult;
use logger::{NC, RED};
use runtime::build_runtime;

/// Resolve to an absolute path so `.parent()` never yields "" for inputs
/// given as bare filenames (df/mkvextract choke on empty dirs).
fn absolutize(p: &PathBuf) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())
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
            .map(absolutize)
            .ok_or_else(|| "Missing DV source path".to_string())?;
        let hdr_target = cli
            .hdr_target
            .as_ref()
            .map(absolutize)
            .ok_or_else(|| "Missing HDR target path".to_string())?;

        hybrid::process_hybrid(
            &dv_source,
            &hdr_target,
            cli.custom_output.as_deref(),
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
