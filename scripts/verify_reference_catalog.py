#!/usr/bin/env python3
"""Verify fixture provenance; --release also requires recorded acceptance evidence.
A passing catalog check is not a passing color or playback validation.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
REQUIRED_GATES = {
    'container-preservation', 'app-report-consistency',
    'full-length-performance', 'dolby-playback-qc',
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def verify(root, release=False):
    catalog = json.loads((root/'tests/fixtures/catalog.json').read_text())
    require(catalog['schema_version'] == 1, 'Unknown catalog schema')
    revision = subprocess.check_output(['git', '-C', str(root/'dovi_tool'), 'rev-parse', 'HEAD'], text=True).strip()
    require(revision == catalog['upstream']['revision'], 'Unexpected dovi_tool revision')
    require((root/catalog['upstream']['license']).is_file(), 'Upstream license missing')
    for fixture in catalog['fixtures']:
        require(digest(root/fixture['path']) == fixture['sha256'], fixture['path'])
    require(catalog['synthetic']['kind'] == 'synthetic-mechanics-only', 'Invalid fixture kind')
    require(catalog['synthetic']['reference_color'] is None, 'Synthetic RPU is not a P5 color reference')
    gates = json.loads((root/'tests/fixtures/release-gates.json').read_text())
    require(gates['schema_version'] == 1, 'Unknown gate schema')
    require(gates.get('workflow') == 'p7-p8-only' and gates.get('p5_hybrid') == 'disabled', 'Main must remain P7/P8-only; P5 acceptance belongs on its branch')
    require(len({g['id'] for g in gates['gates']}) == len(gates['gates']), 'Duplicate gates')
    require({g['id'] for g in gates['gates']} == REQUIRED_GATES, 'Missing or unknown release gate')
    pending = []
    for gate in gates['gates']:
        require(gate['status'] in ('pending', 'passed'), gate['id'])
        if gate['status'] == 'pending':
            pending.append(gate['id'])
            continue
        # Evidence is reviewed acceptance documentation, not a boolean waiver.
        require(gate['evidence'], f"Missing evidence for {gate['id']}")
        for evidence in gate['evidence']:
            path = (root/evidence['path']).resolve()
            require(root.resolve() in path.parents, 'Evidence must be a repository artifact')
            require(evidence['reviewer'] and evidence['method'], 'Evidence needs provenance')
            require(digest(path) == evidence['sha256'], f"Changed evidence: {path}")
    result = {'catalog':'passed', 'release':'pending' if pending else 'passed', 'pending':pending}
    print(json.dumps(result, indent=2))
    if release and pending:
        raise SystemExit('Release withheld: acceptance evidence is pending; see tests/fixtures/release-gates.json')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    args = parser.parse_args()
    verify(ROOT, args.release)
