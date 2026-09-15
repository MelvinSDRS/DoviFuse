#!/usr/bin/env python3
"""Real-tool, disposable-media regression checks. Never modifies library media.
Run after cargo build; requires bundled ffmpeg/libx265 and dovi_tool plus MKVToolNix.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import signal

ROOT = Path(__file__).resolve().parents[1]
WORK = Path(tempfile.mkdtemp(prefix='dovifuse-audit-'))
FF = Path(os.environ.get('DOVIFUSE_AUDIT_FFMPEG', ROOT / 'tools/ffmpeg'))
RESOURCES = Path(os.environ.get('DOVIFUSE_AUDIT_RESOURCES', ROOT))
DOVI = RESOURCES / 'tools/dovi_tool'
BIN = Path(os.environ.get('DOVIFUSE_AUDIT_BIN', ROOT / 'dovifuse_converter/target/debug/dovifuse_converter'))
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RESOURCES), DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
results = []
app_replays = []


def run(args, name, expected=0):
    p = subprocess.run([str(a) for a in args], env=ENV, capture_output=True, text=True)
    (WORK/f'{name}.log').write_text(p.stdout+'\n'+p.stderr)
    assert (p.returncode == 0) == (expected == 0), f'{name}: exit {p.returncode}; see {WORK}/{name}.log'
    return p


def convert(args, name, expected=0):
    p = run([BIN, '--report', WORK/f'{name}.report.json', '--progress', 'jsonl', '--hwaccel', 'off', *args], name, expected)
    events = [json.loads(line) for line in p.stdout.splitlines() if line.startswith('{')]
    report = json.loads((WORK/f'{name}.report.json').read_text())
    assert report['execution'] == ('failed' if expected else 'completed'), name
    assert report['checks'] == [e for e in events if e['event']=='check_result'], name
    final = [e for e in events if e['event']=='job_finalized']
    assert len(final)==1 and final[0]['status']==report['validation'], name
    assert report['inputs_before'] and report['tools'], name
    if expected:
        assert report['validation']=='fail', name
        assert any(e['event'] == 'failed' for e in events), name
        assert not any(e['event'] == 'completed' for e in events), name
    else:
        assert any(e['event'] == 'completed' for e in events), name
    results.append(name)
    app_replays.append({'name':name, 'log':str(WORK/f'{name}.log'),
                        'report':str(WORK/f'{name}.report.json'), 'exit_status':p.returncode})
    return events


def digest(p):
    with p.open('rb') as stream:
        h = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
        return h.hexdigest()


print(f'Audit fixtures and logs: {WORK}', flush=True)
from audit_fixtures import prepare
prepare(WORK, ROOT, FF, DOVI, run)
originals = {p:digest(p) for p in [WORK/'dv.mkv',WORK/'hdr.mkv']}
events = convert(['--check',WORK/'dv.mkv'],'checker-valid')
assert any(e.get('key')=='sync' and e.get('status')=='pass' for e in events)
convert(['--check',WORK/'hdr.mkv'],'checker-missing-dv',1)
convert(['--hybrid',WORK/'dv.mkv',WORK/'hdr.mkv'],'hybrid-default')
convert(['--check',WORK/'hdr.DV8.Hybrid.mkv'],'checker-hybrid')
convert(['--hybrid',WORK/'dv.mkv',WORK/'hdr.mkv'],'existing-output-rejected',1)
convert(['--hybrid','--sync','framecount','--grade-check','full','-o',WORK/'full.mkv',WORK/'dv.mkv',WORK/'hdr.mkv'],'hybrid-full-grade')
convert(['--hybrid',WORK/'dv.mkv',WORK/'dv.mkv'],'identical-inputs',1)
# Intentionally offset only RPU cuts by +5, maintaining the exact frame count.
shift_hash=digest(WORK/'shifted.mkv')
events=convert(['--check',WORK/'shifted.mkv'],'checker-offset',1)
assert any(e.get('fix_value')==5 for e in events)
convert(['--repair-sync','4',WORK/'shifted.mkv'],'repair-stale-offset',1)
convert(['--repair-sync','5','--allow-padding',WORK/'shifted.mkv'],'repair-offset')
assert digest(WORK/'shifted.mkv')==shift_hash
convert(['--check',WORK/'shifted.DV8.Fixed.mkv'],'checker-repaired')
# Assemble P7 signalling from upstream BL/EL and FEL RPU fixtures.
# This verifies metadata plumbing; it is not a creatively matched FEL master.
source_p7=digest(WORK/'p7.mkv')
# Preserve uppercase source spelling without replacing an unrelated lowercase file.
shutil.copyfile(WORK/'p7.mkv',WORK/'Case.MKV')
case_sensitive = not (WORK/'Case.mkv').exists()
if case_sensitive:
    (WORK/'Case.mkv').write_text('unrelated lowercase file')
convert(['-n',WORK/'Case.MKV'],'uppercase-in-place')
if case_sensitive:
    assert (WORK/'Case.mkv').read_text()=='unrelated lowercase file'
assert 'Case.MKV' in {p.name for p in WORK.iterdir()}
assert digest(WORK/'Case.MKV')!=source_p7
# Existing archives must survive a failed conversion unchanged.
archive=WORK/'archive';archive.mkdir()
shutil.copyfile(WORK/'p7.mkv',WORK/'archive-test.mkv')
archive_file=archive/'archive-test.DV7.EL_RPU.hevc';archive_file.write_text('previous archive')
ENV['DOVIFUSE_EL_RPU_DIR']=str(archive)
convert([WORK/'archive-test.mkv'],'archive-collision',1)
assert archive_file.read_text()=='previous archive' and digest(WORK/'archive-test.mkv')==source_p7
ENV.pop('DOVIFUSE_EL_RPU_DIR')

# Cancel during full validation: child stops, original and cleanup boundaries hold.
shutil.copyfile(WORK/'p7.mkv', WORK/'cancel.mkv')
fake_tools=WORK/'fake-tools';fake_tools.mkdir()
fake_ff=fake_tools/'ffmpeg'
fake_ff.write_text('#!/usr/bin/env python3\nimport sys,time\nfrom pathlib import Path\nif "-version" in sys.argv: sys.exit(0)\nPath('+repr(str(WORK/'decode-started'))+').touch()\ntime.sleep(60)\n')
fake_ff.chmod(0o755)
cancel_env=dict(ENV, PATH=str(fake_tools)+os.pathsep+os.environ['PATH'])
with (WORK/'cancel.log').open('w') as log:
    process=subprocess.Popen([str(BIN),'--progress','jsonl','--hwaccel','off','-n',str(WORK/'cancel.mkv')],env=cancel_env,stdout=log,stderr=log)
    deadline=time.monotonic()+30
    while not (WORK/'decode-started').exists() and process.poll() is None and time.monotonic()<deadline:
        time.sleep(0.05)
    assert (WORK/'decode-started').exists(), 'Cancellation never reached decode'
    process.send_signal(signal.SIGTERM)
    assert process.wait(timeout=10)!=0
assert digest(WORK/'cancel.mkv')==source_p7 and not (WORK/'cancel.DV8_TMP.mkv').exists()
assert '"event":"cancelled"' in (WORK/'cancel.log').read_text()
results.append('cancellation-preserves-original')

# Dry-run failure must not remove similarly named pre-existing files.
sentinel=WORK/'p7.BL_EL_RPU.hevc';sentinel.write_text('keep me')
reserved=WORK/'p7.DV8_TMP.mkv';reserved.write_text('keep output')
convert(['-n','--dry-run',WORK/'p7.mkv'],'standard-dry-collision',1)
assert sentinel.read_text()=='keep me' and reserved.read_text()=='keep output'
reserved.unlink()
convert(['-n',WORK/'p7.mkv'],'standard-p7')
assert digest(WORK/'p7.mkv')!=source_p7
convert(['--check',WORK/'p7.mkv'],'checker-standard')
assert sentinel.read_text()=='keep me'
# Mixed folders skip ordinary AVC while converting the DV7 candidate.
mixed=WORK/'mixed';mixed.mkdir()
shutil.copyfile(WORK/'ordinary.mkv', mixed/'ordinary.mkv')
shutil.copyfile(WORK/'archive-test.mkv',mixed/'candidate.mkv')
convert(['-n',mixed],'mixed-directory')
# Multi-video input cannot silently choose/drop a different stream.
run(['mkvmerge','-o',WORK/'multi.mkv',WORK/'hdr.mkv',WORK/'dv.mkv'],'mux-multi')
convert(['--check',WORK/'multi.mkv'],'multi-video-rejected',1)
# Unreadable and empty files must fail without reporting completion.
(WORK/'empty.mkv').touch()
convert(['--check',WORK/'empty.mkv'],'empty-check',1)
raw=(WORK/'dv.mkv').read_bytes()
(WORK/'truncated.mkv').write_bytes(raw[:len(raw)//2])
convert(['--check',WORK/'truncated.mkv'],'truncated-input',1)

for p,h in originals.items():
    assert digest(p)==h, f'Original modified: {p}'
# Timestamp preservation across hybrid remux.
for name in ['hdr','hdr.DV8.Hybrid']:
    run(['mkvextract', WORK/f'{name}.mkv','timestamps_v2',f'0:{WORK}/{name}.timestamps'],'timestamps-'+name)
source_times=[float(v) for v in (WORK/'hdr.timestamps').read_text().splitlines() if not v.startswith('#')]
output_times=[float(v) for v in (WORK/'hdr.DV8.Hybrid.timestamps').read_text().splitlines() if not v.startswith('#')]
assert len(source_times)==len(output_times)
assert all(abs(a-b)<=1 for a,b in zip(source_times,output_times)), 'Timestamps differ by more than Matroska millisecond rounding'
results.append('video-timestamps-preserved')
required = json.loads((ROOT/'tests/fixtures/regression-cases.json').read_text())
assert len(results) == len(set(results)), 'Duplicate regression names'
assert set(required).issubset(results), f'Missing regressions: {set(required)-set(results)}'
(WORK/'summary.json').write_text(json.dumps({'passed':results,'work':str(WORK)},indent=2))
Path(os.environ.get('DOVIFUSE_APP_REPLAY_MANIFEST', WORK/'app-replay.json')).write_text(json.dumps(app_replays, indent=2))
print(json.dumps({'passed':len(results),'work':str(WORK),'cases':results},indent=2))
