# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This repo is a **Dolby Vision conversion toolkit** with two modes:

1. **Standard mode**: convert DV Profile 7 MKV files to Profile 8 (`dovi_tool` demux → convert → remux).
2. **Hybrid mode** (`--hybrid`): build a DV Profile 8 hybrid by injecting the RPU from a DV source (P5/P8 WEB-DL) into an HDR10-only target (Blu-ray remux), with scene-cut sync, a brightness grade gate, measured letterbox L5, and post-inject sync verification.

The engine is the Rust binary `dv8_converter`; `DV7toDV8.sh` is a thin launcher that finds or builds it.

## Repository Structure

- `dv8_converter/` — Rust conversion engine (the real entry point).
  - `src/main.rs` — dispatch only.
  - `src/cli.rs` — arg parsing, `HybridOptions`, usage text. Hybrid-only flags are rejected outside `--hybrid`.
  - `src/runtime.rs` — external tool resolution (`which()` → `tools/`; ffmpeg/ffprobe probed with `-version`, single dash). `DV8_SCRIPT_DIR` overrides the script dir (which otherwise falls back to cwd — needed when running the binary from elsewhere).
  - `src/exec.rs` — process wrappers, `CleanupGuard` (removes registered temp files on failure/drop).
  - `src/pq.rs` — ST 2084 PQ math, 10-bit limited-range code → PQ/nits.
  - `src/ffmpeg.rs` — ffmpeg invocations + pure parsers: `signalstats` (per-frame YAVG/YMAX), `scdet` (scene cuts, exact frame numbers under CFR), `cropdetect`, `sample_windows` (10–85% of runtime).
  - `src/standard.rs` — DV7→DV8 single-file pipeline.
  - `src/hybrid/` — hybrid pipeline modules:
    - `mod.rs` — `process_hybrid`, 15 steps: output path → media info → DV profile → preflight → disk/ffmpeg check → extract RPU → alignment → grade check → letterbox L5 → editor → extract HEVC → inject RPU → remux → validate → post-inject sync verification → cleanup.
    - `scenes.rs` — DV cuts via `dovi_tool export -d scenes`; `correlate_scene_cuts` (offset voting, ±1 tolerance, dominance ≥ 1.5, per-tercile agreement).
    - `align.rs` — `alignment_from_offset` mirrors dovi_tool editor semantics: `remove` ranges in ORIGINAL RPU index space applied first; `duplicate` ops after, in post-remove space, applied offset-descending.
    - `grade.rs` — static metadata gate + sampled/full brightness comparison in PQ space. Windows are cropped to the active picture (letterbox bars would poison YAVG).
    - `letterbox.rs` — cropdetect over sample windows, min bar per edge, diffed against the RPU's own L5 (`export -d level5`).
    - `editor.rs` — serde `EditorConfig` mirroring dovi_tool's `EditConfig` (which uses `deny_unknown_fields`, so schema drift fails loudly). Snapshot-tested.
    - `preflight.rs`, `validate.rs` — gate checks; post-inject verification re-extracts the output RPU, requires frame count == target and scene cuts at offset 0.
- `DV7toDV8.sh` — launcher; probes `DV8_CONVERTER_BIN` → `tools/dv8_converter` → `dv8_converter/target/release/` → cargo build.
- `qbt_autorun_wrapper.sh` — qBittorrent queue/lock wrapper.
- `config/DV7toDV8.json` — dovi_tool editor config for standard mode (mode 2, remove_mapping).
- `tools/` — bundled Linux binaries: `mkvextract`, `mkvmerge`, `mediainfo`, `dovi_tool`, static `ffmpeg`/`ffprobe` (gitignored). System versions preferred.
- `dovi_tool/` — vendored [quietvoid/dovi_tool](https://github.com/quietvoid/dovi_tool) source. `src/dovi/editor.rs` and `exporter.rs` are the authoritative reference for editor/export semantics the hybrid code must mirror.

## Building and Testing

```bash
# Converter (primary)
cd dv8_converter
cargo build --release
cargo test          # unit + snapshot tests, no external tools needed
cargo clippy

# Vendored dovi_tool (only if tools/dovi_tool is missing)
cd dovi_tool && cargo build --release
```

## Running

```bash
# Standard DV7→DV8
./DV7toDV8.sh [--dry-run] /path/to/movie.mkv     # or a directory

# Hybrid: DV source + HDR10 target
./DV7toDV8.sh --hybrid [-o out.mkv] dv_source.mkv hdr_target.mkv
```

Hybrid flags: `--sync scenes|framecount`, `--force`, `--max-offset <frames>`, `--scene-threshold <f>`, `--grade-check metadata|sampled|full`, `--grade-windows <n>`, `--skip-grade-check`, `--letterbox measured|resolution|off`, `--delete-sources`. See README for details.

When invoking the built binary directly (not via the launcher), set `DV8_SCRIPT_DIR=<repo>` so bundled tools and config resolve, and optionally `DV8_PROCESSING_LOG_FILE` to redirect the log.

## Key Behavior Notes

- **Standard mode replaces the original MKV in place** (same filename) after successful conversion — intentional, so qbt hardlink repointing keeps working. `make_dv8_name` (rename to `*.DV8.*`) exists in `standard.rs` but is deliberately disabled. EL+RPU archived to `DV8_EL_RPU_DIR` (default `/media/NAS/EL_RPU/`; `-n` to skip).
- **Hybrid mode keeps both sources** by default (`--delete-sources` opts into deletion). Failed outputs are renamed `.FAILED.mkv` and kept, along with `*.hybrid.dv_scenes.txt` / `*.hybrid.hdr_scenes.txt` for inspection.
- Scene-cut sync fails honestly on content without detectable cuts (single-shot demos, periodic synthetic cuts) — use `--sync framecount` or `--force`; the post-inject verification then warns instead of verifying.
- P5 sources use editor mode 3, P7/P8 use mode 2. An undetectable DV profile fails preflight unless `--force` (which assumes P7/P8) — a misdetected P5 would otherwise skip the IPT-PQ-c2 conversion. A P5 base layer is NOT a valid HDR10 target — preflight warns about missing HDR metadata/non-PQ transfer.
- Grade check thresholds live in `hybrid/grade.rs`: window fail at mean |ΔPQ| > 0.015, gate fails on ≥2 bad windows or p99 peak nits ratio > 1.5. `--grade-check metadata` hard-fails when either source lacks static metadata (there is no measured fallback in that mode).
- `.env` at repo root is auto-loaded by both shell scripts (see `.env.example`).
