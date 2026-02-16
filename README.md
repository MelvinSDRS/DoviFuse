# DV7 to DV8 Conversion Toolkit

This repository provides a Bash-based workflow to convert Dolby Vision Profile 7 MKVs into Profile 8 MKVs.

## What It Does
- Detects DV7 files with `mediainfo`.
- Extracts video, demuxes EL/RPU, converts metadata to DV8 via `dovi_tool`, and remuxes the final MKV.
- Optionally archives the original DV7 EL+RPU stream.
- Supports single-file and recursive directory processing.

## Repository Layout
- `DV7toDV8.sh`: main conversion script.
- `qbt_autorun_wrapper.sh`: queued/background wrapper for automated triggers.
- `config/DV7toDV8.json`: `dovi_tool` conversion settings (currently mode 2, mapping removal).
- `tools/`: bundled fallback binaries (`mkvextract`, `mkvmerge`, `mediainfo`, `dovi_tool`).
- `dovi_tool/`: Rust source for upstream `dovi_tool` + `dolby_vision` crate.
- `logs/jobs/`: per-job logs written by the wrapper.

## Requirements
- Linux shell environment.
- `mkvextract`, `mkvmerge`, `mediainfo`, and `dovi_tool`.
- The scripts prefer system tools (`command -v`) and fall back to `tools/`.

## Quick Start
```bash
# Dry run first (no file modifications)
./DV7toDV8.sh --dry-run /path/to/movie.mkv

# Convert one file
./DV7toDV8.sh /path/to/movie.mkv

# Convert all .mkv files under a directory
./DV7toDV8.sh /path/to/folder
```

Flags:
- `-n`: do not archive DV7 EL+RPU output.
- `-d` / `--debug`: verbose debug logging.
- `--dry-run`: preview actions only.

## Environment Variables
- `DV8_EL_RPU_DIR`: override archive directory (default `/NAS/EL_RPU/` or `/media/NAS/EL_RPU/`).
- `DV8_PROCESSING_LOG_FILE`: override conversion log path.
- `DV8_AUTORUN_DRY_RUN`, `DV8_MAX_PARALLEL_JOBS`, `DV8_QUEUE_WAIT_SECONDS`: wrapper behavior controls.

## Safety Notes
- On successful conversion, `DV7toDV8.sh` deletes the original input MKV.
- The script keeps the source if output is empty or suspiciously small.
- Always run `--dry-run` and validate output naming/path behavior before batch processing.

## Build `dovi_tool` From Source (Optional)
```bash
cd dovi_tool
cargo build --release
cargo test --all-features
```

If this repository is moved, update absolute paths in `qbt_autorun_wrapper.sh` (`SCRIPT` and `BASE_DIR`).
