# DV8 Maker for macOS

`DV8 Maker` is a native Apple Silicon SwiftUI front end for the repository's
standard and hybrid conversion modes. It stores large intermediates in a
user-selected external-SSD folder and bundles every runtime command-line tool.

Build on an Apple Silicon Mac with Xcode installed:

```bash
DEVELOPER_DIR="$(xcode-select -p)" ./macapp/build-macos-app.sh
```

The reproducible build cache is written to `.macos-build-cache/`; the finished,
ad-hoc-signed application is `dist/DV8 Maker.app`. Homebrew is not required at
build or runtime. The app bundles FFmpeg/ffprobe, dovi_tool, MKVToolNix and
MediaInfo. Connect the server share in Finder, choose a scratch folder on the
external SSD and drop the MKV inputs into the appropriate zones.

The app contains Hybrid, DV7 → DV8 and Checker modes. Profile 5 inputs are
not supported.

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
`main` run `.github/workflows/release-macos.yml`. After Linux regressions,
packaged Mac tests, and DMG verification pass, a separate publishing job creates
a regular release with the DMG, portable SHA-256 checksums, and the
release source archive. Pull requests build and verify the DMG without publishing.

To rerun publication, dispatch the workflow on `main` with the default
`build` channel. To require the additional playback acceptance checks, select
`validated`: it also requires `scripts/verify_reference_catalog.py --release`
to pass. Pending acceptance is reported in the normal CI summary and continues
to block the validated channel. Published commit releases are kept unchanged on reruns; failed draft
uploads can be retried.

The app is ad-hoc signed, not Developer ID signed or notarized. A downloaded app
may require approval under **System Settings → Privacy & Security**. Full
playback acceptance and notarization are separate from a successful automated
build.

Job reports are saved automatically under
`~/Library/Application Support/DV8 Maker/Reports/` and can be revealed with
**Show report**. Final UI state waits for process exit and both output streams to
finish; an absent or inconsistent saved report is an error. Repair eligibility
is bound to the checked file's path, size, inode, device, mtime and ctime. Changing
or replacing the input requires another check. These identities are filesystem
observations, not content hashes.
