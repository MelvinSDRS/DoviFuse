# Supported-workflow stability audit

Baseline: `71739e599efe57905719237046b710a0175a00ef`. Work branch:
`codex/stability-audit`. Profile 5 remains unsupported, including overrides;
the experimental branch is not merged.

## Reproduced problems and repairs

The unchanged converter exceeded the ten-second cancellation deadline when a
captured decoder or streamed tool spawned a descendant holding its output pipes.
Version discovery also ignored cancellation. A tool that exited while leaving
a descendant attached to its pipes could hang completion indefinitely. These
four failures were reproduced against a preserved baseline executable.

All external media tools, including discovery and report-version probes, now use
private process groups. Cancellation terminates the group and remains active
during pipe drainage. A leader that exits without closing its pipes fails after
two seconds; teardown waits at most another two seconds for reader threads.
Probes have a ten-second deadline. Ordinary conversions have no wall-clock timeout.

Captured metadata has an explicit 256 MiB budget per pipe (1 MiB for probes).
Exceeding it fails validation instead of truncating the data or growing memory
indefinitely. Streaming commands bound individual output chunks to 64 KiB.
Temporary-file cleanup failures now produce warnings with the affected path.
No CLI options or report schema changes are required.

A separate control reproduced replacement of a source whose filesystem identity
changed during standard conversion. Standard jobs now compare the original
identity immediately before rename and refuse to overwrite a changed source.
Cancellation is checked at that boundary too. This is a filesystem-stat guard,
not a content hash or an atomic lock against uncooperative writers.

Code inspection also found that archive copying could not observe cancellation
until a whole-file copy returned. Archives now copy in 4 MiB chunks, check
cancellation between reads/writes, and sync the completed archive before removing
its scratch source. A regression cancels after copying starts, verifies removal
of the partial archive, and verifies a clean retry. Blocking filesystem calls
remain subject to the operating system's I/O behavior.

Process-group semantics were checked against the
[Rust CommandExt documentation](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#method.process_group).
DV conversion semantics remain those of the pinned
[dovi_tool implementation](https://github.com/quietvoid/dovi_tool/tree/b25558062e4a56973482ec70133bd7b891320e48):
standard P7 conversion preserves base pictures and converted RPU, not FEL residual
reconstruction. No new grade-equivalence claim is introduced.

## Reproducible verification

Run `scripts/test-linux.sh`, then build the Mac app and run
`scripts/test-macos-bundle.sh <app> <prepared-seeds>`. Both suites include
`scripts/audit_faults.py`; set `DV8_AUDIT_FIXTURES` to the prepared seed directory.
For an unchanged executable, set `DV8_AUDIT_BIN` and pass `--baseline` to retain
all failed controls instead of stopping at the first failure.

The 20 additional scenarios cover cancellation during captured output, streamed
output and probing; abandoned descendant pipes; simultaneous output reservation;
forced termination/restart; extraction, conversion, editing, injection, remux and
decode errors; scratch/archive disappearance; a read-only replacement directory;
report replacement after output validation, and source changes before replacement. Existing suites cover truncated
inputs, archive collisions, stale repair, header/payload preservation, P5 refusal,
and the qBittorrent failure boundary.

ENOSPC and EIO are deterministic **tool-boundary injections**, not a physical NAS
disconnect. Directory removal and permission changes affect only owned test
directories. The Mac suite additionally fills an isolated 16 MiB HFS+ disk image
to actual ENOSPC: archive failure retains the source, removes the partial archive,
and succeeds on retry after the owned fill file is removed. Both reports are
replayed through the app, bringing the final replay count to 46.

SIGKILL leaves a running/inconclusive report and owned temporary
files; a subsequent job refuses them. It does not automatically resume or erase
them. A test supervisor explicitly stops surviving test tools after this control.
SIGKILL, power loss, escaped process groups and uninterruptible kernel/storage I/O
are not covered by the ordinary cancellation guarantee.
The forced-kill control targets the converter process. A native GUI force-quit
and relaunch during an active disposable job remains a separate unperformed check;
report replay tests do not substitute for it.

Standard replacement remains intentional: before the validated rename the source
is retained; after that rename a later report-write failure must fail the job
without undoing a valid replacement. Inspect the output and old report before
retrying. Never interpret an interrupted or missing report as repair authority.

## Full-length acceptance

Private media copies and logs live outside the repository. `stability_measure.py`
creates copies with exclusive creation, streamed SHA-256, source stat checks and
independent destination checksum verification. It never hardlinks inputs.
Its `run` command records elapsed time, sampled process-tree RSS, sampled scratch
use, remaining scratch and surviving processes. Peaks sampled every half second
may miss shorter spikes and are not exact kernel high-water marks.

Hardware: Apple Silicon Mac mini, 16 GiB RAM; SMB NAS; Samsung T5 APFS scratch
volume (approximately 196 GiB available when attached). Run large jobs sequentially.
Initial samples identify an available MEL source and an FEL source; complete RPU
classification must come from the full conversion reports.

Required matrix: standard MEL, standard FEL, P7 hybrid, P8 hybrid; checker on each
output; baseline/candidate comparisons using identical independent copies and
tool versions. Investigate repeatable elapsed-time regressions above 10%.
Measure real cancellation as well as synthetic controls. Keep genuine matched
donor/target coverage separate from any derived same-source mechanics fixtures.

The full-length and playback release gates remain pending until this entire
matrix and the manual checklist have supporting observations. Synthetic tests,
a successful build, or partial real-media runs cannot close either gate.

The first completed full-length comparison is the P8 checker: 148,262 frames,
912.223 seconds on the baseline and 911.370 seconds on the measured candidate.
Both fully decoded the input and passed frame/RPU and offset-zero sync checks.
Sampled scratch peaked at 33,191,052 bytes, with no remaining scratch or surviving
children. The candidate measurement predates the archive-only change; its exact
binary identity and the initial overlapping copy workload are recorded in
[the partial measurement evidence](evidence/2026-09-10-full-length.json).

## Action plan status

| Work | Evidence and next acceptance step |
| --- | --- |
| Reproducible baseline | Complete: preserved executables, source hashes, Linux/Mac suites, independent media-copy checksums and hardware/storage record. |
| Demonstrated failure fixes | Complete for the tested cases: 20 fault scenarios on each platform, real isolated Mac ENOSPC/retry, 99 Rust tests and 46 actual app report replays. |
| Native GUI interruption | Pending: force-quit/relaunch while an isolated disposable job is active, then inspect reports, source retention and recovery. Existing SIGKILL coverage targets the converter. |
| Full-length resource/performance matrix | In progress: P8 checker pair complete; standard FEL/MEL, same-source P7/P8 hybrid pairs and real-media cancellation remain. Review all reports, resource estimates and any repeatable runtime increase over 10%. |
| Independent hybrid reference coverage | Pending: same-source positive controls cannot establish independent WEB/Blu-ray grade equivalence. Retain conservative grade, alignment and L5 refusals. |
| Dolby playback QC | Pending user observations on Apple TV 4K / Infuse / LG C1 over SMB, with output identities and timestamps. |
| Hosted CI | Passed for candidate `343e5f36691ff9cf594cbbf890c2fedb3c828cbc`, [Linux and Apple Silicon run](https://github.com/MelvinSDRS/DV8/actions/runs/34542446424). |
| Mac installation | Queued behind the active conversion, with preserved rollback, installed smoke tests, signature verification and relaunch required. |
| Publication | Blocked by pending full-length and Dolby playback release gates. |

## Playback and release

Use [the playback checklist](PLAYBACK-CHECKLIST.md) for Apple TV 4K, Infuse,
LG C1 and NAS via SMB. Record actual output checksums and timestamps in the
private test record. Record player/OS versions and observations; do not infer
successful Dolby playback from metadata or software decode alone.

Final acceptance also requires Linux and bundled Mac suites, hosted CI on the
candidate commit, and installation/signature/relaunch proof for that candidate.
Publication stays behind the existing release-evidence gate.

The first hosted run exposed a Clippy 1.98 lint in the pre-existing frame-count
parser; its equivalent reverse-iterator lookup is included in this branch.
The hosted Swift compiler also rejected shared weak captures across the pipe
reader and main queue. Each queued callback now owns its own weak capture, while
retaining per-stream FIFO delivery and the existing exit/EOF completion rule.
App replay tests explicitly compile in Swift 6 with complete concurrency checking.
