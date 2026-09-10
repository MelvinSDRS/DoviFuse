# Main regression fixtures

`main` supports standard P7→P8 conversion, P7/P8 hybrid donors and checking.
P5 hybrid donors are rejected unconditionally. The synthetic `p5-mechanics.mkv`
fixture exists only to test that refusal, including attempted overrides.
It is not a P5 color-reference fixture.

`catalog.json` pins upstream metadata and bitstreams. `audit_fixtures.py`
authors frame counts and offsets; preservation tests compare actual track,
chapter and attachment payloads. No private film is needed by these suites.

P5 reconstruction, full-picture diagnostics, calibration fixtures and the
nine-gate experimental workflow are preserved on `codex/p5-workflow`.
The release gates here describe only the supported main workflow. Pending
performance and physical-playback acceptance remain explicit.
