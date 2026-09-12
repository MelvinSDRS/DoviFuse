#!/usr/bin/env python3
"""Regression checks for the hybrid donor media contract.

The invalid donors are prepared on Linux, where the CI job has the media
toolchain needed to build them.  The resulting Matroska files are portable
test inputs for the bundled macOS tools; this test never asks the Mac to
encode video.

Prepare once with::

    python3 scripts/test_donor_eligibility.py --prepare DIR \
        --base-fixtures "$DV8_AUDIT_FIXTURES"

Run with ``DV8_DONOR_ELIGIBILITY_FIXTURES=DIR``.  The regular audit seed
directory is still required as ``DV8_AUDIT_FIXTURES`` for the positive P8.1
and P7 donors and the HDR10 target.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
from typing import Iterable


ROOT = Path(__file__).resolve().parents[1]
REQUIRED = (
    "donor-p84-hlg.mkv",
    "donor-p81-unknown.mkv",
    "target-p81.mkv",
    "donor-p7.mkv",
    "target-p7.mkv",
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def executable(name: str, resources: Path) -> Path:
    override = os.environ.get(f"DV8_DONOR_{name.upper()}")
    if override:
        return Path(override)
    # Linux CI installs MKVToolNix/MediaInfo system-wide.  The repository's
    # Linux copies are retained for the macOS bundle, but may depend on the
    # private shared-library set used by that bundle and are not suitable for
    # preparing fixtures on a developer host.
    found = shutil.which(name)
    if found:
        return Path(found)
    bundled = resources / "tools" / name
    if bundled.exists():
        return bundled
    raise SystemExit(f"Required media tool not found: {name}")


def command(
    args: Iterable[object],
    name: str,
    work: Path,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        [str(arg) for arg in args],
        env=env,
        capture_output=True,
        text=True,
    )
    (work / f"{name}.log").write_text(result.stdout + "\n" + result.stderr)
    if result.returncode:
        raise RuntimeError(
            f"{name} failed with exit {result.returncode}; see {work / f'{name}.log'}"
        )
    return result


def video_frame_count(path: Path, mediainfo: Path, work: Path) -> int:
    output = command(
        [mediainfo, "--Output=Video;%FrameCount%\\n", path],
        "frame-count",
        work,
    ).stdout.strip()
    try:
        count = int(output)
    except ValueError as exc:
        raise RuntimeError(f"Could not read frame count for {path}: {output!r}") from exc
    if count <= 0:
        raise RuntimeError(f"Invalid frame count for {path}: {count}")
    return count


def prepare(destination: Path, base: Path, resources: Path) -> None:
    """Create portable donor eligibility fixtures from the regular audit seeds."""

    destination = destination.resolve()
    base = base.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    for name in ("dv.mkv", "p7.mkv", "hdr.mkv"):
        if not (base / name).is_file():
            raise SystemExit(
                f"Missing {base / name}; prepare regular audit fixtures first"
            )

    mkvextract = executable("mkvextract", resources)
    mkvmerge = executable("mkvmerge", resources)
    dovi = executable("dovi_tool", resources)
    ffmpeg = executable("ffmpeg", resources)
    work = Path(tempfile.mkdtemp(prefix="dv8-donor-fixture-build-"))
    try:
        frames = video_frame_count(base / "hdr.mkv", executable("mediainfo", resources), work)

        # Profile 8.4 uses the same broad Dolby Vision profile number (8) in
        # dovi_tool's summary as Profile 8.1. The HLG container signalling is
        # therefore an intentional half of this regression fixture.
        config = work / "profile84.json"
        config.write_text(
            json.dumps(
                {
                    "cm_version": "V40",
                    "profile": "8.4",
                    "length": frames,
                    "level6": {
                        "max_display_mastering_luminance": 1000,
                        "min_display_mastering_luminance": 1,
                        "max_content_light_level": 1000,
                        "max_frame_average_light_level": 400,
                    },
                }
            )
        )
        base_hevc = work / "hdr.hevc"
        command([mkvextract, base / "hdr.mkv", "tracks", f"0:{base_hevc}"], "extract-hdr", work)
        # Signal HLG in the HEVC VUI as well as the container. These synthetic
        # code values test eligibility, not a calibrated PQ-to-HLG conversion.
        hlg_hevc = work / "hlg.hevc"
        command(
            [ffmpeg, "-nostdin", "-v", "error", "-i", base_hevc,
             "-map", "0:v:0", "-c:v", "copy", "-bsf:v",
             "hevc_metadata=transfer_characteristics=18:colour_primaries=9:matrix_coefficients=9",
             "-f", "hevc", hlg_hevc],
            "signal-hlg-vui", work,
        )
        p84_rpu = work / "profile84.rpu.bin"
        command([dovi, "generate", "-j", config, "-o", p84_rpu], "generate-profile84", work)
        p84_hevc = work / "profile84.hevc"
        command(
            [dovi, "inject-rpu", "-i", hlg_hevc, "-r", p84_rpu, "-o", p84_hevc],
            "inject-profile84",
            work,
        )

        # Match the container colour fields to the HLG bitstream.
        command(
            [
                mkvmerge,
                "-o",
                destination / "donor-p84-hlg.mkv",
                "--color-transfer-characteristics",
                "0:18",
                "--color-primaries",
                "0:9",
                "--color-matrix-coefficients",
                "0:9",
                "--color-range",
                "0:1",
                p84_hevc,
            ],
            "mux-profile84-hlg",
            work,
        )

        # Clear the transfer tag on a real generated Profile 8.1 donor. The
        # resulting file keeps the DV RPU and frame layout while its colour
        # interpretation is unknown, so the donor gate must reject it.
        command(
            [
                mkvmerge,
                "-o",
                destination / "donor-p81-unknown.mkv",
                "--color-transfer-characteristics",
                "0:0",
                "--color-primaries",
                "0:0",
                "--color-matrix-coefficients",
                "0:0",
                "--color-range",
                "0:0",
                base / "dv.mkv",
            ],
            "mux-profile81-unknown",
            work,
        )

        shutil.copyfile(base / "hdr.mkv", destination / "target-p81.mkv")
        shutil.copyfile(base / "p7.mkv", destination / "donor-p7.mkv")

        # Make an HDR10-only target with exactly the P7 donor's frame timing.
        # dovi_tool's remove command preserves the HEVC VUI while removing
        # the RPU NALs, making this a genuine positive P7 eligibility pair.
        p7_hevc = work / "p7.hevc"
        p7_target_hevc = work / "p7-target.hevc"
        command([mkvextract, base / "p7.mkv", "tracks", f"0:{p7_hevc}"], "extract-p7", work)
        command(
            [dovi, "remove", "-i", p7_hevc, "-o", p7_target_hevc],
            "remove-p7-rpu",
            work,
        )
        command(
            [mkvmerge, "-o", destination / "target-p7.mkv", p7_target_hevc],
            "mux-p7-target",
            work,
        )

        manifest = {
            "kind": "donor-eligibility-v1",
            "recipe": "scripts/test_donor_eligibility.py",
            "generated_from": str(base),
            "files": {name: sha256(destination / name) for name in REQUIRED},
        }
        (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print(f"Prepared donor eligibility fixtures: {destination}")


def load_manifest(fixtures: Path) -> None:
    manifest_path = fixtures / "manifest.json"
    if not manifest_path.is_file():
        raise SystemExit(
            f"Missing {manifest_path}; run this script with --prepare on Linux first"
        )
    manifest = json.loads(manifest_path.read_text())
    if manifest.get("kind") != "donor-eligibility-v1":
        raise SystemExit(f"Unexpected donor fixture manifest kind: {manifest.get('kind')!r}")
    for name in REQUIRED:
        path = fixtures / name
        expected = manifest.get("files", {}).get(name)
        if not path.is_file() or not expected or sha256(path) != expected:
            raise SystemExit(f"Donor fixture checksum mismatch or missing file: {path}")


def invoke(
    binary: Path,
    resources: Path,
    donor: Path,
    target: Path,
    flags: list[str],
    label: str,
    work: Path,
    expected_failure: bool,
) -> None:
    output = work / f"{label}.mkv"
    report = work / f"{label}.report.json"
    env = dict(os.environ)
    env.update(
        DV8_SCRIPT_DIR=str(resources),
        DV8_PROCESSING_LOG_FILE=str(work / f"{label}.processing.log"),
    )
    process = subprocess.run(
        [
            str(binary),
            "--report",
            report,
            "--progress",
            "jsonl",
            "--hwaccel",
            "off",
            "--hybrid",
            *flags,
            "-o",
            output,
            donor,
            target,
        ],
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )
    text = process.stdout + "\n" + process.stderr
    (work / f"{label}.log").write_text(text)
    if expected_failure:
        assert process.returncode != 0, f"{label}: unexpectedly passed; see {work / f'{label}.log'}"
        assert not output.exists(), f"{label}: emitted output after donor rejection"
        assert "Extract RPU" not in text and "hybrid.editor" not in text, (
            f"{label}: donor rejection occurred after mutation/editor stage"
        )
        assert report.exists(), f"{label}: missing failure report"
        data = json.loads(report.read_text())
        assert data.get("execution") == "failed", f"{label}: report execution"
        assert data.get("validation") == "fail", f"{label}: report validation"
        events = [
            json.loads(line)
            for line in process.stdout.splitlines()
            if line.startswith("{")
        ]
        assert not any(event.get("event") == "completed" for event in events), label
    else:
        assert process.returncode == 0, f"{label}: rejected positive donor; see {work / f'{label}.log'}"
        assert not output.exists(), f"{label}: dry run emitted output"
        assert report.exists(), f"{label}: missing report"
        data = json.loads(report.read_text())
        assert data.get("execution") == "completed", f"{label}: report execution"


def run_regressions(fixtures: Path, binary: Path, resources: Path) -> None:
    load_manifest(fixtures)
    base = Path(os.environ.get("DV8_AUDIT_FIXTURES", fixtures.parent))
    for name in ("dv.mkv", "p7.mkv", "hdr.mkv"):
        if not (base / name).is_file():
            raise SystemExit(f"Missing regular audit fixture required for positives: {base / name}")

    work = Path(tempfile.mkdtemp(prefix="dv8-donor-eligibility-"))
    print(f"Donor eligibility logs: {work}", flush=True)
    invalid_cases = {
        "p84-hlg": (fixtures / "donor-p84-hlg.mkv", "compatibility|HLG|P8.4"),
        "p81-unknown": (fixtures / "donor-p81-unknown.mkv", "compatibility|transfer|PQ"),
    }
    variants = [
        ("default", []),
        ("metadata", ["--grade-check", "metadata"]),
        ("sampled", ["--grade-check", "sampled", "--grade-windows", "1"]),
        ("full", ["--grade-check", "full"]),
        ("skip", ["--skip-grade-check"]),
        ("force", ["--force"]),
        ("force-skip-delete", ["--force", "--skip-grade-check", "--delete-sources"]),
        ("dry-run", ["--dry-run"]),
        # This is the historical bypass combination: the old preflight
        # allowed a HLG donor through when both grade and sync gates were
        # overridden. The donor contract must still reject it before edit.
        ("dry-run-force-skip", ["--dry-run", "--force", "--skip-grade-check"]),
    ]
    passed: list[str] = []
    for case, (donor, reason) in invalid_cases.items():
        before = {path: sha256(path) for path in (donor, fixtures / "target-p81.mkv")}
        for variant, flags in variants:
            label = f"reject-{case}-{variant}"
            invoke(binary, resources, donor, fixtures / "target-p81.mkv", flags, label, work, True)
            log = (work / f"{label}.log").read_text()
            assert any(token.lower() in log.lower() for token in reason.split("|")), (
                f"{label}: no donor eligibility reason in log"
            )
            passed.append(label)
        assert {path: sha256(path) for path in before} == before, f"{case}: source changed"

    # These are intentionally metadata-only dry runs. They prove that the
    # hard media gate admits real P8.1 and P7 donors without doing a costly
    # decode/remux; full conversion coverage remains in audit_smoke.py.
    positives = [
        ("accept-p81", base / "dv.mkv", fixtures / "target-p81.mkv"),
        ("accept-p7", fixtures / "donor-p7.mkv", fixtures / "target-p7.mkv"),
    ]
    for label, donor, target in positives:
        before = {path: sha256(path) for path in (donor, target)}
        invoke(
            binary,
            resources,
            donor,
            target,
            ["--dry-run", "--grade-check", "metadata", "--letterbox", "off"],
            label,
            work,
            False,
        )
        assert {path: sha256(path) for path in before} == before, f"{label}: source changed"
        passed.append(label)

    summary = {"passed": len(passed), "work": str(work), "cases": passed}
    (work / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", type=Path, help="prepare portable fixtures into this directory")
    parser.add_argument(
        "--base-fixtures",
        type=Path,
        default=Path(os.environ.get("DV8_AUDIT_FIXTURES", "")) if os.environ.get("DV8_AUDIT_FIXTURES") else None,
        help="regular audit seed directory used by --prepare",
    )
    parser.add_argument(
        "--fixtures",
        type=Path,
        default=Path(os.environ.get("DV8_DONOR_ELIGIBILITY_FIXTURES", ""))
        if os.environ.get("DV8_DONOR_ELIGIBILITY_FIXTURES")
        else None,
        help="prepared donor fixture directory",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    resources = Path(os.environ.get("DV8_AUDIT_RESOURCES", ROOT))
    if args.prepare:
        if args.base_fixtures is None:
            raise SystemExit("--prepare requires --base-fixtures or DV8_AUDIT_FIXTURES")
        prepare(args.prepare, args.base_fixtures, resources)
        return
    if args.fixtures is None:
        raise SystemExit("Set DV8_DONOR_ELIGIBILITY_FIXTURES or pass --fixtures")
    binary = Path(
        os.environ.get("DV8_AUDIT_BIN", ROOT / "dv8_converter/target/debug/dv8_converter")
    )
    run_regressions(args.fixtures.resolve(), binary, resources)


if __name__ == "__main__":
    main()
