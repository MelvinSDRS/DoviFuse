#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
unset PYTHONOPTIMIZE
export TMPDIR=/tmp
LOG=$(mktemp /tmp/dovifuse-rust-tests.XXXXXX)
cargo test --locked --manifest-path dovifuse_converter/Cargo.toml 2>&1 | tee "$LOG"
python3 - "$LOG" <<'PY'
from pathlib import Path
import re, sys
counts = re.findall(r'test result: ok\. (\d+) passed;', Path(sys.argv[1]).read_text())
assert sum(map(int, counts)) >= 83, 'The audited 83-test baseline must remain covered'
PY
cargo clippy --locked --manifest-path dovifuse_converter/Cargo.toml -- -D warnings
cargo build --locked --manifest-path dovifuse_converter/Cargo.toml
cargo test --locked --manifest-path dovifuse_converter/Cargo.toml --example l5_timeline
cargo clippy --locked --manifest-path dovifuse_converter/Cargo.toml --example l5_timeline -- -D warnings
cargo build --locked --manifest-path dovifuse_converter/Cargo.toml --example l5_timeline
if [[ -z ${DOVIFUSE_AUDIT_FIXTURES:-} ]]; then
  FIXTURE_PARENT=$(mktemp -d /tmp/dovifuse-linux-fixtures.XXXXXX)
  python3 scripts/audit_fixtures.py "$FIXTURE_PARENT/seeds"
  export DOVIFUSE_AUDIT_FIXTURES="$FIXTURE_PARENT/seeds"
fi
export DOVIFUSE_DONOR_ELIGIBILITY_FIXTURES="${DOVIFUSE_DONOR_ELIGIBILITY_FIXTURES:-$DOVIFUSE_AUDIT_FIXTURES/donor-eligibility}"
if [[ ! -f "$DOVIFUSE_DONOR_ELIGIBILITY_FIXTURES/manifest.json" ]]; then
  python3 scripts/test_donor_eligibility.py --prepare "$DOVIFUSE_DONOR_ELIGIBILITY_FIXTURES" \
    --base-fixtures "$DOVIFUSE_AUDIT_FIXTURES"
fi
python3 -m unittest discover -s scripts -p "test_release_gates.py"
python3 -m unittest discover -s scripts -p "test_release_publication.py"
python3 -m unittest discover -s scripts -p "test_rename_compatibility.py"
python3 -m unittest discover -s scripts -p "test_release_sources.py"
python3 -m unittest discover -s scripts -p "test_bundle_licenses.py"
python3 scripts/verify_reference_catalog.py
python3 scripts/audit_smoke.py
python3 scripts/test_donor_eligibility.py
python3 scripts/test_mapping_policy.py
python3 scripts/test_metadata_transport.py
python3 scripts/test_temporal_alignment.py
export DOVIFUSE_PICTURE_FIXTURES="${DOVIFUSE_PICTURE_FIXTURES:-$DOVIFUSE_AUDIT_FIXTURES/picture-coverage}"
if [[ ! -f "$DOVIFUSE_PICTURE_FIXTURES/manifest.json" ]]; then
  python3 scripts/test_picture_coverage.py --prepare
fi
python3 scripts/test_picture_coverage.py
python3 scripts/test_p2_reuse.py
export DOVIFUSE_TEMPORAL_LOCAL_FIXTURES="${DOVIFUSE_TEMPORAL_LOCAL_FIXTURES:-$DOVIFUSE_AUDIT_FIXTURES/temporal-local}"
if [[ ! -f "$DOVIFUSE_TEMPORAL_LOCAL_FIXTURES/manifest.json" ]]; then
  python3 scripts/test_temporal_local_edits.py --prepare "$DOVIFUSE_TEMPORAL_LOCAL_FIXTURES"
fi
python3 scripts/test_temporal_local_edits.py
python3 scripts/test_p5_disabled.py
python3 scripts/audit_standard_source.py
python3 scripts/test_job_report.py
python3 scripts/audit_faults.py
python3 scripts/audit_preservation.py
python3 scripts/audit_l5.py
python3 scripts/audit_wrapper_smoke.py
bash -n DoviFuse.sh qbt_autorun_wrapper.sh macapp/build-macos-app.sh macapp/create-dmg.sh scripts/test-linux.sh scripts/test-macos-bundle.sh
git diff --check
