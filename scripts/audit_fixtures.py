"""Disposable synthetic media shared by Linux and bundled macOS tests.
Only fixture preparation needs libx264/libx265. Mac verification uses the
packaged decoder and tools with no external codec installation.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

SEEDS = ('hdr.mkv', 'dv.mkv', 'shifted.mkv', 'p7.mkv', 'ordinary.mkv', 'p5-mechanics.mkv')


def sha256(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def prepare(WORK, ROOT, FF, DOVI, run):
    source = os.environ.get('DV8_AUDIT_FIXTURES')
    if source:
        source = Path(source)
        manifest = json.loads((source/'manifest.json').read_text())
        if manifest['kind'] != 'synthetic-mechanics-only':
            raise ValueError('These fixtures must remain labeled synthetic')
        for name in SEEDS:
            if sha256(source/name) != manifest['sha256'][name]:
                raise ValueError(f'Fixture checksum mismatch: {name}')
            shutil.copyfile(source/name, WORK/name)
        return

    def mux(hevc, out, name):
        run(['mkvmerge', '-o', out, hevc], name)

    def rpu(shots, out, name):
        config = {'cm_version':'V40', 'profile':'8.1', 'level6':{
            'max_display_mastering_luminance':1000, 'min_display_mastering_luminance':1,
            'max_content_light_level':1000, 'max_frame_average_light_level':400},
            'shots':shots}
        path = WORK/f'{name}.json'
        path.write_text(json.dumps(config))
        run([DOVI, 'generate', '-j', path, '-o', out], name)

    # Irregular hard cuts with known frame numbers, BT.2020 PQ 10-bit HEVC.
    durations = [31,47,23,59,37,71,29,43,61,19,53,41,67,27,73,35,49,57,21,63,39,69,25,51]
    frames = sum(durations)
    expected = json.loads((ROOT/'tests/fixtures/catalog.json').read_text())['synthetic']['expected']
    if frames != expected['frames']:
        raise ValueError('Authored frame schedule no longer matches catalog expectations')
    shots, start, terms = [], 0, []
    for i, duration in enumerate(durations):
        shots.append({'start':start, 'duration':duration, 'metadata_blocks':[]})
        terms.append(f'if(between(N,{start},{start+duration-1}),{300 if i%2 == 0 else 700},0)')
        start += duration
    expr = '+'.join(terms)
    run([FF,'-nostdin','-v','error','-f','lavfi','-i',
        f"nullsrc=s=320x180:r=24,format=yuv420p10le,geq=lum='{expr}':cb=512:cr=512",
        '-frames:v',frames,'-c:v','libx265','-preset','ultrafast','-x265-params',
        'pools=2:frame-threads=2:log-level=error:colorprim=9:transfer=16:colormatrix=9:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400',
        '-y', WORK/'hdr.hevc'], 'generate-hdr')
    mux(WORK/'hdr.hevc',WORK/'hdr.mkv','mux-hdr')
    rpu(shots,WORK/'rpu.bin','generate-rpu')
    run([DOVI,'inject-rpu','-i',WORK/'hdr.hevc','-r',WORK/'rpu.bin','-o',WORK/'dv.hevc'],'inject')
    mux(WORK/'dv.hevc',WORK/'dv.mkv','mux-dv')
    shifted = [{'start':0,'duration':5,'metadata_blocks':[]}]
    for shot in shots:
        shifted.append(dict(shot,start=shot['start']+5,duration=min(shot['duration'],frames-shot['start']-5)))
    rpu(shifted,WORK/'shifted.bin','generate-shifted')
    run([DOVI,'inject-rpu','-i',WORK/'hdr.hevc','-r',WORK/'shifted.bin','-o',WORK/'shifted.hevc'],'inject-shifted')
    mux(WORK/'shifted.hevc',WORK/'shifted.mkv','mux-shifted')
    (WORK/'p7.bin').write_bytes((ROOT/'dovi_tool/assets/tests/fel_orig.bin').read_bytes()*259)
    run([DOVI,'inject-rpu','-i',ROOT/'dovi_tool/assets/hevc_tests/regular_start_code_4_muxed_el.hevc',
         '-r',WORK/'p7.bin','-o',WORK/'p7.hevc'],'inject-p7')
    mux(WORK/'p7.hevc',WORK/'p7.mkv','mux-p7')
    run([FF,'-nostdin','-v','error','-f','lavfi','-i','color=s=64x64:r=24','-frames:v','2',
         '-c:v','libx264',WORK/'ordinary.mkv'],'generate-avc')
    # Deliberately synthetic: P5 metadata on authored HDR10 pictures. This tests
    # rejection of unsupported P5 donors, never color reconstruction.
    (WORK/'p5-mechanics.bin').write_bytes((ROOT/'dovi_tool/assets/tests/profile5.bin').read_bytes()*frames)
    run([DOVI,'inject-rpu','-i',WORK/'hdr.hevc','-r',WORK/'p5-mechanics.bin',
         '-o',WORK/'p5-mechanics.hevc'],'inject-p5-mechanics')
    mux(WORK/'p5-mechanics.hevc',WORK/'p5-mechanics.mkv','mux-p5-mechanics')
    manifest = {
        'kind':'synthetic-mechanics-only',
        'recipe':'scripts/audit_fixtures.py',
        'p5-mechanics':'Synthetic P5 metadata: unsupported-donor refusal fixture only',
        'frames':frames, 'rpu_offset_frames':5,
        'sha256':{name:sha256(WORK/name) for name in SEEDS},
    }
    (WORK/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')


if __name__ == '__main__':
    import subprocess
    root = Path(__file__).resolve().parents[1]
    work = Path(sys.argv[1]).resolve()
    work.mkdir(parents=True, exist_ok=False)
    def run(args, name):
        result = subprocess.run([str(a) for a in args], capture_output=True)
        (work/f'{name}.log').write_bytes(result.stdout+result.stderr)
        result.check_returncode()
    prepare(work, root, Path(os.environ.get('DV8_AUDIT_FFMPEG', root/'tools/ffmpeg')),
            root/'tools/dovi_tool', run)
    print(work)
