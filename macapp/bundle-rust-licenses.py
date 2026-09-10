#!/usr/bin/env python3
"""Copy crate license notices from the locked, already downloaded Rust sources."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
destination = Path(sys.argv[1]) / 'licenses' / 'rust'
destination.mkdir(parents=True, exist_ok=True)
metadata = json.loads(subprocess.check_output([
    'cargo', 'metadata', '--locked', '--offline', '--format-version', '1',
    '--manifest-path', str(root / 'dv8_converter' / 'Cargo.toml'),
]))
notices = []
for package in metadata['packages']:
    if package['name'] == 'dv8_converter':
        continue
    source = Path(package['manifest_path']).parent
    files = sorted({p for pattern in ['LICENSE*', 'COPYING*', 'COPYRIGHT*']
                    for p in source.glob(pattern) if p.is_file()})
    if not files:
        raise SystemExit('Missing crate license files: ' + package['name'])
    target = destination / (package['name'] + '-' + package['version'])
    target.mkdir(exist_ok=True)
    hashes = {}
    for path in files:
        shutil.copyfile(path, target / path.name)
        hashes[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    notices.append({'package': package['name'], 'version': package['version'],
                    'license': package['license'], 'files_sha256': hashes})
(destination / 'manifest.json').write_text(json.dumps(notices, indent=2) + '\n')
print('Bundled Rust license notices:', len(notices))
