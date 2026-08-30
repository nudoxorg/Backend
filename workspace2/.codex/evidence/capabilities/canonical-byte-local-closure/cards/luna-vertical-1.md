# Frozen Luna card: borrowed root to bound operation vertical proof

Registered role: `nudox_luna_implementer`; expected runtime `gpt-5.6-luna` / `max`; config `.codex/agents/nudox-luna-implementer.toml`.

Baseline: `b062d77e9805d8be1a4ae85f7efb4b28402e700d`, tree `85881e08979c0d54911f605a8dbfa2f08932cc33`. Allowed paths: `crates/nudox-root/**`, `crates/nudox-hydration/**`, `crates/nudox-operation/**`, and this capability evidence directory only. Never edit the chief test/brief, object-pack, store-memory, compiler/index/durability/controller paths, or shared roadmap/skills.

## One terminal

Make the frozen chief explicit-red journey compile and pass using: caller canonical root bytes -> `ValidatedRoot` borrowed witness -> `BorrowedGenerationView` paired to existing validated locality -> `Need::bind_borrowed` / `plan_borrowed` -> existing sealed full-store verification -> `LocalObjectProvider::bind_verified` returning a bound no-argument fused run. Successful verified pack/store body transfer remains the existing borrowed `pack_bytes` control; do not redesign it.

## Assigned matrix rows

CBC-01 through CBC-07 vertically, with the smallest owning-crate exact fault tests necessary to prevent raw/root/locality/bind regressions. CBC-08/09 comprehensive campaigns remain red for the next card, but this card must expose measurement hooks/oracles sufficient for that card; do not claim them closed.

## Fixed authority and bounds

- Root wire grammar has one owner; validate its header/rows once into a private coherent borrowed witness. Typed-cast `RootWireRecord` so nested `SchemaId` validity is established once; validate every observed `descriptor.content[0]` with `ContentAuthority<DomainTag>`, retaining one authority only after all rows agree. Privately project `ObjectRef` by binding each typed descriptor’s observed 31-byte payload through that authority. Do not call `ContentId::from_digest`, fallible `ObjectRef::try_from`, or any duplicate object grammar during trusted traversal. Empty and populated validated roots use closed state, never `Option` fallback/unwrap. Exact error priority/source/operands, pointer containment, canonical generation identity, and no trusted reparse/panic/fallback/omission are required.
- Pair the borrowed root and existing `ValidatedLocality` privately; root identity/count mismatch rejects before selection.
- Hydration’s borrowed bind/planner must preserve existing complete-versus-range semantics and sealed full-store verification.
- Sol authorized only moving existing `nudox-hydration` to operation production dependencies to name `VerifiedGeneration`. Add no other dependency, trait/generic adapter, witness relocation, unsafe, macro, SIMD, allocator policy, `Box`/`Arc`/`dyn`, body copy, or refcount. Sol also authorized exactly one fallible `Vec<u32>`-equivalent transient lane during `ValidatedRoot::try_from`, capped at four bytes per declared row, only to prove linear arbitrary-parent hierarchy validation. It must retain `TryReserveError` as source, be dropped before return, and never appear in a warmed path or returned witness.
- `VerifiedGeneration` is reusable. Associated `LocalObjectProvider::bind_verified` returns a distinct bound-provider type whose inherent `start()` takes no request; it contains already-checked generation/object/locality only and its run never compares generation/object again. Keep existing `LocalObjectProvider` plus `Provider::start(request)` behavior unchanged for compatibility.
- No native row arena may be retained by the new borrowed terminal witness.

## Explicit later-resource/fault falsifiers to preserve

Borrowed witness/view construction and warmed plan/bind must allocate zero; the witness owns zero root-row/backing bytes; admitted body copy count is zero; every required stored pointer equals its freshly selected verified pack-body pointer. The sole permitted ingress allocation is the explicitly measured `u32` lane. One- and 100,000-row controls must emit retained backing, scratch high-water, exact passes/row reads/lookups/branches/batches and compare the constant-memory adversarial-chain control. Any hidden allocation/copy, missing/extra required row, non-selected pointer, retained lane, or non-linear chain work fails. CBC-09 must name exact typed errors for malformed root, locality mismatch, malformed/substituted pack, partial/missing store, and stale bind; input-removal and constant-body mutants must fail.

## Required focused commands

```sh
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
cargo test -p nudox-root --locked --offline
cargo test -p nudox-hydration --locked --offline
cargo test -p nudox-operation --locked --offline
cargo fmt --check
```

## Commit and return

Commit the coherent proof checkpoint. Return exact commit, changed paths, pre-fix red and post-fix commands, matrix rows addressed, new public-item/dependency ledger, strongest self-counterexample, retained/deleted mechanisms, and smallest still-red row. Do not call the capability complete.
