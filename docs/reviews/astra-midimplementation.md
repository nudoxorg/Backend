# Astra adversarial mid-implementation review

**Review date:** 2026-09-08. **Reviewer:** one independent `gpt-6-astra`
agent at high reasoning effort. The reviewer was read-only and was explicitly
forbidden from delegating.

The review found strong low-level mechanisms but incomplete production
composition. Passing helper tests did not establish several advertised type,
ordering, memory, and delta-work contracts. The following findings are release
gates for the v2 cutover.

## P1 findings

1. `ArrangementRelation<V>` builds canonical nodes in logical `(RowKey, V)`
   order while the lazy storage adapter decodes and searches the same key bytes
   as digest order. A 19-row executable reproduction failed canonical admission
   with ten adjacent order reversals. Construction, admission, and lookup must
   use one reversible canonical order and randomized multi-leaf reopen laws.
2. The compiled locald profile commits a two-state numeric fixture toggle while
   actual package changes live in a separate view journal. A crash between the
   two publications leaves no authoritative package intent from which the view
   can be rebuilt. Package/source intent must be a typed workspace relation;
   the view must be derived and recoverable.
3. Flow byte budgets charge fixed row headers before cloning variable payloads.
   A reproduced one-row range seek copied a 1 MiB `String` under a 256-byte
   budget while reporting roughly 120 bytes. Admission must account for owned
   payload, capacity, indexes, and simultaneous old/new allocations before
   allocation, preferably through shared immutable payloads and affine output
   leases.
4. The view journal appends and syncs before checking its 64 MiB compaction
   threshold, while recovery rejects any file above that threshold. Compaction
   must reserve space before append or use the shared crash-safe journal
   lifecycle; a threshold-crossing crash is a required fault test.
5. Live subscription events build complete base and target view certificates.
   A one-row change therefore traverses and serializes the full view, and claim
   deduplication is quadratic. Stateful delivery must reuse the compact checked
   delta protocol; complete snapshots belong only to paged reset hydration.
6. The retained join computes the simultaneous delta term with a nested loop.
   Two disjoint 100,000-row changes can perform 10 billion uncharged
   comparisons. Changed keys must be merged or indexed, all probes charged,
   output emitted in bounded batches, and skew allowed to select scoped rebuild.
7. Compaction bounds individual calls but can promote oversized runs forever,
   allowing total runs, levels, and retained bytes to grow. Large runs need
   bounded segments and resumable merge/frontier state with global debt and
   backpressure. Promotion is not compaction progress.

## Required unifications

- One canonical, reversible arrangement-key grammar and ordering for build,
  persistence, admission, seek, and path-copy update.
- One admitted memory-owning batch/run representation with real byte accounting,
  reusable storage, and lending cursors that actually govern reuse.
- One compact checked view delta used by durable journaling and live subscribers,
  with authenticated descriptor/page reset hydration.
- One authoritative typed package/source intent relation whose committed head
  can reconstruct every derived product view.

Coverage evidence must also be renamed or strengthened: equal caller-created
scope digests prove a matching claim, not an authority observation. Observation
evidence must originate from a sealed admitted producer capability. The app
must reuse library-owned canonical view encoders instead of maintaining a
second preimage. Finally, at least one real product journey must invoke demand
and refresh placement over source facts, retained arrangements, local reuse,
remote acceleration, and local fallback before K6/K8 can be marked complete.

## Acceptance evidence

The fixes require independent laws that defeat the two false-positive patterns
found here: randomized multi-row/multi-leaf orderings and values much larger
than row headers. Add sustained churn with a stalled observer, broad disjoint
join deltas, one-row updates over growing views, threshold-crossing crashes,
and authoritative-view rebuild after every publication boundary.

## Implementation disposition

All seven P1 findings changed production code and gained independent regression
evidence. None was closed by weakening admission or by changing only the
review's reproducer.

| Finding | Production disposition | Independent evidence |
|---|---|---|
| Arrangement order | `ArrangementKey` is the single reversible byte grammar used by build, persisted admission, seek, and path-copy update. Lazy lookup no longer substitutes digest order for logical order. | `randomized_multi_leaf_orderings_match_full_rebuild_oracle`; flow's randomized path-copy and lazy multi-level reopen tests. |
| Product authority | `ProductSourceRelation` is typed workspace intent. `ProductSourceSnapshot` binds checked base/target relation roots and supplies delta and retention facts to locald and worker execution. Derived product views can be reconstructed from the committed workspace relation. | Product worker/locald process journeys plus checked workspace reopen and view-delta journeys. |
| Memory accounting | Flow batches and runs share immutable payload owners. Bounded lending cursors charge retained payload bytes before cloning or emitting, and publication remains affine until admission. | `large_payload_is_rejected_before_a_budgeted_page_clones_it`; owned batch/run sharing and multi-megabyte replication output tests. |
| Journal threshold | The view journal reserves against the scan bound before append and uses crash-safe snapshot replacement and torn-tail repair. | `compacts_before_an_event_can_cross_the_scan_file_bound`; compaction publication-fault and second-open torn-tail tests. |
| Subscription work | Live delivery carries the checked compact transition and changed rows. Complete materialization is a root-bound, credit-bounded paged reset. | `one_row_delta_delivery_does_not_hydrate_the_growing_view`; 100,000-row paged reset and compact subscription tests. |
| Join work | Retained sides use flat sorted borrowed indexes. Changed-key merge/probe work is charged, and high fanout emits bounded segments through a lending cursor. | `disjoint_hundred_thousand_row_join_is_bounded_and_matches_full_oracle`; flow's simultaneous randomized join and high-fanout segment tests. |
| Compaction debt | Oversized runs are segmented; merge state resumes from a frontier; global retained debt backpressures producers when a pin prevents progress. | `stalled_observer_gets_bounded_compaction_debt_then_releases_it`; repeated-pause frontier and oversized-churn tests. |

The required unifications are also reflected at boundaries: producer-originated
coverage is admitted through an opaque capability bound to producer, scope,
context, and evidence digest; the library owns the canonical view codec; and
the Product process path crosses source facts, retained roots, local execution,
remote tickets, fallback, and exact reuse. The final workspace and process
gates, rather than this table, decide whether the implementation is releasable.
