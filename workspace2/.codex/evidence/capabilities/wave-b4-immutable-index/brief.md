# Wave B4 immutable-index Phase 0 brief

## Public terminal

One sealed, published index snapshot names typed immutable exact and lexical segments derived from
sealed IR-like deltas. A caller pins that snapshot and issues one of three closed operations:
`ExactLookup`, `PrefixScan`, or `LexicalSearch`. The result is bounded, deterministically ordered
borrowed rows plus exactly one terminal fact: `Complete`, or `Partial` naming every missing declared
segment/range. A no-result lookup is `Complete` with zero rows; it is never a substitute for an absent
declared segment. Snapshot, segment, and range identifiers—not node, tier, cache, route, or compiler
availability—determine semantics.

The first candidate is representation-open. It may choose a fixed-width lane, FST, or another
validated family-owned format only after the stated safe control and falsifiers decide it. It may not
introduce a universal request/document DTO, `dyn`, a boxed stream, serde, a backend branch inside a
posting loop, mutable distributed truth, or a cache as the default answer.

## Chief-owned public red integration seam

The following is the literal integration seam Sol may make red before selecting a format or backend.
`exact_at`, `prefix_at`, and `lexical_at` are three typed public operations, not variants of a
universal DTO; their concrete receiver/name is intentionally left to the product owner.

```rust
#[test]
fn pinned_index_snapshot_is_stable_across_routes_tiers_and_compaction() {
    let s1 = published_snapshot("s1"); // exact E1, lexical L1
    let s2 = published_snapshot("s2"); // reuses E1/L1, adds E2/L2 for update/delete D2
    assert_eq!(exact_at(s1, key("alpha"), limits(3)).complete_rows(), [(key("alpha"), value("v1"))]);
    assert_eq!(exact_at(s2, key("alpha"), limits(3)).complete_rows(), [(key("alpha"), value("v2"))]);
    assert_eq!(exact_at(s2, key("beta"), limits(3)).complete_rows(), []); // tombstoned zero hit

    let local = lexical_at(s2, terms("alpha"), top_k(3));
    let retried = lexical_at_via_stale_route_then_retry(s2, [E1, E2, L1, L2], terms("alpha"), top_k(3));
    assert_eq!(retried, local); // rows, score, order, and segment provenance
    assert_partial_names_exact_segment(lexical_at_with_missing(s2, [E2], terms("alpha"), top_k(3)), E2);

    let s3 = independently_compacted_snapshot(s2); // replaces E1/E2 and L1/L2 atomically
    assert_eq!(exact_at(s3, key("alpha"), limits(3)), exact_at(s2, key("alpha"), limits(3)));
    assert_eq!(lexical_at(s3, terms("alpha"), top_k(3)), local);
    assert_eq!(exact_at(s1, key("alpha"), limits(3)).complete_rows(), [(key("alpha"), value("v1"))]);
}
```

The fault schedule sends declared E2 to one lost rendezvous-selected worker. The request itself names
S2 and `[E1, E2, L1, L2]`; retrying another worker produces the same semantic result, while disabling
retry yields `Partial { missing: [E2] }`. Moving a complete segment between RAM, NVMe, and object
storage leaves its ID and result unchanged. The fixture receives D1/D2 as sealed bytes; no compiler
invocation is permitted.

## Non-negotiable laws and negative space

1. Canonical generation/object bytes and durable publication are truth; index artifacts are replaceable
   projections whose IDs bind input, family, recipe/schema, and bytes.
2. A query pins one `IndexSnapshotId`; no leaf silently substitutes a current head or another snapshot.
3. Exact and lexical are distinct typed families. Exact data applies insert/update/delete/tombstone in
   snapshot order; lexical scores/ties/order/provenance are recipe-versioned deterministic facts.
4. Manifest metadata selects segments before payload I/O. Plans name selected segments, warmup ranges,
   merge order, `TopK`, and credits before execution.
5. S2 reuses untouched E1/L1; compaction builds one replacement set, proves exact values and lexical
   document sets equivalent, then atomically publishes S3. Node-local repacking is forbidden.
6. Requests carry snapshot and segment IDs. Rendezvous ranks warm workers only. A stale route or lost
   node returns exact absence/partial and may retry; RAM/NVMe/object-store never changes IDs.
7. Validated views borrow original bytes; local and remote use identical query semantics. File/network
   waiting is a later leased adapter, never a whole-segment staging or async disguise for CPU work.
8. Disabled typed probes neither format nor allocate; no exporter owns data-plane credits.

Out of scope: relation/usage/vector, publication-log implementation, real object-store/file adapters,
runtime SDKs, Tantivy writer/reader integration, Qdrant, Trustfall, serde, SIMD/unsafe, compiler
integration, schema-wide registry generators, cache policy, and an abstract query family. Tantivy may
later be a nested lexical differential adapter only; it cannot own logical schema, snapshot truth,
segment identity, or terminal semantics.

## Baseline, custody, paths, and consumers

| Fact | Recorded value |
| --- | --- |
| Capability | `wave-b4-immutable-index` |
| Manager runtime task | `/root/wave_b4_terra` |
| Registered role/config | `nudox_terra_orchestrator`; `workspace2/.codex/config.toml`; `workspace2/.codex/agents/nudox-terra-orchestrator.toml` |
| Requested/actual dispatch model and effort | `gpt-5.6-terra` / `xhigh` |
| Manager sandbox | `danger-full-access` runtime; no production write is authorized in Phase 0 |
| Checkout | `/Users/mileswirht/.config/codex/worktrees/f7b8/backend` |
| Branch and baseline | `codex/wave-b4-immutable-index`; `f2565a9fb33af06053bd19721d4dc2753ec09ed5` |
| Baseline tree | `8477cb2ab93763c468d5431740cfb1d5e4c4cf82` |
| Forbidden checkout | `/Users/mileswirht/Downloads/backend` is never read, written, cleaned, or merged |

Current owners are deliberately small: `workspace2/planes/index/crates/nudox-index-vocab` exports
`IndexSnapshotId`, `ExactSegmentId`, and `LexicalSegmentId`; `workspace2/crates/nudox-id/src/marker.rs`
owns their closed domains; the nested index workspace owns focused gates. No snapshot, segment, delta,
query, publication, transport, or server consumer exists. `workspace2/INDEX_I0_*.md` and
`workspace2/evidence/index-i0/**` are historical evidence, not a compatibility surface.

Only this Phase 0 path is writable:

```text
workspace2/.codex/evidence/capabilities/wave-b4-immutable-index/**
```

All production Rust, Cargo manifests/locks, fixtures, shipping tests, roadmaps, existing I0 evidence,
and every other path remain read-only. No Luna is authorized to write production during this phase.

## Coupling skeleton

| Owner/module boundary | Invariant owner | Public terminal | Dependencies | State/control boundary |
| --- | --- | --- | --- | --- |
| sealed-delta input | compiler/publication producer | sealed D1/D2 supplied as facts | current ID vocabulary | unsealed/corpus rescan rejected |
| snapshot manifest/view | format/view candidate | pinned snapshot selects typed IDs | canonical IDs + family records | validate once; no request/head/route mix |
| exact segment/query | exact family owner | exact lookup/prefix | pinned manifest + borrowed range | visible/tombstoned key fixed by delta order |
| lexical segment/query | lexical family owner | deterministic lexical top-k | pinned manifest + borrowed range | score/tie/provenance are semantic |
| plan/range layer | query planner | selected IDs and warmups | explicit limits/credits | all fan-out/ranges admitted before I/O |
| merge terminal | root query owner | complete or exact partial | typed leaf batches | no-result differs from absent |
| placement adapter | nested adapter | optional retry by declared IDs | durable bytes only | route/tier/health are advisory |
| compaction verifier | build/publish candidate | equivalent S3 replacement | explicit input set | verify before atomic replacement |

## Safe control and ledgers

The safe control is a test-only semantic reference evaluating four declared segments in sealed-delta
order and sorting bounded rows by the documented score/tie rule. It is not a shipping backend, cache,
or mutable shadow truth. Candidate format choices are one sorted-descriptor control and, for lexical
terms only, one measured FST candidate.

| Ledger | Control / candidate admission |
| --- | --- |
| Allocation/owner | no production owner exists; reject hot `Vec`/`Box`/`Arc` unless an escaping owner, exact bound, deleted copy, and measurement are recorded |
| Work/branch | red fixture permits 4 segments, 4 warmups, 3 rows, 12 merge candidates; record selected IDs, bytes, comparisons, decoded blocks, and branches |
| Range/credits | plan admits only named IDs/ranges; fixture has 4 segment and 4 range credits; no fetch-all fallback |
| Code size/generics | no new production text; generic/static dispatch requires two present consumers plus release-text control |
| Dependencies/unsafe/SIMD | none; stop before Cargo change, dependency, unsafe, SIMD, macro, `dyn`, boxed stream, serde, or backend SDK |
| Diagnostics | later probes use lazy typed construction; disabled builder execution is a direct falsifier |

No timing, allocation, branch, code-size, or layout result is claimed. The first non-empty candidate
must produce raw isolated measurements and safe-control comparison.

## TESTING.md mapping

`TESTING.md` SHA-256: `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b`.

| TESTING.md clause group | Matrix rows | Phase 0 disposition |
| --- | --- | --- |
| allocation/layout: size/align/offset, non-empty allocation, retained owner capacity/reuse/drop | B4-11 | applies to first packed view/owner; red |
| allocation/layout: closed tags, sources, wire records, pointer containment/no reparse | B4-02, B4-03, B4-11 | applies to manifest/segment validator; red |
| universal: negative space, exact errors/order/state, deterministic sequences, observable work | B4-01 through B4-12 | applies; each row has a mutant/falsifier |
| universal: idempotence/conflict, extension cardinality, semantic lint, registry/hash/routing entropy | B4-02, B4-04, B4-10, B4-11 | applies when delta/format/registry is introduced; I0 registry is read-only now |
| foundation fabric: truncation, mutation, untouched output, borrowed containment, single scan | B4-03, B4-05, B4-11 | applies to future format/segments; red |
| object/root/store/hydration: canonical/locality/collision laws | B4-02, B4-06, B4-09 | sealed root/store IDs are inputs; direct owner changes are excluded |
| runtime/workflow: credits, wake/cancel, durability/replay | B4-07, B4-08, B4-10 | applies only to later range/publication adapters; no runtime source is in this card |
| end-to-end: canonical bytes, stale generation, bounds/cancel/outage, provenance | B4-01, B4-07, B4-09, B4-12 | applies as chief journey; Phase 0 supplies no executable shipping test |

Tests will live with their owner or ordinary top-level `tests/`; no test-only crate is permitted.

## Product-authority questions

None. The four-segment/three-result fixture is an evidence control, not a product-wide policy. A
proposal that changes durable identity grammar, adds dependency/unsafe/SIMD authority, or makes a
backend/transport a source of truth returns to Sol as an authority fork.
