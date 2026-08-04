# Edge technology research (streamlining candidates)

> **Status (LIBRARIFICATION-PLAN Rev 2, 2026-07-16):** Verdicts in this index reflect the pre-Rev-2 "phase-2 / watch / evaluate" framing; Rev 2 promotes iroh (now v1-critical, GD-30), promotes LadybugDB as desktop default (GD-34), rejects Doltgres outright (GD-32), and cuts composefs/EROFS and CID/CAR export (GD-35). The research docs below remain valid evidence trails; use LIBRARIFICATION-PLAN §0.4–0.5 for current binding verdicts.

Exploratory deep-dives against the librarification + resolution designs.
**Not** implementation plans — decision inputs.

**Start here:** **[00-DECISIONS.md](./00-DECISIONS.md)** — adopt / borrow / reject / phase-2 map with links into librarification addenda.

| Doc | Focus | Headline verdict (2026-07-16) |
|---|---|---|
| **[00-DECISIONS](./00-DECISIONS.md)** | Unified pointer | Single table for all edge tech + phase checklist |
| [01-doltgres](./01-doltgres/PLAN.md) | Doltgres for versioned package metadata + symbol catalog | **Not** Terminus/ops SoT; **A′/B′ evaluate** post-1.0 (crates.io-page + moniker rows); graph stays 07/20 |
| [02-iroh](./02-iroh/PLAN.md) | iroh INDEX→REGISTRY data plane | **v1 HTTP+S3**; **Phase-2** `BlobTransport` + `IrohBlobsTransport` (§11); subscription stays HTTP |
| [03-edenfs](./03-edenfs/PLAN.md) | EdenFS for source/codebase hosting | **Reject EdenFS** · **CAS+tmpdir** materialize · steal lazy-hydrate ideas · optional later **composefs/EROFS** on Linux |
| [04-ipfs-and-cas](./04-ipfs-and-cas/PLAN.md) | IPFS/IPLD/Filecoin | **Reject public IPFS** · borrow CAS/Merkle already in plan 13 · optional **CID/CAR** at boundary · Bitswap = pattern only |
| [05-surrounding-edge](./05-surrounding-edge/PLAN.md) | Kùzu/Ladybug, SOCI/eStargz, WASM, DoltLite, NATS, … | Keep 22-crate planes · highest ROI: **SOCI-like archive indexes** · optional **LadybugDB** · **WASM pure steps** · **stay qdrant-edge** · no second vector/hot-graph stack |
| [06-compiler-fleet-cache](./06-compiler-fleet-cache/PLAN.md) | k8s compiler cache sharing | **Share L0/L1/L3/L5**; **reject L2** cross-tenant `target`/sccache |
| [07-materializer-archive-index](./07-materializer-archive-index/PLAN.md) | Materializer + SOCI-like pack index | **ADOPT** CasHardlink + LazyArchive MVP; FullExtract fallback; no Eden/FUSE |

**Master plan:** [`LIBRARIFICATION-PLAN.md`](../../LIBRARIFICATION-PLAN.md) freezes edge outcomes as **GD-29..GD-35**.

## Librarification addenda (pointers only — plans not rewritten)

| Plan | Addendum section | Edge inputs |
|---|---|---|
| [13-storage](../librarification/13-storage/PLAN.md) | §12 Edge-tech decisions; **§13 Materializer pointer** | 03, 04, 05, **07** |
| [14-client-sync](../librarification/14-client-sync/PLAN.md) | §15 Edge-tech decisions (transport & CAS network) | 02, 04, 05 |
| [07-terminus-tiering](../librarification/07-terminus-tiering/PLAN.md) | Alternatives considered (edge-tech) | 01, 05, 20 |
| [09-vector](../librarification/09-vector/PLAN.md) | §18 Edge confirmation | 05 |
| [20-graph-over-ir](../librarification/20-graph-over-ir/PLAN.md) | §30 LadybugDB optional acceleration | 05 |
| [22-crate-topology](../librarification/22-crate-topology/PLAN.md) | Optional future crates/backends | 01–05 |
| [12-orchestration](../librarification/12-orchestration/PLAN.md) | § Compiler fleet cache | 06 |
| [11-sqlite-index](../librarification/11-sqlite-index/PLAN.md) | §10.4 A′/B′ | 01 |

## Quick verdict matrix

| Theme | Adopt | Phase-2 / watch | Reject |
|---|---|---|---|
| **Catalog** | SQLite INDEX ops (11) | Doltgres **A′/B′** versioned package+symbol catalog | Doltgres as ops SoT or Terminus |
| **Sync** | Manifest want/have + HTTP/S3; **BlobTransport** | **iroh-blobs** transport | IPFS swarm, iroh-docs, CRDT engines |
| **Materialize** | CAS+tmpdir; **Materializer**; **ArchiveIndex** (07/master) | composefs/EROFS (Linux) | **EdenFS** |
| **Graph** | Terminus hot + IR cold | **Ladybug** feature | Kùzu upstream, dual hot stores |
| **Vectors** | **qdrant-edge** | Lance hatch only | New full stacks from 05 |
| **Fleet cache** | **L0+L1 stage CAS+L3 images+L5** (06) | L4 sticky sessions; per-file stage keys | Shared mutable L2/`target/` across untrusted |

Related: `.research/librarification/` (architecture), `.research/resolution/` (consumer→IR resolve).
