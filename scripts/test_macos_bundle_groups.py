#!/usr/bin/env python3
"""Focused tests for the bounded macOS audit scheduler."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import unittest

from run_macos_bundle_groups import Task, build_lanes, build_tasks, load_replays, run_tasks


class MacOSBundleGroupTests(unittest.TestCase):
    def test_all_audits_are_assigned_to_four_lanes(self):
        tasks = build_tasks(Path("/tmp"), {})
        lanes = build_lanes(tasks)
        self.assertEqual(len(lanes), 4)
        self.assertEqual(
            {task.name for lane in lanes for task in lane},
            {task.name for task in tasks},
        )

    def test_optional_smb_audit_is_assigned_without_dropping_core_audits(self):
        tasks = build_tasks(Path("/tmp"), {"DOVIFUSE_AUDIT_SMB_ROOT": "/Volumes/audit"})
        lanes = build_lanes(tasks)
        self.assertIn("smb", [task.name for task in lanes[-1]])
        self.assertEqual(sum(len(lane) for lane in lanes), len(tasks))

    def test_failure_exit_is_recorded_and_later_lane_task_still_runs(self):
        with tempfile.TemporaryDirectory(prefix="dovifuse-runner-test-") as directory:
            run_dir = Path(directory)
            tasks = [
                Task("missing", "this-script-does-not-exist.py"),
                Task("launcher-after-failure", "test_launcher.py"),
            ]
            results = run_tasks([tasks], run_dir, dict(os.environ), workers=1)
        codes = {str(result["name"]): result["exit_code"] for result in results}
        self.assertNotEqual(codes["missing"], 0)
        self.assertEqual(codes["launcher-after-failure"], 0)

    def test_replay_count_mismatch_is_reported(self):
        with tempfile.TemporaryDirectory(prefix="dovifuse-replay-test-") as directory:
            run_dir = Path(directory)
            task = Task("audit", "audit.py", expected_replays=1)
            (run_dir / "audit.app-replay.json").write_text("[]\n")
            aggregate, errors = load_replays([task], run_dir)
        self.assertEqual(aggregate, [])
        self.assertTrue(any("expected 1 replay entries" in error for error in errors))

    def test_duplicate_replay_names_are_rejected(self):
        with tempfile.TemporaryDirectory(prefix="dovifuse-replay-test-") as directory:
            run_dir = Path(directory)
            tasks = [
                Task("first", "first.py", expected_replays=1),
                Task("second", "second.py", expected_replays=1),
            ]
            for task in tasks:
                (run_dir / f"{task.name}.app-replay.json").write_text(
                    json.dumps([{"name": "duplicate-job"}])
                )
            aggregate, errors = load_replays(tasks, run_dir)
        self.assertEqual(len(aggregate), 2)
        self.assertIn("duplicate job names", " ".join(errors))


if __name__ == "__main__":
    unittest.main()
