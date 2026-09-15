#!/usr/bin/env python3
"""Real-tool mapping controls on disposable synthetic pictures, on Linux/macOS.

The mismatched P8 donor deliberately places P8.4 reshaping metadata on a PQ
base. It tests mapping rejection, not color conversion or creative grading.
"""
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
WORK = Path(tempfile.mkdtemp(prefix='dovifuse-mapping-', dir='/tmp'))
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RES), DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
ENV['PATH'] = ENV.get('PATH', '') + os.pathsep + str(RES/'tools')
DOVI = shutil.which('dovi_tool', path=ENV['PATH'])
assert DOVI, 'dovi_tool unavailable'
RESULTS = []


def run(args, name, success=True, env=ENV):
    p = subprocess.run(list(map(str, args)), env=env, capture_output=True, text=True, timeout=180)
    (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
    assert (p.returncode == 0) == success, f'{name}: unexpected exit {p.returncode}; see {WORK}'
    return p


def write_json(name, value):
    path = WORK/name
    path.write_text(json.dumps(value))
    return path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def mappings(rpu, name):
    out = WORK/(name+'.json')
    run([DOVI, 'export', '-i', rpu, '-o', out], name)
    return [r['rpu_data_mapping'] for r in json.loads(out.read_text())]


def convert(donor, target, name, flags=(), success=True, env=ENV):
    output, report = WORK/(name+'.mkv'), WORK/(name+'.report.json')
    p = run([BIN, '--hybrid', '--progress', 'jsonl', '--report', report, '--hwaccel', 'off',
             '--sync', 'framecount', '--letterbox', 'off', *flags, '-o', output, donor, target],
            name, success, env)
    data = json.loads(report.read_text())
    assert data['execution'] == ('completed' if success else 'failed'), name
    if not success:
        assert data['validation'] == 'fail' and '"event":"completed"' not in p.stdout, name
        assert not output.exists(), name
    RESULTS.append(name)
    return p, output


print('Mapping controls:', WORK, flush=True)
manifest = json.loads((SEEDS/'manifest.json').read_text())
for name in ('dv.mkv', 'hdr.mkv', 'p7.mkv', 'shifted.mkv'):
    assert sha(SEEDS/name) == manifest['sha256'][name], name
    shutil.copyfile(SEEDS/name, WORK/name)
frames = manifest['frames']
run(['mkvextract', WORK/'hdr.mkv', 'tracks', '0:'+str(WORK/'hdr.hevc')], 'extract-hdr')
run([DOVI, 'extract-rpu', WORK/'dv.mkv', '-o', WORK/'identity.bin'], 'extract-identity')
config = write_json('nonidentity.json', {'profile':'8.4', 'cm_version':'V40', 'length':1,
    'level6':{'max_display_mastering_luminance':1000, 'min_display_mastering_luminance':1,
              'max_content_light_level':1000, 'max_frame_average_light_level':400}})
run([DOVI, 'generate', '-j', config, '-o', WORK/'nonidentity-one.bin'], 'generate-nonidentity')
trim = write_json('remove-last.json', {'mode':0, 'remove':[f'{frames-1}-{frames-1}']})
run([DOVI, 'editor', '-i', WORK/'identity.bin', '-j', trim, '-o', WORK/'identity-prefix.bin'], 'identity-prefix')
(WORK/'late.bin').write_bytes((WORK/'identity-prefix.bin').read_bytes()+(WORK/'nonidentity-one.bin').read_bytes())
run([DOVI, 'inject-rpu', '-i', WORK/'hdr.hevc', '-r', WORK/'late.bin', '-o', WORK/'late.hevc'], 'inject-late')
run(['mkvmerge', '-o', WORK/'late.mkv', WORK/'late.hevc'], 'mux-late')
inputs = {p:sha(p) for p in [WORK/'dv.mkv', WORK/'hdr.mkv', WORK/'late.mkv', WORK/'p7.mkv']}

for name, flags in [
    ('default', []), ('metadata', ['--grade-check','metadata']),
    ('skip', ['--skip-grade-check']), ('force', ['--force']),
    ('force-skip-delete', ['--force','--skip-grade-check','--delete-sources']),
]:
    p, _ = convert(WORK/'late.mkv', WORK/'hdr.mkv', 'reject-late-'+name, flags, False)
    assert f'frame {frames-1}' in p.stdout+p.stderr, name
    assert 'canonical identity mapping' in p.stdout+p.stderr, name
    # This case must reach RPU inspection, not merely fail color eligibility.
    assert 'Extract RPU from DV source' in p.stdout, name
    assert 'Apply editor' not in p.stdout, name
    assert not (WORK/('reject-late-'+name+'.FAILED.mkv')).exists(), name

p, output = convert(WORK/'dv.mkv', WORK/'hdr.mkv', 'preserve-identity', ['--skip-grade-check'])
run([DOVI, 'extract-rpu', output, '-o', WORK/'output.bin'], 'extract-output')
assert mappings(WORK/'identity.bin', 'source-mapping') == mappings(WORK/'output.bin', 'output-mapping')
assert 'output_mapping' in p.stdout

# Known P7 FEL conversion remains available; it must disclose the loss.
run(['mkvextract', WORK/'p7.mkv', 'tracks', '0:'+str(WORK/'p7.hevc')], 'extract-p7')
# The upstream FEL metadata fixture has feature-sized L5 bars on tiny test
# pictures. Clear only that unrelated geometry for this mapping control.
run([DOVI, 'extract-rpu', WORK/'p7.hevc', '-o', WORK/'p7.bin'], 'extract-p7-rpu')
crop = write_json('p7-crop.json', {'mode':0, 'active_area':{'crop':True}})
run([DOVI, 'editor', '-i', WORK/'p7.bin', '-j', crop, '-o', WORK/'p7-cropped.bin'], 'p7-fixture-geometry')
run([DOVI, 'inject-rpu', '-i', WORK/'p7.hevc', '-r', WORK/'p7-cropped.bin', '-o', WORK/'p7-valid.hevc'], 'inject-p7-valid')
run(['mkvmerge', '-o', WORK/'p7-valid.mkv', WORK/'p7-valid.hevc'], 'mux-p7-valid')
inputs[WORK/'p7-valid.mkv'] = sha(WORK/'p7-valid.mkv')
run([DOVI, 'remove', '-i', WORK/'p7.hevc', '-o', WORK/'p7-hdr.hevc'], 'remove-p7-rpu')
run(['mkvmerge', '-o', WORK/'p7-hdr.mkv', WORK/'p7-hdr.hevc'], 'mux-p7-hdr')
p, _ = convert(WORK/'p7-valid.mkv', WORK/'p7-hdr.mkv', 'p7-compatibility', ['--skip-grade-check'])
assert 'FEL' in p.stdout and 'mapping' in p.stdout.lower(), 'Missing FEL provenance'

# Corrupt only the last mapping at injection after the valid input gate.
fault = WORK/'fault-tools'; fault.mkdir()
wrapper = fault/'dovi_tool'
wrapper.write_text('#!'+sys.executable+'\nimport os,sys\nargs=sys.argv[1:]\n'
    'if "inject-rpu" in args:\n args[args.index("-r")+1]='+repr(str(WORK/'late.bin'))+'\n'
    'os.execv('+repr(DOVI)+', ['+repr(DOVI)+']+args)\n')
wrapper.chmod(0o755)
fault_resources = WORK/'fault-resources'
(fault_resources/'tools').mkdir(parents=True)
(fault_resources/'tools/dovi_tool').symlink_to(wrapper)
(fault_resources/'config').symlink_to(RES/'config', target_is_directory=True)
fault_env = dict(ENV, PATH=str(fault)+os.pathsep+ENV['PATH'], DOVIFUSE_SCRIPT_DIR=str(fault_resources))
p, _ = convert(WORK/'dv.mkv', WORK/'hdr.mkv', 'reject-output-mapping', ['--skip-grade-check'], False, fault_env)
assert 'Output mapping verification failed' in p.stdout+p.stderr
assert (WORK/'reject-output-mapping.FAILED.mkv').exists()

# Sync repair acts on the same picture stream and must retain nonidentity maps.
run([DOVI, 'extract-rpu', WORK/'shifted.mkv', '-o', WORK/'shifted.bin'], 'extract-shifted')
run([DOVI, 'editor', '-i', WORK/'shifted.bin', '-j', trim, '-o', WORK/'shifted-prefix.bin'], 'shifted-prefix')
(WORK/'shifted-late.bin').write_bytes((WORK/'shifted-prefix.bin').read_bytes()+(WORK/'nonidentity-one.bin').read_bytes())
run([DOVI, 'inject-rpu', '-i', WORK/'hdr.hevc', '-r', WORK/'shifted-late.bin', '-o', WORK/'shifted-late.hevc'], 'inject-shifted-late')
run(['mkvmerge', '-o', WORK/'shifted-late.mkv', WORK/'shifted-late.hevc'], 'mux-shifted-late')
before_repair = sha(WORK/'shifted-late.mkv')
run([BIN, '--repair-sync', '5', '--allow-padding', '--hwaccel', 'off', WORK/'shifted-late.mkv'], 'repair-nonidentity')
run([DOVI, 'extract-rpu', WORK/'shifted-late.DV8.Fixed.mkv', '-o', WORK/'repaired.bin'], 'extract-repaired')
original_maps = mappings(WORK/'shifted-late.bin', 'repair-source-mappings')
repaired_maps = mappings(WORK/'repaired.bin', 'repair-output-mappings')
assert repaired_maps == original_maps[5:]+[original_maps[-1]]*5, 'Sync repair changed mapping'
assert sha(WORK/'shifted-late.mkv') == before_repair
RESULTS.append('repair-preserves-nonidentity')
assert {p:sha(p) for p in inputs} == inputs, 'Source modified'
summary = {'passed':len(RESULTS), 'cases':RESULTS, 'work':str(WORK), 'synthetic_only':True}
(WORK/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
print(json.dumps(summary, indent=2))
