#!/usr/bin/env python3
"""Synthetic temporal/padding controls; no Dolby playback or picture-equivalence proof."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RES = Path(os.environ.get('DOVIFUSE_AUDIT_RESOURCES', ROOT))
BIN = Path(os.environ.get('DOVIFUSE_AUDIT_BIN', ROOT/'dovifuse_converter/target/debug/dovifuse_converter'))
SEEDS = Path(os.environ['DOVIFUSE_AUDIT_FIXTURES'])
WORK = Path(tempfile.mkdtemp(prefix='dovifuse-temporal-', dir='/tmp'))
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RES), DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
ENV['PATH'] += os.pathsep + str(RES/'tools')
DOVI = shutil.which('dovi_tool', path=ENV['PATH'])
RESULTS = []


def run(args, name, success=True, env=ENV):
    p = subprocess.run(list(map(str, args)), env=env, capture_output=True, text=True, timeout=180)
    (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
    assert (p.returncode == 0) == success, f'{name}: exit {p.returncode}; see {WORK}'
    return p


def convert(name, donor='shifted.mkv', flags=(), success=False, env=ENV):
    output = WORK/(name+'.mkv')
    report = WORK/(name+'.json')
    p = run([BIN, '--hybrid', '--hwaccel', 'off', '--progress', 'jsonl', '--report', report,
             '--skip-grade-check', '--letterbox', 'off', *flags, '-o', output,
             WORK/donor, WORK/'hdr.mkv'], name, success, env)
    data = json.loads(report.read_text())
    assert data['execution'] == ('completed' if success else 'failed')
    if not success:
        assert not output.exists() and '"event":"completed"' not in p.stdout
    RESULTS.append(name)
    return p


print('Temporal controls:', WORK, flush=True)
manifest = json.loads((SEEDS/'manifest.json').read_text())
for name in ('dv.mkv', 'hdr.mkv', 'shifted.mkv'):
    assert hashlib.sha256((SEEDS/name).read_bytes()).hexdigest() == manifest['sha256'][name]
    shutil.copyfile(SEEDS/name, WORK/name)
before = {p:hashlib.sha256(p.read_bytes()).hexdigest() for p in WORK.glob('*.mkv')}
for name, flags in [('padding-default', []), ('padding-force', ['--force']),
                    ('padding-explicit-offset', ['--offset', '5'])]:
    p = convert(name, flags=flags)
    assert '--allow-padding' in p.stdout+p.stderr
    assert 'Apply editor' not in p.stdout
convert('padding-reviewed', flags=['--allow-padding'], success=True)
convert('explicit-offset-reviewed', flags=['--offset', '5', '--allow-padding'], success=True)
convert('explicit-zero', donor='dv.mkv', flags=['--offset', '0'], success=True)
p = convert('explicit-contradiction', donor='dv.mkv', flags=['--offset', '48', '--allow-padding', '--force'])
assert 'contradict' in p.stdout+p.stderr
# Framecount cannot skip a measurable contradiction.
p = convert('framecount-output-mismatch', flags=['--sync', 'framecount'])
assert 'contradict' in p.stdout+p.stderr
assert 'Apply editor' not in p.stdout
p = run([BIN, '--repair-sync', '5', '--hwaccel', 'off', WORK/'shifted.mkv'], 'repair-padding-unreviewed', False)
assert '--allow-padding' in p.stdout+p.stderr
RESULTS.append('repair-padding-unreviewed')
# Corrupt only the output RPU after the input passes all temporal checks.
run([DOVI, 'extract-rpu', WORK/'shifted.mkv', '-o', WORK/'shifted.bin'], 'extract-shifted')
fault = WORK/'fault-tools'
fault.mkdir()
wrapper = fault/'dovi_tool'
wrapper.write_text('#!'+sys.executable+'\nimport os,sys\nargs=sys.argv[1:]\n'
    'if "inject-rpu" in args:\n args[args.index("-r")+1]='+repr(str(WORK/'shifted.bin'))+'\n'
    'os.execv('+repr(DOVI)+', ['+repr(DOVI)+']+args)\n')
wrapper.chmod(0o755)
fault_resources = WORK/'fault-resources'
(fault_resources/'tools').mkdir(parents=True)
(fault_resources/'tools/dovi_tool').symlink_to(wrapper)
(fault_resources/'config').symlink_to(RES/'config', target_is_directory=True)
fault_env = dict(ENV, PATH=str(fault)+os.pathsep+ENV['PATH'], DOVIFUSE_SCRIPT_DIR=str(fault_resources))
p = convert('output-shift-rejected', donor='dv.mkv', flags=['--sync', 'framecount'], env=fault_env)
assert 'Post-inject sync FAILED' in p.stdout+p.stderr
assert (WORK/'output-shift-rejected.FAILED.mkv').exists()
for path, digest in before.items():
    assert hashlib.sha256(path.read_bytes()).hexdigest() == digest
print('Passed:', len(RESULTS), RESULTS)
