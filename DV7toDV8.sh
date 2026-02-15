#!/bin/bash
########################################################################
#  DV7toDV8.sh – Convert Dolby Vision Profile-7 MKVs to Profile-8
#  Author : 2025-04-27 (ChatGPT-assisted)
#  Flags  :
#     -n        : do NOT keep the DV7 EL+RPU file
#     -d|--debug: extra log lines
########################################################################

# ========== Colours ==========
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BLUE='\033[0;34m'; CYAN='\033[0;36m'; NC='\033[0m'

# ========== Globals ==========
rawDir="$(cd -- "$(dirname -- "$0")" &>/dev/null && pwd)"
scriptDir="$(realpath "$rawDir")"     # follows symlinks
LOG_FILE="$scriptDir/processing_log.txt"
doviToolPath="$scriptDir/dovi_tool/target/release/dovi_tool"
mkvextractPath="$(command -v mkvextract || echo "$scriptDir/tools/mkvextract")"
mkvmergePath="$(command -v mkvmerge || echo "$scriptDir/tools/mkvmerge")"
mediainfoPath="$(command -v mediainfo || echo "$scriptDir/tools/mediainfo")"
jsonFilePath="$scriptDir/config/DV7toDV8.json"
output_dir="/media/NAS/EL_RPU/"          # where to archive EL+RPU
mkdir -p "$output_dir"

save_el_rpu=true
DEBUG=false
[[ "$1" == "--debug" ]] && { DEBUG=true; shift; set -x; }   # bash -x

# ========== Logging helpers ==========
log()  { echo "$(date '+%F %T') - $*" >> "$LOG_FILE"; }
dbg()  { $DEBUG && log "DEBUG: $*"; }
step() { echo -e "\n${BLUE}=== $1 ===${NC}\n"; }
ok()   { echo -e "${GREEN}$1${NC}"; }
err()  { echo -e "${RED}$1${NC}"; }

# ========== Dependency check ==========
for tool in "$mkvextractPath" "$mkvmergePath" "$mediainfoPath" "$doviToolPath"; do
  [[ -x "$tool" ]] || {
    err "Missing tool: ${tool:-'(empty)'}"
    log "Missing tool: ${tool:-'(empty)'}"
    exit 1
}
done

# ========== Parse flags ==========
while [[ "$1" == -* ]]; do
  case "$1" in
      -n) save_el_rpu=false ;;
   -d|--debug) DEBUG=true ;;
      --) shift; break ;;
      *)  err "Unknown flag $1"; exit 1 ;;
  esac
  shift
done

[[ $# -eq 0 ]] && { err "No file/folder specified"; exit 1; }
input_path="$1"

log "Running script with arguments: $*"
log "scriptDir: $scriptDir"
log "doviToolPath: $doviToolPath"
log "mkvextractPath: $mkvextractPath"
log "mkvmergePath: $mkvmergePath"
log "mediainfoPath: $mediainfoPath"
log "jsonFilePath: $jsonFilePath"
log "output_dir: $output_dir"
# ========== Functions ==========
is_dv7_file() {
  local file="$1"
  [[ $(basename "$file") == ._* ]] && { log "$file skipped: hidden"; return 1; }
  local info="$("$mediainfoPath" "$file")"
  echo "$info" | grep -qiE 'HDR format.*(Dolby[[:space:].]?Vision).*Profile[[:space:]]*7' && { log "$file DV7 detected"; return 0; }
  echo "$info" | grep -qi 'dvhe\.07' && { log "$file DV7 detected (dvhe.07)"; return 0; }
  log "$file not DV7"; return 1
}

process_file() {
  local file="$1" input_dir mkvBase BL_EL_RPU_HEVC DV7_EL_RPU_HEVC DV8_BL_RPU_HEVC DV8_RPU_BIN
  input_dir="$(dirname "$file")"
  mkvBase="$(basename "$file" .mkv)"
  BL_EL_RPU_HEVC="$input_dir/$mkvBase.BL_EL_RPU.hevc"
  DV7_EL_RPU_HEVC="$input_dir/$mkvBase.DV7.EL_RPU.hevc"
  DV8_BL_RPU_HEVC="$input_dir/$mkvBase.DV8.BL_RPU.hevc"
  DV8_RPU_BIN="$input_dir/$mkvBase.DV8.RPU.bin"

  step "1 | Extract BL+EL+RPU"
  "$mkvextractPath" tracks "$file" "0:$BL_EL_RPU_HEVC" || { log "$file fail: mkvextract"; return 1; }

  step "2 | Demux EL+RPU"
  "$doviToolPath" demux --el-only "$BL_EL_RPU_HEVC" -e "$DV7_EL_RPU_HEVC" || { log "$file fail: demux"; return 1; }
  $save_el_rpu && mv "$DV7_EL_RPU_HEVC" "$output_dir" || rm -f "$DV7_EL_RPU_HEVC"

  step "3 | Convert to DV8"
  "$doviToolPath" --edit-config "$jsonFilePath" convert --discard "$BL_EL_RPU_HEVC" -o "$DV8_BL_RPU_HEVC" || { log "$file fail: convert"; return 1; }

  step "4 | Extract RPU (optional)"
  "$doviToolPath" extract-rpu "$DV8_BL_RPU_HEVC" -o "$DV8_RPU_BIN" 2>/dev/null || true

  step "5 | Prepare output name"
  if   [[ "$mkvBase" =~ \.DV\.    ]]; then mkvBase="${mkvBase/.DV./.DV8.}"
  elif [[ "$mkvBase" =~ \.DoVi\.  ]]; then mkvBase="${mkvBase/.DoVi./.DV8.}"
  elif [[ "$mkvBase" =~ \.DOVI\.  ]]; then mkvBase="${mkvBase/.DOVI./.DV8.}"
  elif [[ "$mkvBase" =~ \.Dovi\.  ]]; then mkvBase="${mkvBase/.Dovi./.DV8.}"
  elif [[ "$mkvBase" =~ \.Dolby\.Vision\. ]]; then mkvBase="${mkvBase/.Dolby.Vision./.DV8.}"
  else mkvBase="${mkvBase}.DV8"
  fi

  step "6 | Remux final MKV"
  "$mkvmergePath" -o "$input_dir/$mkvBase.mkv" -D "$file" "$DV8_BL_RPU_HEVC" --track-order 1:0 || { log "$file fail: mkvmerge"; return 1; }

  # ---------- Cleanup ----------
  rm -f "$BL_EL_RPU_HEVC" "$DV8_BL_RPU_HEVC" "$DV8_RPU_BIN"
  rm -f "$file"
  log "$file processed successfully → $mkvBase.mkv"
}

process_directory() {
  local dir="$1" count=0
  step "Scan folder $dir"
  while IFS= read -r -d '' f; do
    [[ "$f" == *.mkv ]] && is_dv7_file "$f" && { process_file "$f"; ((count++)); }
  done < <(find "$dir" -type f -name "*.mkv" -print0 | sort -z)
  ok "$count file(s) converted."
}

# ========== Main ==========
if   [[ -d "$input_path" ]]; then process_directory "$input_path"
elif [[ -f "$input_path" ]]; then is_dv7_file "$input_path" && process_file "$input_path" || { err "Not DV7"; exit 1; }
else err "Path not found: $input_path"; exit 1; fi