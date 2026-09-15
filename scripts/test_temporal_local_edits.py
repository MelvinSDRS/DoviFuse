#!/usr/bin/env python3
"""Real-tool regression for locally shifted RPU scene cuts.

The fixture is deliberately synthetic: a small 64x36 PQ HEVC video is made
of 300 irregular, high-contrast scenes.  The two DV donors use the same video
and canonical Profile 8.1 RPUs, except that twenty consecutive RPU scene
starts are shifted by +12 and +48 frames.  The video scene cuts therefore
still support the global offset 0 while exposing a local edit contradiction.

Prepare a portable fixture on an encoder-capable host (the Linux CI job)::

    python3 scripts/test_temporal_local_edits.py --prepare /tmp/dv8-temporal-fixture

Then run it with ``DV8_TEMPORAL_LOCAL_FIXTURES`` on Linux or macOS.  The Mac
bundle check consumes this prebuilt fixture; it does not require an HEVC
encoder.  The fixture and all conversion outputs remain outside the
repository.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from typing import Iterable, Mapping, Sequence


ROOT = Path(__file__).resolve().parents[1]
RESOURCES = Path(os.environ.get("DV8_AUDIT_RESOURCES", ROOT))
FRAME_COUNT = 300
LOCAL_START = 120
LOCAL_COUNT = 20
LOCAL_DELTAS = (12, 48)
# Keeping every authored scene at least 72 frames means even the +48 edit
# leaves a positive interval at both sides of the edited run.


def authored_durations() -> tuple[int, ...]:
    # A small deterministic PRNG avoids periodic cut schedules.  A repeated
    # color pattern is useful for scene detection, but repeated durations can
    # make a false nonzero offset dominate the correlation vote.
    state = 0x5EED1234
    values = []
    for _ in range(FRAME_COUNT):
        state = (1103515245 * state + 12345) & 0x7FFFFFFF
        values.append(72 + ((state >> 8) % 25))
    return tuple(values)


DURATIONS = authored_durations()
TOTAL_FRAMES = sum(DURATIONS)
SCENE_STARTS = tuple(sum(DURATIONS[:index]) for index in range(FRAME_COUNT))
MEDIA_NAMES = ("hdr.mkv", "local-12.mkv", "local-48.mkv")
MANIFEST_KIND = "synthetic-temporal-local-edits"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def executable(name: str, resources: Path = RESOURCES) -> Path:
    override = os.environ.get(f"DV8_TEMPORAL_{name.upper()}")
    if override:
        return Path(override)
    # System MKVToolNix is preferred on Linux because the bundled copies can
    # depend on the private libraries shipped only in the macOS bundle.
    found = shutil.which(name)
    if found:
        return Path(found)
    bundled = resources / "tools" / name
    if bundled.exists():
        return bundled
    raise RuntimeError(f"Required media tool not found: {name}")


def command(
    args: Iterable[object],
    name: str,
    work: Path,
    *,
    env: Mapping[str, str] | None = None,
    timeout: int = 900,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        [str(argument) for argument in args],
        env=dict(env) if env is not None else None,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    (work / f"{name}.log").write_text(result.stdout + "\n" + result.stderr)
    if result.returncode:
        raise RuntimeError(
            f"{name} failed with exit {result.returncode}; see {work / f'{name}.log'}"
        )
    return result


def shifted_starts(delta: int) -> tuple[int, ...]:
    starts = list(SCENE_STARTS)
    for index in range(LOCAL_START, LOCAL_START + LOCAL_COUNT):
        starts[index] += delta
    return tuple(starts)


def shots(starts: Sequence[int]) -> list[dict[str, object]]:
    result: list[dict[str, object]] = []
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else TOTAL_FRAMES
        duration = end - start
        if duration <= 0:
            raise RuntimeError(f"Invalid generated RPU interval at shot {index}: {duration}")
        result.append({"start": start, "duration": duration, "metadata_blocks": []})
    return result


def generate_rpu(
    dovi: Path,
    destination: Path,
    starts: Sequence[int],
    work: Path,
    name: str,
) -> None:
    config = {
        "cm_version": "V40",
        "profile": "8.1",
        "level6": {
            "max_display_mastering_luminance": 1000,
            "min_display_mastering_luminance": 1,
            "max_content_light_level": 1000,
            "max_frame_average_light_level": 400,
        },
        "shots": shots(starts),
    }
    config_path = work / f"{name}.json"
    config_path.write_text(json.dumps(config, separators=(",", ":")) + "\n")
    command([dovi, "generate", "-j", config_path, "-o", destination], name, work)


def generate_video(ffmpeg: Path, destination: Path, work: Path) -> None:
    # Each color source is a constant frame run, so this remains cheap to
    # decode and avoids a 300-branch per-pixel geq expression.  Alternating
    # black/gray/white levels provide deterministic scdet cuts.
    segments: list[str] = []
    colors = ("black", "gray", "white", "gray")
    for index, duration in enumerate(DURATIONS):
        seconds = duration / 24.0
        segments.append(
            f"color=c={colors[index % len(colors)]}:s=64x36:r=24:d={seconds:.9f}[s{index}]"
        )
    concat_inputs = "".join(f"[s{index}]" for index in range(FRAME_COUNT))
    filter_graph = ";".join(segments) + ";" + concat_inputs + (
        f"concat=n={FRAME_COUNT}:v=1:a=0,format=yuv420p10le"
    )
    command(
        [
            ffmpeg,
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            filter_graph,
            "-frames:v",
            TOTAL_FRAMES,
            "-an",
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p10le",
            "-x265-params",
            "pools=2:frame-threads=2:log-level=error:colorprim=9:transfer=16:colormatrix=9:range=limited:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
            "-y",
            destination,
        ],
        "generate-hdr-hevc",
        work,
        timeout=300,
    )


def mux(mkvmerge: Path, hevc: Path, destination: Path, work: Path, name: str) -> None:
    command([mkvmerge, "-o", destination, hevc], name, work)


def prepare(destination: Path) -> None:
    if destination.exists():
        raise RuntimeError(f"Refusing to overwrite fixture directory: {destination}")
    destination.mkdir(parents=True)
    work = Path(tempfile.mkdtemp(prefix="dv8-temporal-prepare-", dir="/tmp"))
    ffmpeg = executable("ffmpeg")
    dovi = executable("dovi_tool")
    mkvmerge = executable("mkvmerge")
    try:
        hevc = work / "hdr.hevc"
        generate_video(ffmpeg, hevc, work)
        mux(mkvmerge, hevc, destination / "hdr.mkv", work, "mux-hdr")

        canonical = work / "canonical.bin"
        generate_rpu(dovi, canonical, SCENE_STARTS, work, "generate-canonical-rpu")
        for delta in LOCAL_DELTAS:
            rpu = work / f"local-{delta}.bin"
            generate_rpu(dovi, rpu, shifted_starts(delta), work, f"generate-local-{delta}-rpu")
            local_hevc = work / f"local-{delta}.hevc"
            command(
                [dovi, "inject-rpu", "-i", hevc, "-r", rpu, "-o", local_hevc],
                f"inject-local-{delta}",
                work,
            )
            mux(mkvmerge, local_hevc, destination / f"local-{delta}.mkv", work, f"mux-local-{delta}")

        manifest = {
            "kind": "synthetic-temporal-local-edits",
            "recipe": "scripts/test_temporal_local_edits.py",
            "synthetic_only": True,
            "frames": TOTAL_FRAMES,
            "cut_count": FRAME_COUNT,
            "local_start_index": LOCAL_START,
            "local_cut_count": LOCAL_COUNT,
            "local_deltas": list(LOCAL_DELTAS),
            "scene_durations_min": min(DURATIONS),
            "scene_durations_max": max(DURATIONS),
            "sha256": {name: sha256(destination / name) for name in MEDIA_NAMES},
        }
        (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print(destination)


def load_fixture(source: Path) -> dict[str, object]:
    try:
        manifest = json.loads((source / "manifest.json").read_text())
    except (OSError, ValueError) as error:
        raise RuntimeError(
            f"Unable to read temporal-local fixture manifest: {source / 'manifest.json'}"
        ) from error
    if not isinstance(manifest, dict):
        raise RuntimeError("Temporal-local fixture manifest must be a JSON object")
    if manifest.get("kind") != MANIFEST_KIND or not manifest.get("synthetic_only"):
        raise RuntimeError("Fixture is not the synthetic temporal-local-edits fixture")
    if manifest.get("frames") != TOTAL_FRAMES or manifest.get("cut_count") != FRAME_COUNT:
        raise RuntimeError("Fixture schedule does not match this regression script")
    if (
        manifest.get("local_start_index") != LOCAL_START
        or manifest.get("local_cut_count") != LOCAL_COUNT
    ):
        raise RuntimeError("Fixture local-edit schedule does not match this regression script")
    if manifest.get("local_deltas") != list(LOCAL_DELTAS):
        raise RuntimeError("Fixture local offsets do not match this regression script")
    if (
        manifest.get("scene_durations_min") != min(DURATIONS)
        or manifest.get("scene_durations_max") != max(DURATIONS)
    ):
        raise RuntimeError("Fixture scene duration bounds do not match this regression script")
    checksums = manifest.get("sha256")
    if not isinstance(checksums, dict):
        raise RuntimeError("Fixture manifest is missing media checksums")
    for name in MEDIA_NAMES:
        path = source / name
        expected = checksums.get(name)
        if not isinstance(expected, str) or len(expected) != 64:
            raise RuntimeError(f"Fixture manifest is missing a valid checksum: {name}")
        if not path.is_file() or sha256(path) != expected:
            raise RuntimeError(f"Fixture checksum mismatch: {name}")
    return manifest


def report_temporal(report: Path, expected_delta: int) -> dict[str, object]:
    data = json.loads(report.read_text())
    measurements = data.get("measurements", [])
    entries = [entry for entry in measurements if entry.get("key") == "temporal_alignment"]
    if not entries:
        raise AssertionError(f"No temporal_alignment measurement in {report}")
    value = entries[-1].get("value", {})
    contradictions = value.get("contradictions", [])
    if not contradictions:
        raise AssertionError(f"No local contradiction recorded in {report}")
    if not any(abs(item.get("alternate_offset", 0)) == expected_delta for item in contradictions):
        raise AssertionError(
            f"Expected local offset {expected_delta} in {report}: {contradictions}"
        )
    return value


def run_conversion(
    binary: Path,
    source: Path,
    target: Path,
    work: Path,
    name: str,
    flags: Sequence[str],
    *,
    expect_success: bool,
    expected_delta: int,
    resources: Path,
) -> subprocess.CompletedProcess[str]:
    output = work / f"{name}.mkv"
    report = work / f"{name}.json"
    log = work / f"{name}.processing.log"
    env = dict(os.environ)
    env["DV8_SCRIPT_DIR"] = str(resources)
    env["DV8_PROCESSING_LOG_FILE"] = str(log)
    # Keep host tools first on Linux; bundled MKVToolNix copies may depend on
    # private libraries.  On macOS the host lookup falls through to resources.
    env["PATH"] = os.pathsep.join((env.get("PATH", ""), str(resources / "tools")))
    result = subprocess.run(
        [
            str(binary),
            "--hybrid",
            "--hwaccel",
            "off",
            "--progress",
            "jsonl",
            "--report",
            report,
            "--skip-grade-check",
            "--letterbox",
            "off",
            *flags,
            "-o",
            output,
            source,
            target,
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=300,
    )
    (work / f"{name}.stdout.log").write_text(result.stdout + "\n" + result.stderr)
    if expect_success and result.returncode:
        raise AssertionError(f"{name} failed with exit {result.returncode}; see {work}")
    if not expect_success:
        if result.returncode == 0:
            raise AssertionError(f"{name} unexpectedly succeeded; see {work}")
        data = json.loads(report.read_text())
        if data.get("execution") != "failed":
            raise AssertionError(f"{name} report was not failed: {data.get('execution')}")
        report_temporal(report, expected_delta)
        text = result.stdout + result.stderr
        if "local scene offsets contradict" not in text:
            raise AssertionError(f"{name} did not explain the local contradiction")
    else:
        data = json.loads(report.read_text())
        if data.get("execution") != "completed":
            raise AssertionError(f"{name} report was not completed: {data.get('execution')}")
    return result


def verify(
    source: Path,
    resources: Path,
    binary: Path,
    baseline: Path | None,
) -> None:
    manifest = load_fixture(source)
    work = Path(tempfile.mkdtemp(prefix="dv8-temporal-local-", dir="/tmp"))
    before = {name: sha256(source / name) for name in MEDIA_NAMES}
    print(f"Temporal local-edit controls: {work}", flush=True)
    try:
        # The pre-gate binary is a reproduction control: both local edits
        # retain global offset zero and were accepted with ordinary overrides.
        if baseline is not None:
            for delta in LOCAL_DELTAS:
                run_conversion(
                    baseline,
                    source / f"local-{delta}.mkv",
                    source / "hdr.mkv",
                    work,
                    f"baseline-{delta}",
                    (),
                    expect_success=True,
                    expected_delta=delta,
                    resources=resources,
                )

        for delta in LOCAL_DELTAS:
            donor = source / f"local-{delta}.mkv"
            cases = (
                ("default", ()),
                ("force", ("--force",)),
                ("explicit-zero-reviewed", ("--offset", "0", "--allow-padding")),
            )
            for suffix, flags in cases:
                run_conversion(
                    binary,
                    donor,
                    source / "hdr.mkv",
                    work,
                    f"fixed-{delta}-{suffix}",
                    flags,
                    expect_success=False,
                    expected_delta=delta,
                    resources=resources,
                )
    finally:
        for name, digest in before.items():
            if sha256(source / name) != digest:
                raise AssertionError(f"Input fixture changed: {name}")
    print(
        json.dumps(
            {
                "fixture": str(source),
                "synthetic_only": manifest["synthetic_only"],
                "frames": manifest["frames"],
                "local_deltas": manifest["local_deltas"],
                "source_hashes_preserved": True,
                "baseline_reproduced": baseline is not None,
                "reports": "temporal_alignment contradictions asserted for every fixed case",
                "work": str(work),
            },
            indent=2,
        )
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path, metavar="DIR")
    parser.add_argument(
        "--fixtures",
        type=Path,
        default=Path(os.environ.get("DV8_TEMPORAL_LOCAL_FIXTURES", ""))
        if os.environ.get("DV8_TEMPORAL_LOCAL_FIXTURES")
        else None,
    )
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path(os.environ.get("DV8_AUDIT_BIN", ROOT / "dv8_converter/target/debug/dv8_converter")),
    )
    baseline_value = os.environ.get("DV8_TEMPORAL_BASELINE_BIN")
    parser.add_argument(
        "--baseline",
        type=Path,
        default=Path(baseline_value) if baseline_value else None,
        help="Optional pre-gate binary for the local-contradiction acceptance reproduction",
    )
    parser.add_argument("--resources", type=Path, default=RESOURCES)
    args = parser.parse_args()
    try:
        if args.prepare is not None:
            prepare(args.prepare.resolve())
            return 0
        if args.fixtures is None:
            raise RuntimeError("Set DV8_TEMPORAL_LOCAL_FIXTURES or pass --fixtures")
        paths = [(args.fixtures, "fixture"), (args.binary, "fixed binary")]
        if args.baseline is not None:
            paths.append((args.baseline, "baseline binary"))
        for path, label in paths:
            if not path.exists():
                raise RuntimeError(f"Missing {label}: {path}")
        verify(
            args.fixtures.resolve(),
            args.resources.resolve(),
            args.binary.resolve(),
            args.baseline.resolve() if args.baseline is not None else None,
        )
    except (AssertionError, OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
