#!/usr/bin/env python3
"""Actual ENOSPC and archive retry on an isolated 16 MiB Mac disk image.

Never fills or detaches a production volume. Requires macOS hdiutil.
"""
import errno
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from audit_fixtures import sha256

ROOT = Path(__file__).resolve().parents[1]
WORK = Path(tempfile.mkdtemp(prefix='dovifuse-storage-'))
RES = Path(os.environ['DOVIFUSE_AUDIT_RESOURCES'])
BIN = Path(os.environ['DOVIFUSE_AUDIT_BIN'])
SEEDS = Path(os.environ['DOVIFUSE_AUDIT_FIXTURES'])
ENV = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(RES), DOVIFUSE_PROCESSING_LOG_FILE=str(WORK/'processing.log'))
image = WORK/'disk.dmg'
mount = WORK/'volume'
mount.mkdir()
source = WORK/'source.mkv'
shutil.copyfile(SEEDS/'p7.mkv', source)
original = sha256(source)
replays = []


def run(args, name):
    result = subprocess.run(list(map(str, args)), env=ENV, capture_output=True, text=True, timeout=120)
    (WORK/(name+'.log')).write_text(result.stdout+'\n'+result.stderr)
    return result


def convert(name, success):
    report = WORK/(name+'.json')
    result = run([BIN, '--progress', 'jsonl', '--hwaccel', 'off', '--report', report,
                  '--archive-dir', mount/'archive', source], name)
    assert (result.returncode == 0) == success, (name, result.stderr, WORK)
    saved = json.loads(report.read_text())
    assert saved['execution'] == ('completed' if success else 'failed'), saved
    if not success:
        assert saved['validation'] == 'fail' and '"event":"completed"' not in result.stdout
        assert 'space' in str(saved['error']).lower(), saved['error']
    replays.append({'name': name, 'report': str(report), 'log': str(WORK/(name+'.log')),
                    'exit_status': result.returncode})
    return saved


run(['hdiutil', 'create', '-size', '16m', '-fs', 'HFS+', '-volname', 'DoviFuse-storage-audit', image], 'create').check_returncode()
attached = False
try:
    run(['hdiutil', 'attach', '-nobrowse', '-mountpoint', mount, image], 'attach').check_returncode()
    attached = True
    (mount/'archive').mkdir()
    fill = mount/'owned-fill-file'
    fd = os.open(fill, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        # A rejected 1 MiB write can still leave enough room for a small RPU
        # archive. Exhaust the final allocation block as well.
        for size in (1024*1024, 4096, 1):
            try:
                while True:
                    os.write(fd, b'x'*size)
            except OSError as error:
                assert error.errno == errno.ENOSPC, error
    finally:
        os.close(fd)
    failed = convert('archive-native-enospc', False)
    assert sha256(source) == original, 'Source changed on archive failure'
    assert not (WORK/'source.DV8_TMP.mkv').exists()
    assert not (mount/'archive/source.DV7.EL_RPU.hevc').exists(), 'Partial archive left under final name'
    fill.unlink()
    succeeded = convert('archive-retry-after-space-restored', True)
    assert sha256(source) != original
    assert (mount/'archive/source.DV7.EL_RPU.hevc').stat().st_size > 0
    summary = {'kind': 'native-filesystem-failure', 'filesystem': 'isolated 16 MiB HFS+ image',
               'native_error': 'ENOSPC', 'failed_execution': failed['execution'],
               'retry_execution': succeeded['execution'], 'source_retained_on_failure': True,
               'partial_archive_removed': True, 'production_volumes_modified': False}
finally:
    if attached:
        run(['hdiutil', 'detach', mount], 'detach').check_returncode()

(WORK/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
if os.environ.get('DOVIFUSE_APP_REPLAY_MANIFEST'):
    path = Path(os.environ['DOVIFUSE_APP_REPLAY_MANIFEST'])
    path.write_text(json.dumps(json.loads(path.read_text())+replays, indent=2)+'\n')
print('Native storage controls passed:', WORK, json.dumps(summary))
