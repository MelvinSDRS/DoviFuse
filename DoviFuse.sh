#!/bin/bash
set -o pipefail

rawDir="$(cd -- "$(dirname -- "$0")" &>/dev/null && pwd)"
scriptDir="$(realpath "$rawDir")"
envFile="${DOVIFUSE_ENV_FILE-$scriptDir/.env}"
if [[ -f "$envFile" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$envFile"
  set +a
fi
manifestPath="$scriptDir/dovifuse_converter/Cargo.toml"

bundledBin="$scriptDir/tools/dovifuse_converter"
releaseBin="$scriptDir/dovifuse_converter/target/release/dovifuse_converter"
cargoTargetReleaseBin=""
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  cargoTargetReleaseBin="$CARGO_TARGET_DIR/release/dovifuse_converter"
fi

run_converter_bin() {
  local bin="$1"
  shift

  [[ -x "$bin" ]] || return 1

  (
    exec env DOVIFUSE_SCRIPT_DIR="$scriptDir" "$bin" "$@"
  )
  local rc=$?
  if (( rc == 126 || rc == 127 )); then
    echo "Warning: unable to execute '$bin' (rc=$rc), trying fallback..." >&2
    return 2
  fi

  exit "$rc"
}

try_local_binaries() {
  local status=0
  local converter_bin="${DOVIFUSE_CONVERTER_BIN-}"

  if [[ -n "$converter_bin" ]]; then
    run_converter_bin "$converter_bin" "$@"
    status=$?
    if (( status == 2 )); then
      :
    elif (( status == 1 )); then
      echo "Warning: DOVIFUSE_CONVERTER_BIN is not executable: $converter_bin" >&2
    fi
  fi

  run_converter_bin "$bundledBin" "$@"
  status=$?
  if (( status == 2 )); then
    :
  fi

  run_converter_bin "$releaseBin" "$@"
  status=$?
  if (( status == 2 )); then
    :
  fi

  if [[ -n "$cargoTargetReleaseBin" ]]; then
    run_converter_bin "$cargoTargetReleaseBin" "$@"
    status=$?
    if (( status == 2 )); then
      :
    fi
  fi

  return 0
}

try_local_binaries "$@"

# Build from source on first run
if ! command -v cargo >/dev/null 2>&1; then
  echo "Error: cargo is required to build dovifuse_converter (not found in PATH)." >&2
  exit 1
fi

cargo build --release --manifest-path "$manifestPath" || {
  echo "Error: failed to build dovifuse_converter" >&2
  exit 1
}

try_local_binaries "$@"

exec env DOVIFUSE_SCRIPT_DIR="$scriptDir" cargo run --release --manifest-path "$manifestPath" -- "$@"
