# Rubric — Index Frontier (ingest · search · IR/Qdrant · Turso)

**Goal.** Delta-aware index: work tracks *change*, never accumulated state.
All ecosystems live, extract a consistent information model, one Turso-backed
versioned-row store. Search is typed factors, not a lib.rs god-module.
IR and Qdrant project only deltas.

## Non-negotiables (verifier fails the round if any break)

1. **Delta-first.** Every poll/emit/project path takes a cursor and emits
   `Delta { added, changed, removed }` (or proves `Unchanged`). Full
   re-list / full re-upsert / full re-embed of an unchanged corpus is a defect.
2. **One information model.** `record::PackageRecord` is the only currency
   between ingest → catalog → facets → search. No ecosystem may invent a
   parallel fact shape.
3. **Edges are real.** Manifest deps become catalog `edges` (kind +
   requirement + optional). Dependents sweep may still use facets, but facets
   and edges must agree on names for the same version.
4. **Row versioning.** Every catalog row carries `revision`, `valid_from_ms`,
   `valid_to_ms`, `payload_hash`. Reads at tip filter `valid_to IS NULL`.
   As-of reads filter the bitemporal window. Engine = Turso (MVCC journal);
   path override ready for the org fork.
5. **Search factors are typed.** No 1600-line cascade file. Ranking is
   `RankingFactors` → `Score` with deterministic tiebreak. ID-8 weights live
   as named consts on the type, not prose.
6. **IR dual-write.** Emit always calls `set_generation_root` alongside
   `set_ir`. Vector upsert keys on `(package, intro_id, content_hash)` and
   skips unchanged hashes.
7. **Policies honored.** `retry_budget`, queue backoff `after`, `last_error`,
   `DeadLettered` — declared means enforced.
8. **Dan Luu tests.** Prefer property/differential/adversarial generators
   over hand-picked happy paths. Every new module ships: (a) round-trip
   property, (b) one adversarial fixture that would have caught a past gap,
   (c) a differential check (two paths, same answer).

## Style

- Small files, strong newtypes, no `unwrap` outside tests.
- Fully-qualified descriptive names; typed errors (`thiserror`).
- Object-safe seams only where runtime dispatch is required.
- Comments explain *why* / contracts, never narrate the next line.

## Round exit criteria

- `cargo test -p index --lib` green on the new surface.
- Verifier agent signs off against this rubric (cite file:line for each
  non-negotiable).
- No new god-module > 400 lines without an explicit waiver in the PR.
