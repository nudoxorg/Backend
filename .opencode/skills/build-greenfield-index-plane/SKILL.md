---
name: build-greenfield-index-plane
description: Scope and evidence rules for one greenfield workspace2 immutable-index capability. Use with deliver-reviewed-rust-slice and manage-rust-swarm when designing, building, or reviewing typed snapshots, segments, borrowed queries, publication, compaction, range access, or horizontal execution.
---

# Build the greenfield index plane

Read `../deliver-reviewed-rust-slice/SKILL.md` completely, then
`../../../INDEX_GREENFIELD_PLAN.md`. A manager also reads `../manage-rust-swarm/SKILL.md`,
`../write-evidence-rubric/SKILL.md`, and `../review-rust-gem/SKILL.md`. Read
`../../../PACKED_COLLECTIONS.md` and `../../../TESTING.md` for any format/view task.
Before the first production edit in a new phase, the manager applies
`../calibrate-rust-agent-contract/SKILL.md` to the exact phase card.

The plan is greenfield. Legacy `workspace/index` can supply counterexamples and candidate product
ideas only. Never preserve an old trait, table, DTO, query behavior, backend, or test by default.

No index identity boundary closes while foundation raw identity decode can rebrand identical bytes.
Index prototypes may use a private experimental representation, but shared I0/I1 integration waits
for fixed-width checked domain/encoding authority, compile-fail negative API evidence, and routing-word
entropy over digest cells rather than authority prefixes.

## One phase at a time

The root names one `I0`–`I6` capability. The Terra manager autonomously selects its smallest
observable child slice and freezes allowed paths, identity/schema edits, dependencies, retained/live
bytes, allocations, copies, touched ranges, comparisons, branch/work counters, code size, and
integration terminal before production edits.

For `I0`, the default allowed surface is a new complete nested workspace under
`workspace2/planes/index/**`, plus separately authorized additions to the central ID/schema registries.
Do not pre-create query, publication, server, vector, or object-store implementations. A crate/module
must own a current invariant, not reserve a name for later.

The wildcard above is architectural ownership, not a worker card. The manager enumerates every
writable manifest/source/test/registry path, names the public journey and literal package commands,
and derives numeric resource/text bounds before a worker sees it. Discoverable omissions are
repaired autonomously under `manage-rust-swarm`; only a materially different index semantic returns
to root.

## Required design packet

Return before code:

```text
query consumer and exact terminal
snapshot/segment identity preimages
typed wire diagram and one layout authority
family and coordinate type matrix
pruning and exact touched-range/work budget
borrow/owner/lease lifetime diagram
complete/partial/cancel/degraded/failed matrix
scalar baseline and any measured candidate optimization
explicit legacy ideas rejected
```

## Index-specific laws

- Truth is canonical objects/generations plus an immutable publication log. Indexes are disposable.
- Every query pins one snapshot. Routing, node, cache, and provider identity never enter semantics.
- Exact, lexical, relation, usage, and vector are distinct typed families. Share only proven substrate.
- Families are ranked, never blended. A query spanning families declares a typed family precedence and
  orders within each family; a score from one family never compares against a score from another.
- Manifest metadata prunes before segment I/O. Plans reserve fan-out/range/TopK credits up front.
- Views validate once and borrow original bytes. Lookup does not deserialize documents or build IDs in
  comparisons.
- Segment family types exist only for current family consumers. Do not ship relation/usage/vector
  variants, projection traits, raw codes, or errors during an exact/lexical slice merely because the
  architecture plan names later phases.
- A missing segment/range produces an exact partial terminal; it is never zero hits or empty success.
- Compaction output is built once, equivalence-checked, and published atomically. Replicas download it.
- Rendezvous placement is cache affinity only. A stale route can waste one attempt, never change truth.
- Local and remote use identical snapshot/segment bytes and pure query code with different I/O owners.
- SIMD is opt-in after a scalar work profile identifies posting, bitset, or vector kernels.

## Do / don't

```text
DON'T: struct Registry { catalog, tantivy, qdrant, cache, outbox, server }
DO:    SnapshotView -> selected SegmentRef<Family> -> borrowed family query

DON'T: type SearchKey = (f32, Uuid)
DO:    RankedKey { score: Score<Recipe>, document: DocumentKey }

DON'T: candidates.sort_by(|a, b| b.score.total_cmp(&a.score))
DO:    FamilyRanked { family: Family, within: RankedKey }

DON'T: async fn search(...) -> Vec<ResultDto>
DO:    sync leaf cursor over borrowed regions + async leased range adapter + typed terminal

DON'T: assign SegmentId to node N and call that durable placement
DO:    name the artifact in the snapshot; rank warm workers ephemerally

DON'T: deserialize a manifest into Vec<Segment> plus three side maps
DO:    one validated packed owner with typed lane views and cursors
```

## Stop triggers

Stop and return exact file/type evidence before:

- adding serde, a database/ORM, Qdrant/Tantivy, a server SDK, async runtime, unsafe, or SIMD;
- adding `dyn`, boxed streams, a generic `Value`, dynamic schema reflection, or backend enums to core;
- retaining an allocation without owner lifetime/bound and borrowed/caller-scratch alternatives;
- adding a query variant that weakens snapshot/family/terminal invariants;
- editing portable foundation types beyond a named registry addition;
- inventing compatibility with legacy index behavior;
- claiming horizontal or lock-free scale without failure/model evidence.

## Proof gates

Every applicable slice includes golden bytes, every truncation boundary, structured mutations, exact
errors and sources, pointer containment, allocation/copy/retained-byte measurements, input-permutation
determinism, work counters, density/cardinality cliffs, terminal variants, and top-level real public
integration. `is_err`, happy-path getters, elapsed time alone, and a backend mock are not evidence.

For any query spanning two families add the case where the lower-precedence family holds the
numerically higher score. A case where precedence and score agree is satisfied by a comparator that
ignores family, and is not evidence.

For horizontal work add stale routing, lost/delayed/duplicate leaf responses, node loss, hot keys,
rebalance, compaction replacement, remote outage, and local prefix behavior. For SIMD add scalar
differential, alignment/tails, runtime dispatch outside the loop, and crossover evidence.

## Research routing

Use primary sources to test a concrete decision, not to collect fashionable names:

- Tantivy immutable segment/mmap design:
  <https://github.com/quickwit-oss/tantivy/blob/main/ARCHITECTURE.md>
- Quickwit object-store splits, hotcache, stateless search, and rendezvous affinity:
  <https://quickwit.io/docs/main-branch/overview/architecture>
- Cursor Continuity durable object-store WAL and shared compaction:
  <https://cursor.com/blog/git-at-any-scale>
- Qdrant shard/segment transfer and consistency tradeoffs:
  <https://qdrant.tech/documentation/scaling/distributed_deployment/>
- lakeFS local partial checkout and immutable local tiers:
  <https://lakefs.io/blog/scalable-data-version-control-getting-the-best-of-both-worlds-with-lakefs/>

Copy no architecture wholesale. Record the useful mechanism, rejected coupling, and falsifier.

## Closure

Return the shared handoff plus snapshot/segment diagrams, identity matrix, selected/touched range ledger,
allocation/work/code-size evidence, strongest partial/failure counterexample, rejected legacy ideas, and
the next smallest parent decision. Never claim another index family, publication, compaction, routing, or
distributed execution capability from substrate-only evidence.
