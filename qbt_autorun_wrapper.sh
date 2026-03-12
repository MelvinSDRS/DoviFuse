#!/bin/bash
set -u

script_dir="$(cd -- "$(dirname -- "$0")" &>/dev/null && pwd)"
env_file="${DV8_ENV_FILE:-$script_dir/.env}"
if [[ -f "$env_file" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$env_file"
  set +a
fi

BASE_DIR="${DV8_BASE_DIR:-$script_dir}"
SCRIPT="${DV8_SCRIPT_PATH:-$BASE_DIR/DV7toDV8.sh}"
LOG_FILE="${DV8_TRIGGER_LOG_FILE:-$BASE_DIR/qbt_trigger.log}"
LOG_MAX_BYTES="${DV8_TRIGGER_LOG_MAX_BYTES:-10485760}"
JOB_LOG_DIR="$BASE_DIR/logs/jobs"
RUN_DIR="${DV8_RUN_DIR:-/tmp/dv8-qbt}"

TARGET="${1:-}"
DEFAULT_ARCHIVE_DIR="/NAS/EL_RPU"
if [[ ! -d /NAS && -d /media/NAS ]]; then
  DEFAULT_ARCHIVE_DIR="/media/NAS/EL_RPU"
fi
ARCHIVE_DIR="${DV8_EL_RPU_DIR:-$DEFAULT_ARCHIVE_DIR}"
DRY_RUN_FLAG="${DV8_AUTORUN_DRY_RUN:-false}"
MAX_PARALLEL_JOBS="${DV8_MAX_PARALLEL_JOBS:-1}"
QUEUE_WAIT_SECONDS="${DV8_QUEUE_WAIT_SECONDS:-15}"
JOB_LOG_RETENTION_DAYS="${DV8_JOB_LOG_RETENTION_DAYS:-30}"
QBT_API_URL="${DV8_QBT_API_URL:-http://127.0.0.1:8080}"
QBT_REMOVE_CONVERTED="${DV8_QBT_REMOVE_CONVERTED:-true}"
MEDIA_ROOTS_CSV="${DV8_MEDIA_ROOTS:-/NAS/Movies:/NAS/TV Shows:/media/NAS/Movies:/media/NAS/TV Shows}"

mkdir -p "$(dirname "$LOG_FILE")" "$JOB_LOG_DIR"

ensure_run_dir() {
  local dir="$1"
  mkdir -p "$dir" 2>/dev/null || return 1
  [[ -w "$dir" && -x "$dir" ]]
}

if ! ensure_run_dir "$RUN_DIR"; then
  RUN_DIR="/tmp/dv8-qbt-$(id -u)"
  if ! ensure_run_dir "$RUN_DIR"; then
    printf '%s - ERROR: unable to prepare run dir (%s)\n' "$(date '+%F %T')" "$RUN_DIR" >> "$LOG_FILE" 2>/dev/null || true
    exit 1
  fi
fi

if [[ ! "$MAX_PARALLEL_JOBS" =~ ^[0-9]+$ ]] || (( MAX_PARALLEL_JOBS < 1 )); then
  MAX_PARALLEL_JOBS=1
fi
if [[ ! "$QUEUE_WAIT_SECONDS" =~ ^[0-9]+$ ]] || (( QUEUE_WAIT_SECONDS < 1 )); then
  QUEUE_WAIT_SECONDS=15
fi
if [[ ! "$LOG_MAX_BYTES" =~ ^[0-9]+$ ]] || (( LOG_MAX_BYTES < 1048576 )); then
  LOG_MAX_BYTES=10485760
fi
if [[ ! "$JOB_LOG_RETENTION_DAYS" =~ ^[0-9]+$ ]] || (( JOB_LOG_RETENTION_DAYS < 1 )); then
  JOB_LOG_RETENTION_DAYS=30
fi

now() { date '+%F %T'; }

map_mount_alias() {
  local path="$1"
  case "$path" in
    /NAS/*)
      if [[ -e "/media$path" ]]; then
        echo "/media$path"
        return
      fi
      ;;
    /media/NAS/*)
      local alt="${path#/media}"
      if [[ -e "$alt" ]]; then
        echo "$alt"
        return
      fi
      ;;
  esac
  echo "$path"
}

map_archive_dir() {
  local path="$1"
  case "$path" in
    /NAS/*)
      if [[ ! -d /NAS && -d /media/NAS ]]; then
        echo "/media$path"
        return
      fi
      ;;
    /media/NAS/*)
      if [[ ! -d /media/NAS && -d /NAS ]]; then
        echo "${path#/media}"
        return
      fi
      ;;
  esac
  echo "$path"
}

IFS=':' read -r -a MEDIA_ROOTS <<< "$MEDIA_ROOTS_CSV"

snapshot_existing_hardlinks() {
  local snapshot_file="$1"
  local target="$2"
  : > "$snapshot_file"

  local -a candidates=()
  local file root hardlink_path
  local -a links=()

  if [[ -f "$target" ]]; then
    candidates+=("$target")
  elif [[ -d "$target" ]]; then
    while IFS= read -r -d '' file; do
      candidates+=("$file")
    done < <(find "$target" -type f -name '*.mkv' -print0 2>/dev/null)
  fi

  for file in "${candidates[@]}"; do
    [[ -f "$file" ]] || continue
    links=()

    for root in "${MEDIA_ROOTS[@]}"; do
      [[ -d "$root" ]] || continue
      while IFS= read -r -d '' hardlink_path; do
        [[ "$hardlink_path" == "$file" ]] && continue
        links+=("$hardlink_path")
      done < <(find "$root" -xdev -type f -samefile "$file" -print0 2>/dev/null)
    done

    if (( ${#links[@]} > 0 )); then
      printf '%s\t' "$file" >> "$snapshot_file"
      printf '%s|' "${links[@]}" >> "$snapshot_file"
      printf '\n' >> "$snapshot_file"
    fi
  done
}

repoint_hardlinks_from_snapshot() {
  local snapshot_file="$1"
  local job_log="$2"
  local src dst links_blob
  local -a link_paths=()
  local link tmp replaced_count
  replaced_count=0

  [[ -s "$snapshot_file" ]] || return 0
  [[ -s "$job_log" ]] || return 0

  while IFS=$'\t' read -r src dst; do
    [[ -n "$src" && -n "$dst" ]] || continue
    [[ -f "$dst" ]] || continue

    links_blob=$(awk -F'\t' -v key="$src" '$1==key { print $2; exit }' "$snapshot_file")
    [[ -n "$links_blob" ]] || continue

    IFS='|' read -r -a link_paths <<< "$links_blob"
    for link in "${link_paths[@]}"; do
      [[ -n "$link" ]] || continue
      [[ -e "$link" ]] || continue

      tmp="${link}.dv8link.$$"
      rm -f "$tmp" 2>/dev/null || true
      if ln "$dst" "$tmp" 2>/dev/null && mv -f "$tmp" "$link" 2>/dev/null; then
        replaced_count=$((replaced_count + 1))
      else
        rm -f "$tmp" 2>/dev/null || true
        echo "$(now) - WARNING: failed to relink media hardlink '$link' -> '$dst'"
      fi
    done
  done < <(sed -nE 's/^.* - (.*) processed successfully -> (.*)$/\1\t\2/p' "$job_log")

  if (( replaced_count > 0 )); then
    echo "$(now) - Media relink completed: $replaced_count hardlink(s) now point to converted file(s)"
  fi
}

rotate_log() {
  local file="$1" max_bytes="$2"
  if [[ -f "$file" ]]; then
    local size
    size=$(stat -c%s "$file" 2>/dev/null || echo 0)
    if (( size > max_bytes )); then
      mv "$file" "$file.old"
    fi
  fi
}

try_lock_dir() {
  local lock_dir="$1"
  if mkdir "$lock_dir" 2>/dev/null; then
    echo "$BASHPID" > "$lock_dir/pid"
    return 0
  fi
  if [[ -f "$lock_dir/pid" ]]; then
    local pid
    pid=$(cat "$lock_dir/pid" 2>/dev/null || true)
    if [[ -n "$pid" ]] && ! kill -0 "$pid" 2>/dev/null; then
      rm -rf "$lock_dir" 2>/dev/null || true
      if mkdir "$lock_dir" 2>/dev/null; then
        echo "$BASHPID" > "$lock_dir/pid"
        return 0
      fi
    fi
  fi
  return 1
}

write_index() {
  local message="$*"
  local index_lock="$RUN_DIR/index-log.lock"
  local attempts=0
  until try_lock_dir "$index_lock"; do
    attempts=$((attempts + 1))
    if (( attempts >= 200 )); then
      rotate_log "$LOG_FILE" "$LOG_MAX_BYTES"
      printf '%s - %s\n' "$(now)" "$message" >> "$LOG_FILE" 2>/dev/null || true
      return 0
    fi
    sleep 0.1
  done
  rotate_log "$LOG_FILE" "$LOG_MAX_BYTES"
  printf '%s - %s\n' "$(now)" "$message" >> "$LOG_FILE"
  rm -rf "$index_lock" 2>/dev/null || true
}

sanitize_name() {
  local name="$1"
  name=$(echo "$name" | sed -E 's/[^A-Za-z0-9._-]+/_/g')
  echo "${name:0:80}"
}

if [[ ! -x "$SCRIPT" ]]; then
  write_index "ERROR: DV7toDV8 launcher is not executable: $SCRIPT"
  exit 1
fi

qbt_find_hash_by_target() {
  local target="$1"
  local target_no_slash="${target%/}"
  curl -s "$QBT_API_URL/api/v2/torrents/info" 2>/dev/null | jq -r --arg t "$target" --arg tn "$target_no_slash" '
    map(
      select(
        .content_path == $t or
        .content_path == $tn or
        ((.save_path + "/" + .name) == $t) or
        ((.save_path + "/" + .name) == $tn)
      )
    )
    | .[0].hash // empty
  ' 2>/dev/null
}

qbt_stop_and_remove_torrent() {
  local hash="$1"
  [[ -n "$hash" ]] || return 1

  curl -s -X POST "$QBT_API_URL/api/v2/torrents/stop" \
    --data-urlencode "hashes=$hash" >/dev/null 2>&1 || return 1

  curl -s -X POST "$QBT_API_URL/api/v2/torrents/delete" \
    --data-urlencode "hashes=$hash" \
    --data-urlencode "deleteFiles=false" >/dev/null 2>&1 || return 1

  return 0
}

if [[ -z "$TARGET" ]]; then
  write_index "ERROR: missing torrent path argument"
  exit 1
fi

TARGET=$(map_mount_alias "$TARGET")
ARCHIVE_DIR=$(map_archive_dir "$ARCHIVE_DIR")

find "$JOB_LOG_DIR" -type f -name '*.log' -mtime +"$JOB_LOG_RETENTION_DAYS" -delete 2>/dev/null || true

target_hash=$(printf '%s' "$TARGET" | sha1sum | awk '{print $1}')
safe_name=$(sanitize_name "$(basename "$TARGET")")
job_id="$(date '+%Y%m%d-%H%M%S')-${target_hash:0:12}-${safe_name}"
JOB_LOG="$JOB_LOG_DIR/$job_id.log"

write_index "Accepted target=$TARGET job_log=$JOB_LOG max_parallel=$MAX_PARALLEL_JOBS"

(
  FILE_LOCK_DIR=""
  SLOT_LOCK_DIR=""
  SLOT_NUMBER=""
  LAST_WAIT_LOG=0

  release_locks() {
    [[ -n "$SLOT_LOCK_DIR" ]] && rm -rf "$SLOT_LOCK_DIR" 2>/dev/null || true
    [[ -n "$FILE_LOCK_DIR" ]] && rm -rf "$FILE_LOCK_DIR" 2>/dev/null || true
  }
  trap release_locks EXIT

  echo "$(now) - Worker pid=$BASHPID started"
  echo "$(now) - Target: $TARGET"
  echo "$(now) - Archive dir: $ARCHIVE_DIR"
  echo "$(now) - Dry run: $DRY_RUN_FLAG"

  FILE_LOCK_DIR="$RUN_DIR/file-$target_hash.lock"
  if ! try_lock_dir "$FILE_LOCK_DIR"; then
    echo "$(now) - Duplicate target already being processed, skipping"
    write_index "Duplicate skip target=$TARGET"
    exit 0
  fi
  echo "$(now) - File lock acquired"

  SNAPSHOT_FILE="$RUN_DIR/snapshot-$job_id.tsv"
  snapshot_existing_hardlinks "$SNAPSHOT_FILE" "$TARGET"
  if [[ -s "$SNAPSHOT_FILE" ]]; then
    echo "$(now) - Hardlink snapshot captured: $(wc -l < "$SNAPSHOT_FILE") source entrie(s)"
  else
    echo "$(now) - Hardlink snapshot captured: none"
  fi

  while true; do
    for ((slot = 1; slot <= MAX_PARALLEL_JOBS; slot++)); do
      SLOT_LOCK_DIR="$RUN_DIR/slot-$slot.lock"
      if try_lock_dir "$SLOT_LOCK_DIR"; then
        SLOT_NUMBER="$slot"
        echo "$(now) - Slot acquired: $slot/$MAX_PARALLEL_JOBS"
        write_index "Start target=$TARGET slot=$slot/$MAX_PARALLEL_JOBS job_log=$JOB_LOG"
        break 2
      fi
    done

    now_epoch=$(date +%s)
    if (( now_epoch - LAST_WAIT_LOG >= 60 )); then
      echo "$(now) - Waiting for free slot..."
      write_index "Queue wait target=$TARGET active_limit=$MAX_PARALLEL_JOBS"
      LAST_WAIT_LOG=$now_epoch
    fi
    sleep "$QUEUE_WAIT_SECONDS"
  done

  if [[ "$DRY_RUN_FLAG" == "true" ]]; then
    DV8_EL_RPU_DIR="$ARCHIVE_DIR" DV8_PROCESSING_LOG_FILE="$JOB_LOG" "$SCRIPT" --dry-run "$TARGET"
  else
    DV8_EL_RPU_DIR="$ARCHIVE_DIR" DV8_PROCESSING_LOG_FILE="$JOB_LOG" "$SCRIPT" "$TARGET"
  fi
  rc=$?

  converted=false
  if grep -Eq 'processed successfully ->|[1-9][0-9]* file\(s\) converted\.' "$JOB_LOG" 2>/dev/null; then
    converted=true
  fi

  # "Not a DV7 file" is expected and should not be treated as a failure.
  if [[ "$rc" -ne 0 ]] && grep -q 'Not a DV7 file:' "$JOB_LOG" 2>/dev/null; then
    rc=0
  fi

  if [[ "$converted" == "true" && "$DRY_RUN_FLAG" != "true" ]]; then
    repoint_hardlinks_from_snapshot "$SNAPSHOT_FILE" "$JOB_LOG"
  fi

  if [[ "$converted" == "true" && "$QBT_REMOVE_CONVERTED" == "true" && "$DRY_RUN_FLAG" != "true" ]]; then
    hash="$(qbt_find_hash_by_target "$TARGET")"
    if qbt_stop_and_remove_torrent "$hash"; then
      echo "$(now) - Converted DV7: torrent stopped+removed (files kept), hash=$hash"
      write_index "Converted cleanup success target=$TARGET hash=$hash"
    else
      echo "$(now) - WARNING: converted DV7 but failed to stop/remove torrent for target=$TARGET"
      write_index "Converted cleanup failed target=$TARGET"
    fi
  fi

  echo "$(now) - Completed rc=$rc"
  write_index "Completed rc=$rc target=$TARGET slot=${SLOT_NUMBER:-n/a} job_log=$JOB_LOG"
  rm -f "$SNAPSHOT_FILE" 2>/dev/null || true
  exit "$rc"
) >> "$JOB_LOG" 2>&1 &

exit 0
