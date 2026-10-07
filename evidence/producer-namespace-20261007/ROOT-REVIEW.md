# Durable producer namespace and terminal publication admission

Root reviewed the complete two-file final change and its journal transitions.
The caller's original package spelling and request digest remain immutable. An
Accepted operation now durably binds the admitted producer package before work;
capture reads, refresh and recovery use that binding rather than resolving a
caller alias again after restart. Legacy entries retain their original package.
Conflicting bindings and binding after preparation are refused.

A reconstructed publication is admitted through the ordinary IndexProgress
reply validator before it can be returned. A selected workspace/view alone
cannot turn a Pending source capture into a Published reply. Missing or
unreconciled source evidence yields the existing typed unresolved observation.

Root's integrated source commit is recorded in `root-source-native-audit.json`.
Both changed file blobs exactly match frozen worker source
`495bb765df55c7be9dd631326a9e792b10d185d8`. Root independently hashed the raw
stdout and fleet proofs and checked the actual native test results:

- Producer namespace cold reopen, caller/digest preservation, binding conflict,
  late binding refusal and legacy fallback: 1 passed, 0 failed.
- Reconstructed Pending publication refusal and terminal reply admission:
  1 passed, 0 failed.

Both controls ran on native ARM macOS in one exact retained service test image,
SHA256 `4b2014319e043fd19c9a7edb6ce443131d5554a5f58bc95986da8da3eec9bc75`.
Their full Cargo logs, source identities, executable events, admissions,
completion and owned-process observations are retained under `native-controls`.

The separate actual production-owner Rust publication/restart test ran on
`387120e65d2d4b472bc834c7c3281656c880c6a6`, whose production changes match this
candidate. It exercised two changed-source publications and strictly admitted
terminal replies, then read the exact second terminal state after cold restart.
The retained `real-owner` receipts and wire bodies describe that limited scope.
The same unique test ran in parent and child; it counts as one distinct test.

This is not screenshot, animation, all-language GUI or current matched CLI/MCP
acceptance. The selected-capture diagnostic file was empty and is not evidence
of a selected capture. Ordinary idle-timeout and broader interface gates remain
separate.
