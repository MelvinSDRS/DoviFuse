# Command-line and developer reference

[Back to the project overview](../README.md).

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
- `ffmpeg` (all modes: full video validation; hybrid also uses scene cuts, grade samples and letterbox measurement; found via `PATH` or `tools/`).
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
- `--archive-dir <path>`: explicit standard-mode EL+RPU archive directory,
  overriding `DV8_EL_RPU_DIR`. Cannot be combined with `-n` or another mode.
- `-d`, `--debug`: verbose logging, including command traces.
- `--dry-run`: preview mutating operations.
- `--tmp-dir <path>`: put large intermediate streams in a separate scratch
  directory (or set `DV8_TMP_DIR`). Each run receives its own subdirectory.
- `--hwaccel <auto|videotoolbox|off>`: select decode acceleration. `auto`
  probes VideoToolbox on macOS and otherwise uses software decoding.
- `--progress <human|jsonl>`: human terminal output (default) or versioned
  machine-readable events for GUI integrations.
- `--check`: perform a read-only validation of an existing DV8 MKV.
- `--repair-sync <frames>`: create a corrected copy for a checker-confirmed
  RPU/video offset. The offset is measured again before any output is written.
  Edge duplication requires `--allow-padding`; the app presents a padding review.
- `-h`, `--help`: show usage.

### Checker mode

Validate a converted, hybrid, or downloaded DV8 file without modifying it:

```bash
./DV7toDV8.sh --check /path/to/movie.dv8.mkv
```

Checker mode verifies the Matroska and HEVC structure, Dolby Vision Profile 8
and HDR10-compatible base layer, parses the RPU, compares video/RPU frame
counts, fully decodes the video stream, and correlates RPU scene changes with
the picture. Failed checks produce a nonzero exit status and no successful
completion event. The decoded frame count is checked independently of container
metadata. Multi-video inputs are rejected to avoid ambiguous track selection.
An inconclusive scene correlation is reported as a warning rather
than a false failure.

When the checker finds a confident, repairable RPU/video offset on an otherwise
valid Profile 8 file, the macOS app offers **Fix**. The repair is
non-destructive: it creates a sibling `*.DV8.Fixed.mkv` file (or a numbered
variant if that name exists), re-confirms the offset from a fresh scene scan,
applies the RPU edit, and then runs strict post-inject validation plus the full
checker. Findings such as missing HDR10 metadata, unreadable media, or an
inconclusive correlation remain report-only because there is no safe automatic
repair.

Standard conversion validates the output profile, HDR10 base, parsed RPU count
and full decode before atomically replacing the source. It preserves video
timestamps. Discarding a Profile 7 FEL loses enhancement-layer reconstruction;
this is not lossless Dolby Vision conversion. Existing EL archives are never
overwritten.

### Hybrid mode (P8 hybrid maker)

Inject Dolby Vision metadata from a Profile 7 or 8.1 source into an HDR10-only
target (typically a Blu-ray remux), producing a Profile 8 hybrid.
Donors require explicit HDR10 compatibility and a 10-bit BT.2020 PQ base
layer with limited range and BT.2020 non-constant matrix coefficients.
Missing or contradictory interpretation fields, HLG/Profile 8.4, and other
unsupported variants fail regardless of `--force`, grade mode, or
`--skip-grade-check`. Before editing, the extracted RPU must also have a
consistent supported profile matching the media probe. Dry runs check media
eligibility only; the extracted-RPU check remains pending until a real run.
Hybrid transfers also require an explicit mapping policy across **every RPU**:

- P8.1 donors must use the supported identity polynomial mapping in all three
  channels. It is preserved with lossless metadata editing (mode 0).
- P7 MEL donors require the same identity mapping; P7 FEL donors use the pinned
  tool's established mode 2 compatibility conversion. FEL mapping and picture
  residuals are discarded by that conversion, which is reported explicitly.
  This does not preserve full FEL reconstruction.
- Non-identity P8/MEL mapping, missing mapping, reused mapping references, and
  unrecognized representations are rejected, including with `--force` or grade
  overrides. There is no automatic mapping removal to make these donors pass.

The re-extracted hybrid output must have identity mapping in every RPU as well.
Same-file sync repair preserves existing mappings because it does not transfer
them to another picture stream. Mapping checks do not establish grade equivalence;
both hybrid sources remain retained. Dry runs leave mapping inspection pending.
**Profile 5 hybrids are disabled on `main`, including with `--force`,
`--skip-grade-check` or metadata-only grading.**

```bash
# Dry run (preflight only)
./DV7toDV8.sh --hybrid --dry-run /path/to/dv_source.mkv /path/to/hdr_target.mkv

# Convert with default output naming (<target>.DV8.Hybrid.mkv)
./DV7toDV8.sh --hybrid /path/to/dv_source.mkv /path/to/hdr_target.mkv

# Convert with explicit output path
./DV7toDV8.sh --hybrid -o /path/to/output.mkv /path/to/dv_source.mkv /path/to/hdr_target.mkv
```

The hybrid pipeline checks supported inputs, extracts the RPU and estimates
one temporal offset from scene evidence. A brightness/chroma screen rejects
measured mismatches and records its coverage; it cannot verify the creative
grade. Adequately observed stable bars can supply a constant target L5 edit.
Temporal edits run before target-indexed L5 edits. The pipeline then injects,
remuxes, decodes the output and verifies metadata and container transport.
Scene anchors and sampled crops do not establish whole-film picture alignment.
Input probe results and equivalent crop observations are reused within each job.
Output checks inspect newly produced media; no measurements are cached across jobs.

Complete target HDR10 static fields are applied to L6 on every hybrid RPU,
even when donor and target container tags match. The mastering minimum uses
0.0001-nit units; explicit zero remains zero. Missing target fields leave donor
L6 intact with reconciliation marked inconclusive. Invalid or unrepresentable
values are rejected. Sync repair preserves L6.
This does not rewrite L9 mastering primaries or establish grade compatibility.
After remuxing, every parsed output RPU is compared with the final edited RPU,
including trims and display metadata; only the encoding CRC is excluded.

Hybrid-only flags:

- `--sync <scenes|framecount>`: alignment mode (default `scenes`).
  `framecount` assumes offset zero only when counts match; it cannot locate edits.
- `--offset <signed frames>`: explicitly set `dv_frame = hdr_frame + offset`
  after inspecting the sources. Fresh scene evidence still rejects contradictory
  offsets or local edits.
- `--allow-padding`: explicitly accept repeating first/last RPU metadata where
  donor pictures are missing. The report records every duplicate operation;
  those pictures remain unverified. Neither `--force` nor an offset approves
  padding implicitly. Same-file checker repairs also require this flag; the app asks before padding.
- `--force`: allow an unverified zero-offset fallback for equal counts when
  scene correlation is inconclusive. Local contradictions still fail.
  Unknown DV profiles still fail; no RPU conversion mode is guessed.
- `--max-offset <frames>`: correlation search window (default 5 minutes).
- `--scene-threshold <f>`: ffmpeg scdet threshold (default 8.0).
- `--grade-check <metadata|sampled|full>`: mismatch-screen coverage (default
  `sampled`). Full mode checks the aligned overlap in intervals of at most five
  seconds and reports missing overlap; it does not certify the creative grade.
  Reports list requested, measured and skipped intervals. Y-derived peak values
  are brightness surrogates, not measured luminance or replacement MaxCLL.
  U/V averages detect some chroma mismatches but cannot establish RGB equivalence.
- `--grade-windows <n>`: sample windows for the sampled check (default 6).
- `--skip-grade-check`: bypass the grade gate (use only when you are
  certain the grades match). Donor eligibility checks still apply.
- `--letterbox <measured|off>`: L5 active-area handling (default `measured`).
  Measured mode requires at least three distinct usable windows, plausible
  crops and sufficient lighting. Reports retain usable/skipped observations
  and disagreement. Variable measured bars or multiple donor L5 presets stop
  measured mode. Short changes between samples remain unverified.
  `off` retains existing donor L5 after review. The old `resolution` option is
  rejected: dimensions alone cannot distinguish scaling, padding and cropping.
- Both source files are always retained. The legacy `--delete-sources` flag
  is deprecated, hidden from help, and ignored with a warning. Remove sources
  separately after reviewing the result.

Scene correlation estimates a global offset. Job reports (`--report <path>`,
automatic in the app) retain matched anchors,
unverified intervals over the full duration, and local offset contradictions.
A short alternate edit cannot be approved by the global match ratio alone.
Intervals between anchors still require picture inspection; neither matching
counts nor matching scene flags establishes whole-film picture equivalence.
Post-inject checks use scene cuts decoded from the finished output.

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
- Both remux paths preserve video language, name, flags, display geometry and
  supported stereo metadata, and retain the original track order. Container
  header verification runs before standard replacement and hybrid completion. Canonically equivalent Unicode accents are accepted.
  Non-pixel display units and unsupported stereo values fail conservatively.
- Hybrid mode retains both sources while full-timeline active-picture validation
  remains inconclusive, including when `--delete-sources` is requested.
- Failed hybrid outputs are renamed to `.FAILED.mkv` and kept for inspection.
- `--dry-run` skips mutating commands and is recommended before batch runs.

## Development

### Native macOS app

`macapp/` contains the Apple Silicon SwiftUI **DV8 Maker** application and its
self-contained packaging script. On the target Mac:

```bash
DEVELOPER_DIR="$(xcode-select -p)" ./macapp/build-macos-app.sh
```

The output is `dist/DV8 Maker.app`. The app bundles the converter, dovi_tool,
FFmpeg, MKVToolNix, MediaInfo, config, and third-party license notices (including
the locked converter Rust dependencies); none of
those tools need to be installed separately at runtime.

```bash
# Build converter
cargo build --release --manifest-path dv8_converter/Cargo.toml

# Optional: build/test upstream dovi_tool
cd dovi_tool
cargo build --release
cargo test --all-features
```

### L5 interval verification

The converter retains target-frame L5 editing and output re-extraction checks.
Unresolved measured bars or variable aspect ratios require review.

## Saved job reports

Use `--report /new/path.json` with standard conversion, `--hybrid`, `--check`, or
`--repair-sync`. The destination must not exist; its parent directory must exist.
For example:

```bash
./DV7toDV8.sh --check --report /tmp/movie-check.json /path/to/movie.mkv
```

Schema version 1 separates `execution` (`running`, `completed`, `failed`,
`cancelled`) from `validation` (`pass`, `warn`, `fail`, `inconclusive`). Reports
include checks and repair hints, explicitly scoped coverage, measurements when
available, tool paths/versions, arguments/overrides, elapsed time, input identities
before/after, and completed output identities. Filesystem identities include size,
timestamps, device and inode; they are not cryptographic content hashes. A
standalone checker cannot establish donor-grade compatibility.

The converter reserves the report before tool setup and saves the terminal
report before emitting JSONL completion. Existing/replaced report files are
preserved. Unwritable reports fail the job; an interrupted write can leave an
incomplete report and must never be treated as acceptance. Malformed CLI arguments
or an unavailable destination fail before a report can be created. Dry runs and
jobs without recorded checks remain inconclusive. Standard conversion retains
its documented in-place behavior; report persistence is not a transaction that
rolls back an already completed conversion.

The Mac app requests a report for every operation under
`~/Library/Application Support/DV8 Maker/Reports/`. **Show report** reveals it in
Finder. The app waits for both output streams to finish, verifies agreement with
the saved terminal report, and binds repair availability to the unchanged checked
file identity.
