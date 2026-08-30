# Separate Terra pre-edit hostile review — raw return and clarification

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_preedit_hostile_review`
Model: `gpt-5.6-terra`
Spawn contract: `fork_turns: none`; read-only pre-edit review.

Custody verified: frozen card SHA-256 is `ffce71ee4fd8d00f1ff5d1bd4d12397bc8938136a48c1ab0e6434bc48f3c86e8`.

`src/lib.rs` is clean, 34 LOC, and exactly `247023…e5a78`; baseline test is clean, 14 LOC, and exactly `7ca6d4…fdc43`. Skeleton is clean, 94 LOC, and exactly `e1138c…d7b7d7`. This yields 80 added lines, 110 hard cap, and 16-line reserve. Relevant manifests are unchanged.

### review-rust-gem tripwire table

| Tripwire | Location | Read-only finding | Status |
|---|---:|---|---|
| Source custody / 0 production delta | `src/lib.rs`, 34 LOC | Exact pinned digest; `EntityId`/`TypeId` are aliases over private phantom brands; no conversion API added. | **UNVERIFIED** |
| Baseline-test custody | `tests/coordinates.rs`, 14 LOC | Exact pinned baseline digest. | **UNVERIFIED** |
| Skeleton custody / budget | skeleton, 94 LOC | Exact skeleton digest; 80-delta / 110-cap / 16-reserve arithmetic holds. | **UNVERIFIED** |
| Literal layout law | skeleton 17–22 | Six literal `4` assertions cover size and alignment for `DenseId<Entity>`, `EntityId`, and `TypeId`. | **UNVERIFIED** |
| Direct typed-use proof | skeleton 10–11, 67–75, 78–90 | Uses actual public `nudox_ir_vocab` rlib via `--extern`; mismatch is an `EntityId` argument where `TypeId` is required. Not a local/shadow type proof. | **UNVERIFIED** |
| Exact-one artifact causality | skeleton 25–39, 79–82 | Zero/multiple matching rlibs yield coded `io::Error`; selected path is passed directly to `--extern`. | **UNVERIFIED** |
| Target-directory linkage | skeleton 78–82 | Dependency directory comes from the running test executable’s parent, consistent with Cargo’s supplied target directory under the stated gate. | **UNVERIFIED** |
| Diagnostic predicate | skeleton 61–69, 83–89 | Rejects only failed compile with exactly one `error[E`, an `E0308`, and source-visible `EntityId`/`TypeId`; retained stderr is inspected, not discarded. | **UNVERIFIED** |
| Causal legal mutant | skeleton 11, 84–89 | `MISMATCH → LEGAL` changes only `EntityId::new(7)` to `TypeId::new(7)` in the call argument; legal compile must succeed and fail the mismatch predicate. | **UNVERIFIED** |
| Error propagation / cleanup | skeleton 42–59, 78–90 | All operational failures use `io::Result`; stdin is explicitly dropped, child is waited, and `--emit=metadata=-` avoids artifact cleanup. No `unwrap`, `expect`, `panic!`, or `unreachable!`. Assertions remain terminal predicates. | **UNVERIFIED** |
| Raw-rebrand scope | card + `src/lib.rs` 15–31 | Public `raw` and `new(u32)` permit deliberate copied-raw rebranding; card correctly limits the claim to direct typed use and does not claim prevention. | **UNVERIFIED** |
| Boundary / manifests | relevant manifests and listed paths | No manifest, compiler, or production-path diff in inspected boundary. | **UNVERIFIED** |

Strongest counterexample: this proves only a direct typed call-site mismatch. A consumer can still construct `TypeId::new(entity.raw)` (or copy the public raw value) and intentionally rebrand it. That is an existing, correctly scoped limitation—not a new conversion—but it prevents any claim of provenance, wire-decode, or cross-domain identity safety.

Hidden cost: the fixture invokes bare `rustc` resolved from `PATH`; a divergent PATH toolchain could make the process proof about a different compiler than Cargo’s test toolchain. The stated Cargo gate substantially constrains the artifact directory, but it does not itself pin the runtime `rustc` executable.

simple-std: Uses only `std` process/filesystem/path/io facilities; no dependency, allocation-policy, unsafe, or manifest expansion is visible in the inspected material.

**Strict pre-edit authority verdict: UNVERIFIED / no implementation acceptance.** The frozen custody, narrow boundary, and skeleton’s stated mechanisms are internally consistent on read-only inspection, but no execution or evidence review was performed; builder authority must remain contingent on the full calibration deck and later retained command/evidence checks.

## Narrow authority clarification

**APPROVE.**

A non-inheriting Luna builder may copy the exact 94-line frozen skeleton verbatim into the sole writable `tests/coordinates.rs`, with `src/lib.rs` remaining byte-identical.

The bare-`rustc` PATH point does not make this bounded checkpoint unsound: a divergent compiler can only make the fixture fail or leave the terminal unproven; it does not authorize a production change or create a false implementation acceptance at this pre-evidence stage.

Later runtime evidence must retain:

- the exact Cargo/toolchain identity and resolved `rustc` command;
- source SHA-256 matching `247023…e5a78`;
- exact resolved public rlib path and SHA-256, after the exact-one candidate check;
- literal mismatch and legal-mutant inputs;
- raw stderr for the rejected mismatch;
- legal-mutant command/result showing success and false diagnostic predicate;
- required Cargo gate outputs and clean boundary status.

This approval is only for the verbatim one-test-file checkpoint, not runtime-proof acceptance or any repair/expansion.
