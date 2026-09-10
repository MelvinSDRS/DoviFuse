"""Negative controls for false release acceptance and corrupted fixtures."""
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from audit_fixtures import prepare, SEEDS, sha256
from verify_reference_catalog import ROOT, verify


class ReferenceGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root/'tests/fixtures').mkdir(parents=True)
        (self.root/'dovi_tool').symlink_to(ROOT/'dovi_tool', target_is_directory=True)
        for name in ['catalog.json', 'release-gates.json']:
            (self.root/'tests/fixtures'/name).write_bytes((ROOT/'tests/fixtures'/name).read_bytes())
        gates = json.loads((self.root/'tests/fixtures/release-gates.json').read_text())
        for gate in gates['gates']:
            for evidence in gate['evidence']:
                target = self.root/evidence['path']
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((ROOT/evidence['path']).read_bytes())

    def edit(self, name, change):
        path = self.root/'tests/fixtures'/name
        data = json.loads(path.read_text())
        change(data)
        path.write_text(json.dumps(data))

    def verify(self, release=False):
        with contextlib.redirect_stdout(io.StringIO()):
            return verify(self.root, release)

    def test_catalog_success_is_not_release_success(self):
        self.assertEqual(self.verify()['release'], 'pending')
        with self.assertRaises(SystemExit):
            self.verify(release=True)

    def test_changed_upstream_bytes_are_rejected(self):
        self.edit('catalog.json', lambda c: c['fixtures'][0].update(sha256='0'*64))
        with self.assertRaises(ValueError):
            self.verify()

    def test_synthetic_metadata_cannot_be_promoted_to_color_reference(self):
        self.edit('catalog.json', lambda c: c['synthetic'].update(reference_color='self-generated'))
        with self.assertRaises(ValueError):
            self.verify()

    def test_removing_pending_gates_does_not_unlock_publication(self):
        self.edit('release-gates.json', lambda c: c.update(gates=[]))
        with self.assertRaises(ValueError):
            self.verify(release=True)

    def test_passed_requires_evidence(self):
        self.edit('release-gates.json', lambda c: c['gates'][0].update(status='passed'))
        with self.assertRaises(ValueError):
            self.verify()

    def test_changed_preservation_evidence_is_rejected(self):
        gates = json.loads((self.root/'tests/fixtures/release-gates.json').read_text())
        gate = next(g for g in gates['gates'] if g['id'] == 'container-preservation')
        self.assertEqual(gate['status'], 'passed')
        (self.root/gate['evidence'][0]['path']).write_text('altered evidence')
        with self.assertRaisesRegex(ValueError, 'Changed evidence'):
            self.verify()

    def test_corrupted_prepared_media_is_rejected_before_conversion(self):
        source = self.root/'seeds'
        source.mkdir()
        for name in SEEDS:
            (source/name).write_bytes(b'disposable fixture')
        manifest = {'kind':'synthetic-mechanics-only', 'sha256':{n:sha256(source/n) for n in SEEDS}}
        (source/'manifest.json').write_text(json.dumps(manifest))
        (source/SEEDS[0]).write_bytes(b'corrupted')
        output = self.root/'output'
        output.mkdir()
        with patch.dict(os.environ, DV8_AUDIT_FIXTURES=str(source)):
            with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                prepare(output, self.root, None, None, None)
        self.assertEqual(list(output.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
