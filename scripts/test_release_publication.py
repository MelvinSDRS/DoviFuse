"""Exercise publication guards without contacting GitHub."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'scripts').mkdir()
        (self.root / 'dist').mkdir()
        shutil.copyfile(Path(__file__).with_name('publish-release.sh'), self.root / 'scripts/publish-release.sh')
        for name in ['DV8-Maker-arm64.dmg', 'DV8-Maker-sources.tar.gz']:
            data = ('fixture ' + name).encode()
            (self.root / 'dist' / name).write_bytes(data)
            (self.root / 'dist' / (name + '.sha256')).write_text(
                hashlib.sha256(data).hexdigest() + '  ' + name + '\n')
        self.log = self.root / 'calls.jsonl'
        fake = self.root / 'gh'
        fake.write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['CALL_LOG'], 'a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\\n')
if sys.argv[1:3] == ['release', 'view']:
    if '--jq' in sys.argv and '.url' in sys.argv:
        print('https://example.invalid/release')
    elif os.environ.get('EXISTING'):
        print(os.environ['EXISTING'])
    else:
        sys.exit(1)
if sys.argv[1:3] == ['release', 'upload'] and os.environ.get('FAIL_UPLOAD'):
    sys.exit(1)
''')
        fake.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH'],
                        GITHUB_REF='refs/heads/main', GITHUB_SHA='a' * 40,
                        RELEASE_CHANNEL='build', CALL_LOG=str(self.log))

    def run_publish(self, **env):
        return subprocess.run(['bash', str(self.root / 'scripts/publish-release.sh')],
                              env={**self.env, **env}, capture_output=True, text=True)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_build_uploads_all_assets_before_publishing(self):
        self.assertEqual(self.run_publish().returncode, 0)
        calls = self.calls()
        create = next(c for c in calls if c[:2] == ['release', 'create'])
        upload = next(c for c in calls if c[:2] == ['release', 'upload'])
        edit = next(c for c in calls if c[:2] == ['release', 'edit'])
        self.assertIn('--draft', create)
        self.assertIn('--prerelease=false', edit)
        self.assertIn('--latest=true', edit)
        self.assertEqual(sum(c.startswith('dist/') for c in upload), 4)
        self.assertLess(calls.index(upload), calls.index(edit))

    def test_failed_upload_never_publishes_draft(self):
        self.assertNotEqual(self.run_publish(FAIL_UPLOAD='1').returncode, 0)
        self.assertFalse(any(c[:2] == ['release', 'edit'] for c in self.calls()))

    def test_invalid_checksum_never_contacts_github(self):
        (self.root / 'dist/DV8-Maker-arm64.dmg').write_bytes(b'corrupted')
        self.assertNotEqual(self.run_publish().returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_missing_sources_never_contacts_github(self):
        (self.root / 'dist/DV8-Maker-sources.tar.gz').unlink()
        self.assertNotEqual(self.run_publish().returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_non_main_cannot_publish(self):
        self.assertNotEqual(self.run_publish(GITHUB_REF='refs/pull/1/merge').returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_validated_cannot_publish_without_acceptance(self):
        verifier = self.root / 'scripts/verify_reference_catalog.py'
        verifier.write_text('raise SystemExit("Acceptance pending")\n')
        result = self.run_publish(RELEASE_CHANNEL='validated')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Acceptance pending', result.stderr)
        self.assertEqual(self.calls(), [])

    def test_rerun_keeps_published_release(self):
        self.assertEqual(self.run_publish(EXISTING='false').returncode, 0)
        self.assertEqual(len(self.calls()), 1)


if __name__ == '__main__':
    unittest.main()
