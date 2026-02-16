# Repository Guidelines

## Project Structure & Module Organization
This repository is a DV7-to-DV8 conversion toolkit with a Bash entrypoint and an embedded Rust toolchain:
- `DV7toDV8.sh`: main conversion pipeline for MKV files.
- `qbt_autorun_wrapper.sh`: queue/lock wrapper for automated trigger-based runs.
- `config/DV7toDV8.json`: conversion/editor settings consumed by `dovi_tool`.
- `tools/`: bundled binaries (`mkvextract`, `mkvmerge`, `mediainfo`, `dovi_tool`) used as fallbacks.
- `dovi_tool/`: upstream Rust project (CLI + `dolby_vision` crate), with source in `dovi_tool/src`, integration tests in `dovi_tool/tests`, docs in `dovi_tool/docs`, and examples/assets in `dovi_tool/assets`.
- `logs/`: runtime job logs (generated).

## Build, Test, and Development Commands
- `cd dovi_tool && cargo build --release`: build CLI at `dovi_tool/target/release/dovi_tool`.
- `cd dovi_tool && cargo test --all-features`: run tests for the CLI crate.
- `cd dovi_tool && cargo test --all-features --all-targets --manifest-path dolby_vision/Cargo.toml`: run crate/library targets.
- `cd dovi_tool && cargo fmt --check && cargo clippy --all-features --all-targets --tests -- --deny warnings`: match CI formatting and lint gates.
- `./DV7toDV8.sh --dry-run /path/to/file_or_dir`: validate pipeline behavior without file changes.

## Coding Style & Naming Conventions
- Bash: keep strict/safe patterns (`set -o pipefail`, quoted expansions, explicit error handling).
- Bash naming: `snake_case` for functions/locals, `UPPER_SNAKE_CASE` for environment-style constants.
- Rust: default `rustfmt` style, idiomatic `snake_case` modules/functions, `UpperCamelCase` types.
- Keep scripts and config ASCII; prefer concise functions over monolithic blocks.

## Testing Guidelines
- Primary test framework is Rust `cargo test` (integration tests under `dovi_tool/tests/hevc` and `dovi_tool/tests/rpu`).
- New behavior should include or update integration tests named by command area (example: `tests/hevc/inject_rpu.rs`).
- For script changes, run at least one `--dry-run` against a sample path and capture key log output in the PR.

## Commit & Pull Request Guidelines
- Commit messages are short, imperative, and specific (examples from history: `Fix clippy`, `Update dependencies`).
- Prefer format: `[scope] imperative summary` (example: `script: handle duplicate output names`).
- PRs should include:
  - What changed and why.
  - Commands run (build/test/lint/dry-run).
  - Risk notes for file deletion, remux behavior, or archive path handling.
