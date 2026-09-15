#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
FIXTURES=${1:?Usage: prepare_ci_fixtures.sh destination}
unset DOVIFUSE_AUDIT_FIXTURES
python3 scripts/audit_fixtures.py "$FIXTURES"
export DOVIFUSE_AUDIT_FIXTURES="$FIXTURES"
python3 scripts/test_donor_eligibility.py --prepare "$FIXTURES/donor-eligibility" --base-fixtures "$FIXTURES"
DOVIFUSE_PICTURE_FIXTURES="$FIXTURES/picture-coverage" python3 scripts/test_picture_coverage.py --prepare
python3 scripts/test_temporal_local_edits.py --prepare "$FIXTURES/temporal-local"
