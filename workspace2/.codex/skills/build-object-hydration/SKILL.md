---
name: build-object-hydration
description: Scope rules for workspace2 immutable objects, roots, storage ownership, locality, partial hydration, and publication. Use with deliver-reviewed-rust-slice and audit-data-layout for nudox-object, root, store-memory, or hydration.
---

# Object and hydration scope

Read `../deliver-reviewed-rust-slice/SKILL.md` completely first. It is authoritative for idioms,
review, allocation, generics, concurrency, diagnostics, errors, and tests. This skill adds only object
plane scope. Use `../audit-data-layout/SKILL.md` before a numerous/hot representation change.

## Routing and ownership

Own `nudox-object`, `nudox-root`, `nudox-store-memory`, and `nudox-hydration`. Request pack/protocol
changes from foundation; keep I/O, cache, and platform machinery in adapters. Read
`PACKED_COLLECTIONS.md` for canonical-owner/range work and only the matching retained-owner section
of `LAYOUT_AUDIT.md`.

## Required packet additions

Draw `canonical bytes -> validated view -> selection scratch -> store owner -> consumer borrow` and
label every copy, move, allocation, lifetime extension, peak owner, and rejection. For roots/stores,
compare borrowed/caller-output, inline, arena/region/slab, mmap/lease, and exact heap shapes that
actually satisfy the lifetime.

## Object-plane invariants

- Identity is independent of locality, pack placement, cache tier, and promotion.
- Canonical root/locality bytes are primary. Owned construction and borrowed/mmap/leased reading are
  distinct adapters over one grammar; a retained native `Box<[Row]>` is a measured control, not the
  presumed semantic owner.
- Arbitrary-order construction may allocate to sort; range read/validation does not.
- Payload owners transfer or return unchanged. Store metadata policy is static and first-write-wins.
- Scratch is caller-owned/reusable where lifetimes align. Construction input is released before
  later hierarchy scratch whenever possible.
- Planning touches requested/changed rows and ancestors only. Sequential sparse traversal advances a
  cursor; random lookup performs bounded membership/rank work.
- Boundary tests prove pointer identity, zero/one/limit/+1, exact rejection ownership, malformed and
  collision cases, allocation failure sources, lifetime compile-fail, unchanged root identity across
  locality/tier movement, bounded work, and exact aggregate events.
- Verification and publication are distinct authorities only when publication consumes a real stable
  receipt. Prefer one `Generation<State>` representation when both states have consumers; otherwise
  keep one non-forgeable verified fact and delete effect-free marker transitions.

### Proof records and evidence owners

Plans, verified closures, leases, and publication receipts are authority records, not ordinary DTOs.
Attack them from downstream code before approval:

- mutate every public identity, projection, dependency set, bound, and state field after legitimate
  construction;
- mix facts from two valid roots, stores, snapshots, or leases;
- replace a range projection with complete-generation authority;
- use a constant-true presence predicate, then drop or substitute the alleged store before the
  consumer runs.

If any attack compiles, the proof boundary is open. Keep correlated construction private. Put
independently readable facts in one public fact record and expose them through immutable `Deref`
without `DerefMut`; add downstream compile-fail assignments, not merely a struct-literal failure.
When verification depends on physical presence, the result must retain the exact immutable store,
snapshot, or lease borrow and the consumer must use that retained owner. `PhantomData<&Store>`, a
copied store ID, and `FnMut(ObjectRef) -> bool` alone do not prove instance identity or continued
residence.

```rust
// DON'T: public facts can be relabeled, and an arbitrary predicate has no owner.
pub struct Verified { pub root: GenerationId, pub dependencies: DepSetId }
let verified = plan.verify(|_| true)?;

// DO: readable facts are immutable and the exact checked evidence remains borrowed.
pub struct VerifiedFacts { pub root: GenerationId, pub dependencies: DepSetId }
pub struct Verified<'evidence, Evidence: ?Sized> {
    facts: VerifiedFacts,
    evidence: &'evidence Evidence,
}
impl<Evidence: ?Sized> Deref for Verified<'_, Evidence> { /* facts only */ }
impl<Evidence: ?Sized> AsRef<Evidence> for Verified<'_, Evidence> { /* exact owner */ }
```

The retained evidence type is earned even with one current store: it removes owner substitution and
keeps mutation/drop excluded for the witness lifetime. Do not add generic policy parameters around
it. A publication adapter still needs the stable receipt that names its real effect.

### Store-bound completion and consumption

A pure planner may use a caller predicate to classify wanted/have work, but that predicate never
mints completion authority. The completion transition must query the exact immutable evidence owner
for every descriptor in the complete projection. A content-key hit is insufficient: compare the
retained length, schema, and kind with the root descriptor before issuing the witness. Reject a
partial projection before any lookup.

The first consumer binds the retained witness, validated generation view, and requested row once.
Check generation equality before row lookup, then return a provider that retains the witness and can
start without another request or presence check. Keep the legacy request-checked provider separate
until it is deleted; do not widen its error type with failures impossible on the verified route.

```rust
// DON'T: planning policy is treated as storage evidence, then rechecked through another request.
let verified = complete_plan.verify(|descriptor| policy_says_present(descriptor))?;
provider.start(Request { generation: verified.root, required: descriptor })?;

// DO: exact owner proves all descriptor facts and remains borrowed through execution.
let verified = complete_plan.stage().verify_store(&store)?;
let provider = Provider::bind_verified(&view, key, &verified)?;
let run = provider.start();
```

Exact missing/conflicting descriptor reports may be larger than a hot `Result`. Preserve every
operand in one boxed cold-path report when that keeps the successful path compact; never box the
witness, store, provider, or ordinary success data merely to silence a lint. Tests must prove empty
store rejection, same-content/different-metadata rejection, partial-before-lookup ordering, stale-
before-missing ordering, exact owner pointer identity, fused execution, and compile-time failure when
the store is dropped or substituted while the witness is live.

Prototype owner work compares borrowed callback, caller-retained bytes, exact heap bytes, mmap/lease,
and a self-referential adapter only when a view must escape. Measure validation repetition, pointer
depth, construction/drop, peak simultaneous owners, and code size before promoting any shape.

## Packed range progression

Build a new packed collection in separately approved vertical witnesses, in this order: canonical
writer; fixed header geometry; borrowed directory validation; binary range lookup; borrowed full-body
view and selected content verification; optional whole-artifact authentication and typed diagnostics;
public hydration/E2E adapter. A phase does not preload the next phase's errors, witnesses, reexports,
or dependencies. This sequence is a scope control, not a requirement to keep a bad format: hostile
evidence may send the design back to the preceding invariant owner.

## Closure

Return the shared handoff plus ownership/peak diagram, static storage profiles, identity law, bounded
planning work, and the next owner decision. Never claim index/cache/transport completion.
