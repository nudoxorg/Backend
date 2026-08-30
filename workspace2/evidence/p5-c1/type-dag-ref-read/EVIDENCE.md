# P5 C1 recursive type-reference read evidence

Status: DIRECT SOL VERIFIED; INDEPENDENT CALIBRATION/REVIEW EVIDENCE_BLOCKED.

## Custody and decomposition

- Active card: `P5_C1_TYPE_DAG_REF_READ_CARD.md` at pre-code digest
  `23c1e6e00bc6b0788677fb584444ff3b6613b0319e68b0a23e5b5848d4aefaad`, then generated-custody
  cap amendment `4a802ecb`.
- First required fresh role: `task_name="type_dag_cold_reader_a"`, `model="gpt-5.6-luna"`,
  `fork_turns="none"`; literal failure: `collab spawn failed: agent thread limit reached`.
- Rejected combined writer/reader: 389 production lines at `41fbb3ff`, removed at `cda6a396`.
- Rejected product reader: 278 production lines at `f9742596`, removed at `78bb3b83`.
- Accepted reference-reader source: `70f60971`; causal consumer/mutants: `9e713da9`.
- Production SHA-256: `9e3ccfabb4457a0647d3524ad73a9fce54098d99240c65fc6c0412c36171682a`.
- Test SHA-256: `11c55ce1e4e5b7f84ff32d6d83fa21c7ca40d11b93dc65280e0cf398f594f59b`.

## Contract evidence

| law | direct result |
| --- | --- |
| retained fragment | all existing fragment/prepared goldens passed; no `0xc1` source body changed |
| recursive wire | empty, Bool, I32, self, forward, and backward reference goldens passed |
| validation priority | every header prefix/cell, geometry, scratch, node prefix/tag/code/edge, and trailer asserted exact operands |
| atomic scratch | every rejection compared the complete two-entry sentinel scratch unchanged |
| one proof | validator stages at most two typed nodes, then copies the complete 0/8/16-byte proof into caller scratch |
| cursor | exact length, complete order, repeated fused `None`; source slice contains only typed `split_first` |
| containment | input borrow pointer equals caller bytes; each node reference lies within caller typed scratch |
| static kind | actual exported format+vocab rlibs emitted one E0308 (`DenseId<Type>` vs `DenseId<Entity>`); one-token `TypeId` source compiled |
| host layout | Rust 1.97.1 AArch64: `TypeNode` 8/4, view 32/8, cursor 16/8; other hosts UNVERIFIED |
| caps | production 218/240, lib delta 7/20, tests 255/260, rlib-test delta 22/50 |

Two distinct fresh-target gates each passed formatting, all 20 workspace tests, warnings-denied
Clippy, `git diff --check`, and an empty `git status --short`.

The constant-reference patch SHA-256 is
`8de67996a3d57b75898efec4fc2c0590b4865d2bfdd43a448f40ab5d2504ba4e`; it made the forward reference
decode as `TypeId(0)` instead of `TypeId(1)` and failed status 101. The input-removal patch replaces
the caller body with `[1, 0]`; the empty golden failed with exact `NodeBytes { expected: 0, actual: 2 }`
and status 101. Pristine source was restored before custody.

## Full codegen custody

Toolchain: rustc 1.97.1, LLVM 22.1.6, `aarch64-apple-darwin`. All artifacts use deterministic
`gzip -n -9`; replay reproduced each raw hash.

| artifact | raw bytes / SHA-256 | gzip bytes / SHA-256 |
| --- | --- | --- |
| consumer LLVM | 299,388 / `ecf9f20c6ce94cd61a9fae1e7b434b3550537737c70f0041bc8c8a98d0576426` | 37,268 / `ace5b3e2be417ebd0708febd743eeb30829507158f355b79ce01bf645dd800b1` |
| consumer asm | 106,784 / `44aae0ae1e5565864b03b19dcd6a9c7af3399947d816a2f78870b8a8db766c4e` | 19,645 / `2c0f09def127900373274be5bd03a81985d9e5b68717d391e99cb4dd3047700f` |
| owner LLVM | 34,438 / `89ef427d524dd6da62c280d64096223b009274efc13748bfd81bf74b51baaf5f` | 5,449 / `b8fc12019d3f2911967d5a1faeb660194c7ff646e604e926eaa037b221a5d17e` |
| owner asm | 10,362 / `7922460bc014dd0fbe83fe1a1dfdac84ad4a66c19108a04e43313c3bed0ef2fa` | 2,542 / `3167eb529d1397e8b632f3a7e1454aeb7b55fa38666e2b4f14eb55c19647998a` |

Compressed total is 64,904 bytes, leaving 632 bytes below the frozen 65,536-byte cap.

| callable | allocator | copy/staging | panic/unwind | calls/indirection | input evidence |
| --- | --- | --- | --- | --- | --- |
| `recursive_consumer`, LLVM 634–713 / asm 554–626 | none | 40-byte result slot only; no payload memcpy | personality only, no panic call | direct validate at LLVM 639/asm 572; no indirect/`blr` | consumes typed scratch discriminants and reference raw word at LLVM 687–709 |
| `TypeDagView::validate`, LLVM 19–359 / asm 10–270 | none | fixed 16-byte stack proof and exact 0/8/16-byte memcpy to caller at LLVM 127/asm 124 | two residual bounds-panic calls at LLVM 169/218/255 converge to asm 259/266 | no indirect/`blr` | direct header/body byte loads throughout LLVM 26–323 |

This proves no owned allocation and one bounded proof copy; it does not prove zero cost or panic
freedom. The apparent `MAX_TYPE_DAG_NODES as usize` tripwire was tested with `usize::from` and rejected
by rustc because array lengths still require a const-stable conversion; widening `u8` to `usize` is
lossless on every Rust target and remains the smallest stable expression.

## Direct hostile tripwire table

| tripwire | count and exact location | disposition |
| --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 in `src/type_dag.rs` and `tests/type_dag.rs` | clear |
| source-dropping conversion/map_err | 0 in changed code | clear |
| lossy From/TryFrom/raw bypass | 0; one lossless const widening at `src/type_dag.rs:111` | compiler-constrained, cleared above |
| sentinel/saturation/operand loss | 0 | clear |
| dyn/Box/Vec/Arc/Rc | 0 | clear |
| public tuple/positional semantic fields | 0 | clear |
| unit/stateless namespace structs | 0 | clear |
| public local traits/delegation | 0 | clear |
| one-letter generics | 0 | clear |
| numeric tags/bounds | 0/1 wire tags and bound 2 are owned by `src/type_dag.rs:3-6` | required wire authority |
| test-only Option/discarded results | iterator `Option` only; no discarded result in new test | required iterator contract |
| unsafe/SIMD/allocator/dependency | 0 | clear |
| public item without consumer/falsifier | 0 | every item appears in public runtime or actual-rlib proof |

Strongest counterexample: a legal two-node forward/back cycle validates and returns both exact typed
coordinates, while changing only the first target to 2 returns the first source-bearing edge error and
leaves scratch unchanged. Independent Terra review, Miri, fuzz, other targets, product integration,
ordered products, and emission remain UNVERIFIED.
