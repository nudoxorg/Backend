# Cold reader two — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_cold_reader_two`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Fresh cold calibration completed read-only.

Frozen custody verified exactly:

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Card: `workspace2/P5_C0_MANAGER_CARD.md`
- Commit: `8d990dfe1d8c74ba468d2be70e5b9cf656af63ff`
- Computed SHA: `ffce71ee4fd8d00f1ff5d1bd4d12397bc8938136a48c1ab0e6434bc48f3c86e8`
- Status: custody matches; not stale.

Literal calibration findings:

- First terminal is unambiguous: an ordinary public consumer compile-fail fixture proves `EntityId` cannot be used where `TypeId` is required, with exactly one `E0308`, then a legal mutant must compile and invalidate the diagnostic predicate.
- Writable scope is unambiguous: only `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`; production vocabulary source is read-only and digest-pinned.
- Negative space is explicit: no compiler dispatch, manifest/lockfile, dependency, unsafe, allocation, conversion API, registry, later-phase surface, or unlisted path.
- Budgets agree: zero production delta; 94-line skeleton; 110-line hard cap; 16-line unused test reserve; 4-byte/4-alignment layout.
- Known raw-rebrand limitation is correctly preserved and must not be misreported as newly repaired or newly introduced.
- The test’s exact-one rlib and current-executable-parent resolution are the right freshness controls.

Remaining unverified / blockers to calibration acceptance:

- No raw builder evidence was supplied, so freshness of the actual target directory, resolved rlib, source mutation capture, legal-mutant capture, and retained stderr cannot yet be independently verified.
- The card asserts Cargo’s target directory and the running test executable’s dependency parent are identical, but the supplied calibration material contains no literal path-comparison artifact. This remains `UNVERIFIED`, not a semantic card ambiguity.
- No implementation approval is granted by this calibration.

Calibration-only verdict: `PASS WITH IMPLEMENTATION EVIDENCE UNVERIFIED`. The card is sufficiently precise for edit authority once the required four-role calibration deck agrees; implementation and final acceptance remain outside this read.
