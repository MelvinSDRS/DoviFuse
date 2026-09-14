#!/usr/bin/env python3
"""Real-tool metadata transport controls on the disposable DV8 seed media.

This test deliberately does not encode video.  It uses the checked-in audit
seed, dovi_tool, mkvextract, and mkvmerge to exercise the RPU transport
boundary that is hard to cover with unit fixtures:

* a donor whose RPU L6 differs from the HDR target while the stream tags are
  identical must receive the target L6 on every frame;
* L1, L2 trim, L5, L9, and the other RPU blocks must survive the edit and
  remux unchanged;
* a late L1, L2, or L6 mutation injected either before injection or while
  extracting the output RPU must be refused; and
* same-input sync repair must retain the donor's original L6.

All generated media and logs are kept in a temporary directory under /tmp.
The optional ``--baseline`` mode is for an older binary: it expects the
known L6 transport bug to reproduce rather than expecting the fixed result.
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
from typing import Any, Iterable, Mapping


ROOT = Path(__file__).resolve().parents[1]
RESOURCES = Path(os.environ.get("DV8_AUDIT_RESOURCES", ROOT))
BIN = Path(os.environ.get("DV8_AUDIT_BIN", ROOT / "dv8_converter/target/debug/dv8_converter"))
SEEDS = Path(os.environ["DV8_AUDIT_FIXTURES"])
WORK = Path(tempfile.mkdtemp(prefix="dv8-metadata-transport-", dir="/tmp"))
ENV = dict(
    os.environ,
    DV8_SCRIPT_DIR=str(RESOURCES),
    DV8_PROCESSING_LOG_FILE=str(WORK / "processing.log"),
)
ENV["PATH"] = ENV.get("PATH", "") + os.pathsep + str(RESOURCES / "tools")


def executable(name: str) -> Path:
    override = os.environ.get(f"DV8_METADATA_{name.upper()}")
    if override:
        return Path(override)
    found = shutil.which(name, path=ENV["PATH"])
    if found:
        return Path(found)
    bundled = RESOURCES / "tools" / name
    if bundled.exists():
        return bundled
    raise RuntimeError(f"Required media tool not found: {name}")


DOVI = executable("dovi_tool")
MKVEXTRACT = executable("mkvextract")
MKVMERGE = executable("mkvmerge")
MEDIAINFO = executable("mediainfo")
RESULTS: list[str] = []


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(
    args: Iterable[object],
    name: str,
    *,
    success: bool = True,
    env: Mapping[str, str] = ENV,
    timeout: int = 300,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        [str(argument) for argument in args],
        env=dict(env),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    (WORK / f"{name}.log").write_text(result.stdout + "\n" + result.stderr)
    assert (result.returncode == 0) == success, (
        f"{name}: unexpected exit {result.returncode}; see {WORK / f'{name}.log'}"
    )
    return result


def write_json(name: str, value: Any) -> Path:
    path = WORK / name
    path.write_text(json.dumps(value, indent=2) + "\n")
    return path


def export_rpu(rpu: Path, name: str) -> list[dict[str, Any]]:
    destination = WORK / f"{name}.json"
    run([DOVI, "export", "-i", rpu, "-o", destination], f"export-{name}")
    value = json.loads(destination.read_text())
    assert isinstance(value, list) and value, f"{name}: empty RPU export"
    return value


def semantic(value: Any) -> Any:
    """Drop the per-NAL CRC while retaining all semantic RPU fields."""

    if isinstance(value, dict):
        return {
            key: semantic(item)
            for key, item in value.items()
            if key != "rpu_data_crc32"
        }
    if isinstance(value, list):
        return [semantic(item) for item in value]
    return value


def metadata_blocks(frame: Mapping[str, Any], level: int) -> list[dict[str, Any]]:
    dm = frame.get("vdr_dm_data") or {}
    result: list[dict[str, Any]] = []
    for name in ("cmv29_metadata", "cmv40_metadata"):
        for block in (dm.get(name) or {}).get("ext_metadata_blocks", []):
            if f"Level{level}" in block:
                result.append(block)
    return result


def level(frame: Mapping[str, Any], number: int) -> Any:
    blocks = metadata_blocks(frame, number)
    return blocks[0][f"Level{number}"] if blocks else None


def l6(frame: Mapping[str, Any]) -> tuple[int, int, int, int] | None:
    value = level(frame, 6)
    if value is None:
        return None
    return tuple(
        int(value[key])
        for key in (
            "max_display_mastering_luminance",
            "min_display_mastering_luminance",
            "max_content_light_level",
            "max_frame_average_light_level",
        )
    )


def l5_sequence(frames: list[dict[str, Any]]) -> list[Any]:
    return [level(frame, 5) for frame in frames]


def scene_frames(frames: list[dict[str, Any]]) -> list[int]:
    return [
        index
        for index, frame in enumerate(frames)
        if (frame.get("vdr_dm_data") or {}).get("scene_refresh_flag") == 1
    ]


def video_track(path: Path, name: str) -> dict[str, Any]:
    result = run([MEDIAINFO, "--Output=JSON", path], f"mediainfo-{name}")
    tracks = json.loads(result.stdout)["media"]["track"]
    return next(track for track in tracks if track.get("@type") == "Video")


def assert_identical_stream_tags(donor: Path, target: Path) -> None:
    donor_track = video_track(donor, "rich-donor")
    target_track = video_track(target, "hdr-target")
    fields = (
        "colour_range",
        "colour_primaries",
        "transfer_characteristics",
        "matrix_coefficients",
        "MasteringDisplay_Luminance_Min",
        "MasteringDisplay_Luminance_Max",
        "MaxCLL",
        "MaxFALL",
    )
    for field in fields:
        assert donor_track.get(field) == target_track.get(field), (
            f"stream tag mismatch {field}: {donor_track.get(field)!r} != "
            f"{target_track.get(field)!r}"
        )


def make_generator_config(
    seed_config: Mapping[str, Any],
    *,
    donor_l6: tuple[int, int, int, int],
    l1_delta: int = 0,
    trim_slope: int = 1111,
) -> dict[str, Any]:
    config = json.loads(json.dumps(seed_config))
    config["level6"] = {
        "max_display_mastering_luminance": donor_l6[0],
        "min_display_mastering_luminance": donor_l6[1],
        "max_content_light_level": donor_l6[2],
        "max_frame_average_light_level": donor_l6[3],
    }
    config["default_metadata_blocks"] = [
        {
            "Level2": {
                "target_max_pq": 3000,
                "trim_slope": trim_slope,
                "trim_offset": 1222,
                "trim_power": 1333,
                "trim_chroma_weight": 1444,
                "trim_saturation_gain": 1555,
                "ms_weight": 1666,
            }
        },
        {"Level9": {"length": 1, "source_primary_index": 0}},
        {
            "Level11": {
                "content_type": 1,
                "whitepoint": 0,
                "reference_mode_flag": True,
            }
        },
    ]
    for index, shot in enumerate(config["shots"]):
        shot["metadata_blocks"] = [
            {
                "Level1": {
                    "min_pq": 7,
                    "max_pq": 2700 + index + l1_delta,
                    "avg_pq": 900 + index,
                }
            }
        ]
    return config


def make_one_frame_variant(
    source: Path,
    name: str,
    *,
    level6: tuple[int, int, int, int] | None = None,
    source_rpu: Path | None = None,
    levels: list[int] | None = None,
) -> Path:
    """Return only the final frame after applying a real dovi_tool edit."""

    frame_count = len(export_rpu(source, f"count-{name}"))
    remove_prefix = write_json(
        f"{name}-remove-prefix.json",
        {"mode": 0, "remove": [f"0-{frame_count - 2}"]},
    )
    edited = source
    if level6 is not None:
        level6_config = write_json(
            f"{name}-level6.json",
            {
                "mode": 0,
                "level6": {
                    "max_display_mastering_luminance": level6[0],
                    "min_display_mastering_luminance": level6[1],
                    "max_content_light_level": level6[2],
                    "max_frame_average_light_level": level6[3],
                },
            },
        )
        edited = WORK / f"{name}-level6-all.bin"
        run([DOVI, "editor", "-i", source, "-j", level6_config, "-o", edited], f"edit-{name}-level6")
    if source_rpu is not None:
        replace_config = write_json(
            f"{name}-replace.json",
            {
                "mode": 0,
                "source_rpu": str(source_rpu),
                "rpu_levels": levels or [1],
            },
        )
        replaced = WORK / f"{name}-replace-all.bin"
        run([DOVI, "editor", "-i", edited, "-j", replace_config, "-o", replaced], f"edit-{name}-replace")
        edited = replaced
    final = WORK / f"{name}-final.bin"
    run([DOVI, "editor", "-i", edited, "-j", remove_prefix, "-o", final], f"edit-{name}-final")
    assert len(export_rpu(final, f"verify-{name}-final")) == 1
    return final


def late_mutation(expected: Path, name: str, *, level6: tuple[int, int, int, int] | None = None, source_rpu: Path | None = None, levels: list[int] | None = None) -> Path:
    """Build expected-RPU prefix plus one changed final RPU frame."""

    expected_frames = export_rpu(expected, f"expected-{name}")
    count = len(expected_frames)
    remove_last = write_json(
        f"{name}-remove-last.json",
        {"mode": 0, "remove": [f"{count - 1}-{count - 1}"]},
    )
    prefix = WORK / f"{name}-prefix.bin"
    run([DOVI, "editor", "-i", expected, "-j", remove_last, "-o", prefix], f"edit-{name}-prefix")
    changed_last = make_one_frame_variant(
        expected,
        name,
        level6=level6,
        source_rpu=source_rpu,
        levels=levels,
    )
    mutated = WORK / f"late-{name}.bin"
    mutated.write_bytes(prefix.read_bytes() + changed_last.read_bytes())
    mutated_frames = export_rpu(mutated, f"verify-late-{name}")
    assert len(mutated_frames) == count
    assert scene_frames(mutated_frames) == scene_frames(expected_frames)
    assert l5_sequence(mutated_frames) == l5_sequence(expected_frames)
    assert semantic(mutated_frames[:-1]) == semantic(expected_frames[:-1])
    assert semantic(mutated_frames[-1]) != semantic(expected_frames[-1])
    return mutated


def make_fault_resources(name: str, mutation: Path, *, operation: str) -> tuple[Path, dict[str, str]]:
    fault_dir = WORK / f"fault-{name}-{operation}"
    fault_dir.mkdir()
    wrapper = fault_dir / "dovi_tool"
    wrapper.write_text(
        "#!" + sys.executable + "\n"
        "import os, shutil, subprocess, sys\n"
        "from pathlib import Path\n"
        f"REAL = {str(DOVI)!r}\n"
        f"MUTATION = {str(mutation)!r}\n"
        f"OPERATION = {operation!r}\n"
        "args = sys.argv[1:]\n"
        "if OPERATION == 'inject' and 'inject-rpu' in args:\n"
        "    args[args.index('-r') + 1] = MUTATION\n"
        "    os.execv(REAL, [REAL] + args)\n"
        "if OPERATION == 'extract' and 'extract-rpu' in args:\n"
        "    subprocess.run([REAL] + args, check=True)\n"
        "    output = Path(args[args.index('-o') + 1])\n"
        "    if '.hybrid.verify.rpu.bin' in output.name:\n"
        "        shutil.copyfile(MUTATION, output)\n"
        "    sys.exit(0)\n"
        "os.execv(REAL, [REAL] + args)\n"
    )
    wrapper.chmod(0o755)
    resources = WORK / f"resources-{name}-{operation}"
    (resources / "tools").mkdir(parents=True)
    (resources / "tools" / "dovi_tool").symlink_to(wrapper)
    (resources / "config").symlink_to(RESOURCES / "config", target_is_directory=True)
    env = dict(ENV, DV8_SCRIPT_DIR=str(resources), PATH=str(fault_dir) + os.pathsep + ENV["PATH"])
    return resources, env


def convert(
    donor: Path,
    target: Path,
    name: str,
    *,
    binary: Path = BIN,
    env: Mapping[str, str] = ENV,
    success: bool = True,
) -> tuple[subprocess.CompletedProcess[str], Path, dict[str, Any]]:
    output = WORK / f"{name}.mkv"
    report = WORK / f"{name}.report.json"
    result = run(
        [
            binary,
            "--hybrid",
            "--progress",
            "jsonl",
            "--report",
            report,
            "--hwaccel",
            "off",
            "--sync",
            "framecount",
            "--skip-grade-check",
            "--letterbox",
            "off",
            "-o",
            output,
            donor,
            target,
        ],
        name,
        success=success,
        env=env,
        timeout=900,
    )
    report_data = json.loads(report.read_text())
    assert report_data["execution"] == ("completed" if success else "failed"), name
    if not success:
        assert report_data["validation"] == "fail", name
        assert '"event":"completed"' not in result.stdout, name
        assert not output.exists(), name
    RESULTS.append(name)
    return result, output, report_data


def assert_transport_failure(
    donor: Path,
    target: Path,
    name: str,
    mutation: Path,
    *,
    operation: str,
    source_hashes: Mapping[Path, str],
) -> None:
    _, _, report = convert(
        donor,
        target,
        name,
        env=make_fault_resources(name, mutation, operation=operation)[1],
        success=False,
    )
    log = (WORK / f"{name}.log").read_text()
    assert "Output metadata transport differs from the edited RPU at frame" in log, name
    checks = report.get("checks", [])
    check = next((item for item in checks if item.get("key") == "output_metadata_transport"), None)
    assert check and check.get("status") == "fail", (name, checks)
    assert "metadata transport" in log.lower(), name
    assert {path: sha256(path) for path in source_hashes} == dict(source_hashes), name
    assert (WORK / f"{name}.FAILED.mkv").exists(), name


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--baseline",
        action="store_true",
        help="Run only the pre-fix L6 regression against DV8_METADATA_BASELINE_BIN or DV8_AUDIT_BIN",
    )
    args = parser.parse_args()

    print(f"Metadata transport controls: {WORK}", flush=True)
    manifest = json.loads((SEEDS / "manifest.json").read_text())
    for name in ("dv.mkv", "hdr.mkv", "shifted.mkv"):
        assert (SEEDS / name).exists(), name
    for name in ("dv.mkv", "hdr.mkv", "shifted.mkv"):
        if name in manifest["sha256"]:
            assert sha256(SEEDS / name) == manifest["sha256"][name], name
        shutil.copyfile(SEEDS / name, WORK / name)
    frames = manifest["frames"]
    assert frames > 1

    run([MKVEXTRACT, WORK / "dv.mkv", "tracks", f"0:{WORK / 'dv.hevc'}"], "extract-dv")
    run([DOVI, "extract-rpu", WORK / "dv.mkv", "-o", WORK / "seed-rpu.bin"], "extract-seed-rpu")
    seed_frames = export_rpu(WORK / "seed-rpu.bin", "seed-rpu")
    starts = scene_frames(seed_frames)
    assert starts and starts[0] == 0
    seed_config = {
        "cm_version": "V40",
        "profile": "8.1",
        "shots": [
            {
                "start": start,
                "duration": (starts[index + 1] if index + 1 < len(starts) else frames) - start,
                "metadata_blocks": [],
            }
            for index, start in enumerate(starts)
        ],
    }
    donor_l6 = (4000, 0, 4000, 1600)
    target_l6 = (1000, 1, 1000, 400)
    rich_config = make_generator_config(seed_config, donor_l6=donor_l6)
    rich_config_path = write_json("rich-donor.json", rich_config)
    rich_rpu = WORK / "rich-donor.bin"
    run([DOVI, "generate", "-j", rich_config_path, "-o", rich_rpu], "generate-rich-donor")
    rich_hevc = WORK / "rich-donor.hevc"
    run([DOVI, "inject-rpu", "-i", WORK / "dv.hevc", "-r", rich_rpu, "-o", rich_hevc], "inject-rich-donor")
    rich_donor = WORK / "rich-donor.mkv"
    run([MKVMERGE, "-o", rich_donor, rich_hevc], "mux-rich-donor")
    assert_identical_stream_tags(rich_donor, WORK / "hdr.mkv")

    rich_frames = export_rpu(rich_rpu, "rich-donor")
    assert len(rich_frames) == frames
    assert all(l6(frame) == donor_l6 for frame in rich_frames)
    assert all(level(frame, 1) is not None for frame in rich_frames)
    assert all(level(frame, 2) is not None for frame in rich_frames)
    assert all(level(frame, 9) is not None for frame in rich_frames)
    assert all(level(frame, 5) is not None for frame in rich_frames)
    assert l6(rich_frames[0]) != target_l6

    target_config = write_json(
        "target-level6.json",
        {
            "mode": 0,
            "level6": {
                "max_display_mastering_luminance": target_l6[0],
                "min_display_mastering_luminance": target_l6[1],
                "max_content_light_level": target_l6[2],
                "max_frame_average_light_level": target_l6[3],
            },
        },
    )
    expected_rpu = WORK / "expected-edited.bin"
    run([DOVI, "editor", "-i", rich_rpu, "-j", target_config, "-o", expected_rpu], "build-expected-edited-rpu")
    expected_frames = export_rpu(expected_rpu, "expected-edited")
    assert len(expected_frames) == frames
    assert all(l6(frame) == target_l6 for frame in expected_frames)
    assert l5_sequence(expected_frames) == l5_sequence(rich_frames)
    assert scene_frames(expected_frames) == scene_frames(rich_frames)
    for number in (1, 2, 5, 9, 11, 254):
        assert [level(frame, number) for frame in expected_frames] == [level(frame, number) for frame in rich_frames], number

    if args.baseline:
        # This deliberately describes the known old behavior and therefore
        # must be run with the pre-transport-fix executable.
        baseline_bin = Path(os.environ.get("DV8_METADATA_BASELINE_BIN", str(BIN)))
        _, baseline_output, baseline_report = convert(
            rich_donor,
            WORK / "hdr.mkv",
            "baseline-l6-mismatch",
            binary=baseline_bin,
        )
        run([DOVI, "extract-rpu", baseline_output, "-o", WORK / "baseline.bin"], "extract-baseline")
        baseline_frames = export_rpu(WORK / "baseline.bin", "baseline-output")
        assert all(l6(frame) == donor_l6 for frame in baseline_frames), baseline_report
        RESULTS.append("baseline-reproduces-donor-l6-transport")
        summary = {"passed": len(RESULTS), "cases": RESULTS, "work": str(WORK), "synthetic_only": True}
        (WORK / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
        return

    source_hashes = {path: sha256(path) for path in (rich_donor, WORK / "hdr.mkv")}
    _, output, report = convert(rich_donor, WORK / "hdr.mkv", "l6-reconciled")
    run([DOVI, "extract-rpu", output, "-o", WORK / "l6-reconciled.bin"], "extract-l6-reconciled")
    output_frames = export_rpu(WORK / "l6-reconciled.bin", "l6-reconciled-output")
    assert len(output_frames) == frames
    assert semantic(output_frames) == semantic(expected_frames)
    assert all(l6(frame) == target_l6 for frame in output_frames)
    assert source_hashes == {path: sha256(path) for path in source_hashes}
    measurements = report.get("measurements", [])
    l6_measurement = next((item for item in measurements if item.get("key") == "l6_reconciliation"), None)
    assert l6_measurement and l6_measurement.get("value", {}).get("policy") == "apply_complete_target", report
    RESULTS.append("l6-reconciled-all-frames")

    # Alternate RPU values are used only to construct late fault frames. The
    # base stream and all unrelated metadata remain identical.
    alt_config = make_generator_config(seed_config, donor_l6=donor_l6, l1_delta=71, trim_slope=1777)
    alt_path = write_json("alternate-metadata.json", alt_config)
    alt_rpu = WORK / "alternate-metadata.bin"
    run([DOVI, "generate", "-j", alt_path, "-o", alt_rpu], "generate-alternate-metadata")
    late_l1 = late_mutation(expected_rpu, "l1", source_rpu=alt_rpu, levels=[1])
    late_trim = late_mutation(expected_rpu, "trim", source_rpu=alt_rpu, levels=[2])
    late_l6 = late_mutation(expected_rpu, "l6", level6=(3000, 0, 3000, 1200))
    for label, mutation, changed_level in (
        ("l1", late_l1, 1),
        ("trim", late_trim, 2),
        ("l6", late_l6, 6),
    ):
        mutation_frames = export_rpu(mutation, f"assert-late-{label}")
        for number in (1, 2, 5, 6, 9, 11, 254):
            expected_value = level(expected_frames[-1], number)
            actual_value = level(mutation_frames[-1], number)
            if number == changed_level:
                assert actual_value != expected_value, (label, number)
            else:
                assert actual_value == expected_value, (label, number)
    for label, mutation in (("l1", late_l1), ("trim", late_trim), ("l6", late_l6)):
        for operation in ("inject", "extract"):
            assert_transport_failure(
                rich_donor,
                WORK / "hdr.mkv",
                f"reject-late-{label}-{operation}",
                mutation,
                operation=operation,
                source_hashes=source_hashes,
            )

    # Same-input repair has no target metadata to reconcile. Its original
    # donor L6 must survive the alignment edit on every output frame. The
    # shifted seed supplies the deliberate +5 RPU scene offset required by
    # the checker; only its metadata levels are replaced with the rich donor
    # levels, so the sync repair still has to preserve L6 and L9.
    shifted_hevc = WORK / "shifted.hevc"
    run([MKVEXTRACT, WORK / "shifted.mkv", "tracks", f"0:{shifted_hevc}"], "extract-shifted-base")
    shifted_seed_rpu = WORK / "shifted-seed.bin"
    run([DOVI, "extract-rpu", WORK / "shifted.mkv", "-o", shifted_seed_rpu], "extract-shifted-rpu")
    shifted_config = write_json(
        "shifted-rich-replace.json",
        {
            "mode": 0,
            "source_rpu": str(rich_rpu),
            "rpu_levels": [1, 2, 5, 6, 9, 11, 254],
        },
    )
    shifted_rpu = WORK / "rich-shifted.bin"
    run([DOVI, "editor", "-i", shifted_seed_rpu, "-j", shifted_config, "-o", shifted_rpu], "build-rich-shifted-rpu")
    shifted_rpu_frames = export_rpu(shifted_rpu, "rich-shifted")
    assert len(shifted_rpu_frames) == frames
    assert all(l6(frame) == donor_l6 for frame in shifted_rpu_frames)
    assert scene_frames(shifted_rpu_frames)[:3] == [0, 5, 36]
    shifted_hevc_with_rpu = WORK / "rich-shifted.hevc"
    run([DOVI, "inject-rpu", "-i", shifted_hevc, "-r", shifted_rpu, "-o", shifted_hevc_with_rpu], "inject-rich-shifted")
    shifted_donor = WORK / "rich-shifted.mkv"
    run([MKVMERGE, "-o", shifted_donor, shifted_hevc_with_rpu], "mux-rich-shifted")
    repair_source_before = sha256(shifted_donor)
    run([BIN, "--repair-sync", "5", "--allow-padding", "--hwaccel", "off", shifted_donor], "repair-original-l6", timeout=900)
    repaired = WORK / "rich-shifted.DV8.Fixed.mkv"
    assert repaired.exists(), repaired
    run([DOVI, "extract-rpu", repaired, "-o", WORK / "repaired.bin"], "extract-repaired")
    repaired_frames = export_rpu(WORK / "repaired.bin", "repaired-output")
    assert len(repaired_frames) == frames
    assert all(l6(frame) == donor_l6 for frame in repaired_frames)
    assert all(level(frame, 9) == level(shifted_rpu_frames[5], 9) for frame in repaired_frames)
    assert sha256(shifted_donor) == repair_source_before
    RESULTS.append("sync-repair-preserves-original-l6")

    summary = {"passed": len(RESULTS), "cases": RESULTS, "work": str(WORK), "synthetic_only": True}
    (WORK / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
