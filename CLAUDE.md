# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This repo is a **Dolby Vision Profile 7 to Profile 8 conversion toolkit**. The main entry point is `DV7toDV8.sh`, a bash script that automates converting DV7 MKV files to DV8 by orchestrating several external tools.

## Repository Structure

- `DV7toDV8.sh` — Main conversion script (bash). Accepts a file or directory, detects DV7 MKVs via mediainfo, and runs a 6-step pipeline: mkvextract → dovi_tool demux → dovi_tool convert → dovi_tool extract-rpu → rename → mkvmerge remux.
- `config/DV7toDV8.json` — dovi_tool editor config used during conversion (mode 2, remove_mapping).
- `tools/` — Bundled Linux binaries: `mkvextract`, `mkvmerge`, `mediainfo`. The script prefers system-installed versions, falling back to these.
- `dovi_tool/` — Git submodule checkout of [quietvoid/dovi_tool](https://github.com/quietvoid/dovi_tool) (Rust). Built locally; the script uses `dovi_tool/target/release/dovi_tool`.
- `processing_log.txt` — Append-only log written by DV7toDV8.sh at runtime.

## Building dovi_tool

```bash
cd dovi_tool
cargo build --release
```

Requires Rust >= 1.70.0. On Linux, fontconfig is needed (or build with `--no-default-features --features internal-font` to bypass).

The built binary lands at `dovi_tool/target/release/dovi_tool`.

To run tests:
```bash
cd dovi_tool
cargo test
```

## Running the Conversion Script

```bash
# Single file
./DV7toDV8.sh /path/to/movie.mkv

# Entire directory (recursively finds DV7 MKVs)
./DV7toDV8.sh /path/to/folder/

# Flags
#   -n         Do NOT save the DV7 EL+RPU file (default: saved to /media/NAS/EL_RPU/)
#   -d|--debug Enable debug logging (bash set -x)
```

## Key Behavior Notes

- The script **deletes the original MKV** after successful conversion (line 114). The output file gets "DV8" in its name, replacing DV/DoVi/DOVI/Dovi/Dolby.Vision patterns.
- EL+RPU files are archived to `/media/NAS/EL_RPU/` by default (use `-n` to discard instead).
- DV7 detection uses mediainfo, checking for "Profile 7" in HDR format or `dvhe.07` codec string.
- The `output_dir` path is hardcoded in the script — change it if the NAS mount point differs.
