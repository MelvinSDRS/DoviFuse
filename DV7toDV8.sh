#!/bin/bash
########################################################################
#  DV7toDV8.sh – Convert Dolby Vision Profile-7 MKVs to Profile-8
#  Author : 2025-04-27 (ChatGPT-assisted)
#  Flags  :
#     -n          : do NOT keep the DV7 EL+RPU file
#     -d|--debug  : extra log lines + bash set -x
#     --dry-run   : show what would be done without modifying files
#     -h|--help   : show usage
########################################################################

set -o pipefail

# ========== Colours ==========
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BLUE='\033[0;34m'; CYAN='\033[0;36m'; NC='\033[0m'

# ========== Usage ==========
usage() {
  cat <<'USAGE'
Usage: DV7toDV8.sh [OPTIONS] <file.mkv|directory>

Convert Dolby Vision Profile 7 MKV files to Profile 8.

Options:
  -n          Do NOT save the DV7 EL+RPU file (default: archived to NAS)
  -d, --debug Enable debug logging (verbose + bash set -x)
  --dry-run   Show what would be done without modifying any files
  -h, --help  Show this help message

Examples:
  DV7toDV8.sh /path/to/movie.mkv
  DV7toDV8.sh /path/to/folder/
  DV7toDV8.sh -n /path/to/movie.mkv
USAGE
}

# ========== Globals ==========
rawDir="$(cd -- "$(dirname -- "$0")" &>/dev/null && pwd)"
scriptDir="$(realpath "$rawDir")"     # follows symlinks
LOG_FILE="${DV8_PROCESSING_LOG_FILE:-$scriptDir/processing_log.txt}"
LOG_MAX_BYTES=10485760  # 10 MB
mkvextractPath="$(command -v mkvextract || echo "$scriptDir/tools/mkvextract")"
mkvmergePath="$(command -v mkvmerge || echo "$scriptDir/tools/mkvmerge")"
mediainfoPath="$(command -v mediainfo || echo "$scriptDir/tools/mediainfo")"
jsonFilePath="$scriptDir/config/DV7toDV8.json"
if [[ -n "${DV8_EL_RPU_DIR:-}" ]]; then
  output_dir="$DV8_EL_RPU_DIR"
elif [[ -d "/NAS" ]]; then
  output_dir="/NAS/EL_RPU/"
else
  output_dir="/media/NAS/EL_RPU/"
fi

save_el_rpu=true
DEBUG=false
DRY_RUN=false

# ========== Tool resolution ==========
resolve_executable_tool() {
  local candidate
  for candidate in "$@"; do
    [[ -n "$candidate" ]] || continue
    [[ -x "$candidate" ]] || continue
    "$candidate" --version >/dev/null 2>&1 && { echo "$candidate"; return 0; }
  done
  return 1
}

doviToolPath="$(resolve_executable_tool \
  "$scriptDir/tools/dovi_tool" \
  "$(command -v dovi_tool 2>/dev/null)" \
  "$scriptDir/dovi_tool/target/release/dovi_tool" || true)"

# ========== Parse flags ==========
while [[ "$1" == -* ]]; do
  case "$1" in
    -n)         save_el_rpu=false ;;
    -d|--debug) DEBUG=true; set -x ;;
    --dry-run)  DRY_RUN=true ;;
    -h|--help)  usage; exit 0 ;;
    --)         shift; break ;;
    *)          echo -e "${RED}Unknown flag $1${NC}"; usage; exit 1 ;;
  esac
  shift
done

[[ $# -eq 0 ]] && { echo -e "${RED}No file/folder specified${NC}"; usage; exit 1; }
input_path="$1"

# ========== Logging helpers ==========
rotate_log() {
  if [[ -f "$LOG_FILE" ]]; then
    local size
    size=$(stat -c%s "$LOG_FILE" 2>/dev/null || echo 0)
    if (( size > LOG_MAX_BYTES )); then
      mv "$LOG_FILE" "$LOG_FILE.old"
    fi
  fi
}

rotate_log
log()  { echo "$(date '+%F %T') - $*" >> "$LOG_FILE"; }
dbg()  { [[ "$DEBUG" == true ]] && log "DEBUG: $*"; }
step() { echo -e "\n${BLUE}=== $1 ===${NC}\n"; }
ok()   { echo -e "${GREEN}$1${NC}"; }
warn() { echo -e "${YELLOW}$1${NC}"; }
err()  { echo -e "${RED}$1${NC}"; }

# ========== Dependency check ==========
for tool in "$mkvextractPath" "$mkvmergePath" "$mediainfoPath" "$doviToolPath"; do
  [[ -x "$tool" ]] || {
    err "Missing tool: ${tool:-'(empty)'}"
    log "Missing tool: ${tool:-'(empty)'}"
    exit 1
  }
done

# Create archive directory only for real runs
if [[ "$save_el_rpu" == true && "$DRY_RUN" != true ]]; then
  mkdir -p "$output_dir" || { err "Cannot create archive dir: $output_dir"; exit 1; }
fi

log "Running script with arguments: $*"
log "scriptDir: $scriptDir"
log "doviToolPath: $doviToolPath"
log "mkvextractPath: $mkvextractPath"
log "mkvmergePath: $mkvmergePath"
log "mediainfoPath: $mediainfoPath"
log "jsonFilePath: $jsonFilePath"
log "output_dir: $output_dir"
log "save_el_rpu: $save_el_rpu"
log "DRY_RUN: $DRY_RUN"

# ========== Cleanup trap ==========
cleanup_files=()
cleanup() {
  if (( ${#cleanup_files[@]} > 0 )); then
    dbg "Cleaning up intermediate files: ${cleanup_files[*]}"
    rm -f "${cleanup_files[@]}"
  fi
}
trap cleanup EXIT

# ========== Functions ==========

get_hevc_track_id() {
  local file="$1"
  local track_id
  track_id=$("$mediainfoPath" --Output='Video;%ID%\n' "$file" | head -n1)
  # mediainfo reports 1-based IDs; mkvextract uses 0-based
  if [[ -n "$track_id" ]] && (( track_id > 0 )); then
    echo $(( track_id - 1 ))
  else
    echo 0
  fi
}

check_disk_space() {
  local file="$1" input_dir
  input_dir="$(dirname "$file")"
  local file_size_kb avail_kb
  file_size_kb=$(du -k "$file" | cut -f1)
  avail_kb=$(df -k "$input_dir" | awk 'NR==2 {print $4}')
  # Need roughly 3x the file size for intermediate files
  local needed_kb=$(( file_size_kb * 3 ))
  if (( avail_kb < needed_kb )); then
    err "Insufficient disk space in $input_dir"
    err "  Available: $(( avail_kb / 1024 )) MB, Estimated need: $(( needed_kb / 1024 )) MB"
    return 1
  fi
  dbg "Disk space OK: $(( avail_kb / 1024 )) MB available, ~$(( needed_kb / 1024 )) MB needed"
  return 0
}

is_dv7_file() {
  local file="$1"
  [[ $(basename "$file") == ._* ]] && { log "$file skipped: hidden"; return 1; }
  local info
  info="$("$mediainfoPath" "$file")"
  echo "$info" | grep -qiE 'HDR format.*(Dolby[[:space:].]?Vision).*Profile[[:space:]]*7' && { log "$file DV7 detected"; return 0; }
  echo "$info" | grep -qi 'dvhe\.07' && { log "$file DV7 detected (dvhe.07)"; return 0; }
  log "$file not DV7"; return 1
}

make_dv8_name() {
  local name="$1"
  # Replace known DV-related tags only when they are separated tokens.
  local result
  result=$(echo "$name" | sed -E 's/(^|[._-])(Dolby[._-]?Vision|DoVi|DOVI|Dovi|DV7?|DV)([._-]|$)/\1DV8\3/Ig')
  # If no substitution happened, append .DV8
  if [[ "$result" == "$name" ]]; then
    result="${name}.DV8"
  fi
  # Clean up any double dots from substitution
  result="${result//../.}"
  echo "$result"
}

process_file() {
  local file="$1" input_dir mkvBase BL_EL_RPU_HEVC DV7_EL_RPU_HEVC DV8_BL_RPU_HEVC DV8_RPU_BIN
  input_dir="$(dirname "$file")"
  mkvBase="$(basename "$file" .mkv)"
  BL_EL_RPU_HEVC="$input_dir/$mkvBase.BL_EL_RPU.hevc"
  DV7_EL_RPU_HEVC="$input_dir/$mkvBase.DV7.EL_RPU.hevc"
  DV8_BL_RPU_HEVC="$input_dir/$mkvBase.DV8.BL_RPU.hevc"
  DV8_RPU_BIN="$input_dir/$mkvBase.DV8.RPU.bin"

  # Register intermediate files for cleanup on failure
  cleanup_files=("$BL_EL_RPU_HEVC" "$DV7_EL_RPU_HEVC" "$DV8_BL_RPU_HEVC" "$DV8_RPU_BIN")

  # Prepare output name early to check for collisions
  local outBase
  outBase="$(make_dv8_name "$mkvBase")"
  local outFile="$input_dir/$outBase.mkv"

  if [[ -f "$outFile" ]]; then
    warn "Output file already exists, skipping: $outFile"
    log "$file skipped: output already exists: $outFile"
    cleanup_files=()
    return 0
  fi

  # Check disk space before starting
  check_disk_space "$file" || { cleanup_files=(); return 1; }

  if [[ "$DRY_RUN" == true ]]; then
    ok "[DRY RUN] Would convert: $file"
    ok "[DRY RUN] Output: $outFile"
    [[ "$save_el_rpu" == true ]] && ok "[DRY RUN] Archive EL+RPU to: $output_dir"
    cleanup_files=()
    return 0
  fi

  # Detect video track ID
  local track_id
  track_id=$(get_hevc_track_id "$file")
  dbg "Using video track ID: $track_id"

  step "1 | Extract BL+EL+RPU"
  "$mkvextractPath" tracks "$file" "$track_id:$BL_EL_RPU_HEVC" || { err "mkvextract failed"; log "$file fail: mkvextract"; return 1; }

  step "2 | Demux EL+RPU"
  "$doviToolPath" demux --el-only "$BL_EL_RPU_HEVC" -e "$DV7_EL_RPU_HEVC" || { err "demux failed"; log "$file fail: demux"; return 1; }

  if [[ "$save_el_rpu" == true ]]; then
    mv "$DV7_EL_RPU_HEVC" "$output_dir" || { err "Failed to archive EL+RPU to $output_dir"; log "$file fail: mv EL+RPU"; return 1; }
  else
    rm -f "$DV7_EL_RPU_HEVC"
  fi

  step "3 | Convert to DV8"
  "$doviToolPath" --edit-config "$jsonFilePath" convert --discard "$BL_EL_RPU_HEVC" -o "$DV8_BL_RPU_HEVC" || { err "convert failed"; log "$file fail: convert"; return 1; }

  step "4 | Extract RPU (optional)"
  "$doviToolPath" extract-rpu "$DV8_BL_RPU_HEVC" -o "$DV8_RPU_BIN" 2>/dev/null || true

  step "5 | Prepare output name"
  dbg "Output name: $outBase.mkv"

  step "6 | Remux final MKV"
  "$mkvmergePath" -o "$outFile" -D "$file" "$DV8_BL_RPU_HEVC" --track-order 1:0 || { err "mkvmerge failed"; log "$file fail: mkvmerge"; return 1; }

  # Validate output before deleting original
  local orig_size out_size
  orig_size=$(stat -c%s "$file" 2>/dev/null || echo 0)
  out_size=$(stat -c%s "$outFile" 2>/dev/null || echo 0)
  if (( out_size == 0 )); then
    err "Output file is empty — keeping original: $file"
    log "$file fail: output is empty"
    rm -f "$outFile"
    return 1
  fi
  # Warn if output is suspiciously small (< 50% of original)
  if (( orig_size > 0 && out_size * 100 / orig_size < 50 )); then
    warn "Output is much smaller than original ($(( out_size / 1048576 )) MB vs $(( orig_size / 1048576 )) MB) — keeping original"
    log "$file warning: output suspiciously small, keeping original"
    return 1
  fi

  # ---------- Cleanup ----------
  rm -f "$BL_EL_RPU_HEVC" "$DV8_BL_RPU_HEVC" "$DV8_RPU_BIN"
  cleanup_files=()  # Prevent trap from double-deleting
  rm -f "$file"
  log "$file processed successfully -> $outBase.mkv"
  ok "Done: $outBase.mkv"
}

process_directory() {
  local dir="$1" count=0 failures=0
  step "Scan folder $dir"
  while IFS= read -r -d '' f; do
    if is_dv7_file "$f"; then
      if process_file "$f"; then
        ((count++))
      else
        ((failures++))
        err "Failed: $f"
      fi
    fi
  done < <(find "$dir" -type f -name "*.mkv" -print0 | sort -z)
  ok "$count file(s) converted."
  if (( failures > 0 )); then
    warn "$failures file(s) failed."
  fi
}

# ========== Main ==========
if [[ -d "$input_path" ]]; then
  process_directory "$input_path"
elif [[ -f "$input_path" ]]; then
  if is_dv7_file "$input_path"; then
    process_file "$input_path" || { err "Conversion failed for $input_path"; exit 1; }
  else
    err "Not a DV7 file: $input_path"
    exit 1
  fi
else
  err "Path not found: $input_path"
  exit 1
fi
