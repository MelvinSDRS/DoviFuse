#!/bin/bash
set -u

SCRIPT="/path/to/DV8/DV7toDV8.sh"
BASE_DIR="/path/to/DV8"
LOG_FILE="$BASE_DIR/qbt_trigger.log"
LOG_MAX_BYTES="${DV8_TRIGGER_LOG_MAX_BYTES:-10485760}"
JOB_LOG_DIR="$BASE_DIR/logs/jobs"
RUN_DIR="/tmp/dv8-qbt"

TARGET="${1:-}"
ARCHIVE_DIR="${DV8_EL_RPU_DIR:-/NAS/EL_RPU}"
DRY_RUN_FLAG="${DV8_AUTORUN_DRY_RUN:-false}"
MAX_PARALLEL_JOBS="${DV8_MAX_PARALLEL_JOBS:-1}"
QUEUE_WAIT_SECONDS="${DV8_QUEUE_WAIT_SECONDS:-15}"
JOB_LOG_RETENTION_DAYS="${DV8_JOB_LOG_RETENTION_DAYS:-30}"

mkdir -p "$(dirname "$LOG_FILE")" "$JOB_LOG_DIR" "$RUN_DIR"

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
  until try_lock_dir "$index_lock"; do
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

if [[ -z "$TARGET" ]]; then
  write_index "ERROR: missing torrent path argument"
  exit 1
fi

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

  echo "$(now) - Completed rc=$rc"
  write_index "Completed rc=$rc target=$TARGET slot=${SLOT_NUMBER:-n/a} job_log=$JOB_LOG"
  exit "$rc"
) >> "$JOB_LOG" 2>&1 &

exit 0
