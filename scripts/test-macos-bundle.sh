#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP=${1:?Usage: test-macos-bundle.sh app-path prepared-fixture-directory}
FIXTURES=${2:?Missing Linux-prepared fixture directory}
APP=$(cd "$APP" && pwd)
FIXTURES=$(cd "$FIXTURES" && pwd)
PYTHON=$(command -v python3)
RESOURCES="$APP/Contents/Resources"
# Runtime lookup must use the bundle, even on developer machines with Homebrew.
export PATH="$RESOURCES/tools:/usr/bin:/bin:/usr/sbin:/sbin"
export DOVIFUSE_AUDIT_RESOURCES="$RESOURCES"
export DOVIFUSE_AUDIT_BIN="$RESOURCES/tools/dovifuse_converter"
export DOVIFUSE_AUDIT_FIXTURES="$FIXTURES"
export DOVIFUSE_DONOR_ELIGIBILITY_FIXTURES="${DOVIFUSE_DONOR_ELIGIBILITY_FIXTURES:-$FIXTURES/donor-eligibility}"
export DOVIFUSE_APP_REPLAY_MANIFEST=$(mktemp /tmp/dovifuse-app-replay.XXXXXX)
unset PYTHONOPTIMIZE
export TMPDIR=/tmp
codesign --verify --deep --strict "$APP"
if [[ -e "$RESOURCES/vendor/color" ]]; then
  echo "Main must not bundle the experimental P5 color backend." >&2
  exit 1
fi
if "$RESOURCES/tools/ffmpeg" -hide_banner -filters 2>/dev/null | grep -q libplacebo; then
  echo "Main FFmpeg unexpectedly includes libplacebo." >&2
  exit 1
fi
"$PYTHON" - "$RESOURCES" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])/'licenses/rust'
notices = json.loads((root/'manifest.json').read_text())
names = {p['package'] for p in notices}
assert {'serde', 'serde_json', 'sha2', 'unicode-normalization', 'tinyvec', 'tinyvec_macros'} <= names
for package in notices:
    assert package['files_sha256'], 'Empty license notice'
    for name, expected in package['files_sha256'].items():
        path = root/(package['package']+'-'+package['version'])/name
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected, path
print('Passed: bundled Rust license notices', len(notices))
PY
"$PYTHON" "$ROOT/scripts/audit_smoke.py"
"$PYTHON" "$ROOT/scripts/test_donor_eligibility.py"
"$PYTHON" "$ROOT/scripts/test_mapping_policy.py"
"$PYTHON" "$ROOT/scripts/test_metadata_transport.py"
"$PYTHON" "$ROOT/scripts/test_temporal_alignment.py"
"$PYTHON" "$ROOT/scripts/test_picture_coverage.py"
"$PYTHON" "$ROOT/scripts/test_p2_reuse.py"
export DOVIFUSE_TEMPORAL_LOCAL_FIXTURES="${DOVIFUSE_TEMPORAL_LOCAL_FIXTURES:-$DOVIFUSE_AUDIT_FIXTURES/temporal-local}"
if [[ ! -f "$DOVIFUSE_TEMPORAL_LOCAL_FIXTURES/manifest.json" ]]; then
  echo "Missing Linux-prepared temporal-local fixture manifest: $DOVIFUSE_TEMPORAL_LOCAL_FIXTURES/manifest.json" >&2
  echo "Upload and download the temporal-local fixture directory before running the Mac bundle checks." >&2
  exit 1
fi
"$PYTHON" "$ROOT/scripts/test_temporal_local_edits.py" \
  --fixtures "$DOVIFUSE_TEMPORAL_LOCAL_FIXTURES"
"$PYTHON" "$ROOT/scripts/test_p5_disabled.py"
"$PYTHON" "$ROOT/scripts/audit_standard_source.py"
"$PYTHON" "$ROOT/scripts/test_job_report.py"
"$PYTHON" "$ROOT/scripts/audit_faults.py"
"$PYTHON" "$ROOT/scripts/audit_macos_storage.py"
if [[ -n ${DOVIFUSE_AUDIT_SMB_ROOT:-} ]]; then
  "$PYTHON" "$ROOT/scripts/audit_macos_smb.py" --destination-root "$DOVIFUSE_AUDIT_SMB_ROOT"
fi
"$PYTHON" "$ROOT/scripts/audit_preservation.py"
DOVIFUSE_L5_TIMELINE_BIN="$ROOT/dovifuse_converter/target/release/examples/l5_timeline" \
  "$PYTHON" "$ROOT/scripts/audit_l5.py"
# Tests and logs above live in an independently owned /tmp directory.
codesign --verify --deep --strict "$APP"
TEST_DIR=$(mktemp -d /tmp/dovifuse-app-state.XXXXXX)
TEST_BIN="$TEST_DIR/test"
trap 'rm -rf "$TEST_DIR"' EXIT
xcrun swiftc -swift-version 6 -strict-concurrency=complete -parse-as-library \
  "$ROOT/macapp/DoviFuse/AppModel.swift" \
  "$ROOT/macapp/DoviFuse/ScratchCapacity.swift" \
  "$ROOT/macapp/Tests/AppModelAuditTests.swift" -o "$TEST_BIN"
"$TEST_BIN"
