# Cold reader one — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_typed_cold_reader_one`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Calibration-only cold-read result for P5 C0-IR.

Frozen custody verified:

- Worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- Branch: `codex/prototype-real-compiler-ir`
- Card: `workspace2/P5_C0_MANAGER_CARD.md`
- Commit: `91f7fe86e93f934ff639dd74cd2a313f4abddc03`
- SHA-256: `3977db393490c5e64d6bbcb75f0ef2ab5a9d311e6a2a62e912f2d97cf6ba0e90`
- Computed hash matches exactly; deck is not stale.

Assessment:

- First terminal is unambiguous: typed-use safety for actual public `EntityId`/`TypeId`, with no production-source change.
- Sole writable implementation path is explicit: `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`.
- `src/lib.rs` is clearly read-only and digest-pinned at 34 formatted LOC.
- Budget is literal: 100-line skeleton, 110-line hard cap, 10 unused-line reserve; 86 added test LOC over the 14-line baseline.
- Layout proof is literal and falsifiable: size and alignment must each equal `4`.
- Negative API scope is direct `EntityId` where `TypeId` is required, using the actual public crate artifact.
- Existing `raw`/public-`new(u32)` rebranding is explicitly preserved as a known limitation, not a new conversion requirement.
- Current-executable target discovery, exactly one fresh public rlib, one total `E0308`, retained stderr, and successful legal `TypeId` mutant are all specified precisely.
- Evidence custody is explicit; C0-COMPILER and all adjacent compiler/API work are prohibited.

Cheapest tempting misread: “Add a public conversion or repair the raw-coordinate rebranding limitation.” Literal rejection: the card forbids production/API edits and explicitly preserves raw rebranding as an out-of-scope counterexample; only the test path may change.

Blockers/ambiguities: none found in the canonical contract. The relationship among source-candidate commit `44c221...`, restoration commit `ebb81...`, and frozen custody commit is provenance context rather than an implementation ambiguity.

Unverified by required cold-read boundary: actual source bytes, skeleton bytes, rlib discovery, compiler output, toolchain, clean gates, and reviewer execution. No source/history/evidence/tree inspection was performed.

Verdict: `CALIBRATION PASS — no implementation approval`.
