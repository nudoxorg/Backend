# P5 C0-IR reserve recard churn

The former card digest `916dd59a57da11f7264efba9d45ace85453e3bcfc6082082daee0a5061e7db66`
forecast a 67-line test from a 14-line baseline while claiming 22 unused lines below a 75-line cap.
The arithmetic leaves eight lines, not 22, and no source-backed 67-line skeleton existed. Its fresh
calibration deck is therefore stale and cannot be cited as acceptance evidence.

The first replacement used an 85-line skeleton, but root-level hostile inspection rejected two latent
causality gaps: the first matching `libnudox_ir_vocab-*.rlib` was nondeterministic if stale artifacts
coexisted, and the predicate counted `E0308` without proving it was the only coded compiler error.
That replacement digest is stale. The current replacement uses the runnable, rustfmt-formatted 91-line
skeleton at
`evidence/p5-c0/ir/skeleton/coordinates.rs` (SHA-256
`40e666b28f4df7773a641555bdce2ac9a6dfcd05dcf8d2e53e88889bdcec008f`). The skeleton was compiled
as the actual test target against the current built `nudox_ir_vocab` rlib and ran both layout and
negative-API tests. It derives a 77-line test delta and preserves 19 literal unused lines below the
unchanged 110-line hard cap. It rejects zero or multiple matching rlibs, counts all `error[E` codes,
then requires the one error to be `E0308`; it is not a compressed or speculative implementation
allowance.

No compiler path, manifest, lockfile, workspace root, dependency, or production vocabulary source
changed during the recard. The test skeleton eliminates temporary fixture files and cleanup by piping
each source to `rustc` stdin and emitting legal-mutant metadata to stdout, which the test discards
through `Stdio::null` only after observing its successful status and false diagnostic predicate.

The subsequent fresh reviewer calibration invalidated its own deck because digest custody and clean
artifact provenance were still implicit. The next card requires the manager to commit the card, record
its complete SHA-256 externally, include it in every role prompt, clean and rebuild the vocabulary,
require exactly one rlib candidate, and record source plus rlib SHA-256 before it accepts raw stderr.

A later custody-complete calibration still rejected its packet because the 91-line skeleton was only a
hashed external artifact and production source was nominally writable despite a zero-production-delta
terminal. The current card delivers the complete 94-line skeleton as a frozen additional artifact only
to the Terra reviewer calibration, keeps Luna readers/misreader card-only, pins the vocabulary source
byte-for-byte, and reduces builder write authority to the ordinary public test.

The resulting reviewer found that the legal `TypeId` mutant only asserted a false error predicate; an
unrelated legal-mutant compiler failure could therefore pass. The current 95-line skeleton requires
legal compiler success before asserting the false predicate, consumes one forecast line, and retains
the mandatory 15-line test reserve below the unchanged 110-line cap.

The separate hostile pre-edit Terra review then denied the 95-line card: its layout checks compared
aliases to `u32` rather than asserting literal four-byte/four-alignment layout, and the pinned baseline
itself exposes `raw` plus `new(u32)`, allowing deliberate consumer-side rebranding. The canonical
replacement adds both alias-alignment and literal-layout assertions in a normally formatted 94-line
skeleton (80 added lines, 16 literal reserve), discovers its dependency directory from the running test
executable rather than a hard-coded target path, and shares a literal `CARGO_TARGET_DIR` with every
manager gate. It honestly narrows the terminal to direct typed-use safety and records raw-coordinate
rebranding as a preserved baseline counterexample that zero production-delta work cannot repair. This
is a semantic re-card; the 003193... fresh deck is retained as churn and cannot grant builder authority.

The next partial cold-reader deck stopped before its latter two roles when it found that a hard-coded
manager target path did not literally bind to `current_exe().parent()` and that the legal mutant did
not name its sole changed call argument. The current card pins every Cargo command to the shared
`CARGO_TARGET_DIR`, retains both source inputs, and names exactly `EntityId::new(7)` to
`TypeId::new(7)`. Its readable `occurrences` helper and `io::Error::other` errors reduce the real
skeleton to 94 lines while preserving a 16-line minimum reserve; this is not cap inflation.

The exact-builder checkpoint `48044df` was rejected only because its otherwise causal 94-line rlib
resolver tripped `clippy::collapsible_if`. The post-build Terra reviewer localized a behavior-preserving
repair: fold `resolved.replace(entry.path()).is_some()` into the existing filename predicate, preserving
short-circuiting and exact-one causality. The repaired skeleton is normally formatted at 93 lines (79
added lines, 17 literal reserve); the 94-line builder checkpoint and its raw review are retained as
rejected counterexample evidence. This new frozen artifact/card digest requires a fresh full deck.

The following partial fresh deck found that a stop rule said “25 lines above the 14-line test baseline,”
which contradicted the explicit 79-line test delta. The card now states both variance limits against the
current 79-line delta; this is a wording correction only, with no skeleton or terminal change. Its two
reader returns are retained as churn, and the full deck restarts before a Luna repair.
