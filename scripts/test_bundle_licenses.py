#!/usr/bin/env python3
"""Integration checks for the packaged Rust license notice collector.

The collector is exercised as a subprocess with a temporary ``cargo`` shim.
The shim returns metadata for both packaged executables without invoking a
real Cargo installation or accessing the network.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
COLLECTOR = ROOT / "macapp" / "bundle-rust-licenses.py"


MOCK_CARGO = f"""#!{sys.executable}
import json
import os
from pathlib import Path
import sys

metadata = json.loads(Path(os.environ["DOVIFUSE_TEST_CARGO_METADATA"]).read_text())
manifest = Path(sys.argv[sys.argv.index("--manifest-path") + 1])
calls = Path(os.environ["DOVIFUSE_TEST_CARGO_CALLS"])
with calls.open("a") as stream:
    stream.write(manifest.parent.name + "\\n")
print(json.dumps(metadata[manifest.parent.name]))
"""


def package(
    package_id: str,
    name: str,
    version: str,
    source: Path,
    license_name: str = "MIT",
) -> dict[str, str]:
    return {
        "id": package_id,
        "name": name,
        "version": version,
        "manifest_path": str(source / "Cargo.toml"),
        "license": license_name,
    }


def dependency(package_id: str, kind: str = "normal") -> dict[str, object]:
    return {"pkg": package_id, "dep_kinds": [{"kind": kind}]}


def metadata(
    root_id: str,
    packages: list[dict[str, str]],
    deps: list[dict[str, object]],
) -> dict[str, object]:
    nodes = {
        package_data["id"]: {"id": package_data["id"], "deps": []}
        for package_data in packages
    }
    nodes[root_id]["deps"] = deps
    return {
        "packages": packages,
        "resolve": {"root": root_id, "nodes": list(nodes.values())},
    }


class BundleLicenseCollectorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="dovifuse-bundle-licenses-test-")
        self.work = Path(self.temp.name)
        self.packages = self.work / "packages"
        self.packages.mkdir()
        self.mock_bin = self.work / "bin"
        self.mock_bin.mkdir()
        cargo = self.mock_bin / "cargo"
        cargo.write_text(MOCK_CARGO)
        cargo.chmod(cargo.stat().st_mode | stat.S_IXUSR)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def make_package(self, name: str) -> Path:
        source = self.packages / name
        source.mkdir()
        (source / "Cargo.toml").write_text("[package]\n")
        return source

    def run_collector(
        self, metadata_by_root: dict[str, dict[str, object]]
    ) -> tuple[subprocess.CompletedProcess[str], list[str], Path]:
        metadata_file = self.work / "metadata.json"
        metadata_file.write_text(json.dumps(metadata_by_root))
        calls_file = self.work / "cargo-calls.log"
        calls_file.write_text("")
        destination = self.work / "resources"
        env = dict(os.environ)
        env["PATH"] = os.pathsep.join((str(self.mock_bin), env.get("PATH", "")))
        env["DOVIFUSE_TEST_CARGO_METADATA"] = str(metadata_file)
        env["DOVIFUSE_TEST_CARGO_CALLS"] = str(calls_file)
        result = subprocess.run(
            [sys.executable, str(COLLECTOR), str(destination)],
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        return result, calls_file.read_text().splitlines(), destination

    def test_collects_both_roots_deduplicates_and_hashes_nested_notices(self) -> None:
        dovifuse_root = self.make_package("dovifuse-root")
        dovi_root = self.make_package("dovi-root")
        shared = self.make_package("shared-runtime")
        dovi_only = self.make_package("dovi-only")
        dev_only = self.make_package("dev-only")

        (dovi_root / "LICENSE").write_text("dovi tool license\n")
        nested = shared / "LICENSES" / "vendor" / "NOTICE.txt"
        nested.parent.mkdir(parents=True)
        nested.write_text("nested shared notice\n")
        (dovi_only / "COPYRIGHT.txt").write_text("dovi-only copyright\n")

        dovifuse_packages = [
            package("dovifuse-root", "dovifuse_converter", "1.0.0", dovifuse_root),
            package("shared", "shared-runtime", "2.0.0", shared),
            package("dev", "dev-only", "9.0.0", dev_only),
        ]
        dovi_packages = [
            package("dovi-root", "dovi_tool", "3.0.0", dovi_root),
            package("shared", "shared-runtime", "2.0.0", shared),
            package("dovi-only", "dovi-only", "4.0.0", dovi_only),
        ]
        result, calls, destination = self.run_collector(
            {
                "dovifuse_converter": metadata(
                    "dovifuse-root",
                    dovifuse_packages,
                    [dependency("shared"), dependency("dev", "dev")],
                ),
                "dovi_tool": metadata(
                    "dovi-root",
                    dovi_packages,
                    [dependency("shared"), dependency("dovi-only")],
                ),
            }
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, ["dovifuse_converter", "dovi_tool"])
        manifest_path = destination / "licenses" / "rust" / "manifest.json"
        notices = json.loads(manifest_path.read_text())
        self.assertEqual(
            {(notice["package"], notice["version"]) for notice in notices},
            {
                ("dovi-only", "4.0.0"),
                ("dovi_tool", "3.0.0"),
                ("shared-runtime", "2.0.0"),
            },
        )
        self.assertEqual(
            len([notice for notice in notices if notice["package"] == "shared-runtime"]),
            1,
        )
        self.assertNotIn("dev-only", {notice["package"] for notice in notices})
        shared_notice = next(notice for notice in notices if notice["package"] == "shared-runtime")
        relative = "LICENSES/vendor/NOTICE.txt"
        expected_hash = hashlib.sha256(nested.read_bytes()).hexdigest()
        self.assertEqual(shared_notice["files_sha256"], {relative: expected_hash})
        copied = destination / "licenses" / "rust" / "shared-runtime-2.0.0" / relative
        self.assertEqual(copied.read_bytes(), nested.read_bytes())

    def test_runtime_package_without_notice_fails(self) -> None:
        dovifuse_root = self.make_package("dovifuse-root")
        dovi_root = self.make_package("dovi-root")
        missing = self.make_package("missing-runtime")
        (dovi_root / "LICENSE").write_text("dovi tool license\n")
        packages = [
            package("dovifuse-root", "dovifuse_converter", "1.0.0", dovifuse_root),
            package("dovi-root", "dovi_tool", "3.0.0", dovi_root),
            package("missing", "missing-runtime", "5.0.0", missing),
        ]
        result, _, destination = self.run_collector(
            {
                "dovifuse_converter": metadata("dovifuse-root", [packages[0]], []),
                "dovi_tool": metadata("dovi-root", packages[1:], [dependency("missing")]),
            }
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Missing crate license files: missing-runtime", result.stderr)
        self.assertFalse((destination / "licenses" / "rust" / "manifest.json").exists())


if __name__ == "__main__":
    unittest.main()
