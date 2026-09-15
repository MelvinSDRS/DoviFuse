#!/usr/bin/env python3
"""Archive and report integration on an explicitly supplied SMB audit directory.

Creates one disposable directory beneath --destination-root. Never mounts,
unmounts, fills, or deletes a share or modifies the supplied fixture files.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from audit_fixtures import sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--destination-root', type=Path, required=True)
    parser.add_argument('--baseline', action='store_true')
    opts = parser.parse_args()
    assert sys.platform == 'darwin', 'This control requires macOS and an existing SMB mount'
    destination = opts.destination_root.resolve(strict=True)
    mounts = subprocess.check_output(['/sbin/mount', '-t', 'smbfs'], text=True)
    mount_paths = [Path(line.split(' on ', 1)[1].rsplit(' (', 1)[0])
                   for line in mounts.splitlines() if ' on ' in line]
    assert any(p == destination or p in destination.parents for p in mount_paths), 'Destination is not on an SMB mount'
    local = Path(tempfile.mkdtemp(prefix='dovifuse-smb-local-'))
    remote = Path(tempfile.mkdtemp(prefix='dovifuse-smb-', dir=destination))
    resources = Path(os.environ['DOVIFUSE_AUDIT_RESOURCES'])
    binary = Path(os.environ['DOVIFUSE_AUDIT_BIN'])
    seeds = Path(os.environ['DOVIFUSE_AUDIT_FIXTURES'])
    manifest = json.loads((seeds/'manifest.json').read_text())
    assert manifest['kind'] == 'synthetic-mechanics-only'
    for name in ['p7.mkv', 'dv.mkv', 'hdr.mkv']:
        assert sha256(seeds/name) == manifest['sha256'][name]
        shutil.copyfile(seeds/name, remote/name)
    env = dict(os.environ, DOVIFUSE_SCRIPT_DIR=str(resources),
               DOVIFUSE_PROCESSING_LOG_FILE=str(local/'processing.log'))

    # Independently derive the expected archive with the bundled media tools.
    encoded, expected = local/'source.hevc', local/'expected-archive.hevc'
    for args in ([resources/'tools/mkvextract', 'tracks', seeds/'p7.mkv', '0:'+str(encoded)],
                 [resources/'tools/dovi_tool', 'demux', '--el-only', encoded, '-e', expected]):
        subprocess.run(list(map(str, args)), env=env, capture_output=True, check=True, timeout=120)
    archive = remote/'archive'
    archive.mkdir()
    cases = [
        ('smb-archive', local/'archive.report.json', ['--archive-dir', archive, remote/'p7.mkv'], 0),
        ('smb-report-success', remote/'success.report.json', ['--check', remote/'dv.mkv'], 0),
        ('smb-report-failure', remote/'failure.report.json', ['--check', remote/'hdr.mkv'], 1),
    ]
    results, replays = [], []
    for name, report_path, args, expected_code in cases:
        log = local/(name+'.log')
        result = subprocess.run(list(map(str, [binary, '--progress', 'jsonl', '--hwaccel', 'off',
            '--tmp-dir', local/'scratch', '--report', report_path, *args])),
            env=env, capture_output=True, text=True, timeout=120)
        log.write_text(result.stdout+'\n'+result.stderr)
        row = {'case': name, 'exit_status': result.returncode}
        try:
            saved = json.loads(report_path.read_text())
            assert result.returncode == expected_code, result.stderr
            assert saved['execution'] == ('failed' if expected_code else 'completed'), saved
            assert saved['validation'] == ('fail' if expected_code else ('warn' if name == 'smb-archive' else 'pass')), saved
            if name == 'smb-archive':
                assert any(c['key'] == 'standard_output' and c['status'] == 'pass' for c in saved['checks'])
                assert sha256(archive/'p7.DV7.EL_RPU.hevc') == sha256(expected)
                assert sha256(remote/'p7.mkv') != manifest['sha256']['p7.mkv']
            else:
                seed = 'hdr.mkv' if expected_code else 'dv.mkv'
                assert sha256(remote/seed) == manifest['sha256'][seed]
            if expected_code:
                assert '"event":"completed"' not in result.stdout
            row.update(status='passed', execution=saved['execution'], validation=saved['validation'])
            replays.append({'name': name, 'report': str(report_path), 'log': str(log), 'exit_status': result.returncode})
        except (AssertionError, ValueError, OSError) as error:
            row.update(status='failed', error=str(error))
        if name == 'smb-archive' and result.returncode:
            row['source_retained_on_failure'] = sha256(remote/'p7.mkv') == manifest['sha256']['p7.mkv']
            row['partial_archive_removed'] = not (archive/'p7.DV7.EL_RPU.hevc').exists()
            assert row['source_retained_on_failure'] and row['partial_archive_removed'], row
        results.append(row)
    summary = {'kind': 'native-SMB-archive-and-report', 'binary_sha256': sha256(binary),
               'cases': results, 'artifacts': str(remote),
               'limit': 'Successful filesystem/server synchronization is not remote hardware power-loss proof'}
    (local/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    if os.environ.get('DOVIFUSE_APP_REPLAY_MANIFEST'):
        path = Path(os.environ['DOVIFUSE_APP_REPLAY_MANIFEST'])
        path.write_text(json.dumps(json.loads(path.read_text())+replays, indent=2)+'\n')
    print('SMB integration evidence:', local, json.dumps(summary), flush=True)
    if not opts.baseline:
        assert all(row['status'] == 'passed' for row in results), local


if __name__ == '__main__':
    main()
