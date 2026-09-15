#!/usr/bin/env bash
# Compatibility entry point for existing automation; use DoviFuse.sh for new setups.
exec "$(cd -- "$(dirname -- "$0")" && pwd)/DoviFuse.sh" "$@"
