# Canonical-byte local-closure terminal status

State: `evidence-blocked`. The public terminal is implemented, the controller and registered Terra
manager reproduced its source/resource gates, and the chief journey is green. Independent hostile
review is not available because both corrected source-isolated dispatch attempts failed before a
nonempty reviewer runtime identity existed. This document makes no approval claim.

## Frozen identities

- Controller baseline: `f2565a9fb33af06053bd19721d4dc2753ec09ed5`, tree
  `8477cb2ab93763c468d5431740cfb1d5e4c4cf82`.
- Terminal material candidate: `a2fbc7c9a40f535cd683a24c9a7ba06b8d4057a2`, tree
  `f77c157799dc53b2d000d042df32050e3a43a197`.
- Candidate branch: `codex/performance-data-structure-closure` in the isolated checkout
  `/Users/mileswirht/.config/codex/worktrees/ab6a/backend`.
- Dirty saved checkout `/Users/mileswirht/Downloads/backend` was consulted only as read-only legacy
  evidence. It was never edited, built, cleaned, or committed.

## Public terminal

```text
ValidatedRoot::try_from(canonical_root_bytes)
  -> BorrowedGenerationView::new(root, locality)
  -> Need::bind_borrowed(view)
  -> plan_borrowed(bound_need, ClosureScratch, PlanScratch, presence)
  -> ObjectPackView::lookup + selected.verify + borrowed MemoryStore admission
  -> BorrowedStagedGeneration::verify(store_presence)
  -> LocalObjectProvider::bind_verified(view, key, verified)
  -> BoundLocalObjectProvider::start()
  -> one batch, one complete terminal, fused finished
```

The terminal retains canonical root/locality/pack bodies through caller lifetimes. It retains no
native root row or descriptor arena, adds no `Box`, `Arc`, `dyn`, serde/caching layer, unsafe block,
generic policy dimension, macro, SIMD kernel, or external production dependency. The only promoted
edge is the already-used internal `nudox-hydration` dependency required to name the sealed witness.

## Reproduced laws

CBC-01 through CBC-09 are controller-reproduced. Exact typed faults cover extent, parent tag,
nested descriptor, authority, key order, parent relation/cycle, root/locality mismatch, demand
mismatch, scratch capacity, partial verification, missing object, pack corruption, stale operation
binding, and fused terminal behavior. Missing-parent lookup retains the binary-search insertion
coordinate instead of erasing it. Pointer checks prove the validated root borrows its input and the
store retains the selected verified pack body.

The full resource record, public layouts, candidate controls, exact gates, commits, and paths are in
`terminal-report.md`. The 1/100,000-row executable control is
`layout-lab/run-canonical-closure-resources.sh`; the all-shipping-crate atlas is
`layout-lab/run-shipping-atlas.sh`.

## Review custody and findings

The corrected `b3473581` protocol was bound without merging that shared commit. Candidate
`acee01eabae2385337e777d1d2ab6faaec5fdbe0` was exported read-only with identical pre/post aggregate
SHA-256 `86ab1b428d0fe17487a095712db434be6329bb2789f54926332cc297f5f35d75` and zero writable
source files/directories.

The Sol/low primary sidecar `01a051d5-08a3-7403-a66a-3c5c8e4891c6` failed authentication before
spawn. The one-variable alternate `01a051d5-9cf1-7d42-961c-7aae624d4c55` emitted a label but every
wait returned `receiver_thread_ids=[]`, then its runtime panicked on malformed environment value
`â\x88\x99`. Therefore there is no reviewer task ID, verified Terra/xhigh child, effective reviewer
sandbox, reviewer gate, finding, or approval. Historical direct attacks remain attack evidence
only. Their actionable issues were repaired: pack pointer authority, exact fault oracles,
1/100,000 resource evidence, hydration dependency custody, and no-request bound operation start.

External action required to clear the state: the runtime owner must restore authenticated dispatch
and environment propagation so the prescribed Sol/low parent can return a nonempty registered
Terra/xhigh reviewer ID against a new immutable snapshot. No weaker third attempt is authorized.

## Strongest counterexample and uncertainty

`VerifiedGeneration` is sealed against field construction, but it records the result of a
caller-supplied predicate; the type does not bind a particular physical store identity, lease, or
snapshot. The chief journey supplies a real `MemoryStore` predicate, but another caller can pass a
constant-true predicate and mint the same nominal proof. Thus the terminal proves that the selected
predicate was applied to the complete closure, not that a named store will remain physically
faithful. A future consumer must not strengthen that interpretation without a store-bound witness.

Other retained uncertainty is explicit: present-parent resolution is `O(N log N)` because each
parent uses canonical binary search even though cycle traversal is linear; exact dynamic branch and
machine-copy counts remain unknown; compiler layout/code-size numbers are target-specific; measured
latencies are host samples; allocator bookkeeping is outside logical payload bytes; and no valid
independent review covers the final candidate.
