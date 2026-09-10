# Job reports and final app outcomes — 2026-09-09

Historical evidence before the branch split. See [current main scope](MAIN-WORKFLOW.md);
P5 research and its release gates now live on `codex/p5-workflow`.

General `--report` support now covers standard conversion, hybrid conversion,
checking and sync repair. Previously it was limited to the color-backend probe.
The app always requests a fresh report outside its signed bundle and offers a
**Show report** action. The existing color-probe report schema is unchanged.

## Saved evidence and execution ordering

Schema v1 records separate execution and validation outcomes, all emitted check
results and repair hints, available measurements, explicitly scoped coverage,
arguments/overrides, tool paths/versions, elapsed time and filesystem identities
of inputs before/after and completed outputs. Available measurements include
hybrid offset/grade window summaries and checker metadata-sync offsets. Unknown
frame coverage stays null; legacy brightness metrics are not labeled calibrated
RGB grading. No donor-grade claim can be made by a standalone checker.

The report destination is reserved with create-new before runtime/tool setup. A
running/inconclusive checkpoint is synced first. Terminal reporting is synced
before JSONL completion is emitted. Human-progress invocations collect the same
checks. A failed job has a failed report, cancellation remains explicit, and a
dry run never validates media. Existing report files, source aliases and replaced
report paths are preserved. Runtime setup failures leave a failed report when
reservation succeeded. Malformed CLI arguments or an unavailable destination
cannot produce a report. The event collector is bounded; exceeding its budget
fails reporting instead of silently dropping checks and claiming success.

A report write is not a transaction over the movie output. A process or machine
crash during writing can leave an incomplete report, which is not acceptance;
standard conversion retains its documented in-place replacement semantics.

## App and repair identity

The previous termination callback could finish the UI before pending pipe reads.
Both readers now deliver data and EOF in stream order on the main queue. Process
termination cannot finalize the result until stdout and stderr both reach EOF.
A trailing JSON record without newline is consumed. Run IDs reject stale events.
Exit zero without completion or the expected report fails. The saved terminal
schema/status/execution must agree with the observed event and exit status.

Repair availability is bound to the actual checked path, device, inode, size,
mtime and ctime. Changing the selection, replacing the file, or modifying it and
restoring its mtime invalidates that binding. A report-enabled check/hybrid/repair
also fails if input filesystem identity changes during processing. These are
filesystem observations, not cryptographic content hashes or protection against
all concurrent filesystem races. Standard in-place changes are intentionally
excluded from the unchanged-input requirement.

A repair starts with fresh check results. Inconclusive results remain visible and
block automatic repair. Report warnings/uncertainty contribute to the final app
title rather than being lost when individual check rows pass. Runs without
recorded checks, including current standard-mode reports, remain inconclusive
rather than inferring validation from exit zero.

## Verification

Linux: 96 converter tests, Clippy with warnings denied, example/process controls,
22 real-tool scenarios, preservation and L5 controls, report fault controls and
isolated queue-wrapper checks passed. All real-tool scenario reports are compared
with emitted JSONL checks and exit status. CI retains their report artifacts.

macOS: the self-contained bundle passed the same real-tool baseline, report
controls, backend/preservation/L5 checks and Swift state tests. Swift tests force
termination before final pipe data, split/trailing JSON, stale run events, missing
completion, invalid saved reports, input replacement and restored-mtime changes.
Native off-screen hybrid/checker renders were inspected for the report action
and incomplete-validation title. They are test renders, not physical playback QC.

The [evidence](evidence/2026-09-09-job-reports.json) records final installation,
installed-tool checks, signatures, binary identity and rollback location.

## Release boundary

This completes a further part of milestone 4, not the complete hybrid release.
Full default picture analysis, independent P5 reconstruction validation,
calibrated grade thresholds, variable-area ground truth, the remaining app/archive
work and Dolby-display QC still require their own acceptance. The release gate
matrix remains two passed and seven pending; this turn does not promote a gate.
No private movie was processed or modified by these tests.
