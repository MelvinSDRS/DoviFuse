# DV8 Maker for macOS

`DV8 Maker` is a native Apple Silicon SwiftUI front end for the repository's
standard and hybrid conversion modes. It stores large intermediates in a
user-selected external-SSD folder and bundles every runtime command-line tool.

Build on the M4 Mac mini:

```bash
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer ./macapp/build-macos-app.sh
```

The reproducible build cache is written to `.macos-build-cache/`; the finished,
ad-hoc-signed application is `dist/DV8 Maker.app`. Homebrew is not required at
build or runtime. The app bundles FFmpeg/ffprobe, dovi_tool, MKVToolNix and
MediaInfo. Connect the server share in Finder, choose a scratch folder on the
external SSD and drop the MKV inputs into the appropriate zones.

`main` contains Hybrid, DV7 → DV8 and Checker modes. Profile 5 hybrids are
rejected; the P5 Analysis mode and libplacebo/Vulkan/MoltenVK backend are
preserved on `codex/p5-workflow`.

In **Settings → DV7 Enhancement-Layer Archive**, enable **Save the original
EL + RPU** and choose its destination. The choice persists across launches;
an unavailable destination blocks standard conversion while archiving is
enabled. Existing users keep the previous app default: archiving disabled.
The CLI retains its separate default of archiving to the configured NAS path.
Before standard conversion removes the enhancement layer, all source RPUs are
classified as MEL, FEL or mixed and the consequence appears in the checks and
report. An archive preserves the original layer separately; FEL reconstruction
is not present in the converted movie.

Checker results are accumulated into a complete PASS/WARN/FAIL report. When a
warning or failure contains a safely repairable RPU/video sync offset, a
**Fix** button creates and verifies a new `*.DV8.Fixed.mkv` beside the source;
the original file is never replaced. Findings without a deterministic repair
remain visible in the report without offering the button.

Create the distributable disk image after building:

```bash
./macapp/create-dmg.sh
```

The result is `dist/DV8-Maker-arm64.dmg` with a matching SHA-256 file. Pushes to
`main` run `.github/workflows/release-macos.yml`; publishing also requires Linux
and bundled Mac regression tests and all reference acceptance gates. Those
gates are currently pending, so the hybrid release cannot publish yet.

Job reports are saved automatically under
`~/Library/Application Support/DV8 Maker/Reports/` and can be revealed with
**Show report**. Final UI state waits for process exit and both output streams to
finish; an absent or inconsistent saved report is an error. Repair eligibility
is bound to the checked file's path, size, inode, device, mtime and ctime. Changing
or replacing the input requires another check. These identities are filesystem
observations, not content hashes. See [report validation](../docs/AUDIT-REPORTS-2026-09-09.md).
