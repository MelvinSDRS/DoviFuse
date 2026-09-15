#!/usr/bin/env python3
"""Run independent platform audits with bounded concurrency.

Every audit already creates its own disposable work directory.  This runner
keeps those boundaries explicit by giving each process its own replay manifest
and log, then combines the manifests only after every process has exited.  A
failed audit does not cancel the remaining audits: the final summary records
each exit code so a CI failure cannot hide a second failure.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
from collections import deque
from typing import IO, Iterable


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_WORKERS = 4
MAX_WORKERS = 4


@dataclass(frozen=True)
class Task:
    name: str
    script: str
    args: tuple[str, ...] = ()
    expected_replays: int = 0


def build_tasks(
    fixtures: Path, environment: dict[str, str], platform: str = "macos"
) -> list[Task]:
    """Return every audit for ``platform``, including optional macOS SMB coverage."""

    if platform == "linux":
        # Keep this list aligned with the former serial section of
        # test-linux.sh.  The fixture preparation remains in the shell wrapper;
        # only the independent, read-only audits run concurrently here.
        return [
            Task("reference-catalog", "verify_reference_catalog.py"),
            Task("audit-smoke", "audit_smoke.py", expected_replays=20),
            Task("donor-eligibility", "test_donor_eligibility.py"),
            Task("mapping-policy", "test_mapping_policy.py"),
            Task("metadata-transport", "test_metadata_transport.py"),
            Task("temporal-alignment", "test_temporal_alignment.py"),
            Task("picture-coverage", "test_picture_coverage.py"),
            Task("p2-reuse", "test_p2_reuse.py"),
            Task(
                "temporal-local-edits",
                "test_temporal_local_edits.py",
                ("--fixtures", str(fixtures / "temporal-local")),
            ),
            Task("p5-disabled", "test_p5_disabled.py", expected_replays=6),
            Task("standard-source", "audit_standard_source.py"),
            Task("job-report", "test_job_report.py"),
            Task("faults", "audit_faults.py", expected_replays=18),
            Task("preservation", "audit_preservation.py"),
            Task("l5", "audit_l5.py"),
            Task("wrapper-smoke", "audit_wrapper_smoke.py"),
        ]

    if platform != "macos":
        raise ValueError(f"unsupported audit platform: {platform}")

    tasks = [
        # These expected replay counts are part of the app-state coverage
        # contract.  They catch an accidentally omitted audit even when the
        # AppModel's lower-bound assertion would still pass.
        Task("audit-smoke", "audit_smoke.py", expected_replays=20),
        Task("donor-eligibility", "test_donor_eligibility.py"),
        Task("mapping-policy", "test_mapping_policy.py"),
        Task("metadata-transport", "test_metadata_transport.py"),
        Task("temporal-alignment", "test_temporal_alignment.py"),
        Task("picture-coverage", "test_picture_coverage.py"),
        Task("p2-reuse", "test_p2_reuse.py"),
        Task(
            "temporal-local-edits",
            "test_temporal_local_edits.py",
            ("--fixtures", str(fixtures / "temporal-local")),
        ),
        Task("p5-disabled", "test_p5_disabled.py", expected_replays=6),
        Task("standard-source", "audit_standard_source.py"),
        Task("job-report", "test_job_report.py"),
        Task("faults", "audit_faults.py", expected_replays=18),
        Task("native-storage", "audit_macos_storage.py", expected_replays=2),
        Task("preservation", "audit_preservation.py"),
        Task("l5", "audit_l5.py"),
    ]
    smb_root = environment.get("DOVIFUSE_AUDIT_SMB_ROOT")
    if smb_root:
        tasks.append(
            Task(
                "smb",
                "audit_macos_smb.py",
                ("--destination-root", smb_root),
                expected_replays=3,
            )
        )
    return tasks


def build_lanes(tasks: list[Task], platform: str = "macos") -> list[list[Task]]:
    """Partition independent audits into four bounded, duration-balanced lanes.

    Each lane still runs its tasks as separate processes, preserving per-task
    exit codes, logs, and replay manifests.  Layouts are platform-specific
    because Linux has a few catalog/wrapper checks while macOS has native
    storage and optional SMB checks.
    """

    if platform == "linux":
        # Based on the serial timings: the four lane totals are approximately
        # 85s, 92s, 86s, and 70s on the reference runner.  The longest tests
        # start immediately and no lane shares a fixture output directory.
        layout = [
            ("faults", "l5", "p5-disabled"),
            ("temporal-local-edits", "metadata-transport"),
            ("picture-coverage", "preservation", "mapping-policy", "p2-reuse"),
            (
                "audit-smoke",
                "temporal-alignment",
                "donor-eligibility",
                "standard-source",
                "job-report",
                "reference-catalog",
                "wrapper-smoke",
            ),
        ]
        optional_tasks: set[str] = set()
    elif platform == "macos":
        layout = [
            ("preservation", "metadata-transport", "job-report"),
            ("faults", "picture-coverage"),
            ("audit-smoke", "l5", "temporal-alignment", "p5-disabled"),
            (
                "temporal-local-edits",
                "donor-eligibility",
                "native-storage",
                "mapping-policy",
                "standard-source",
                "p2-reuse",
            ),
        ]
        optional_tasks = {"smb"}
    else:
        raise ValueError(f"unsupported audit platform: {platform}")
    by_name = {task.name: task for task in tasks}
    lanes: list[list[Task]] = []
    assigned: set[str] = set()
    for names in layout:
        lane = [by_name[name] for name in names if name in by_name]
        lanes.append(lane)
        assigned.update(task.name for task in lane)
    unassigned = [task for task in tasks if task.name not in assigned]
    # Optional controls belong to the last lane.  Failing loudly here keeps a
    # future audit from silently disappearing from the bounded runner.
    if unassigned:
        if len(lanes) != 4 or {task.name for task in unassigned} != optional_tasks:
            raise RuntimeError(
                f"{platform} audit runner lane layout is missing an explicit task assignment: "
                + ", ".join(task.name for task in unassigned)
            )
        lanes[-1].extend(unassigned)
    if {task.name for lane in lanes for task in lane} != set(by_name):
        raise RuntimeError(f"{platform} audit runner lane layout is incomplete")
    return lanes


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def launch_task(
    task: Task,
    run_dir: Path,
    base_environment: dict[str, str],
) -> tuple[subprocess.Popen[bytes], IO[bytes], Path, Path, float]:
    """Start one audit and return its process plus evidence paths."""

    manifest = run_dir / f"{task.name}.app-replay.json"
    log = run_dir / f"{task.name}.runner.log"
    manifest.write_text("[]\n")
    environment = dict(base_environment)
    environment["DOVIFUSE_APP_REPLAY_MANIFEST"] = str(manifest)
    command = [sys.executable, str(ROOT / "scripts" / task.script), *task.args]
    stream = log.open("wb")
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            command,
            cwd=ROOT,
            env=environment,
            stdout=stream,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    except Exception:
        stream.close()
        log.write_text(
            "Unable to launch task: " + " ".join(command) + "\n",
            encoding="utf-8",
        )
        raise
    return process, stream, manifest, log, started


def terminate_processes(
    active: dict[
        int,
        tuple[int, Task, subprocess.Popen[bytes], IO[bytes], Path, Path, float],
    ],
) -> None:
    """Terminate only children started by this runner after interruption."""

    for _, (_, _, process, _, _, _, _) in active.items():
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    deadline = time.monotonic() + 5
    for _, (_, _, process, stream, _, _, _) in active.items():
        remaining = max(0.0, deadline - time.monotonic())
        try:
            process.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
        stream.close()


def run_tasks(
    lanes: list[list[Task]],
    run_dir: Path,
    base_environment: dict[str, str],
    workers: int,
) -> list[dict[str, object]]:
    """Run fixed sequential lanes while keeping at most ``workers`` active."""

    pending_lanes = deque(range(len(lanes)))
    lane_tasks = {index: deque(tasks) for index, tasks in enumerate(lanes)}
    active: dict[int, tuple[int, Task, subprocess.Popen[bytes], IO[bytes], Path, Path, float]] = {}
    results: list[dict[str, object]] = []

    def start_next_task(lane_index: int) -> None:
        queue = lane_tasks[lane_index]
        while queue:
            task = queue.popleft()
            try:
                process, stream, manifest, log, started = launch_task(
                    task, run_dir, base_environment
                )
                active[process.pid] = (
                    lane_index,
                    task,
                    process,
                    stream,
                    manifest,
                    log,
                    started,
                )
                return
            except Exception as error:
                results.append(
                    {
                        "name": task.name,
                        "script": task.script,
                        "args": list(task.args),
                        "lane": lane_index + 1,
                        "exit_code": 127,
                        "duration_seconds": 0.0,
                        "manifest": str(run_dir / f"{task.name}.app-replay.json"),
                        "log": str(run_dir / f"{task.name}.runner.log"),
                        "expected_replays": task.expected_replays,
                        "error": repr(error),
                    }
                )
        # A launch error does not prevent later tasks in another lane from
        # starting.  The completed lane is simply left without an active PID.

    previous_sigterm = signal.getsignal(signal.SIGTERM)

    def handle_sigterm(signum: int, _frame: object) -> None:
        terminate_processes(active)
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, handle_sigterm)
    try:
        while active or pending_lanes or any(lane_tasks.values()):
            while len(active) < workers and pending_lanes:
                lane_index = pending_lanes.popleft()
                start_next_task(lane_index)
            for pid, (lane_index, task, process, stream, manifest, log, started) in list(active.items()):
                code = process.poll()
                if code is None:
                    continue
                stream.close()
                active.pop(pid)
                result = {
                    "name": task.name,
                    "script": task.script,
                    "args": list(task.args),
                    "lane": lane_index + 1,
                    "exit_code": code,
                    "duration_seconds": round(time.monotonic() - started, 3),
                    "manifest": str(manifest),
                    "log": str(log),
                    "expected_replays": task.expected_replays,
                }
                results.append(result)
                print(
                    f"audit finished: {task.name} "
                    f"exit={code} duration={result['duration_seconds']}s",
                    flush=True,
                )
                if lane_tasks[lane_index]:
                    start_next_task(lane_index)
                elif len(active) < workers and pending_lanes:
                    next_lane = pending_lanes.popleft()
                    start_next_task(next_lane)
            if active:
                time.sleep(0.10)
    except KeyboardInterrupt:
        terminate_processes(active)
        raise
    finally:
        signal.signal(signal.SIGTERM, previous_sigterm)
    return results


def load_replays(
    tasks: Iterable[Task], run_dir: Path
) -> tuple[list[dict[str, object]], list[str]]:
    aggregate: list[dict[str, object]] = []
    errors: list[str] = []
    for task in tasks:
        path = run_dir / f"{task.name}.app-replay.json"
        try:
            entries = json.loads(path.read_text())
            if not isinstance(entries, list) or not all(isinstance(item, dict) for item in entries):
                raise ValueError("manifest must contain a JSON array of objects")
        except (OSError, ValueError, json.JSONDecodeError) as error:
            errors.append(f"{task.name}: invalid replay manifest: {error}")
            continue
        if len(entries) != task.expected_replays:
            errors.append(
                f"{task.name}: expected {task.expected_replays} replay entries, "
                f"found {len(entries)}"
            )
        aggregate.extend(entries)
    names = [str(entry.get("name")) for entry in aggregate]
    if len(names) != len(set(names)):
        errors.append("aggregate replay manifest contains duplicate job names")
    return aggregate, errors


def print_evidence(results: list[dict[str, object]], run_dir: Path) -> None:
    """Print concise per-task output while retaining complete runner logs."""

    lines: list[str] = []
    for result in sorted(results, key=lambda item: str(item["name"])):
        header = (
            f"=== {result['name']} exit={result['exit_code']} "
            f"duration={result['duration_seconds']}s log={result['log']} ==="
        )
        lines.append(header)
        log = Path(str(result["log"]))
        if log.is_file():
            content = log.read_text(errors="replace").rstrip()
            if content:
                lines.append(content)
    (run_dir / "runner.log").write_text("\n".join(lines) + "\n")
    print("\n".join(lines), flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixtures", type=Path)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    parser.add_argument("--platform", choices=("macos", "linux"), default="macos")
    options = parser.parse_args()
    if options.workers < 1 or options.workers > MAX_WORKERS:
        parser.error(f"--workers must be between 1 and {MAX_WORKERS}")
    fixtures = options.fixtures.resolve(strict=True)
    run_dir = options.run_dir.resolve()
    run_dir.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ)
    l5_build = "release" if options.platform == "macos" else "debug"
    environment.setdefault(
        "DOVIFUSE_L5_TIMELINE_BIN",
        str(ROOT / "dovifuse_converter" / "target" / l5_build / "examples" / "l5_timeline"),
    )
    tasks = build_tasks(fixtures, environment, options.platform)
    lanes = build_lanes(tasks, options.platform)
    lane_by_name = {
        task.name: lane_index + 1
        for lane_index, lane in enumerate(lanes)
        for task in lane
    }
    write_json(
        run_dir / "tasks.json",
        {
            "platform": options.platform,
            "workers": options.workers,
            "lanes": [
                {
                    "lane": lane_index + 1,
                    "tasks": [task.name for task in lane],
                }
                for lane_index, lane in enumerate(lanes)
            ],
            "tasks": [
                {
                    "name": task.name,
                    "script": task.script,
                    "args": list(task.args),
                    "expected_replays": task.expected_replays,
                    "lane": lane_by_name[task.name],
                }
                for task in tasks
            ],
        },
    )
    started = time.monotonic()
    print(
        f"Starting {len(tasks)} independent {options.platform} audits with "
        f"{options.workers} workers; evidence: {run_dir}",
        flush=True,
    )
    results = run_tasks(lanes, run_dir, environment, options.workers)
    aggregate, manifest_errors = load_replays(tasks, run_dir)
    aggregate_path = run_dir / "app-replay.json"
    write_json(aggregate_path, aggregate)
    print_evidence(results, run_dir)
    failures = [result for result in results if result["exit_code"] != 0]
    missing = {task.name for task in tasks} - {str(result["name"]) for result in results}
    if missing:
        manifest_errors.append("missing task results: " + ", ".join(sorted(missing)))
    lane_durations: dict[str, float] = {}
    for result in results:
        lane = str(result["lane"])
        lane_durations[lane] = round(
            lane_durations.get(lane, 0.0) + float(result["duration_seconds"]), 3
        )
    summary = {
        "schema_version": 1,
        "platform": options.platform,
        "workers": options.workers,
        "task_count": len(tasks),
        "duration_seconds": round(time.monotonic() - started, 3),
        "all_tasks_passed": not failures,
        "lane_durations_seconds": lane_durations,
        "aggregate_manifest": str(aggregate_path),
        "replay_count": len(aggregate),
        "failures": failures,
        "manifest_errors": manifest_errors,
        "tasks": sorted(results, key=lambda item: str(item["name"])),
    }
    write_json(run_dir / "summary.json", summary)
    print(
        f"{options.platform} audit summary: tasks={len(tasks)} "
        f"replays={len(aggregate)} duration={summary['duration_seconds']}s "
        f"failures={len(failures)} manifest_errors={len(manifest_errors)}",
        flush=True,
    )
    return 1 if failures or manifest_errors or missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
