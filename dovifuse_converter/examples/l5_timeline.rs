//! Validate an externally authored effective-L5 export and emit a separate-pass
//! editor config. This utility does not infer active areas or create a movie.
#[allow(dead_code)]
#[path = "../src/hybrid/active_area.rs"]
mod active_area;
use std::{fs, io::Write, path::PathBuf};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 && args.len() != 6 {
        return Err(
            "Usage: l5_timeline EXPECTED_EXPORT FRAMES WIDTH HEIGHT NEW_CONFIG [ACTUAL_EXPORT]"
                .into(),
        );
    }
    let input = fs::read_to_string(&args[0]).map_err(|e| e.to_string())?;
    let frames = args[1].parse().map_err(|_| "Invalid count")?;
    let w = args[2].parse().map_err(|_| "Invalid width")?;
    let h = args[3].parse().map_err(|_| "Invalid height")?;
    let expected = active_area::Timeline::from_export(&input, frames, w, h)?;
    if let Some(actual) = args.get(5) {
        expected.verify_export(
            &fs::read_to_string(actual).map_err(|e| e.to_string())?,
            w,
            h,
        )?;
    }
    let path = PathBuf::from(&args[4]);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(
        &serde_json::to_vec_pretty(&expected.editor_config()).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("Validated {} L5 intervals over {} frames; config must be applied after alignment. No picture-area acceptance.",expected.intervals.len(),frames);
    Ok(())
}
