# DV7 to DV8 Conversion Toolkit

This project converts Dolby Vision Profile 7 MKV files to Profile 8, builds **DV Profile 8 hybrids** (inject DV metadata from a WEB-DL into an HDR10 remux), and includes an automation wrapper for qBittorrent workflows.

## Overview

- `DV7toDV8.sh` is now a launcher for the Rust converter (`dv8_converter`).
- `dv8_converter` performs detection, extraction, DV metadata processing (`dovi_tool`), remux, validation, and cleanup.
- `qbt_autorun_wrapper.sh` runs queued background jobs with locking, per-job logs, and optional qBittorrent cleanup.

`DV7toDV8.sh` tries converters in this order:

1. `DV8_CONVERTER_BIN` (if set)
2. `tools/dv8_converter`
3. `dv8_converter/target/release/dv8_converter`
4. `$CARGO_TARGET_DIR/release/dv8_converter` (if `CARGO_TARGET_DIR` is set)
5. Build from source with `cargo build --release --manifest-path dv8_converter/Cargo.toml`

## Repository Layout

- `DV7toDV8.sh`: launcher and fallback build logic for `dv8_converter`.
- `dv8_converter/`: Rust conversion engine.
- `qbt_autorun_wrapper.sh`: queue/lock wrapper for torrent-triggered runs.
- `config/DV7toDV8.json`: `dovi_tool` editor config used during conversion.
- `tools/`: optional bundled binaries (`mkvextract`, `mkvmerge`, `mediainfo`, `dovi_tool`, `dv8_converter`).
- `dovi_tool/`: upstream Rust source (`dovi_tool` + `dolby_vision` crate).
- `logs/jobs/`: per-job wrapper logs.

## Requirements

- Linux with `bash`.
- `cargo` (required if no ready-to-run `dv8_converter` binary is available).
- `mkvextract` and `mkvmerge` (MKVToolNix).
- `mediainfo`.
- `dovi_tool` (found via system `PATH`, `tools/dovi_tool`, or `dovi_tool/target/release/dovi_tool`).
- `ffmpeg` and `ffprobe` (hybrid mode only: scene-cut sync, grade check, letterbox measurement; found via `PATH` or `tools/`).
- For `qbt_autorun_wrapper.sh`: `curl`, `jq`, `find`, `sha1sum`.

## Public Repo Setup

This repo supports `.env` configuration to avoid committing machine-specific paths.

```bash
cp .env.example .env
# Edit .env with your local paths and NAS mount points
```

Both `DV7toDV8.sh` and `qbt_autorun_wrapper.sh` auto-load `.env` from repo root.
You can point to a different env file with `DV8_ENV_FILE=/path/to/file.env`.

## Usage

### Standard DV7 to DV8 conversion

```bash
# Dry run (no file changes)
./DV7toDV8.sh --dry-run /path/to/movie.mkv

# Convert one file
./DV7toDV8.sh /path/to/movie.mkv

# Convert recursively in a directory
./DV7toDV8.sh /path/to/folder
```

Flags:

- `-n`: do not archive DV7 EL+RPU.
- `-d`, `--debug`: verbose logging, including command traces.
- `--dry-run`: preview mutating operations.
- `-h`, `--help`: show usage.

### Hybrid mode (P8 hybrid maker)

Inject Dolby Vision metadata from a DV source (P5/P8 WEB-DL) into an
HDR10-only target (typically a Blu-ray remux), producing a DV Profile 8
hybrid. This automates The HDR Dissector's P8 hybrid workflow headlessly:

```bash
# Dry run (preflight only)
./DV7toDV8.sh --hybrid --dry-run /path/to/dv_source.mkv /path/to/hdr_target.mkv

# Convert with default output naming (<target>.DV8.Hybrid.mkv)
./DV7toDV8.sh --hybrid /path/to/dv_source.mkv /path/to/hdr_target.mkv

# Convert with explicit output path
./DV7toDV8.sh --hybrid -o /path/to/output.mkv /path/to/dv_source.mkv /path/to/hdr_target.mkv
```

The hybrid pipeline: preflight checks (codec, fps, resolution, static HDR
metadata gate) → extract RPU → **scene-cut sync** (correlates the RPU's
scene cuts against an ffmpeg scan of the target to find and fix the exact
frame offset) → **grade check** (samples brightness windows from both
sources and aborts when the HDR grades differ, e.g. a 4000-nit DV master
vs a 1000-nit Blu-ray trim) → **measured letterbox L5** (cropdetect sets
the active-area metadata) → dovi_tool editor (P5→mode 3, P7/P8→mode 2) →
inject RPU → remux → validation → **post-inject sync verification**
(re-extracts the RPU from the output and requires scene cuts to line up
at offset 0 across the whole runtime).

Hybrid-only flags:

- `--sync <scenes|framecount>`: alignment mode (default `scenes`).
  Single-shot content with no detectable cuts needs `framecount`.
- `--force`: fall back to the framecount heuristic when scene correlation
  fails instead of aborting (output is then verified post-inject). Also
  lets an undetectable DV profile through preflight (assumed P7/P8).
- `--max-offset <frames>`: correlation search window (default 5 minutes).
- `--scene-threshold <f>`: ffmpeg scdet threshold (default 8.0).
- `--grade-check <metadata|sampled|full>`: grade gate depth (default
  `sampled`; `full` measures the entire runtime).
- `--grade-windows <n>`: sample windows for the sampled check (default 6).
- `--skip-grade-check`: bypass the grade gate (use only when you are
  certain the grades match).
- `--letterbox <measured|resolution|off>`: L5 active-area handling
  (default `measured`).
- `--delete-sources`: delete both inputs after successful validation
  (default: keep both).

On validation or sync-verification failure the output is renamed to
`.FAILED.mkv` and kept for inspection, along with the scene-cut lists
(`*.hybrid.dv_scenes.txt`, `*.hybrid.hdr_scenes.txt`).

## qBittorrent Wrapper

Run wrapper per target path:

```bash
./qbt_autorun_wrapper.sh /NAS/Movies/My.File.mkv
```

Behavior summary:

- Queues jobs with per-target and slot locks (`DV8_MAX_PARALLEL_JOBS`).
- Writes index log to `qbt_trigger.log` and detailed logs to `logs/jobs/*.log`.
- Treats `Not a DV7 file` as a non-fatal outcome.
- Captures and repoints media hardlinks for converted files (`DV8_MEDIA_ROOTS`).
- Optionally stops/removes converted torrents in qBittorrent while keeping files (`DV8_QBT_REMOVE_CONVERTED=true`).
- Normalizes `/NAS/...` and `/media/NAS/...` paths when one mount alias is missing.

## Environment Variables

### Converter / launcher

- `DV8_ENV_FILE`: optional path to env file (default `<repo>/.env`).
- `DV8_BASE_DIR`: repo base directory (default wrapper script directory).
- `DV8_SCRIPT_PATH`: converter launcher path (default `$DV8_BASE_DIR/DV7toDV8.sh`).
- `DV8_CONVERTER_BIN`: force converter binary path.
- `DV8_EL_RPU_DIR`: archive directory override (default `/NAS/EL_RPU/` or `/media/NAS/EL_RPU/`).
- `DV8_PROCESSING_LOG_FILE`: conversion log path override.
- `CARGO_TARGET_DIR`: optional target directory used by launcher fallback probing.

### Wrapper

- `DV8_AUTORUN_DRY_RUN` (default `false`)
- `DV8_MAX_PARALLEL_JOBS` (default `1`)
- `DV8_QUEUE_WAIT_SECONDS` (default `15`)
- `DV8_JOB_LOG_RETENTION_DAYS` (default `30`)
- `DV8_TRIGGER_LOG_FILE` (default `$DV8_BASE_DIR/qbt_trigger.log`)
- `DV8_TRIGGER_LOG_MAX_BYTES` (default `10485760`)
- `DV8_RUN_DIR` (default `/tmp/dv8-qbt`)
- `DV8_QBT_API_URL` (default `http://127.0.0.1:8080`)
- `DV8_QBT_REMOVE_CONVERTED` (default `true`)
- `DV8_MEDIA_ROOTS` (default `/NAS/Movies:/NAS/TV Shows:/media/NAS/Movies:/media/NAS/TV Shows`)
- `DV8_EL_RPU_DIR` (passed through to converter)

## Safety Notes

- Standard mode deletes the original input only after successful output validation.
- Hybrid mode keeps both source files by default; `--delete-sources` removes them after validation AND post-inject sync verification succeed.
- Failed hybrid outputs are renamed to `.FAILED.mkv` and kept for inspection.
- `--dry-run` skips mutating commands and is recommended before batch runs.

## Development

```bash
# Build converter
cargo build --release --manifest-path dv8_converter/Cargo.toml

# Optional: build/test upstream dovi_tool
cd dovi_tool
cargo build --release
cargo test --all-features
```
