# IR delta baseline fixtures

`ir_delta.rs` is the executable Rust harness. It builds deterministic declarations through `IrBuilder`, writes and reopens a full NXFI image, traverses it with `SemanticReader`, and compares an independent owned `BTreeMap` view of entity facets, stable links, and counted occurrences with `SemanticDiff`. The oracle owns documentation fragments, attributes, source positions, authority, core hashes, and Rust extension facts. No SPIR producer API is required; the borrowed `(stable key, row bytes)` adapter is the seam for one later.

`ir_delta_locality.py` is a source-independent byte-locality model. It generates 8,192 deterministic IR-like rows with one ring relation and two source/confidence observations per row. No saved NXFI/NXF image was found in the checkout or `target/`, so the JSON results describe generated fixture bytes, not a production compiler run. Python uses SHA-256 for portable segment/leaf fingerprints; current Rust production hashing uses a different digest. The persistent closure treap uses a declared 136-byte serialized node model, not the store's measured physical encoding or network protocol.

The committed [`ir_delta_locality_8192.json`](ir_delta_locality_8192.json) records all scenario counts for no-op, early/mid/tail attribute growth, docs-only change, rename, insertion, and deletion; raw 1 MiB boundary cases; and a 1.5 MiB jumbo-field edit. Payload fetch and old-byte removal are separate from closure writes. Full planners report planner reads and payload hash bytes separately. `planner_wall_ns_pair` is intentionally omitted from claims because this is a Python model and the timings are not a production comparison.

For this 8,192-row fixture, paired full-plan reads and segment-payload hashes are each about 13.78 MB. On an early 4 KiB attribute growth, fetched payload is 6,893,601 bytes for ordinal windows, 423,755 for row CDC, and 31,849 for 8-bit key prefixes; their modeled closure writes are 952, 816, and 1,360 bytes. On a head insertion, the corresponding fetched payload is 6,890,346, 2,505,339, and 52,983 bytes. These numbers show locality in the declared model; they do not include actual store or network behavior.

The incremental row-hash index is an explicit optimistic model. Its update begins after a complete changed-stable-key stream has been supplied. The fixture derives that set by comparing both complete row maps; the JSON reports but does not charge that discovery scan (`fixture_change_key_discovery_rows_not_charged`). The model charges changed target row reads/hashes and persistent closure path reads/writes. It assumes a persisted index and does not charge its initial build again; the reported resident estimate is 56 bytes per row entry, 136 bytes per persistent treap node, and 256 136-byte bucket roots. This excludes allocator/map overhead, stored row payloads, source IR, and other package state. No-op full planners still read and hash both identical images (about 13.78 MB per paired image fixture); the incremental no-op reports zero target row hashes only under the assumption that the trusted change-key stream is empty and the base index already exists.

The jumbo case inserts 30 bytes near the front of a 1,572,864-byte field. It compares a rejected contiguous >1 MiB row, 512 KiB ordinal continuations, and content-hash leaves in an ordered rope. The CDC path separately reports payload fetch, the plane closure, ordered-rope bytes, target boundary-scan and leaf-hash bytes, and closure/hash node work. Its failure injection checks are source-independent model assertions, not receiver tests. The planner currently makes a boundary pass and a leaf hashing pass; results do not claim one-pass behavior.

For that insertion, 512 KiB continuations fetch 1,573,009 bytes plus 680 modeled plane-closure bytes. Hash-addressed CDC leaves fetch 76,255 bytes; the rope writes 680 bytes and the plane closure writes 816, for 77,751 fetched-plus-tree bytes total. The fixture scans and hashes 1,572,894 bytes each (3,145,788 combined) and constructs 2,720 bytes of closure/rope node hashes. Corrupt, truncated, missing, and reordered leaves all fail the model's checks; missing leaves leave the root unpublished.

Run the locality model with:

```sh
/usr/bin/python3 crates/semantic/benches/ir_delta_locality.py --rows 8192
```

After a Cargo benchmark slot opens, run the Rust harness with:

```sh
IR_DELTA_ROWS=8192 IR_DELTA_SAMPLES=20 IR_DELTA_WARMUPS=3 IR_DELTA_REPEATS=10 cargo bench -p backend-semantic --bench ir-delta
```

The Rust harness prints each case's independent oracle and diff work counts, segment read/hash/fetch/removal totals, and p50/p95 latency plus allocation counts/bytes for the oracle, full-image encoding, `SemanticDiff`, and segmentation planners. It prints a source-derived staged-handle memory formula; compact SPIR sink memory is not measured until that producer lands. This checkpoint has not been Cargo-built or executed.
