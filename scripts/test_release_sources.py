#!/usr/bin/env python3
"""Bounded mechanics checks for package_release_sources.py."""

from __future__ import annotations

import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import package_release_sources as package  # noqa: E402


def run_git(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def init_repo(path: Path, filename: str, content: bytes) -> str:
    path.mkdir(parents=True)
    run_git(path, "init", "-q")
    run_git(path, "config", "user.email", "release-test@example.invalid")
    run_git(path, "config", "user.name", "DV8 release test")
    (path / filename).write_bytes(content)
    run_git(path, "add", filename)
    run_git(path, "commit", "-qm", "fixture")
    return run_git(path, "rev-parse", "HEAD")


def source_tar(path: Path, top_level: str, filename: str) -> str:
    with tarfile.open(path, mode="w:xz") as archive:
        info = tarfile.TarInfo(f"{top_level}/{filename}")
        payload = b"source fixture\n"
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    return package.sha256(path)


class ReleaseSourceMechanicsTests(unittest.TestCase):
    def test_bundle_excludes_untracked_private_checkout_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dv8-release-source-test-") as raw:
            work = Path(raw)
            repo = work / "dv8"
            dovi = repo / "dovi_tool"
            init_repo(repo, "tracked.txt", b"tracked source\n")
            dovi_revision = init_repo(dovi, "LICENSE", b"MIT fixture\n")

            (repo / ".env").write_text("PRIVATE=must not ship\n", encoding="utf-8")
            (repo / ".macos-build-cache").mkdir()
            (repo / ".macos-build-cache" / "private-cache").write_text(
                "must not ship\n", encoding="utf-8"
            )
            (repo / "logs").mkdir()
            (repo / "logs" / "private.log").write_text(
                "must not ship\n", encoding="utf-8"
            )

            ffmpeg = work / "ffmpeg-8.1.2.tar.xz"
            mkv = work / "mkvtoolnix-100.0.tar.xz"
            ffmpeg_hash = source_tar(ffmpeg, "ffmpeg-8.1.2", "COPYING.GPLv3")
            mkv_hash = source_tar(mkv, "mkvtoolnix-100.0", "COPYING")
            output = work / "dist"
            archive_path, checksum_path = package.package_sources(
                output=output,
                root=repo,
                cache=work / "cache",
                allow_download=False,
                ffmpeg_archive=ffmpeg,
                mkv_source_archive=mkv,
                ffmpeg_expected_sha256=ffmpeg_hash,
                mkv_expected_sha256=mkv_hash,
                dovi_revision=dovi_revision,
            )

            self.assertTrue(checksum_path.is_file())
            self.assertEqual(
                checksum_path.read_text(encoding="utf-8"),
                f"{package.sha256(archive_path)}  {archive_path.name}\n",
            )
            with tarfile.open(archive_path, mode="r:gz") as outer:
                names = outer.getnames()
                self.assertIn("DV8-Maker-sources/SOURCE-BUNDLE-README.txt", names)
                self.assertNotIn("DV8-Maker-sources/.env", names)
                dv8_name = next(
                    name
                    for name in names
                    if name.startswith("DV8-Maker-sources/DV8-Maker-")
                    and name.endswith(".tar")
                )
                payload = outer.extractfile(dv8_name).read()
            with tarfile.open(fileobj=io.BytesIO(payload), mode="r:") as dv8_archive:
                archived_names = dv8_archive.getnames()
                self.assertTrue(any(name.endswith("/tracked.txt") for name in archived_names))
                self.assertFalse(any(".env" in name for name in archived_names))
                self.assertFalse(any(".macos-build-cache" in name for name in archived_names))
                self.assertFalse(any("logs/" in name for name in archived_names))

    def test_missing_ffmpeg_fails_before_optional_download(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dv8-release-source-test-") as raw:
            work = Path(raw)
            repo = work / "dv8"
            dovi_revision = init_repo(repo / "dovi_tool", "LICENSE", b"MIT fixture\n")
            run_git(repo, "init", "-q")
            run_git(repo, "config", "user.email", "release-test@example.invalid")
            run_git(repo, "config", "user.name", "DV8 release test")
            (repo / "tracked.txt").write_text("tracked\n", encoding="utf-8")
            run_git(repo, "add", "tracked.txt")
            run_git(repo, "commit", "-qm", "fixture")

            with self.assertRaisesRegex(package.SourceBundleError, "Missing FFmpeg source archive"):
                package.package_sources(
                    output=work / "dist",
                    root=repo,
                    cache=work / "cache",
                    allow_download=False,
                    dovi_revision=dovi_revision,
                )

    def test_private_tar_member_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dv8-release-source-test-") as raw:
            archive_path = Path(raw) / "unsafe.tar"
            with tarfile.open(archive_path, mode="w") as archive:
                info = tarfile.TarInfo("root/.env")
                info.size = 7
                archive.addfile(info, io.BytesIO(b"secret\n"))
            with self.assertRaisesRegex(package.SourceBundleError, "Unsafe or private"):
                package._validate_tar(archive_path, "fixture")

    def test_upstream_private_implementation_headers_are_allowed(self) -> None:
        with tempfile.TemporaryDirectory(prefix="dv8-release-source-test-") as raw:
            archive_path = Path(raw) / "upstream.tar.xz"
            source_tar(archive_path, "mkvtoolnix-100.0", "src/common/private/dts_parser.h")
            package._validate_tar(archive_path, "upstream", "mkvtoolnix-100.0")


if __name__ == "__main__":
    unittest.main()
