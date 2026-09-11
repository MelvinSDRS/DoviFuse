# Stability audit handoff — stopped at user request

The user stopped the audit on 11 September 2026 to continue later. All conversion
and queue workers have exited. Do not restart them without a new user request.
The work is on `codex/stability-audit`, with draft PR
[DV8 #1](https://github.com/MelvinSDRS/DV8/pull/1). Nothing was merged or released.

## Completed

- Supported workflow reliability fixes are implemented. P5 remains disabled,
  including override flags; its experimental branch is separate.
- Linux and bundled Mac regressions passed: 102 Rust tests, four L5 examples,
  Clippy, 20 fault scenarios per platform, native Mac ENOSPC/retry, preservation,
  wrapper and L5 controls. Three native SMB controls and 49 app report replays
  passed on the user Mac; hosted CI has 46 replays without the private SMB cases.
- Process cancellation/draining, source-change detection, archive cancellation,
  MakeMKV UID handling and macOS SMB synchronization fixes are documented in
  [the audit](STABILITY-AUDIT-2026-09-10.md).
- The fixed self-contained Mac app is installed, signed, smoke-tested and
  relaunched, with a rollback copy. Runtime source commit:
  `463802d60d922b7c4df4d46bf69c67197d67985d`. Installed converter SHA-256:
  `67600b6e5252d3f2132685974b6c6387e64885038bec38d7a863dd693eaddb64`.
- Full-length P8 checker comparison passed. FEL conversion and output-checker
  comparisons passed, including full decode, frame/RPU counts and applicable
  container checks, with no leftover scratch or child processes.
- The user confirmed FEL Dolby Vision activation, colors, clean scene changes,
  audio/subtitles, ten uninterrupted minutes, three planned seeks, dark gradients
  and bright highlights. Match Dynamic Range and Infuse Profile 8 Dolby Vision
  were enabled. [Playback evidence](evidence/2026-09-11-playback.json) records the
  exact observations and their scope.

| Full-length case | Reference | Candidate | Result |
| --- | ---: | ---: | --- |
| P8 checker | 912.223 s | 911.370 s | Passed |
| FEL standard conversion | 3,172.313 s | 3,135.125 s | Passed; expected FEL residual limitation |
| FEL output checker | 1,502.300 s | 1,503.882 s | Passed; 149,006 frames/RPUs and 239 detected cuts at offset zero |

The FEL timing reference is the original baseline with **only** the MakeMKV UID
correction. The unchanged baseline's rejection and the first candidate's SMB
archive failure are retained separately. The reference extraction overlapped an
earlier build/suite; the observations are not a controlled speedup claim. The P8
checker uses an earlier candidate whose exact identity is recorded. See
[full-length evidence](evidence/2026-09-10-full-length.json).

## Stop and retained state

The MEL reference run was cancelled during source HEVC extraction after
1,503.531 seconds. Its report says `cancelled` / `inconclusive`; its source's
before/after filesystem identity is unchanged. Cancellation took 0.734 seconds,
without forced termination, leftover scratch or surviving media processes.
This is an interrupted reference run, not a completed MEL result or a substitute
for the planned candidate cancellation tests. See
[stop evidence](evidence/2026-09-11-user-stop.json).

The validated FEL playback output was moved into the NAS Movies playback folder
at the user's request, using a same-volume rename. Its old Downloads path no
longer exists. Reports retain their original paths; the relocation is recorded
privately. Output SHA-256:
`cdc30830a6cda3b050ea863a3dc2e049209a571ce1d86892a18f321691ba7f43`.

Preserved locations:

| Artifact | Location |
| --- | --- |
| Local audit state, logs, installed-app/CI proofs, private playback record | `.verification/stability/` in this checkout |
| Persistent local snapshot of worker scripts, inventories, hashes and raw reports | `.verification/stability/stopped-2026-09-11/` (ignored, not committed) |
| Original worker workspace | `/tmp/dv8-stability-baseline/` on Linux and the Mac |
| Original baseline, corrected reference and fixed candidate bundles | `/tmp/dv8-stability-baseline/{baseline,reference,candidate}.app` on the Mac |
| Disposable NAS media and archives | `Downloads/DV8-stability-audit-20260910/` |
| FEL playback file | NAS `Movies/DV8 Stability Playback - 20260910/`; exact filename in the private playback record |
| T5 scratch root | `/Volumes/Samsung T5/DV8-stability-audit-20260910/` |
| Mac build source | `/tmp/DV8-build` (symlink to the user's build cache) |

A local snapshot of the 16 MiB Linux worker workspace was preserved in the checkout
before committing. The Mac bundles and any Mac-only artifacts still live under
`/tmp`. These and the ignored evidence files are not committed media backups. Preserve
them before rebooting, cleaning temporary storage or moving to another machine.
If they are lost, do not reconstruct proof from assumptions: recreate the inputs
and rerun the affected acceptance work. No original library media was converted
or deleted by the audit.

## Remaining work

1. **MEL full-length comparison:** verify the retained input against its recorded
   checksum, then run the corrected reference and fixed candidate on independent
   disposable copies. Run the checker on both outputs. Full-source MEL/FEL
   classification, final timings, memory/scratch use and output hashes are still
   required; only the initial sample identified this source as MEL.
2. **P7 and P8 hybrid comparisons:** prepare the planned matching HDR10 targets,
   run reference/candidate hybrids and check every output, retain input hashes
   and compare resource use. These planned targets are derived from the same
   source and establish mechanics/performance, not independent-release grade
   equivalence.
3. **Independent hybrid assessment:** the inventoried P8 donor/target pair is
   unverified and differs by six frames. Use default gates, no force/skip
   overrides. Preserve an honest refusal if it mismatches. Independent P7
   donor/target coverage has not been found and remains a coverage gap.
4. **Real candidate cancellation:** run the queued full-media checker decode and
   standard archive-copy cancellation controls. Verify completion within ten
   seconds on responsive storage, honest cancelled reports, retained sources,
   partial-archive cleanup and no surviving children. Keep blocked kernel/NAS I/O
   limitations explicit.
5. **Remaining playback:** obtain actual observations for completed MEL, P7 and
   P8 hybrid outputs, with output identities and timestamps. Establish letterbox
   and changing-aspect-ratio coverage where representative media provides it.
   The FEL observations do not pass these other cases.
6. **Final review:** investigate any repeatable runtime increase above 10%, check
   measured scratch against estimates and review all failed/inconclusive cases.
   Run required final regression/hosted checks if code changes; rebuild and
   redeploy changed code only after active jobs finish, preserving rollback.
   Close the two release gates only when the full evidence supports doing so.

## Safe resumption

Start by reading `.verification/stability/STATE.md`, this report and the saved
inventories/configuration. Reach the Mac at `melvinsiadous@192.168.2.20` if
`mac-mini` does not resolve. Verify the T5 and NAS mounts, free space, tool/bundle
identities, existing reports and retained input identities before writing.

All old session handles have terminated. The old worker scripts contain previous
starting assumptions, completed filenames and report paths; **do not blindly
rerun them**. In particular, the FEL candidate has moved to Movies, and the MEL
reference report now records a deliberate cancellation. Preserve these reports
and use fresh run names/directories when explicitly restarting unfinished work.
Skip the completed FEL and P8 checker comparisons unless a code change or a
specific concern justifies repeating them.

Run large jobs sequentially on the T5 with NAS input/output. Never use library
originals as standard-mode inputs, never hardlink disposable copies, and never
reuse incomplete archives. Both `full-length-performance` and
`dolby-playback-qc` remain **pending** in `tests/fixtures/release-gates.json`.
Release publication stays blocked.
