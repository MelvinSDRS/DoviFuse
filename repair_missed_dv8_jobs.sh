#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" &>/dev/null && pwd)"
ENV_FILE="${DV8_ENV_FILE:-$SCRIPT_DIR/.env}"
if [[ -f "$ENV_FILE" ]]; then
  while IFS= read -r raw_line || [[ -n "$raw_line" ]]; do
    line="${raw_line%$'\r'}"
    [[ -z "$line" ]] && continue
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ "$line" == *=* ]] || continue
    key="${line%%=*}"
    val="${line#*=}"
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    if [[ "$val" =~ ^\".*\"$ ]]; then val="${val:1:${#val}-2}"; fi
    if [[ "$val" =~ ^\'.*\'$ ]]; then val="${val:1:${#val}-2}"; fi
    export "$key=$val"
  done < "$ENV_FILE"
fi

QBT_API_URL="${DV8_QBT_API_URL:-http://127.0.0.1:8080}"
MEDIA_ROOTS_CSV="${DV8_MEDIA_ROOTS:-/NAS/Movies:/NAS/TV Shows:/media/NAS/Movies:/media/NAS/TV Shows}"
JOBS_DIR="${DV8_JOBS_DIR:-$SCRIPT_DIR/logs/jobs}"
MODE="${1:-dry-run}"

if [[ ! -d "$JOBS_DIR" ]]; then
  echo "ERROR: jobs dir not found: $JOBS_DIR" >&2
  exit 1
fi

IFS=':' read -r -a MEDIA_ROOTS <<< "$MEDIA_ROOTS_CSV"

find_hash_by_source() {
  local src="$1"
  local src_no_slash="${src%/}"
  curl -s "$QBT_API_URL/api/v2/torrents/info" 2>/dev/null | jq -r --arg t "$src" --arg tn "$src_no_slash" '
    map(select(
      .content_path == $t or
      .content_path == $tn or
      ((.save_path + "/" + .name) == $t) or
      ((.save_path + "/" + .name) == $tn)
    )) | .[0].hash // empty
  ' 2>/dev/null || true
}

stop_delete_torrent() {
  local hash="$1"
  [[ -n "$hash" ]] || return 1
  curl -s -X POST "$QBT_API_URL/api/v2/torrents/stop" --data-urlencode "hashes=$hash" >/dev/null
  curl -s -X POST "$QBT_API_URL/api/v2/torrents/delete" --data-urlencode "hashes=$hash" --data-urlencode "deleteFiles=false" >/dev/null
}

map_to_fs_path() {
  local p="$1"
  case "$p" in
    /NAS/*)
      if [[ ! -e "$p" && -e "/media$p" ]]; then
        echo "/media$p"
        return
      fi
      ;;
    /media/NAS/*)
      local alt="${p#/media}"
      if [[ ! -e "$p" && -e "$alt" ]]; then
        echo "$alt"
        return
      fi
      ;;
  esac
  echo "$p"
}

relink_media_from_source() {
  local src="$1"
  local dst="$2"
  local changed=0
  local src_dev src_inode root root_dev link tmp
  local src_fs dst_fs

  src_fs="$(map_to_fs_path "$src")"
  dst_fs="$(map_to_fs_path "$dst")"

  [[ -f "$src_fs" ]] || return 0
  [[ -f "$dst_fs" ]] || return 0

  src_dev="$(stat -c '%d' "$src_fs" 2>/dev/null || true)"
  src_inode="$(stat -c '%i' "$src_fs" 2>/dev/null || true)"
  [[ -n "$src_dev" && -n "$src_inode" ]] || return 0

  for root in "${MEDIA_ROOTS[@]}"; do
    [[ -d "$root" ]] || continue
    root_dev="$(stat -c '%d' "$root" 2>/dev/null || true)"
    [[ "$root_dev" == "$src_dev" ]] || continue

    while IFS= read -r -d '' link; do
      [[ -n "$link" ]] || continue
      [[ -f "$link" ]] || continue
      tmp="${link}.dv8repair.$$"

      if [[ "$MODE" == "apply" ]]; then
        rm -f "$tmp" 2>/dev/null || true
        if ln "$dst_fs" "$tmp" 2>/dev/null && mv -f "$tmp" "$link" 2>/dev/null; then
          changed=$((changed+1))
        else
          rm -f "$tmp" 2>/dev/null || true
          echo "WARN: relink failed link=$link dst=$dst_fs"
        fi
      else
        changed=$((changed+1))
      fi
    done < <(find "$root" -xdev -type f -inum "$src_inode" -print0 2>/dev/null)
  done

  echo "$changed"
}

extract_pairs_for_log() {
  local log="$1"
  local target done_lines src dst line_count

  target="$(sed -nE 's/^.*Target: (.*)$/\1/p' "$log" | head -n1)"
  [[ -n "$target" ]] || return 0

  # Legacy marker
  sed -nE 's/^.* - (.*) processed successfully -> (.*)$/\1\t\2/p' "$log"

  # New marker "Done: ..." (strip ANSI first)
  while IFS= read -r dst; do
    [[ -n "$dst" ]] || continue
    src=""

    if [[ "$dst" == *.DV8.mkv ]]; then
      src="${dst%.DV8.mkv}.mkv"
    fi

    if [[ -z "$src" || ! -e "$src" ]]; then
      if [[ -f "$target" ]]; then
        src="$target"
      fi
    fi

    [[ -n "$src" ]] && printf '%s\t%s\n' "$src" "$dst"
  done < <(sed -nE 's/\x1B\[[0-9;]*m//g; s/^Done: (.*)$/\1/p' "$log")
}

job_total=0
job_with_pairs=0
pair_total=0
pair_existing=0
relink_total=0
torrent_cleanup_total=0

while IFS= read -r log; do
  job_total=$((job_total+1))

  # Only jobs that ended rc=0 and have conversion markers but no prior cleanup marker
  grep -q 'Completed rc=0' "$log" || continue
  if ! grep -Eq 'processed successfully ->|Done: .*\.DV8\.mkv' "$log"; then
    continue
  fi

  # If already marked cleanup success in trigger index historically, we still allow idempotent run.
  pairs_file="$(mktemp)"
  extract_pairs_for_log "$log" | awk -F'\t' 'NF==2 && !seen[$0]++ {print}' > "$pairs_file"
  [[ -s "$pairs_file" ]] || { rm -f "$pairs_file"; continue; }

  job_with_pairs=$((job_with_pairs+1))
  echo "JOB: $log"

  while IFS=$'\t' read -r src dst; do
    pair_total=$((pair_total+1))
    dst_fs="$(map_to_fs_path "$dst")"
    if [[ ! -f "$dst_fs" ]]; then
      echo "  SKIP pair (converted missing): src=$src dst=$dst fs_dst=$dst_fs"
      continue
    fi
    pair_existing=$((pair_existing+1))

    relink_count="$(relink_media_from_source "$src" "$dst")"
    relink_total=$((relink_total + relink_count))

    hash="$(find_hash_by_source "$src")"
    if [[ -n "$hash" ]]; then
      if [[ "$MODE" == "apply" ]]; then
        if stop_delete_torrent "$hash"; then
          torrent_cleanup_total=$((torrent_cleanup_total+1))
          echo "  CLEANUP ok hash=$hash src=$src relinked=$relink_count"
        else
          echo "  WARN cleanup failed hash=$hash src=$src relinked=$relink_count"
        fi
      else
        torrent_cleanup_total=$((torrent_cleanup_total+1))
        echo "  PLAN cleanup hash=$hash src=$src relinked=$relink_count"
      fi
    else
      echo "  INFO no torrent found for src=$src relinked=$relink_count"
    fi
  done < "$pairs_file"

  rm -f "$pairs_file"
done < <(find "$JOBS_DIR" -type f -name '*.log' | sort)

echo "--- SUMMARY mode=$MODE"
echo "jobs_scanned=$job_total jobs_with_conversion_pairs=$job_with_pairs"
echo "pairs_total=$pair_total pairs_with_existing_converted=$pair_existing"
echo "media_relinks_count=$relink_total"
echo "torrent_cleanup_actions=$torrent_cleanup_total"
