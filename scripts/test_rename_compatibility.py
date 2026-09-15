#!/usr/bin/env python3
"""Protect existing automation during the DoviFuse upgrade without touching media."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class LauncherCompatibility(unittest.TestCase):
    def test_old_launcher_and_environment_reach_new_engine(self):
        self.check_launcher('DV7toDV8.sh', legacy=True)

    def test_new_launcher_and_environment_reach_new_engine(self):
        self.check_launcher('DoviFuse.sh', legacy=False)

    def test_new_configuration_takes_precedence(self):
        self.check_launcher('DoviFuse.sh', legacy=False, conflict=True)

    def test_legacy_binary_receives_legacy_resources_and_new_options(self):
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            stub = work / 'old-engine'
            stub.write_text('#!/bin/sh\ncat "$DV8_SCRIPT_DIR/config/DV7toDV8.json" >/dev/null || exit 2\nprintf "%s" "$DV8_PROCESSING_LOG_FILE"\n')
            stub.chmod(0o755)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('DV8_', 'DOVIFUSE_'))}
            env.update(DV8_CONVERTER_BIN=str(stub), DV8_ENV_FILE=str(work / 'absent.env'),
                       DOVIFUSE_PROCESSING_LOG_FILE=str(work / 'new.log'))
            result = subprocess.run([str(ROOT / 'DoviFuse.sh'), '--check', 'movie.mkv'],
                                    env=env, check=True, capture_output=True, text=True)
            self.assertEqual(result.stdout, str(work / 'new.log'))

    def check_launcher(self, launcher, legacy, conflict=False):
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            stub = work / 'converter'
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$DOVIFUSE_SCRIPT_DIR" "$@"\n')
            stub.chmod(0o755)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(('DV8_', 'DOVIFUSE_'))}
            prefix = 'DV8_' if legacy else 'DOVIFUSE_'
            env[prefix + 'ENV_FILE'] = str(work / 'absent.env')
            env[prefix + 'CONVERTER_BIN'] = str(stub)
            if conflict:
                env['DV8_CONVERTER_BIN'] = '/does/not/exist'
                env['DV8_ENV_FILE'] = '/does/not/exist'
            result = subprocess.run([str(ROOT / launcher), '--check', 'movie with spaces.mkv'],
                                    env=env, check=True, capture_output=True, text=True)
            self.assertEqual(result.stdout.splitlines(),
                             [str(ROOT), '--check', 'movie with spaces.mkv'])


if __name__ == '__main__':
    unittest.main()
