#!/usr/bin/env python3
"""Check launcher configuration without touching media."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class LauncherConfiguration(unittest.TestCase):
    def test_configured_engine_receives_script_directory_and_arguments(self):
        self.check_launcher('DoviFuse.sh')

    def check_launcher(self, launcher):
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            stub = work / 'converter'
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$DOVIFUSE_SCRIPT_DIR" "$@"\n')
            stub.chmod(0o755)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith('DOVIFUSE_')}
            env['DOVIFUSE_ENV_FILE'] = str(work / 'absent.env')
            env['DOVIFUSE_CONVERTER_BIN'] = str(stub)
            result = subprocess.run([str(ROOT / launcher), '--check', 'movie with spaces.mkv'],
                                    env=env, check=True, capture_output=True, text=True)
            self.assertEqual(result.stdout.splitlines(),
                             [str(ROOT), '--check', 'movie with spaces.mkv'])


if __name__ == '__main__':
    unittest.main()
