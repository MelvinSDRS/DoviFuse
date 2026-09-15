#!/usr/bin/env python3
"""Create the corresponding-source bundle for a DV8 Maker release.

The app build downloads FFmpeg and the official MKVToolNix macOS package. A
release must make the exact source used for FFmpeg available and provide the
corresponding upstream MKVToolNix source archive. This script deliberately
keeps those source artifacts separate from the tracked DV8 and pinned
dovi_tool git archives so that an archive can be inspected or rebuilt on
another host.
"""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile
from typing import Iterable
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".macos-build-cache"

FFMPEG_VERSION = "8.1.2"
FFMPEG_ARCHIVE_NAME = f"ffmpeg-{FFMPEG_VERSION}.tar.xz"
FFMPEG_ARCHIVE_SHA256 = (
    "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c"
)
FFMPEG_SOURCE_URL = f"https://ffmpeg.org/releases/{FFMPEG_ARCHIVE_NAME}"

MKVTOOLNIX_VERSION = "100.0"
MKV_SOURCE_ARCHIVE_NAME = f"mkvtoolnix-{MKVTOOLNIX_VERSION}.tar.xz"
MKV_SOURCE_ARCHIVE_SHA256 = (
    "74480d07a261beeaa8baf898248e668ecc56335e2527bbffa841ef056dc028a1"
)
MKV_SOURCE_URL = (
    f"https://mkvtoolnix.download/sources/{MKV_SOURCE_ARCHIVE_NAME}"
)
MKV_SOURCE_CHECKSUM_URL = f"{MKV_SOURCE_URL}.sha256"
MKV_MACOS_DMG_URL = (
    "https://mkvtoolnix.download/macos/releases/100.0/"
    "MKVToolNix-100.0-1-arm64.dmg"
)
MKV_MACOS_DMG_SHA256 = (
    "155ded045a35bd1079ba466c2e1011bdcb1a85b74c984ed5627841cd313850a0"
)

DOVI_REPOSITORY = "https://github.com/quietvoid/dovi_tool"
DOVI_REVISION = "b25558062e4a56973482ec70133bd7b891320e48"


class SourceBundleError(RuntimeError):
    """An input is missing, untrusted, or cannot be archived safely."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _run_git(repo: Path, *arguments: str) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), *arguments],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        detail = getattr(exc, "stderr", "") or str(exc)
        raise SourceBundleError(
            f"Git command failed in {repo}: {detail.strip()}"
        ) from exc
    return result.stdout.strip()


def _require_regular_file(path: Path, description: str) -> None:
    if path.is_symlink():
        raise SourceBundleError(
            f"{description} must be a regular file, not a symlink: {path}"
        )
    if not path.is_file():
        raise SourceBundleError(f"Missing {description}: {path}")
    if path.stat().st_size == 0:
        raise SourceBundleError(f"Empty {description}: {path}")


def _verify_sha256(path: Path, expected: str, description: str) -> None:
    actual = sha256(path)
    if actual != expected:
        raise SourceBundleError(
            f"{description} checksum mismatch for {path}: "
            f"expected {expected}, got {actual}"
        )


def _unsafe_archive_member(name: str) -> bool:
    """Reject paths that could carry private checkout state into a release."""

    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts:
        return True
    parts = set(path.parts)
    if ".git" in parts or ".macos-build-cache" in parts or ".verification" in parts:
        return True
    # Upstream C++ projects legitimately use directories named "private" for
    # implementation headers (including the pinned MKVToolNix source archive).
    if any(part in {"logs", "secrets"} for part in path.parts):
        return True
    if path.name in {
        ".env",
        ".env.local",
        "processing_log.txt",
        "qbt_trigger.log",
        "qbt_trigger.log.old",
    }:
        return True
    return path.name == ".DS_Store" or path.name.endswith(".pyc")


def _validate_tar(
    path: Path, description: str, expected_root: str | None = None
) -> None:
    try:
        with tarfile.open(path, mode="r:*") as archive:
            members = archive.getmembers()
    except (OSError, tarfile.TarError) as exc:
        raise SourceBundleError(f"Cannot read {description} {path}: {exc}") from exc

    if not members:
        raise SourceBundleError(f"Empty {description}: {path}")
    if any(_unsafe_archive_member(member.name) for member in members):
        raise SourceBundleError(f"Unsafe or private path found in {description}: {path}")
    if expected_root is not None:
        roots = {PurePosixPath(member.name).parts[0] for member in members}
        if expected_root not in roots:
            raise SourceBundleError(
                f"Unexpected top-level directory in {description} {path}; "
                f"expected {expected_root}/"
            )


def _download_mkv_source(
    path: Path,
    *,
    url: str = MKV_SOURCE_URL,
    expected_sha256: str = MKV_SOURCE_ARCHIVE_SHA256,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb",
            prefix=f".{path.name}.",
            suffix=".part",
            dir=path.parent,
            delete=False,
        ) as destination:
            temporary = Path(destination.name)
            request = Request(
                url, headers={"User-Agent": "DV8-Maker release source packager"}
            )
            with urlopen(request, timeout=60) as response:
                while chunk := response.read(1024 * 1024):
                    destination.write(chunk)
        _verify_sha256(
            temporary, expected_sha256, "downloaded MKVToolNix source archive"
        )
        os.replace(temporary, path)
        temporary = None
    except SourceBundleError:
        raise
    except Exception as exc:
        raise SourceBundleError(
            f"Could not download MKVToolNix source archive from {url}: {exc}"
        ) from exc
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _ensure_mkv_source(
    path: Path,
    *,
    allow_download: bool,
    url: str = MKV_SOURCE_URL,
    expected_sha256: str = MKV_SOURCE_ARCHIVE_SHA256,
) -> None:
    if path.exists() or path.is_symlink():
        _require_regular_file(path, "MKVToolNix source archive")
    elif not allow_download:
        raise SourceBundleError(
            "Missing MKVToolNix source archive and downloading is disabled: "
            f"{path} (expected official source: {url})"
        )
    else:
        print(f"Downloading MKVToolNix source archive: {url}")
        _download_mkv_source(path, url=url, expected_sha256=expected_sha256)
    _verify_sha256(path, expected_sha256, "MKVToolNix source archive")


def _git_archive(repo: Path, revision: str, destination: Path, prefix: str) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    try:
        with destination.open("wb") as output:
            result = subprocess.run(
                [
                    "git",
                    "-C",
                    str(repo),
                    "archive",
                    "--format=tar",
                    f"--prefix={prefix}/",
                    revision,
                ],
                check=False,
                stdout=output,
                stderr=subprocess.PIPE,
                text=False,
            )
    except OSError as exc:
        raise SourceBundleError(f"Could not create git archive for {repo}: {exc}") from exc
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip()
        raise SourceBundleError(f"Could not create git archive for {repo}: {detail}")
    _require_regular_file(destination, f"git archive for {repo}")
    _validate_tar(destination, f"git archive for {repo}")


def _manifest_line(component: str, path: Path, provenance: str) -> str:
    return f"{component}\t{sha256(path)}\t{path.name}\t{provenance}\n"


def _write_bundle_notes(
    path: Path,
    *,
    dv8_revision: str,
    dovi_revision: str,
    ffmpeg_sha256: str,
    mkv_sha256: str,
    component_lines: Iterable[str],
) -> None:
    path.write_text(
        """DV8 Maker corresponding-source bundle
=========================================

This archive accompanies an automated Apple Silicon DV8 Maker build. It
contains tracked DV8 Maker source at the release commit, the pinned dovi_tool
source checkout, the exact FFmpeg source archive used by the macOS build, and
the official MKVToolNix source archive corresponding to the upstream macOS
package version used by that build.

The DV8 Maker and dovi_tool entries are git archives: they intentionally omit
.git directories, ignored files, local .env files, build caches, logs, and
verification evidence. The release checkout commit and dovi_tool revision
are recorded below so the archives can be checked independently.

DV8 Maker git repository: https://github.com/MelvinSDRS/DV8
DV8 Maker release commit: {dv8_revision}
dovi_tool repository: {dovi_repository}
dovi_tool pinned revision: {dovi_revision}

FFmpeg source provenance
------------------------
The macOS build downloads the FFmpeg {ffmpeg_version} source archive from:
  {ffmpeg_url}
Its pinned SHA-256 is {ffmpeg_sha256}. The archive in this bundle is that
same downloaded artifact and includes FFmpeg's GPLv3 notice. The build uses
the GPL and version-3 configure options; review the bundled source notice and
the project's license materials before redistribution.

MKVToolNix source and binary provenance
---------------------------------------
The app build obtains the MKVToolNix {mkv_version} arm64 binary from the
official upstream DMG:
  {mkv_dmg_url}
The build pins that DMG to SHA-256 {mkv_dmg_sha256}. The corresponding
official upstream source archive is included here:
  {mkv_source_url}
Its pinned SHA-256 is {mkv_source_sha256}; the upstream checksum file is:
  {mkv_checksum_url}

Including the upstream source archive records source provenance. It does not
claim that the upstream DMG is reproducibly rebuilt from this archive, nor
that this bundle contains every build-time or runtime dependency used by the
upstream macOS package. Consult the MKVToolNix source and upstream build
documentation for its dependency and license details. No source archive for
the separately packaged MediaInfo CLI is asserted by this bundle.

Component SHA-256 manifest
--------------------------
component	sha256	archive	provenance
{component_lines}For the outer release archive, verify the adjacent
DV8-Maker-sources.tar.gz.sha256 file.
""".format(
            dv8_revision=dv8_revision,
            dovi_repository=DOVI_REPOSITORY,
            dovi_revision=dovi_revision,
            ffmpeg_version=FFMPEG_VERSION,
            ffmpeg_url=FFMPEG_SOURCE_URL,
            ffmpeg_sha256=ffmpeg_sha256,
            mkv_version=MKVTOOLNIX_VERSION,
            mkv_dmg_url=MKV_MACOS_DMG_URL,
            mkv_dmg_sha256=MKV_MACOS_DMG_SHA256,
            mkv_source_url=MKV_SOURCE_URL,
            mkv_source_sha256=mkv_sha256,
            mkv_checksum_url=MKV_SOURCE_CHECKSUM_URL,
            component_lines="".join(component_lines),
        ),
        encoding="utf-8",
    )


def package_sources(
    *,
    output: Path,
    root: Path = ROOT,
    cache: Path = CACHE,
    allow_download: bool = True,
    ffmpeg_archive: Path | None = None,
    mkv_source_archive: Path | None = None,
    ffmpeg_expected_sha256: str = FFMPEG_ARCHIVE_SHA256,
    mkv_expected_sha256: str = MKV_SOURCE_ARCHIVE_SHA256,
    dovi_revision: str = DOVI_REVISION,
) -> tuple[Path, Path]:
    """Build and return (archive, checksum_file)."""

    root = root.resolve()
    cache = cache.resolve()
    output = output.resolve()
    dovi_root = root / "dovi_tool"
    if not root.is_dir():
        raise SourceBundleError(f"Missing DV8 Maker repository: {root}")
    if _run_git(root, "rev-parse", "--show-toplevel") != str(root):
        raise SourceBundleError(f"Not a git checkout at repository root: {root}")
    dv8_revision = _run_git(root, "rev-parse", "HEAD")

    if not dovi_root.is_dir():
        raise SourceBundleError(
            f"Missing pinned dovi_tool checkout: {dovi_root} "
            f"(required revision {dovi_revision})"
        )
    if (
        _run_git(dovi_root, "rev-parse", "--verify", f"{dovi_revision}^{{commit}}")
        != dovi_revision
    ):
        raise SourceBundleError(
            f"dovi_tool checkout does not contain revision {dovi_revision}"
        )
    dovi_head = _run_git(dovi_root, "rev-parse", "HEAD")
    if dovi_head != dovi_revision:
        raise SourceBundleError(
            f"dovi_tool checkout is not pinned to {dovi_revision}; found {dovi_head}"
        )

    ffmpeg_path = (
        ffmpeg_archive or cache / "downloads" / FFMPEG_ARCHIVE_NAME
    ).resolve()
    _require_regular_file(ffmpeg_path, "FFmpeg source archive")
    _verify_sha256(ffmpeg_path, ffmpeg_expected_sha256, "FFmpeg source archive")
    _validate_tar(
        ffmpeg_path,
        "FFmpeg source archive",
        f"ffmpeg-{FFMPEG_VERSION}",
    )

    mkv_path = (
        mkv_source_archive or cache / "downloads" / MKV_SOURCE_ARCHIVE_NAME
    ).resolve()
    _ensure_mkv_source(
        mkv_path,
        allow_download=allow_download,
        url=MKV_SOURCE_URL,
        expected_sha256=mkv_expected_sha256,
    )
    _validate_tar(
        mkv_path,
        "MKVToolNix source archive",
        f"mkvtoolnix-{MKVTOOLNIX_VERSION}",
    )

    if output.exists() and not output.is_dir():
        raise SourceBundleError(f"Release output is not a directory: {output}")
    output.mkdir(parents=True, exist_ok=True)
    outer = output / "DV8-Maker-sources.tar.gz"
    checksum_file = output / "DV8-Maker-sources.tar.gz.sha256"

    with tempfile.TemporaryDirectory(
        prefix=".dv8-source-bundle-", dir=output
    ) as temporary:
        stage = Path(temporary) / "DV8-Maker-sources"
        stage.mkdir()
        ffmpeg_copy = stage / FFMPEG_ARCHIVE_NAME
        mkv_copy = stage / MKV_SOURCE_ARCHIVE_NAME
        shutil.copyfile(ffmpeg_path, ffmpeg_copy)
        shutil.copyfile(mkv_path, mkv_copy)

        dovi_copy = stage / f"dovi_tool-{dovi_revision}.tar"
        _git_archive(
            dovi_root,
            dovi_revision,
            dovi_copy,
            f"dovi_tool-{dovi_revision}",
        )
        dv8_copy = stage / f"DV8-Maker-{dv8_revision}.tar"
        _git_archive(root, "HEAD", dv8_copy, f"DV8-Maker-{dv8_revision}")

        lines = [
            _manifest_line(
                "DV8 Maker",
                dv8_copy,
                f"git archive of https://github.com/MelvinSDRS/DV8 at {dv8_revision}",
            ),
            _manifest_line(
                "dovi_tool",
                dovi_copy,
                f"git archive of {DOVI_REPOSITORY} at {dovi_revision}",
            ),
            _manifest_line("FFmpeg", ffmpeg_copy, FFMPEG_SOURCE_URL),
            _manifest_line("MKVToolNix source", mkv_copy, MKV_SOURCE_URL),
        ]
        _write_bundle_notes(
            stage / "SOURCE-BUNDLE-README.txt",
            dv8_revision=dv8_revision,
            dovi_revision=dovi_revision,
            ffmpeg_sha256=ffmpeg_expected_sha256,
            mkv_sha256=mkv_expected_sha256,
            component_lines=lines,
        )

        temporary_outer = Path(temporary) / outer.name
        with tarfile.open(
            temporary_outer, mode="w:gz", compresslevel=9
        ) as archive:
            archive.add(stage, arcname=stage.name, recursive=True)
        _validate_tar(
            temporary_outer,
            "DV8 Maker source bundle",
            "DV8-Maker-sources",
        )

        digest = sha256(temporary_outer)
        temporary_checksum = Path(temporary) / checksum_file.name
        temporary_checksum.write_text(
            f"{digest}  {outer.name}\n", encoding="utf-8"
        )
        os.replace(temporary_outer, outer)
        os.replace(temporary_checksum, checksum_file)

    print(f"Built: {outer}")
    print(f"SHA-256: {sha256(outer)}  {outer.name}")
    print(f"Checksum: {checksum_file}")
    return outer, checksum_file


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="directory receiving DV8-Maker-sources.tar.gz and its .sha256 file",
    )
    parser.add_argument(
        "--cache",
        type=Path,
        default=CACHE,
        help=f"macOS build cache (default: {CACHE})",
    )
    parser.add_argument(
        "--ffmpeg-archive",
        type=Path,
        help="override the FFmpeg source archive path (for testing or a relocated cache)",
    )
    parser.add_argument(
        "--mkv-source-archive",
        type=Path,
        help="override the MKVToolNix source archive path (for testing or a relocated cache)",
    )
    parser.add_argument(
        "--no-download",
        action="store_true",
        help="fail when the official MKVToolNix source archive is absent",
    )
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        package_sources(
            output=args.output,
            cache=args.cache,
            allow_download=not args.no_download,
            ffmpeg_archive=args.ffmpeg_archive,
            mkv_source_archive=args.mkv_source_archive,
        )
    except SourceBundleError as exc:
        print(f"Source bundle failed: {exc}", file=os.sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
