# Historical native view journal

`view-journal-v3-native-v21.bin` is exact native ARM64 output of the v21
snapshot and compact-event codecs, produced by source
`9b0001b3af1b590b8f1e9fc81ac2273782bbb629`, tree
`84ad9425ea576c1b8ee2fe023a00da1481a01b04`. That source differs from the retained
v21 address cohort only in the ignored explicit fixture emitter and a test-only
metadata assertion repair. No production wire version or encoder was changed.

The emitter constructed two checked builtin view transitions with labels
`legacy::first` and `legacy::second`, persisted a snapshot followed by a compact
event, recovered it using the native v21 decoder, and checked both raw inner
versions are 21. No version field or proof bytes were rewritten afterward.
These are codec regression operands, not compiler application IR evidence.

Managed native command: `cargo test --locked --offline -j4 -p
backend-local-service --lib emit_native_v21_journal_snapshot_and_compact_event
-- --ignored --nocapture`, with `SOL61_NATIVE_V21_JOURNAL` naming a new owned path.
One test passed. Permit PID 67648, launcher 67649, start
2026-10-06T23:47:55.895404 UTC, successful wholefleet sample age 5.890338 seconds,
terminal exit 0 at 23:51:39.113815. The fixture was then frozen and copied exactly.

21,790 bytes; SHA256
`464c5ad98c4988c0dcd4feb8e235fbead3d846f8918cf058f8de1ea344d32025`;
BLAKE3 `faed3440288d7bb208272069220d1dcdb87a84a230302d0e331d6b917d2d1a8e`.

The v22 test admits these literal persisted bytes through the closed journal-v3
grammar, refuses them through the strict live v22 codecs, appends a checked v22
event while preserving the old prefix, and verifies two independent cold reopens.
