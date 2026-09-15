#!/usr/bin/env bash
# Compatibility entry point for existing automation; use repair_missed_dovifuse_jobs.sh for new setups.
exec "$(cd -- "$(dirname -- "$0")" && pwd)/repair_missed_dovifuse_jobs.sh" "$@"
