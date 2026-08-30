# Borrowed hydration planning checkpoint

State: implemented and reproduced by the manager; this is neither an independent review nor a capability-closure claim.

## Authority and custody

- Frozen implementation baseline: `9d9738a1ae7ded73f8e9a532deb4515f88eb4f9f`, tree `9fed8113fb07b807a8296fe16f9c3dc0cf23329e`.
- Initial integrated hydration checkpoint: `fa24a3b4c1e620736b7a158f18571c8f4ce8eb65`, tree `c867e4eb59e7b3a5b4fd5a0b3671994c1cf8f1ee`.
- Final hydration checkpoint: `4fdbe439b0d1f29ed5238d1a31cc505b47d5f327`, tree `a21ee93625d9be94243c4e47c6451a969d4103ec`. This narrow amendment wraps the pre-existing `DemandBindError` as `PlanError::Demand(#[from] DemandBindError)` so the frozen chief's existing `From<PlanError>` remains its only conversion boundary.
- Worker task: `/root/canonical_byte_local_closure_terra/luna_hydration_checkpoint_1`; registered role `nudox_luna_implementer`; direct outer collaboration dispatch with `fork_turns="none"`; model `gpt-5.6-luna`; effort `max`; config `.codex/agents/nudox-luna-implementer.toml`.
- Worker checkout: `/Users/mileswirht/.config/codex/worktrees/ab6a/backend`. The outer runtime supplied no effective-sandbox receipt. The worker made the six expected hydration production writes and owning test writes, but did not return a gate, blocker, or commit receipt after bounded requests, so it was interrupted. Terra inspected, repaired the two test-only faults and clippy failures, reproduced the gates, and made the integrated commit.
- No source-isolated review was attempted or claimed. The required later review remains governed by `review-custody.md`.

## Implemented public surface and retained mechanism

`Need::bind_borrowed` compares the pinned generation against the already paired `BorrowedGenerationView` exactly once and preserves `DemandBindError::GenerationMismatch { requested, actual }` as the typed source in `PlanError::Demand`. `plan_borrowed` consumes `BorrowedGenerationView::select_closure` once to derive the dependency-set preimage and classify complete/range coverage. It stores only absent selected ordinals in caller-owned `PlanScratch`; re-iterating the validated selection provides required/present/promised/missing/fetch views without re-running the caller predicate or parsing root bytes.

The existing owned planner now uses the same hydration-owned ordinal representation, preserving its public plan surface and retained-byte facts. `BorrowedHydrationPlanView::stage` produces a distinct borrowed staging wrapper whose verification retains the existing partial-projection-first then canonical-first-missing-object error priority and returns the existing sealed `VerifiedGeneration`.

No new dependency, manifest, root, operation, chief journey, object/object-pack/store, `Box`, `Arc`, `dyn`, unsafe, SIMD, allocator policy, or raw root reconstruction was added. The only `Vec<u32>` is the existing caller-owned `PlanScratch` ordinal buffer; `PlanScratch::new` preserves `TryReserveError` as its source and `plan_borrowed` clears/reuses it without allocating.

## Focused faults and facts reproduced

- Complete borrowed selection preserved canonical required order and exact present/promised/missing routes; its dependency-set identity and coverage match the owned planner.
- Range planning preserves closure-parent inclusion, cannot verify a partial projection, and reports `VerificationError::PartialProjection { pinned_root }`.
- Complete planning with an absent object reports the first canonical missing descriptor through `VerificationError::MissingObject { pinned_root, object }`.
- A mismatched need reports `PlanError::Demand(DemandBindError::GenerationMismatch { requested, actual })`, preserving exact source and operands. Undersized closure scratch retains root's `ClosureError::ScratchTooSmall { required, available }`; undersized plan scratch retains `PlanError::ScratchTooSmall { required, available }` and does not raise its high-water count.
- The three-row fault fixture reports two absent ordinal slots: `sparse_state_bytes = 8` and caller-owned retained ordinal capacity `12` bytes. This is a focused ownership/scratch fact only; no allocation instrumentation, 1/100,000-row resource proof, pointer/pack/store transfer proof, or mutation campaign is claimed here.

## Raw gates

The ambient focused `cargo test` first failed before Rust compilation because the configured `sccache` wrapper could not find `clang` for `blake3`. The Nix quality shell provides the pinned compiler and was used for all acceptance gates.

```text
nix develop .#quality --command cargo test -p nudox-hydration --locked --offline
13 unit tests passed; 1 trybuild compile-fail test passed; doc tests passed.

nix develop .#quality --command cargo clippy -p nudox-hydration --all-targets --locked --offline -- -D warnings
passed.

nix develop .#quality --command cargo fmt -p nudox-hydration -- --check
passed.
```

The unchanged chief configuration was deliberately run after the final hydration amendment and remains red (`exit 101`) only at its operation-owned terminal obligation:

```text
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' \
  nix develop .#quality --command cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline

E0599: `LocalObjectProvider::bind_verified` is absent at canonical_byte_local_closure.rs:163.
```

The strongest remaining counterexample is therefore a well-formed completed borrowed plan that cannot reach the public terminal: no operation bind boundary exists. This checkpoint does not authorize editing the chief test; the next operation-only card owns the permitted `nudox-operation` implementation.

## Remaining rows

CBC-03 is reproduced only for focused borrowed planning semantics and scratch facts. CBC-05 is reproduced only for the hydration verification error paths. CBC-04, CBC-06 through CBC-09, the full resource campaign, top-level named fault journeys, and the source-isolated hostile review remain open.
