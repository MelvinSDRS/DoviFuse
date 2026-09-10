# Main workflow and P5 branch

`main` supports standard DV7→DV8 conversion, Profile 7/8 hybrid donors and
checker/sync repair. It retains the audited remux preservation, MEL/FEL archive
settings, job reports, source-retention safeguards and conservative L5 handling.

Profile 5 hybrid donors are rejected, including `--force`, `--skip-grade-check`,
metadata-only grading and dry runs. There is no P5 Analysis tab or experimental
color-probe/pair-analysis command. The Mac app bundles ordinary FFmpeg/ffprobe;
it does not include libplacebo, Vulkan or MoltenVK.

All P5 development is preserved on `codex/p5-workflow`: reconstructed-picture
and native-area diagnostics, their UI and CLI modes, backend dependencies,
reference-kit tooling, authored controls, full-film observations and research.
The branch preserves its incomplete-validation safeguards; moving it does not
constitute color acceptance or enable automatic P5 conversion.

The P5 branch is based on the finalized main commit, so its diff contains
the isolated development work. Local ignored caches and verification files are
retained, but are not shipped or added to either branch.

## Verification

Main passed 96 Rust tests, four L5 example tests, Clippy, 22 real-tool baseline
scenarios and eight P5/removed-command controls, also checked through the launcher.
The preserved P5 tree passed 108 Rust tests from a clean checkout.

The main Mac bundle was built, tested, installed, signed and relaunched. Its 26
actual job/report traces passed the AppModel replay checks; the installed app
passed P5 rejection and standard source/archive controls. Native UI renders were
inspected. The clean installed bundle contains no experimental color backend.
See [recorded evidence](evidence/2026-09-10-main-p5-split.json).

Full-length performance and Dolby playback gates remain pending. No remote push
or publication is part of this change.
