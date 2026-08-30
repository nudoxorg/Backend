# Rejected builder checkpoint and post-build hostile review

Builder task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_builder`
Builder model: `gpt-5.6-luna`
Spawn contract: `fork_turns: none`; only the ordinary test path writable.

## Builder raw return

Implemented and committed the exact frozen skeleton.

- Commit: `48044df980170ca57b68a9095124f1f2c8f61ffa`
- Modified/staged path only: `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`
- Test: 94 LOC, +80 net lines, 16-line reserve
- Test/skeleton SHA-256: `e1138c221def6262d64f8ce168562394202ac9043bf70227743361b97f9d7b7d`
- Source SHA-256 unchanged: `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78`
- Artifact count: 1
- Rlib: `domains/ir/target/debug/deps/libnudox_ir_vocab-85e88fc5cb4d75d9.rlib`
- Rlib SHA-256: `bcee0f6532c79b9a0cc2ee40e2fdb1137101ff6bfcaa30575a36c45b9d4902e3`
- Clean rebuild, workspace tests, format check, and diff check: pass
- Tests: 2 passed
- Clippy: fails solely on frozen skeleton’s nested `if` (`clippy::collapsible_if`, lines 32–38). Fixing would violate byte-for-byte custody.
- Final worktree: clean.

## Post-build Terra hostile review raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_postbuild_hostile_review`
Model: `gpt-5.6-terra`
Spawn contract: `fork_turns: none`; read-only after material implementation.

| Tripwire | Exact location / count | Disposition |
|---|---|---|
| Checkpoint custody | HEAD `48044df980170ca57b68a9095124f1f2c8f61ffa`; clean status and `git diff --check` | PASS |
| Builder write boundary | Checkpoint changes only `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`; source diff from `44c221…` is empty | PASS |
| Source pin / production scope | `lib.rs:1`, 34 LOC and SHA-256 `247023…5a78`; no new conversion/API | PASS |
| Skeleton / reserve | `coordinates.rs` is byte-identical to frozen skeleton, SHA-256 `e1138c…7b7d`; 94 LOC vs 14 baseline = +80, leaving 16 below 110 | PASS |
| Literal layout | `coordinates.rs:18-24` assert literal size/alignment `4` for all three coordinates | PASS |
| Actual rlib causality | `coordinates.rs:27-62` requires exactly one rlib and passes its explicit path through `--extern`; current executable parent contained exactly `libnudox_ir_vocab-85e88fc5cb4d75d9.rlib` (SHA-256 `bcee…902e`). Focused existing test passed. Fresh clean-build causality remains UNVERIFIED. | PASS / UNVERIFIED fresh rebuild |
| Target binding | `coordinates.rs:83-88` resolves the running test executable’s parent, which was `domains/ir/target/debug/deps`, the same directory holding the sole rlib | PASS |
| Forbidden / legal fixtures | Mismatch at line 13 has one exact `EntityId::new(7)` misuse; legal mutant at line 14 changes only that occurrence to `TypeId::new(7)`. Predicate at lines 72–79 counts `error[E` exactly once and requires E0308, EntityId, TypeId; focused test passed. | PASS |
| Raw-rebrand scope | `lib.rs:20` retains baseline public `raw`; no new raw conversion was added | PASS (known limitation retained) |
| Standard-library-only surface | Test imports only `core`/`std`; manifest has no dependencies | PASS |
| `clippy::collapsible_if` | `coordinates.rs:32-37` is the canonical nested-if shape reported by the builder. With workspace `-D warnings`, it is a blocking lint if that report is retained. | REPAIR GATE / diagnostic rerun UNVERIFIED |

Smallest falsifier-bound repair, only if authorized: in `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs:32-38`, fold `resolved.replace(entry.path()).is_some()` into the outer filename predicate and remove the inner `if`. It preserves short-circuiting and all fixture semantics, but changes the frozen 94-line digest; the manager must treat that custody deviation explicitly rather than silently accepting it.

Strongest preserved counterexample: `TypeId::new(EntityId::new(7).raw)` deliberately rebrands a copied public raw value. This is baseline scope, not a failure of direct typed-use rejection.

Hidden cost: the proof depends on an existing unique rlib and on `rustc` from PATH; it captures stderr but does not itself retain it. Fresh clean preparation, full fmt/test/clippy gates, retained stderr/evidence, and source-to-new-rlib causality are all UNVERIFIED.

Strict post-build verdict: **no acceptance**. The source/test boundary and focused artifact proof hold, but the reported Clippy blocker requires the one narrow repair (and explicit skeleton-custody handling) before the terminal can close.
