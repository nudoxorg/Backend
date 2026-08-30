# Fresh Luna cold reader two — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_cold_reader_two`
Requested model: `gpt-5.6-luna`
Fork mode: `none`
Role: read-only independent reader

Cold contract calibration report (read-only; no source/code/history inspection performed)

1. First observable capability and terminal

- Capability: preserve the existing inward IR vocabulary where `DenseId<Entity>` and `DenseId<Type>` remain distinct four-byte, four-alignment coordinates with private phantom brands.
- Terminal: an ordinary public consumer imports the actual exported `nudox_ir_vocab` artifact and the compiler rejects exactly one `EntityId` supplied where `TypeId` is required.
- This is C0-IR only. It does not authorize compiler dispatch, fragments, frontends, registries, builders, recipes, drivers, schedulers, sandboxes, bundles, publication, or C1.

2. Exact allowed paths and named baseline

- Baseline: source candidate `44c22154fd5238e4769562590420371979306050`; active compiler paths are restored by `ebb81a9f` and are not editable.
- Writable paths exactly:
  - `domains/ir/crates/nudox-ir-vocab/src/lib.rs`
  - `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`
- Baseline formatted LOC/digests:
  - `src/lib.rs`: 34 LOC, SHA-256 `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78`
  - `tests/coordinates.rs`: 14 LOC, SHA-256 `7ca6d45b5fbeb9b0961d5726eea0249e574eb3f76d6b3d9f37fdc17e2aefdc43`
- Manager-only artifacts are the card, `P5_C0_CALIBRATION_RAW.md`, and `evidence/p5-c0/ir/**`.

3. Preserved facts and prohibited adjacent behavior

Preserve:

- `DenseId<Owner>::new(u32)` as the sole local dense-coordinate construction.
- Private brands distinguishing `EntityId` and `TypeId`.
- Four-byte size and four-byte alignment.
- Current public semantics and dependency-free vocabulary.

Forbidden:

- Raw wire decoding, identity rebranding, authority conversion, phase transitions, later-phase values, or compiler edits.
- `From`/`TryFrom`, raw authority bytes, public traits/getters, public tuple fields, phase bits, runtime type tags.
- `dyn`, `Box`, `Vec`, `Arc`, `Rc`, `String`, serde, async, unsafe, SIMD, macros, allocation, fallback/default state.
- Any manifest, lockfile, workspace-root, dependency, shared-crate, central-registry, conversion-API, or unlisted-path change.
- Panic-style test mechanisms (`panic!`, `unwrap`, `expect`, `unreachable!`), source-dropping conversions, or discarded cleanup.

4. Expected public surface and explicitly forbidden surface

Expected public surface:

- The existing vocabulary only: `DenseId`, `EntityId`, `TypeId`, and their already-established construction/typing behavior.
- Public downstream use through an ordinary `tests/coordinates.rs` consumer.
- Exactly one compile-fail mismatch expression and a legal `TypeId` mutant used to falsify the diagnostic predicate.

Explicitly forbidden:

- Any new public item, public field, getter, trait, conversion, registry, compiler API, phase/type marker, test-only workspace member, fixture crate, or dependency.
- Shadow/local replacement types; doctest-only or runtime-only proofs.
- More than one compiler error or any unrelated fixture error.
- A constant-result/routing-only proof that does not forward/use the actual public artifact and mismatch.

5. Evidence row | falsifier | hard cap | stop trigger

| Evidence row | Falsifier | Hard cap | Stop trigger |
| --- | --- | --- | --- |
| Exact layout | `coordinates.rs` asserts size/alignment for `DenseId<Entity>`, `EntityId`, and `TypeId` | Any coordinate not 4 bytes/4 alignment | Representation/layout change |
| Static kind impossibility | Compiler-process fixture imports actual public artifact, yields exactly one `E0308` mentioning both `EntityId` and `TypeId`; legal `TypeId` mutant makes predicate fail | Shadow types, extra errors, doctest/runtime-only proof, noncausal mutant, or missing stderr | Compiler cannot reject exact mismatch causally |
| Public boundary | Ordinary top-level consumer exercises exported crate; no production conversion/compiler dependency | Compiler path/API edit, conversion, manifest edit, or unlisted path | Any boundary expansion |
| Test hygiene | No panic helpers, discarded cleanup, source-dropping conversion, or hidden success conversion | Any prohibited test mechanism | Any such construct appears |
| Budget | Production delta 0; test forecast 53 added formatted LOC | Test file exceeds 75 total LOC or consumes its 22-line reserve | Forecast grows >20% or by 25 lines, reserve exhausted, or public item appears |

6. Budgets and reserve

- Net production-source delta: 0 formatted LOC.
- Net test delta forecast: 53 formatted LOC.
- Test hard cap: 75 formatted LOC, leaving 22 literal unused lines.
- Dependencies / unsafe / production allocations: `0 / 0 / 0`.
- Dense-ID layout: exactly 4 bytes / 4 alignment, measured on host only.
- Required gates include format check, locked workspace tests/all targets, locked clippy with `-D warnings`, `git diff --check`, and clean status.
- Manager must retain toolchain, compiler-process command, resolved public rlib path, source digest, rlib path, stderr, and legal-mutant command/result under the named evidence paths.

7. Questions requiring parent authority

No product-semantic fork is exposed by the card. The intended decision is to close only C0-IR.

Permission/operational gaps to resolve manager-side before edit authority:

- The compiler-process fixture needs temporary build inputs and retained stderr, but only two repository paths are writable; manager must explicitly keep temporary material outside those paths and place only authorized evidence under `evidence/p5-c0/ir/**`.
- “Actual public artifact” must be resolved to the built `nudox_ir_vocab` artifact, never a local shadow type or doctest.
- The card requires two Luna cold readers, one Luna plausible-misreader, and a separate Terra reviewer with explicit model proof; this report is only one independent Luna reader and grants no implementation acceptance.
- Any semantic card change, blocker, or major ambiguity invalidates this calibration and requires a fresh complete cold deck.
