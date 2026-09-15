#!/usr/bin/env python3
"""Disposable brightness/chroma and geometry controls, not creative-grade certification."""
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
WORK = Path(tempfile.mkdtemp(prefix='dovifuse-picture-coverage-', dir='/tmp'))
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RES), DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
ENV['PATH'] += os.pathsep + str(RES/'tools')
FF = RES/'tools/ffmpeg'
DOVI = shutil.which('dovi_tool', path=ENV['PATH'])
RESULTS = []
PREPARED = Path(os.environ.get('DOVIFUSE_PICTURE_FIXTURES', SEEDS/'picture-coverage'))


def run(args, name, success=True, env=ENV):
    p = subprocess.run(list(map(str, args)), env=env, capture_output=True, text=True, timeout=300)
    (WORK/(name+'.log')).write_text(p.stdout+'\n'+p.stderr)
    assert (p.returncode == 0) == success, f'{name}: exit {p.returncode}; see {WORK}'
    return p


def convert(name, target='hdr.mkv', donor='dv.mkv', flags=(), success=True, env=ENV):
    output, report = WORK/(name+'.mkv'), WORK/(name+'.json')
    p = run([BIN, '--hybrid', '--hwaccel', 'off', '--progress', 'jsonl', '--report', report,
             '--sync', 'framecount', *flags, '-o', output, WORK/donor, WORK/target], name, success, env)
    data = json.loads(report.read_text())
    assert data['execution'] == ('completed' if success else 'failed'), name
    if not success:
        assert not output.exists() and '"event":"completed"' not in p.stdout, name
    RESULTS.append(name)
    return p, data


def encode(name, expression=None, vf=None):
    if expression:
        inputs = ['-f', 'lavfi', '-i', f"nullsrc=s=320x180:r=24,format=yuv420p10le,geq=lum='{expression}':cb=512:cr=512", '-frames:v', str(frames)]
    else:
        inputs = ['-i', str(WORK/'hdr.mkv'), '-vf', vf]
    hevc = WORK/(name+'.hevc')
    run([FF, '-nostdin', '-v', 'error', *inputs, '-an', '-fps_mode', 'passthrough', '-pix_fmt', 'yuv420p10le', '-c:v', 'libx265', '-preset', 'ultrafast',
         '-x265-params', 'lossless=1:pools=2:frame-threads=2:log-level=error:colorprim=9:transfer=16:colormatrix=9:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400',
         '-y', hevc], 'encode-'+name)
    run(['mkvmerge', '-o', WORK/(name+'.mkv'), hevc], 'mux-'+name)


def y_hashes(name):
    p = run([FF, '-nostdin', '-v', 'error', '-i', WORK/(name+'.mkv'), '-vf', 'extractplanes=y', '-f', 'framemd5', '-'], 'y-hashes-'+name)
    return [line.rsplit(',', 1)[-1].strip() for line in p.stdout.splitlines() if line and not line.startswith('#')]
print('Picture coverage controls:', WORK, flush=True)
manifest = json.loads((SEEDS/'manifest.json').read_text())
frames = manifest['frames']
for name in ('hdr.mkv', 'dv.mkv'):
    assert hashlib.sha256((SEEDS/name).read_bytes()).hexdigest() == manifest['sha256'][name]
    shutil.copyfile(SEEDS/name, WORK/name)
if sys.argv[1:] == ['--prepare']:
    encode('chroma', vf='lutyuv=u=val+96:v=val')
    # One second dimmed inside an otherwise unchanged grade. Peaks remain unchanged.
    encode('short-dim', vf="geq=lum='if(between(N,40,63),300,lum(X,Y))':cb='cb(X,Y)':cr='cr(X,Y)'")
    encode('short-bars', vf="drawbox=x=0:y=0:w=iw:h=24:color=black:t=fill:enable='between(n,40,63)',drawbox=x=0:y=ih-24:w=iw:h=24:color=black:t=fill:enable='between(n,40,63)'")
    encode('dark-object', expression='if(between(X,128,191)*between(Y,72,107),700,64)')
    # A donor with five additional leading seconds; its entire aligned tail must be screened.
    encode('leading', vf='tpad=start=120:start_mode=clone')
    durations = [31,47,23,59,37,71,29,43,61,19,53,41,67,27,73,35,49,57,21,63,39,69,25,51]
    shots = [{'start':0, 'duration':120, 'metadata_blocks':[]}]
    start = 120
    for length in durations:
        shots.append({'start':start, 'duration':length, 'metadata_blocks':[]})
        start += length
    config = WORK/'leading-rpu.json'
    config.write_text(json.dumps({'profile':'8.1', 'cm_version':'V40', 'shots':shots,
        'level6':{'max_display_mastering_luminance':1000, 'min_display_mastering_luminance':1,
                  'max_content_light_level':1000, 'max_frame_average_light_level':400}}))
    run([DOVI, 'generate', '-j', config, '-o', WORK/'leading.bin'], 'generate-leading-rpu')
    run([DOVI, 'inject-rpu', '-i', WORK/'leading.hevc', '-r', WORK/'leading.bin', '-o', WORK/'leading-dv.hevc'], 'inject-leading')
    run(['mkvmerge', '-o', WORK/'leading-dv.mkv', WORK/'leading-dv.hevc'], 'mux-leading-dv')
    assert y_hashes('hdr') == y_hashes('chroma'), 'Chroma control changed Y pixels'
    PREPARED.mkdir(parents=True, exist_ok=False)
    digests = {}
    for path in WORK.glob('*.mkv'):
        shutil.copyfile(path, PREPARED/path.name)
        digests[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    (PREPARED/'manifest.json').write_text(json.dumps({'sha256': digests, 'chroma_y_identical': True}, indent=2))
    print('Prepared picture fixtures:', PREPARED, flush=True)
    sys.exit(0)
assert not sys.argv[1:], 'Usage: test_picture_coverage.py [--prepare]'
prepared_manifest = json.loads((PREPARED/'manifest.json').read_text())
assert prepared_manifest['chroma_y_identical'] is True
for name, digest in prepared_manifest['sha256'].items():
    source = PREPARED/name
    assert source.name == name and source.suffix == '.mkv'
    assert hashlib.sha256(source.read_bytes()).hexdigest() == digest
    if name in ('hdr.mkv', 'dv.mkv'):
        assert digest == manifest['sha256'][name], 'Prepared fixtures use different source media'
    shutil.copyfile(source, WORK/name)
before = {p:hashlib.sha256(p.read_bytes()).hexdigest() for p in WORK.glob('*.mkv')}

def measurement(data, key):
    return next(m['value'] for m in data['measurements'] if m['key'] == key)

p, data = convert('unchanged-sampled', flags=['--letterbox', 'off'])
screen = measurement(data, 'brightness_screen')
assert screen['color_screened'] and not screen['whole_target_screened']
assert screen['target_unmeasured_intervals']
p, data = convert('unchanged-geometry', flags=['--skip-grade-check'])
assert measurement(data, 'letterbox')['usable_windows'] >= 3
p, data = convert('short-aspect-review', target='short-bars.mkv', flags=['--skip-grade-check'])
assert any(c['key']=='active_area_picture' and c['status']=='inconclusive' for c in data['checks'])
p, data = convert('thin-geometry', flags=['--skip-grade-check', '--grade-windows', '1'], success=False)
assert measurement(data, 'letterbox')['usable_windows'] < 3
convert('unchanged-full', flags=['--letterbox', 'off', '--grade-check', 'full'])
p, data = convert('chroma-rejected', target='chroma.mkv', flags=['--letterbox', 'off'], success=False)
assert 'Brightness comparison failed' in p.stdout+p.stderr or 'screen' in p.stdout+p.stderr
p, data = convert('short-full-mismatch', target='short-dim.mkv', flags=['--letterbox', 'off', '--grade-check', 'full'], success=False)
p, data = convert('negative-offset-full', target='leading.mkv', flags=['--offset', '-120', '--allow-padding', '--letterbox', 'off', '--grade-check', 'full'])
screen = measurement(data, 'brightness_screen')
assert not screen['whole_target_screened'] and screen['target_frames_covered'] == frames
assert screen['target_unmeasured_intervals'] == [{'start_frame': 0, 'end_frame': 120}]
p, data = convert('positive-offset-full', donor='leading-dv.mkv', flags=['--offset', '120', '--letterbox', 'off', '--grade-check', 'full'])
screen = measurement(data, 'brightness_screen')
assert screen['whole_target_screened'] and screen['coverage_complete']
assert screen['target_frames_covered'] == frames
assert screen['windows'][-1]['measured_donor']['end_frame'] == frames+120
# Drop only the final donor measurement rows while ffmpeg still exits successfully.
fault = WORK/'fault-tools'
fault.mkdir()
wrapper = fault/'ffmpeg'
wrapper.write_text('#!'+sys.executable+'\nimport os,re,subprocess,sys\nfrom pathlib import Path\nargs=sys.argv[1:]\n'
    'if os.environ.get("DOVIFUSE_TEST_CROP_FAILURE") and "-vf" in args and "cropdetect" in args[args.index("-vf")+1]:\n'
    ' marker=Path(os.environ["DOVIFUSE_TEST_CROP_FAILURE"])\n count=int(marker.read_text())+1 if marker.exists() else 1\n marker.write_text(str(count))\n'
    ' if count==4: sys.stderr.write("injected crop decoder failure\\n"); sys.exit(19)\n'
    'if "-vf" in args and "signalstats" in args[args.index("-vf")+1] and any(a.endswith("leading-dv.mkv") for a in args):\n'
    ' p=subprocess.run(['+repr(str(FF))+']+args,capture_output=True,text=True)\n'
    ' parts=re.split(r"(?=^frame:)",p.stdout,flags=re.M)\n'
    ' sys.stdout.write("".join(parts[:-96]) if len(parts)>96 else p.stdout)\n'
    ' sys.stderr.write(p.stderr)\n sys.exit(p.returncode)\n'
    'os.execv('+repr(str(FF))+',['+repr(str(FF))+']+args)\n')
wrapper.chmod(0o755)
fault_res = WORK/'fault-resources'
(fault_res/'tools').mkdir(parents=True)
(fault_res/'tools/ffmpeg').symlink_to(wrapper)
(fault_res/'config').symlink_to(RES/'config', target_is_directory=True)
fault_env = dict(ENV, PATH=str(fault)+os.pathsep+ENV['PATH'], DOVIFUSE_SCRIPT_DIR=str(fault_res))
p, data = convert('missing-full-overlap', donor='leading-dv.mkv', flags=['--offset', '120', '--letterbox', 'off', '--grade-check', 'full'], success=False, env=fault_env)
assert 'coverage' in (p.stdout+p.stderr).lower() or 'incomplete' in (p.stdout+p.stderr).lower()
p, data = convert('implausible-geometry', target='dark-object.mkv', flags=['--skip-grade-check'], success=False)
assert 'Active-area' in p.stdout+p.stderr or 'crop' in p.stdout+p.stderr
p, data = convert('geometry-decoder-error', flags=['--skip-grade-check'], success=False,
                  env=dict(fault_env, DOVIFUSE_TEST_CROP_FAILURE=str(WORK/'crop-count')))
assert 'cropdetect failed' in p.stdout+p.stderr
p = run([BIN, '--hybrid', '--letterbox', 'resolution', WORK/'dv.mkv', WORK/'hdr.mkv'], 'resolution-guess-refused', False)
assert 'scaling' in p.stdout+p.stderr
RESULTS.append('resolution-guess-refused')
for path, digest in before.items():
    assert hashlib.sha256(path.read_bytes()).hexdigest() == digest
print('Passed:', len(RESULTS), RESULTS)
