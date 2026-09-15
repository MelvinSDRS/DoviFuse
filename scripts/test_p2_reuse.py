#!/usr/bin/env python3
"""Check per-job probe/crop/export reuse with real tools and disposable media."""
import collections
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RES = Path(os.environ.get("DOVIFUSE_AUDIT_RESOURCES", ROOT))
BIN = Path(os.environ.get("DOVIFUSE_AUDIT_BIN", ROOT / "dovifuse_converter/target/debug/dovifuse_converter"))
SEEDS = Path(os.environ["DOVIFUSE_AUDIT_FIXTURES"])
WORK = Path(tempfile.mkdtemp(prefix="dovifuse-reuse-", dir="/tmp"))
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RES), DOVIFUSE_PROCESSING_LOG_FILE=str(WORK / "processing.log"))
ENV["PATH"] += os.pathsep + str(RES / "tools")
TOOLS = WORK / "tools"
TOOLS.mkdir()
CALLS = WORK / "calls.jsonl"
for name in ("mediainfo", "ffmpeg", "dovi_tool"):
    real = shutil.which(name, path=ENV["PATH"])
    assert real, name
    wrapper = TOOLS / name
    wrapper.write_text("#!" + sys.executable + "\nimport json,os,sys\n"
        + "with open(" + repr(str(CALLS)) + ", 'a') as f: f.write(json.dumps([" + repr(name) + "]+sys.argv[1:])+'\\n')\n"
        + "os.execv(" + repr(real) + ", [" + repr(real) + "]+sys.argv[1:])\n")
    wrapper.chmod(0o755)
ENV["PATH"] = str(TOOLS) + os.pathsep + ENV["PATH"]
resources = WORK / "resources"
resources.mkdir()
(resources / "tools").symlink_to(TOOLS, target_is_directory=True)
(resources / "config").symlink_to(RES / "config", target_is_directory=True)
ENV["DOVIFUSE_SCRIPT_DIR"] = str(resources)
manifest = json.loads((SEEDS / "manifest.json").read_text())
inputs = []
for name in ("dv.mkv", "hdr.mkv"):
    source = SEEDS / name
    assert hashlib.sha256(source.read_bytes()).hexdigest() == manifest["sha256"][name]
    dest = WORK / name
    shutil.copyfile(source, dest)
    inputs.append(dest.resolve())
report = WORK / "report.json"
print("Reuse controls:", WORK, flush=True)
p = subprocess.run([str(BIN), "--hybrid", "--hwaccel", "off", "--sync", "framecount",
    "--report", str(report), "-o", str(WORK / "output.mkv"), *map(str, inputs)],
    env=ENV, capture_output=True, text=True, timeout=300)
(WORK / "run.log").write_text(p.stdout + "\n" + p.stderr)
assert p.returncode == 0, (p.returncode, WORK)
assert json.loads(report.read_text())["execution"] == "completed"
calls = [json.loads(line) for line in CALLS.read_text().splitlines()]
for path in inputs:
    probes = [c for c in calls if c[0] == "mediainfo" and str(path) in c
              and any(a.startswith("--Output=Video;%Format%|") for a in c)]
    assert len(probes) == 1, (path, probes)
crops = [tuple(c[1:]) for c in calls if c[0] == "ffmpeg" and str(inputs[1]) in c
         and any("cropdetect=" in a for a in c)]
assert len(crops) == 6, crops
assert max(collections.Counter(crops).values()) == 1, "Repeated target crop observation"
exports = [c for c in calls if c[0] == "dovi_tool" and "export" in c
           and any(a.startswith("all=") for a in c)]
assert len(exports) == 3, exports
for path in inputs:
    assert hashlib.sha256(path.read_bytes()).hexdigest() == manifest["sha256"][path.name]
summary = {"input_probes_per_source": 1, "target_crop_calls": len(crops),
           "all_rpu_exports": len(exports), "source_hashes_preserved": True}
(WORK / "summary.json").write_text(json.dumps(summary, indent=2))
print("Passed:", json.dumps(summary))
