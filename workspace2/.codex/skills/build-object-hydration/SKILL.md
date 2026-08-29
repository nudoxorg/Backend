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
- Owned root construction and borrowed canonical-root reading are distinct paths sharing one grammar.
- Arbitrary-order construction may allocate to sort; range read/validation does not.
- Payload owners transfer or return unchanged. Store metadata policy is static and first-write-wins.
- Scratch is caller-owned/reusable where lifetimes align. Construction input is released before
  later hierarchy scratch whenever possible.
- Planning touches requested/changed rows and ancestors only. Sequential sparse traversal advances a
  cursor; random lookup performs bounded membership/rank work.
- Boundary tests prove pointer identity, zero/one/limit/+1, exact rejection ownership, malformed and
  collision cases, allocation failure sources, lifetime compile-fail, unchanged root identity across
  locality/tier movement, bounded work, and exact aggregate events.

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
