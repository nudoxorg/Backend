# Deleted-doc decision audit

This audit reads the deleted `docs/` tree at parent commit
`b80862d071b068e6a08c26dc83e378ae1e27d177^`. It prevents the greenfield implementation from
silently losing product invariants while refusing obsolete implementation commitments.

## Precedence

`INDEX-PLAN.md` revision 3 declared itself the normative greenfield master and explicitly
superseded older Global Decisions. Its dead list wins over the earlier librarification plan. In
particular, Terminus and `ApiSurface` are dead. This workspace retains useful semantic laws from
older documents, not their dynamic Rust sketches, dependencies, or backend choices.

## Retained laws

- Local-first truth: paint/answer from locally ready data immediately; remote enrichment may be
  absent, delayed, or degraded and never blocks valid local output.
- One answer grammar across local and remote: typed request determines typed items/notes/terminal
  summary; partial/degraded/truncated are explicit rather than empty success.
- Cancellation by ownership and generation guards: superseded work cannot publish into a newer
  request or root.
- Any prefix before a terminal frame is structurally usable but incomplete. Exactly one honest
  terminal fact closes a run.
- Residence/provenance is real metadata, not a second parallel result model. Pending/missing are
  planning states, not attributes of a delivered value.
- Federation keys derive from plane-independent canonical facts. Instance-salted IDs cannot dedup
  local and remote values.
- Sovereign state is immutable/canonical; text, vector, graph, render, and reverse indexes are
  disposable projections.
- Content identity, encoded artifact identity, generation identity, and location are separate.
- Sync is wanted/have closure difference with atomic visibility after verification, not row
  replication or CRDT state merging.
- Source trees and stage artifacts are deterministic content-addressed objects with range-addressed
  members. Registry archives may reconstruct source but are not the lasting source of truth.
- Local devices may provide verified objects only to trusted remotes. A promoted content-addressed
  object is already the cache; there is no second upload format.
- Work is sealed-generation and changed-only. Re-listing, re-indexing, embedding, or publishing the
  whole accumulated corpus for a small delta is an architectural failure.
- Ordering is canonical and deterministic. Hash-table iteration order never reaches identity,
  ranking, UI order, or golden bytes.
- GUI/application crates consume client/store contracts; core object, graph, compiler, and engine
  crates never depend on a GUI framework.
- Compilation is a producer of the same typed stream/object fabric. Remote compilation scales
  vertically by independently admitted concrete jobs; index/catalog/object serving scales
  horizontally by immutable replication and partitionable reads.

## Rejected or deferred legacy choices

- TerminusDB: rejected completely, including the older hot-tier-only compromise.
- Public IPFS and transport-shaped identity: rejected. Verified range transport remains replaceable.
- `Arc<dyn Serve<S>>`/boxed-stream universal service core: rejected. Finite routes use closed static
  dispatch; adapters may erase types only outside the core when an application boundary requires it.
- JSON/NDJSON/postcard as the internal data plane: rejected. Wave 1 uses precise canonical binary
  frames and borrowed validated views. Human/control adapters may translate at the edge.
- A single giant `heart` crate: rejected. Identity, schema, frames, objects, roots, hydration,
  operation, runtime, and workflow are separate crates with hardened one-way dependencies.
- Persisted parse trees and backend-native graph documents as sovereign state: rejected. Rebuild
  them from canonical source/IR/object facts.
- Unbounded collection before streaming, per-request runtimes, parallel local/remote GUI state
  machines, and silent provenance defaults: rejected.
- Specific catalog engine, iroh/Bao transport, compression, ObjectPack TOC, compiler fleet, search,
  and GUI implementations are later swaths. Wave 1 must leave exact typed seams without pretending
  those systems exist.

## Wave 1 coverage map

| Legacy invariant | Wave 1 evidence owner |
|---|---|
| Content/artifact/generation identity separation | `nudox-id` |
| Precise canonical binary stream | `nudox-schema`, `nudox-frame`, `nudox-view` |
| Residence, promises, overlays, remote tombstones | `nudox-object`, `nudox-root` |
| Wanted/have and atomic ready publication | `nudox-hydration` |
| Bounded immutable local object plane | `nudox-store-memory` |
| Static request/item/terminal pairing and provenance | `nudox-operation` |
| Parallel bounded admission, generation cancellation | `nudox-runtime` |
| Changed/idempotent durable execution and recovery | `nudox-workflow` |
| Local/remote-shaped partial and failure behavior | `nudox-e2e` |

## Later-swath obligations preserved

Future work must add the catalog/index, vertically elastic compiler workers, partial-clone object
packs, range verification, trusted remote provision, deterministic search/relevance evaluation,
and lean GUI client. They must reuse Wave 1 identities and state witnesses rather than create
parallel DTO/status/availability models.
