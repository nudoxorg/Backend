# Cold reader one — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_cold_reader_one`
Model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; read-only; card and governing skills only.

Fresh cold-reader calibration report (read-only; no source, skeleton, candidate, history, evidence, prior report, working tree, or task conversation inspected).

Custody verification:

- worktree: `/private/tmp/nudox-prototype-real-compiler-ir`
- branch: `codex/prototype-real-compiler-ir`
- card: `workspace2/P5_C0_MANAGER_CARD.md`
- commit: `8d990dfe1d8c74ba468d2be70e5b9cf656af63ff`
- observed SHA: `ffce71ee4fd8d00f1ff5d1bd4d12397bc8938136a48c1ab0e6434bc48f3c86e8`
- required SHA: identical; custody is fresh, not stale.

Calibration restatement:

- First and only terminal is an ordinary public consumer proving `EntityId` cannot be passed where `TypeId` is required.
- It is direct typed-use-only evidence; no compiler dispatch, conversion API, raw decoder, registry, frontend, builder, or C1 surface.
- Production source remains byte-identical; only `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs` may be written.
- Layout is literal 4-byte size and 4-byte alignment for `DenseId<Entity>`, `EntityId`, and `TypeId`.
- Test budget is 110 lines hard cap, forecast 94, with 16 literal unused reserve; production delta is zero.
- The negative fixture must use the actual public crate artifact, exactly one forbidden expression, exactly one `error[E` marker, and require `E0308`, `EntityId`, and `TypeId`.
- The causal legal mutant changes only `EntityId::new(7)` to `TypeId::new(7)`; it must compile and make the diagnostic predicate fail.
- Artifact binding is exact-one public rlib, resolved from the running test executable’s parent dependency directory. Manager Cargo gates use the same `domains/ir/target` directory and current executable dependency binding.
- Source digest, rlib path/hash, toolchain, commands, original and mutant inputs, and raw stderr are mandatory evidence.
- The known public `raw` plus `new(u32)` rebrand limitation is explicitly preserved and must not be misreported as repaired or newly introduced.
- No panic-like test terminals, discarded results, shadow/local types, second fixture errors, source-dropping conversions, dependencies, unsafe, allocations, or unlisted paths are allowed.

Cheapest plausible misread/rejection:

A worker could resolve an arbitrary or multiple `libnudox_ir_vocab-*.rlib` candidates, or use the workspace-default target directory, producing a superficially compiling but stale/shadow-artifact diagnostic. The card explicitly rejects this through exact-one discovery, current-executable parent binding, source/rlib digest evidence, and clean preparation. A second cheap rejection is accepting the legal mutant merely because it compiles; the required predicate failure prevents that.

Blockers and ambiguities:

- No semantic blocker or material ambiguity found in the frozen card.
- Actual compiler output, rlib identity, source digest, test implementation, line count, and gates remain `UNVERIFIED` because this calibration is expressly read-only and source-blind.
- The card’s source-pinned comparison path is intentionally read-only/unverified, not an implementation obligation.

Calibration-only verdict: PASS / builder authorization conditions are clear. This is not implementation acceptance or product closure; edit authority remains conditional on the other required cold-reader, plausible-misreader, and Terra reviewer calibrations agreeing on the same tuple.
