#!/usr/bin/env python3
"""Collect locked Rust license notices for both packaged executables."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
destination = Path(sys.argv[1]) / 'licenses' / 'rust'
destination.mkdir(parents=True, exist_ok=True)
packages = {}
for project, features in [
    ('dv8_converter', []),
    ('dovi_tool', ['--no-default-features', '--features', 'internal-font']),
]:
    metadata = json.loads(subprocess.check_output([
        'cargo', 'metadata', '--locked', '--format-version', '1',
        '--filter-platform', 'aarch64-apple-darwin',
        '--manifest-path', str(root / project / 'Cargo.toml'), *features,
    ]))
    # Cargo metadata also lists test-only dependencies. Follow only the runtime
    # and build graph for the executable being distributed.
    nodes = {node['id']: node for node in metadata['resolve']['nodes']}
    pending = [metadata['resolve']['root']]
    included = set()
    while pending:
        package_id = pending.pop()
        if package_id in included:
            continue
        included.add(package_id)
        for dependency in nodes[package_id]['deps']:
            if any(kind['kind'] != 'dev' for kind in dependency['dep_kinds']):
                pending.append(dependency['pkg'])
    for package in metadata['packages']:
        if package['id'] in included and package['name'] != 'dv8_converter':
            packages[package['id']] = package
notices = []
for package in sorted(packages.values(), key=lambda p: (p['name'], p['version'])):
    source = Path(package['manifest_path']).parent
    files = set()
    for pattern in ['LICENSE*', 'COPYING*', 'COPYRIGHT*']:
        for path in source.glob(pattern):
            if path.is_file():
                files.add(path)
            elif path.is_dir():
                files.update(p for p in path.rglob('*') if p.is_file())
    if not files:
        raise SystemExit('Missing crate license files: ' + package['name'])
    target = destination / (package['name'] + '-' + package['version'])
    target.mkdir(exist_ok=True)
    hashes = {}
    for path in sorted(files):
        relative = path.relative_to(source)
        output = target / relative
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, output)
        hashes[relative.as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    notices.append({'package': package['name'], 'version': package['version'],
                    'license': package['license'], 'files_sha256': hashes})
(destination / 'manifest.json').write_text(json.dumps(notices, indent=2) + '\n')
print('Bundled Rust license notices:', len(notices))
