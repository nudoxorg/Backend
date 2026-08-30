# Fresh Luna cold reader one — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_cold_reader_one`
Requested model: `gpt-5.6-luna`
Fork mode: `none`
Role: read-only independent reader

Cold calibration report (read-only; no source, code, history, or prior evidence inspected)

1. First observable capability and terminal

The sole capability is the inward IR vocabulary proof:

- `EntityId` and `TypeId` remain distinct four-byte, four-alignment `DenseId` coordinates with private phantom brands.
- An ordinary public consumer imports the actual exported `nudox_ir_vocab` artifact and attempts to use `EntityId` where `TypeId` is required.
- The terminal is a compiler-process fixture that produces exactly one `E0308` diagnostic naming `EntityId` and `TypeId`; a legal `TypeId` mutant must make the diagnostic predicate fail.
- This does not authorize compiler dispatch, fragments, frontends, registries, builders, schedulers, sandboxes, bundles, publication, or C1/C0-COMPILER work.

2. Exact allowed paths and named baseline

Writable builder paths are exactly:

- `domains/ir/crates/nudox-ir-vocab/src/lib.rs`
- `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`

Named source-candidate baseline: `44c22154fd5238e4769562590420371979306050`.

Active compiler paths are restored to the source candidate by `ebb81a9f`; rejected compiler churn is inspectable but grants no edit authority.

Manager-only artifacts:

- `P5_C0_MANAGER_CARD.md`
- `P5_C0_CALIBRATION_RAW.md`
- `evidence/p5-c0/ir/**`

Baseline formatted sizes/digests:

- `src/lib.rs`: 34 LOC, SHA-256 `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78`
- `tests/coordinates.rs`: 14 LOC, SHA-256 `7ca6d45b5fbeb9b0961d5726eea0249e574eb3f76d6b3d9f37fdc17e2aefdc43`

3. Preserved facts and prohibited adjacent behavior

Preserve:

- `DenseId<Owner>::new(u32)` as the sole local dense-coordinate construction.
- Private phantom brands for `EntityId` and `TypeId`.
- Four-byte size and four-byte alignment for `DenseId<Entity>`, `EntityId`, and `TypeId`.
- The existing production representation and semantics; production source delta is zero.
- The ordinary public downstream-consumer shape and actual exported crate boundary.
- Dependency-free, current-toolchain compile-fail evidence.

Prohibited:

- Raw wire decoding, identity rebranding, authority conversion, phase transitions, later-phase values.
- `From`/`TryFrom`, raw authority bytes, public traits/getters, public tuple fields, phase bits, runtime tags.
- `dyn`, `Box`, `Vec`, `Arc`, `Rc`, `String`, serde, async, unsafe, SIMD, macros, allocation, fallback/default state.
- Compiler paths, manifests, lockfiles, workspace-root edits, dependencies, shared crates, registries, conversion APIs, or unlisted paths.
- Panic or discarded-failure test machinery: `panic!`, `unwrap`, `expect`, `unreachable!`, source-dropping conversion, or discarded cleanup.
- Local/shadow types, extra fixture errors, doctest-only/runtime-only proof, noncausal mutants, or missing retained stderr.
- C0-COMPILER, C1, or any adjacent product behavior.

4. Expected public surface and explicitly forbidden surface

Expected public surface is unchanged. The existing public vocabulary must remain consumable across the crate boundary; the test adds no new production public item and no fixture API.

The proof must exercise the actual public `nudox_ir_vocab` artifact, not a locally recreated type.

Explicitly forbidden public surface:

- New public types, fields, traits, getters, conversions, registries, compiler APIs, phase APIs, or test-support APIs.
- Public tuple fields or public authority/representation access.
- Any `dyn`/erased API, runtime type tag, backend enum, compatibility shim, or future-phase type.
- Any manifest, dependency, reexport, lockfile, or unlisted path change.

5. Evidence row | falsifier | hard cap | stop trigger

| Evidence row | Falsifier | Hard cap | Stop trigger |
| --- | --- | --- | --- |
| Exact layout | `coordinates.rs` checks `size_of` and `align_of` for all three coordinate forms | Any coordinate not exactly 4 bytes and 4 alignment | Stop on representation/layout change |
| Static kind impossibility | Actual exported crate is imported by one ordinary public consumer; exactly one mismatched expression yields `E0308` naming both types; legal `TypeId` mutant invalidates the predicate | Local/shadow fixture, extra error, doctest/runtime-only proof, noncausal mutant, missing stderr | Stop on any noncausal or incomplete compiler proof |
| Boundary/public consumer | Test resides in ordinary top-level `tests/coordinates.rs`; no production conversion/compiler dependency | Any compiler/API/manifest/conversion/unlisted-path edit | Stop immediately on boundary expansion |
| Representation preservation | Production module remains unchanged and private brands remain intact | Any production representation delta | Stop and re-card |
| Test discipline | No panic helpers, unwrap/expect/unreachable, source-dropping errors, or discarded cleanup | Any such test mechanism | Stop and repair scope |
| Commands/evidence | Manager records exact toolchain, compiler-process command, source digest, rlib path, stderr, and legal-mutant command/result | Missing raw artifact or non-reproducible command | Evidence is inadmissible; do not close |
| Formatting/gates | `cargo fmt --check`, locked workspace test/clippy, `git diff --check`, and status pass | Red owned gate or dirty/untracked evidence state | Stop; no acceptance |

6. Budgets and reserve

- Production-source delta: `0` formatted LOC.
- Test delta forecast: `53` formatted LOC.
- Final test forecast: `67` LOC from the 14-line baseline.
- Test hard cap: `75` LOC.
- Unused test reserve: `22` literal lines below the hard cap.
- Dependencies / unsafe / production allocations: `0 / 0 / 0`.
- Dense-ID layout: exactly 4 bytes / 4 alignment, measured on the host.
- Stop and re-card if the forecast grows by more than 20% or 25 lines, a public item appears, or the 22-line reserve would be consumed.

7. Questions requiring parent authority

The card reserves parent authority for:

- A permanent wire-format choice.
- Approval of any dependency, unsafe code, or SIMD.
- A second materially different observable terminal.
- Any semantic card change or blocker/major ambiguity, which invalidates and restarts calibration.
- Any request to broaden into compiler dispatch, C0-COMPILER, C1, or another adjacent capability.

Discoverable manager-owned details—not parent decisions—include exact compiler invocation, toolchain, resolved rlib path, raw stderr, source digest, formatted skeleton, and command results. The cold reader must not infer or supply those from repository inspection.

Ambiguities/permission gaps flagged:

- The card names the expected unchanged public vocabulary but does not enumerate its exact existing exports; confirming those would require prohibited source inspection and therefore remains manager-owned evidence.
- The exact compiler-process command and toolchain are deferred to manager evidence, so this calibration cannot independently validate command reproducibility.
- “Ordinary public consumer” and “actual exported artifact” are clear in intent but the card does not specify the exact fixture invocation mechanics; the manager must resolve that without adding a test crate or dependency.
- The distinction between the 22-line reserve and the “stop if reserve would be consumed” rule should be treated literally: reserve is capacity that must remain unused, not available implementation budget.

This is calibration evidence only; it is not implementation acceptance.
