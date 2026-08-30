# Frozen Luna card: verified borrowed operation binding

Registered role: `nudox_luna_implementer`; expected runtime `gpt-5.6-luna` / `max`; config `.codex/agents/nudox-luna-implementer.toml`.

Source baseline: hydration head `4fdbe439b0d1f29ed5238d1a31cc505b47d5f327`, tree `a21ee93625d9be94243c4e47c6451a969d4103ec`. The checkout may also contain the descendant evidence-only receipt commit `b0aec97d3c8f4a2375bd6e99619b4de6b2de7da2`; do not rewrite it. Allowed paths are **only** `crates/nudox-operation/**` and this capability evidence directory. Root, hydration, chief journey, object/object-pack/store, compiler/index/durability/controller, and shared documentation are read-only.

## One terminal

Promote the existing `nudox-hydration` dev dependency to the production dependency section solely to name its sealed `VerifiedGeneration`; no other manifest/dependency change is authorized. Add:

```rust
LocalObjectProvider::bind_verified(
    &BorrowedGenerationView,
    EntryKey,
    &VerifiedGeneration,
) -> Result<BoundLocalObjectProvider, LocalObjectError>
```

The bind boundary compares `verified.pinned_root` to `view.id` once, before selecting the key, and returns the existing stale-generation type with exact `expected = view.id` and `observed = verified.pinned_root`. A selected missing key receives an explicit typed `LocalObjectError` variant carrying the exact generation/key operands. The function must not manufacture/recheck a generation after this boundary.

`BoundLocalObjectProvider` is a distinct public bound type with inherent `start(&self)` taking no request. It contains only already-checked generation, selected object, and selected locality facts. Its run has no generation/object equality comparison or fallback. Preserve `LocalObjectProvider`, `Provider<PinnedObjectOperation>::start(request)`, `PinnedObjectRequest`, and their legacy semantics unchanged.

Resident and overlay selected rows must retain the legacy one batch, one terminal, then fused-finished cursor behavior and provenance. For a promised locality row, use the existing partial terminal only if that is still coherent with the sealed full-store-presence authority; write the explicit decision in the focused owning test. Do not invent a store handle, body owner, fetch, or a second verification to change locality semantics.

## Required focused tests

Use actual public root/hydration/operation values. Add owning operation tests for:

- stale verified root with exact `LocalObjectError::StaleGeneration` expected/observed operands, including stale-before-missing-key priority;
- a matching verified root with an absent key and exact typed missing-key generation/key operands;
- a reusable verified witness successfully binding two legal keys without consumption;
- resident and overlay batch provenance, promised behavior with its reason, single terminal then fused finish;
- a source-level/input-changing falsifier showing the bound runner consumes prechecked facts and does not call the legacy request comparison path after `bind_verified`.

Keep no `Box`, `Arc`, `dyn`, unsafe, macro, SIMD, raw canonical-root parsing, new generic adapter, new allocation policy, body copy, dependency besides the Sol-authorized hydration edge, or chief edit. Add no test-only crate.

## Gates and return

Run under the Nix quality shell:

```sh
cargo test -p nudox-operation --locked --offline
cargo clippy -p nudox-operation --all-targets --locked --offline -- -D warnings
cargo fmt -p nudox-operation -- --check
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
```

Commit one coherent operation-only checkpoint. Return exact task/config/model/effort/sandbox receipt, commit/tree, changed paths, raw gates, retained/rejected mechanism, strongest counterexample, and no review/closure claim.
