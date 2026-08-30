# Cold reader two — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_clippy_cold_reader_two`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

CALIBRATION-ONLY verdict: **BLOCKED pending card clarification; no edits made.**

Custody passes: the requested commit resolves to SHA-256
`84dbe298d851af1057e9abfdae7fd7893d52c37c3c6917e03830930f35100a6f`, matching the card.

The rlib-assignment concern is causal:

- Folding assignment into a filename predicate is safe only if it inspects an existing candidate and rejects a second before replacement, while still scanning enough entries to detect multiplicity.
- A predicate that assigns and returns the first match can stop early, overwrite the first path, or collapse multiple candidates into one. That changes zero/one/multiple causality.
- Discovery must remain independent from the compiler diagnostic predicate. Otherwise zero/multiple rlibs can become a generic discovery failure, weakening the proof that the actual artifact produced exactly one `E0308` mismatch.
- The legal mutant proof remains valid only when exactly one artifact is resolved, the original produces precisely the expected diagnostic, and the mutant compiles before its diagnostic predicate is required to fail.

Card scope and negative space are otherwise explicit: read-only calibration, one writable test path only for later builder work, no production/API/compiler/dependency changes, and the known raw-rebrand limitation is not overclaimed. Artifact custody and evidence requirements are clearly stated, but actual rlib/stderr/toolchain evidence remains **UNVERIFIED** at calibration time.

Major ambiguity: the card says to stop if the test file grows “25 lines above the 14-line test baseline,” which implies a 39-line ceiling, while simultaneously authorizing a 93-line skeleton, 79-line test delta, and 110-line hard cap. These limits conflict and require canonical wording before calibration can pass.
