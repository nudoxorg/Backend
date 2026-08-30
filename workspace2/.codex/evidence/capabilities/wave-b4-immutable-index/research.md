# Wave B4 immutable-index research journal

| Decision / rows | Source or experiment | Useful mechanism | Rejected coupling / decision change | Saturation |
| --- | --- | --- | --- | --- |
| Exact/lexical artifact separation; B4-02/B4-10 | `INDEX_GREENFIELD_PLAN.md`; I0 vocabulary/history | Separate current exact/lexical domains and immutable replacement sets. | Historical generic family projection/future enum surface was removed in `b1a59b16` because it was speculative; do not restore it for B4. | One internal plan/history; external source pending. |
| Borrowed manifest/segment choice; B4-03/B4-05 | `PACKED_COLLECTIONS.md`; `LAYOUT_AUDIT.md` H1 | One validated owner with borrowed lanes, explicit range grammar, scalar validation control. | Per-region object graph, duplicate directory authority, and trusted raw reprojection rejected. | One internal experiment/control; external format source pending. |
| Lexical term representation; B4-05/B4-06 | `INDEX_GREENFIELD_PLAN.md` first-manifest decision | Sorted descriptor control first; FST only a lexical-term candidate measured against bytes/page touches/build work. | FST cannot become manifest directory, cache owner, or universal schema. | Awaiting upstream FST documentation. |
| Horizontal durable truth; B4-07/B4-08/B4-09 | `INDEX_GREENFIELD_PLAN.md`; `OVERNIGHT_COMPLETION.md` Wave B4 | Request owns snapshot/segment IDs; durable bytes/head are truth; tier/rendezvous advisory. | Node-owned shards, per-update mutable consensus, route-derived semantics rejected. | Awaiting independent upstream source. |
| Shared compaction; B4-10 | `INDEX_GREENFIELD_PLAN.md` | Consume explicit immutable input set, build once, verify equivalence, publish replacement snapshot. | Replica-local repacking, unverified replacement, pre-durable head transition rejected. | Awaiting independent production source. |
| Transport boundary; B4-07/B4-09 | `ASYNC_STREAMING.md`; `build-leased-range-transport` | Range wait yields bounded owned lease; planning/query over borrow stays synchronous; partial is typed terminal data. | Boxed stream, whole-segment staging, ambient runtime, empty-success collapse rejected. | Two internal contract sources agree; adapter research deferred. |

Research is not saturated for format, lexical, horizontal, or compaction decisions. Before admitting a
representation, add two independent relevant sources/experiments that change a candidate, resource
bound, falsifier, or record why none does.

## Symbol/mechanism salvage ledger

No historical material is deleted by this Phase 0 increment.

| Considered material | Classification | Retain/reject decision | Destination / replayed falsifier |
| --- | --- | --- | --- |
| `planes/index/.../nudox-index-vocab::{IndexSnapshotId,ExactSegmentId,LexicalSegmentId}` | representation/invariant | Retain as current typed vocabulary; prerequisite, not B4 closure evidence. | Future manifest/query consumes aliases; B4-02 replays byte/family boundary. |
| Pre-`b1a59b16` `SegmentFamily`, `UnknownSegmentFamily`, `IndexSegmentFamily`, `IndexSegmentId<Family>` | speculative representation/public surface | Reject unless two current consumers prove a closed declaration. The generic associated-domain mechanism was removed as unearned. | Evidence remains in `INDEX_I0_MANAGER_CARD.md`, `INDEX_I0_CALIBRATION_RAW.md`, Git `b1a59b16`; B4-02 kills untyped/mixed-family replacement. |
| I0 local/remote byte and cross-family doctests | invariant/oracle | Retain upstream identity attack. | Replay inside B4-02; do not claim it proves borrowed snapshots/segments. |
| Legacy `workspace/index/{catalog,engine,coordination,outbox,entity,pack}` | anti-pattern/diagnostic | Reject compatibility/code reuse: catalog, search, coordination, schema, backend, and routing are coupled. | Preserve source untouched; B4-08 kills node/location truth and B4-02 kills universal DTO/schema import. |
| Tantivy architecture named by plan | candidate algorithm/adapter evidence | Retain immutable-segment/pruning comparison and a bounded lexical differential adapter obligation. | B4-13 compares document set/order/tie/provenance only; it never owns schema/truth. Two Nix/offline dependency attempts are required before an honest block. |
| Qdrant and Trustfall | dependency/adapter anti-pattern | Exclude from candidate. | B4-02/B4-11 dependency tripwire. |

## Remaining uncertainty

Wire grammar, exact/lexical layout, score representation, publication receipt, segment-count policy,
and physical adapter are intentionally undecided. The card fixes observable semantics and evidence,
not any one mechanism.
