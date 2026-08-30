# Canonical-byte local closure — Phase 0 brief

## Public terminal

Caller-owned canonical root bytes validate into a borrowed root witness. The witness pairs with a canonical locality artifact, proves complete closure during hydration, verifies selected canonical object-pack bodies, transfers those bodies as their original borrowed owners into the bounded immutable memory store, and yields a non-forgeable verified generation that binds one pinned object operation once.

Chief journey: `crates/nudox-operation/tests/canonical_byte_local_closure.rs`.

Frozen red command:

```sh
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' \
  cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
```

Active implementation baseline: commit `b062d77e9805d8be1a4ae85f7efb4b28402e700d`, tree `85881e08979c0d54911f605a8dbfa2f08932cc33`; shared ancestor `f2565a9fb33af06053bd19721d4dc2753ec09ed5`. The earlier `ef72…` packet and its first pre-edit review are retained only as stale history after Sol’s pointer-oracle authority fork.

## Laws and negative space

1. `nudox-root` is the sole owner of canonical root grammar, byte validation, validation-error priority, borrowed pointer containment, and root/locality identity pairing.
2. A borrowed root plus valid locality can make a complete `GenerationView`, but a partial or mismatched locality cannot acquire complete-closure authority.
3. `nudox-hydration` owns demand binding, dependency-set identity, and the complete-closure proof; trusted planning must not reparse root/locality bytes or panic/fallback/omit a descriptor.
4. `nudox-object-pack` remains the current read-only owner of selected directory/body verification; `nudox-store-memory` remains the read-only owner of bounded immutable admission and rejected ownership. The new flow must keep each admitted body borrowed from pack backing without a body copy or per-object owner allocation.
5. Only successful full store-presence verification yields `VerifiedGeneration`; it remains non-forgeable and a stale root fails at the single operation binding boundary.
6. `nudox-operation` owns one identity-check boundary for each pinned operation bind and fused operation progress. `VerifiedGeneration` is reusable authority, not a consumable token; once a bind succeeds, provider/request generation or object equality is not checked again.

Forbidden: retained native row arena in the borrowed journey; raw descriptor/schema reconstruction after validation; panic/fallback/omission; malformed/truncated root or pack reaching admission; partial projection or missing store object yielding `VerifiedGeneration`; body copies, per-object allocation, refcounting, dynamic dispatch, test-only crates, async/runtime/transport/publication/cache work, unsafe, a dependency, or SIMD. No compiler, index, durability, controller, object-pack, or store-memory production path is writable in this capability.

Sol exception `98e4e56e`: move the already-existing internal `nudox-hydration` test edge into `nudox-operation` production dependencies only so `LocalObjectProvider::bind_verified` can consume the sealed `VerifiedGeneration` authority. It authorizes no external dependency, trait/generic adapter, duplicated/relocated witness, or broader coupling.

Sol exception `b062d77e`: `ValidatedRoot::try_from` may reserve one fallible transient `u32` lane, bounded to four bytes per declared row, for linear arbitrary-parent hierarchy validation. The lane must be released before the borrowed witness returns, preserve `TryReserveError` as its source, and be measured against a constant-memory control at one and 100,000 rows including an adversarial chain. It does not authorize a retained lane, second row/descriptor arena, unbounded growth, or any allocation in warmed view/planning/binding.

Sol object-projection decision: `nudox-object` remains read-only. Root ingress uses its existing public typed descriptor record and the existing `nudox-id::ContentAuthority` bridge: typed-cast the nested `SchemaId` once, check every observed `descriptor.content[0]` against the requested domain, retain authority only after all rows agree, then privately project `ObjectRef` from the typed fields by binding the observed 31-byte payload. `ContentId::from_digest`, a second fallible descriptor conversion, and root-owned duplicate descriptor validation are forbidden. Empty and populated validated roots must use closed state, not an `Option` fallback or an unreachable unwrap.

## Path and consumer custody

Terra-owned editable production/test paths are limited to `crates/nudox-root/`, `crates/nudox-hydration/`, and `crates/nudox-operation/`, plus this capability evidence directory. `crates/nudox-object-pack/` and `crates/nudox-store-memory/` are read-only prerequisite controls. The chief-owned red journey and chief evidence directory are read-only: no weakening, removal, or API correction without an `AUTHORITY_FORK`.

Current dependency direction is `root -> hydration -> operation`; operation also consumes object and store values through the chief integration. Root owns canonical IDs and locality pairing; hydration owns complete proof; operation owns the terminal bind. Existing pack `SelectedObject::verify` and memory `MemoryStore::insert_owned` are prerequisite controls, not redesign targets.

## Evidence mapping

`TESTING.md` digest and every applicable clause are mapped in `testing.md`. `proof-matrix.md` is the sole status record. `coupling-skeleton.md` names each invariant/control boundary; `red-falsifiers.md` retains exact failure classes; `resource-controls.md` fixes measurements before implementation. The refreshed chief contract additionally requires owning-crate and ordinary top-level fault journeys for malformed root, mismatched locality, malformed/substituted pack bodies, partial/missing store, and stale bind; it requires isolated one- and 100,000-row owner/backing/peak/scratch/allocation/copy/work evidence.

## Product-authority questions

None at Phase 0. Any necessary public-terminal correction, new dependency, unsafe, allocator policy, or SIMD requires an authority fork.
