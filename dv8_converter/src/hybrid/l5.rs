//! Apply target-indexed L5 only after RPU alignment; verify re-extracted output.
use super::active_area::Timeline;
use crate::{
    exec::{run_status, AppResult},
    logger::Logger,
    runtime::Runtime,
};
use std::{fs, path::Path};

pub fn export_timeline(
    rpu: &Path,
    json_path: &Path,
    frames: u64,
    canvas: (u32, u32),
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<Timeline> {
    run_status(
        logger,
        false,
        true,
        &rt.dovi_tool,
        &[
            "export".into(),
            "-i".into(),
            rpu.as_os_str().to_owned(),
            "-d".into(),
            format!("level5={}", json_path.display()).into(),
        ],
    )?;
    let text = fs::read_to_string(json_path).map_err(|e| e.to_string())?;
    Timeline::from_export(&text, frames, canvas.0, canvas.1)
}
#[allow(clippy::too_many_arguments)]
pub fn apply_after_alignment(
    timeline: &Timeline,
    aligned: &Path,
    output: &Path,
    config: &Path,
    export: &Path,
    canvas: (u32, u32),
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<()> {
    fs::write(
        config,
        serde_json::to_vec_pretty(&timeline.editor_config()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    run_status(
        logger,
        false,
        true,
        &rt.dovi_tool,
        &[
            "editor".into(),
            "-i".into(),
            aligned.as_os_str().to_owned(),
            "-j".into(),
            config.as_os_str().to_owned(),
            "-o".into(),
            output.as_os_str().to_owned(),
        ],
    )?;
    export_timeline(output, export, timeline.frames, canvas, rt, logger)?;
    timeline.verify_export(
        &fs::read_to_string(export).map_err(|e| e.to_string())?,
        canvas.0,
        canvas.1,
    )?;
    fs::rename(output, aligned).map_err(|e| format!("Failed to adopt L5-edited RPU: {e}"))?;
    logger.ok("Target-frame L5 edits verified after RPU alignment");
    Ok(())
}
