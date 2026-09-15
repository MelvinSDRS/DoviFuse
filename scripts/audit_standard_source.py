#!/usr/bin/env python3
"""Real-tool MEL/FEL classification and explicit archive controls on synthetic media."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from audit_fixtures import sha256

ROOT = Path(__file__).resolve().parents[1]
RESOURCES = Path(os.environ.get('DOVIFUSE_AUDIT_RESOURCES', ROOT))
BIN = Path(os.environ.get('DOVIFUSE_AUDIT_BIN', ROOT/'dovifuse_converter/target/debug/dovifuse_converter'))
SEEDS = Path(os.environ['DOVIFUSE_AUDIT_FIXTURES'])
WORK = Path(tempfile.mkdtemp(prefix='dovifuse-standard-source-'))
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RESOURCES),
           DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'),
           DOVIFUSE_EL_RPU_DIR=str(WORK/'unused-environment-destination'))
DOVI = RESOURCES/'tools/dovi_tool'
manifest = json.loads((SEEDS/'manifest.json').read_text())
assert manifest['kind'] == 'synthetic-mechanics-only'
assert sha256(SEEDS/'p7.mkv') == manifest['sha256']['p7.mkv']


def run(args, name, success=True):
    p = subprocess.run([str(a) for a in args], env=ENV, capture_output=True, text=True, timeout=120)
    (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
    assert (p.returncode == 0) == success, f'{name}: {p.returncode}; {WORK}'
    return p


def convert(source, label, flags, success=True):
    report = WORK/(label+'.json')
    p = run([BIN, '--progress', 'jsonl', '--hwaccel', 'off', '--report', report, *flags, source], label, success)
    saved = json.loads(report.read_text())
    events = [json.loads(s) for s in p.stdout.splitlines() if s.startswith('{')]
    assert saved['checks'] == [e for e in events if e['event']=='check_result']
    assert saved['execution'] == ('completed' if success else 'failed')
    return saved


video = WORK/'source.hevc'
ident = json.loads(run(['mkvmerge', '-J', SEEDS/'p7.mkv'], 'identify').stdout)
track = next(t['id'] for t in ident['tracks'] if t['type']=='video')
run(['mkvextract', 'tracks', SEEDS/'p7.mkv', f'{track}:{video}'], 'extract')
mel = (ROOT/'dovi_tool/assets/tests/mel_orig.bin').read_bytes()
fel = (ROOT/'dovi_tool/assets/tests/fel_orig.bin').read_bytes()
results = {}
for label, payload, kind in [('mel', mel*259, 'MEL'), ('fel', fel*259, 'FEL'), ('mixed', mel*258+fel, 'MEL/FEL')]:
    rpu = WORK/(label+'.bin'); rpu.write_bytes(payload)
    hevc = WORK/(label+'.hevc')
    source = WORK/(label+'.mkv')
    run([DOVI, 'inject-rpu', '-i', video, '-r', rpu, '-o', hevc], label+'-inject')
    run(['mkvmerge', '-o', source, hevc], label+'-mux')
    original = sha256(source)
    expected_archive = WORK/(label+'-expected.hevc')
    run([DOVI, 'demux', '--el-only', hevc, '-e', expected_archive], label+'-expected')
    archive = WORK/(label+' archive')
    archive.mkdir()
    report = convert(source, label+'-convert', ['--archive-dir', archive])
    value = next(m['value'] for m in report['measurements'] if m['key']=='source_enhancement_layer')
    assert value['type']==kind and value['rpu_frames']==259 and value['archive_enabled'] is True
    assert report['validation']=='warn' and sha256(source)!=original
    archived = archive/(label+'.DV7.EL_RPU.hevc')
    assert sha256(archived)==sha256(expected_archive)
    assert not Path(ENV['DOVIFUSE_EL_RPU_DIR']).exists(), 'CLI destination must override environment'
    results[label] = {'type':kind, 'archive_payload_sha256':sha256(archived), 'validation':'warn'}

# Existing archive collision must not replace the source or the archive.
source = WORK/'collision.mkv'; shutil.copyfile(SEEDS/'p7.mkv', source)
before = sha256(source)
archive = WORK/'collision archive'; archive.mkdir()
sentinel = archive/'collision.DV7.EL_RPU.hevc'; sentinel.write_bytes(b'existing archive')
convert(source, 'archive-collision', ['--archive-dir', archive], success=False)
assert sha256(source)==before and sentinel.read_bytes()==b'existing archive'
assert not (WORK/'collision.DV8_TMP.mkv').exists()
report = convert(source, 'archive-disabled', ['-n'])
assert next(m['value'] for m in report['measurements'] if m['key']=='source_enhancement_layer')['archive_enabled'] is False
assert not Path(ENV['DOVIFUSE_EL_RPU_DIR']).exists()
assert sha256(SEEDS/'p7.mkv')==manifest['sha256']['p7.mkv']
print('Standard source/archive controls passed:', WORK, json.dumps(results))
