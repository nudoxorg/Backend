# Edge-tech decisions (unified pointer)

> **Status (LIBRARIFICATION-PLAN Rev 2, 2026-07-16):** This document's verdict table is substantially superseded by Rev 2's greenfield freeze. Key changes: iroh-blobs is promoted to sole v1 data plane (GD-30); LadybugDB is promoted to the default embedded desktop graph engine (GD-34); Doltgres is rejected outright — not evaluated (GD-32); composefs/EROFS is cut entirely (GD-35); and CID/CAR export is also cut. The GD crosswalk in §10b remains structurally useful but maps old GD-30/32/34/35 bodies to now-revised decisions — read LIBRARIFICATION-PLAN §0.4 for current frozen text.

**Date:** 2026-07-16  
**Kind:** short decision map — **not** a master architecture rewrite  
**Audience:** assemblers, crate-topology, storage/sync/graph owners  
**Rule:** deep research lives in `01–07`; librarification plans get **addenda only**; master freezes = LIBRARIFICATION-PLAN **GD-29..GD-35**.

| Report | Path |
|---|---|
| Doltgres | [01-doltgres/PLAN.md](./01-doltgres/PLAN.md) |
| iroh | [02-iroh/PLAN.md](./02-iroh/PLAN.md) |
| EdenFS | [03-edenfs/PLAN.md](./03-edenfs/PLAN.md) |
| IPFS / CAS | [04-ipfs-and-cas/PLAN.md](./04-ipfs-and-cas/PLAN.md) |
| Surrounding edge | [05-surrounding-edge/PLAN.md](./05-surrounding-edge/PLAN.md) |
| Compiler fleet cache | [06-compiler-fleet-cache/PLAN.md](./06-compiler-fleet-cache/PLAN.md) |
| Materializer + archive index | [07-materializer-archive-index/PLAN.md](./07-materializer-archive-index/PLAN.md) |
| Index README | [README.md](./README.md) |

**Librarification anchors (addenda landed):**  
[13-storage](../librarification/13-storage/PLAN.md) · [14-client-sync](../librarification/14-client-sync/PLAN.md) · [07-terminus-tiering](../librarification/07-terminus-tiering/PLAN.md) · [09-vector](../librarification/09-vector/PLAN.md) · [20-graph-over-ir](../librarification/20-graph-over-ir/PLAN.md) · [22-crate-topology](../librarification/22-crate-topology/PLAN.md) · [12-orchestration](../librarification/12-orchestration/PLAN.md) (§ Compiler fleet cache)

---

## 1. Verdict key

| Tag | Meaning |
|---|---|
| **ADOPT** | Ship in target path / crate deps |
| **BORROW** | Patterns / formats only — not the product |
| **PHASE-2** | Design interface now; implement after v1 baseline |
| **WATCH** | Revisit when maturity / product need appears |
| **REJECT** | Do not take as core or default path |
| **EVALUATE** | Spike post-gate (e.g. Doltgres 1.0); not core path yet |

---

## 2. Master decision table

| Tech / option | Area | Verdict | One-line rationale | Deep dive | Plan addendum |
|---|---|---|---|---|---|
| **HTTP + presigned S3 CAS** | Sync transport | **ADOPT** (v1) | Server-authoritative multi-tenant bulk path | 02, 14 | 14 §15 |
| **`BlobTransport` trait** | Sync | **ADOPT** interface | Plug HTTP now, iroh later | 02 | 14, 22 |
| **iroh-blobs / `IrohBlobsTransport`** | Sync | **PHASE-2** | BLAKE3 soulmates; LAN/multi-source — not S3 replace | 02 | 14, 22 |
| **iroh-docs / gossip catalog** | Catalog | **REJECT** | CRDT multi-writer wrong for INDEX authority | 02 | 14 |
| **Public IPFS / Kubo swarm** | Distribution | **REJECT** | Legal, pin/GC, gateway decline, small-blob cost | 04 | 13 §12, 14 §15 |
| **Private IPFS Cluster** | Distribution | **WATCH / niche** | Loses to S3+CDN+IAM for INDEX | 04 | 13, 14 |
| **Bitswap as wire** | Sync | **REJECT** | Steal want/have only | 04 | 14 |
| **CID(raw,blake3) / CAR export** | Interop | **PHASE-2 optional** | Boundary export; no Kubo client | 04 | 13, 22 |
| **UnixFS / IPLD codecs SoT** | Storage | **REJECT** day-1 | Path→hash manifests + postcard IR | 04 | 13 |
| **BLAKE3 manifests + `cas/`** | Storage | **ADOPT** | Already plan 13/14 SoT | 04, 13 | 13 |
| **EdenFS** | Materialize | **REJECT** | Unsupported OSS; SCM-keyed; daemon tax | 03 | 13 §12 |
| **CAS + tmpdir / hardlink views** | Materialize | **ADOPT** | Primary compile + desktop path | 03 | 13 |
| **SOCI / eStargz-style `ArchiveIndex`** | Materialize | **ADOPT** (highest ROI) | External TOC + range lazy; no FUSE | 05, **07** | 13 §13, 22 |
| **`Materializer` (CasHardlink + LazyArchive + FullExtract)** | Materialize | **ADOPT** MVP | Real PathBuf trees; L5 hardlinks; pack optional | **07** | 13 §13 |
| **composefs / EROFS** | Materialize | **PHASE-2** Linux pods | Immutable RO generation mounts | 03, 05 | 13, 22 |
| **Doltgres A′ (versioned package metadata catalog)** | Catalog | **EVALUATE** post-1.0 | crates.io-page fields + owners/readme/features; **not** jobs/outbox SoT (SQLite stays) | 01 §9R | 11 §10.4 |
| **Doltgres B′ (versioned symbol/treesitter catalog)** | Catalog | **EVALUATE** after monikers | moniker/kind/path/sig_hash rows + `dolt_diff` across versions; **not** full trees or graph | 01 §9R | 11, 19 |
| **Doltgres as INDEX ops SoT / Terminus** | Ops/Graph | **REJECT** | SQLite+Litestream (11); Terminus hot + cold IR (07/20) | 01 | 07, 11 |
| **DoltHub orgs as multi-tenancy** | Tenancy | **REJECT** | App-level orgs/ACLs | 01 | 07 |
| **DoltLite** | Desktop catalog | **WATCH** α | Versioned SQLite curiosity; not REGISTRY 2026 | 01, 05 | — |
| **Terminus hot + admission** | Graph hot | **ADOPT** | Unchanged plan 07 | 07 | 07 |
| **GraphStore-over-IR cold** | Graph cold | **ADOPT** | Mandatory long-tail | 20 | 20, 07 |
| **LadybugDB** | Graph desktop | **WATCH → optional feature** | Acceleration only; not IR/Terminus replace | 05 | 07, 20, 22 |
| **Upstream Kùzu** | Graph | **REJECT** | Archived | 05 | 07, 20 |
| **Falkor / Neo4j / AGE / … second hot** | Graph | **REJECT** | One hot system | 05 | 07 |
| **qdrant-edge + qdrant-client** | Vectors | **ADOPT** | Stay; no new stack from 05 | 09, 05 | 09 |
| **Lance / USearch / sqlite-vec / SaaS** | Vectors | **REJECT** primary (Lance hatch OK) | Do not open parallel plane | 05, 09 | 09 |
| **Electric / PowerSync products** | Sync engine | **REJECT** / **BORROW** shapes·buckets | Manifest want/have wins | 05, 14 | 14 |
| **WebTorrent primary** | Swarm | **REJECT** | Prefer iroh Phase-2 if peer assist | 05 | 14 |
| **NATS JetStream** | Orch/outbox | **ADOPT optional** | Warm-path only; plan 12 | 05 | — |
| **WASM components** | Compute | **WATCH → pure steps** | Not language oracles | 05 | 22 |
| **ORCH L0 + JobKey stage CAS (L1) + images + L5 hardlinks** | Compile fleet cache | **ADOPT** | Share immutable CAS; no shared mutable L2/`target/` untrusted | 06 | 12 § Compiler fleet cache · master §18.3 · GD-31 |
| **Shared language build caches (sccache/target/GOCACHE)** | Compile L2 | **REJECT** default untrusted | Isolation/poison; RO dep blobs only as L5/image | 06 | 12 · GD-31 |
| **In-process RA warm (L4)** | Compile warm pool | **ADOPT** warm-only | Pod-local; no cross-pod share | 06 | 12 · GD-31 |
| **`BlobTransport` + HTTP v1 / iroh Phase-2** | Sync | **ADOPT** / **PHASE-2** | Interface now; S3 SoT; residual HTTPS | 02 | master §2.8 / §16.4 · GD-30 |

---

## 3. Doltgres reframing (A′ / B′)

**User intent (2026-07-16):** Doltgres as a store for **package metadata** (crates.io-page shape) and **treesitter/symbol catalog rows** — not Terminus, not full INDEX ops.

| Option | Scope | Verdict |
|---|---|---|
| **A′** | Versioned package metadata (description, owners, versions, downloads, license, links, readme, keywords, features, …) | **Evaluate strongly** after Doltgres 1.0 — SQL `dolt_diff` / time-travel on registry crawls |
| **B′** | Symbol catalog (moniker, kind, path, sig_hash, generation) — **not** full tree-sitter Trees | **Evaluate** after plan 19 monikers; commit-per-generation diffs |
| **C** | Full graph replacing Terminus | **Reject** |
| **D** | Jobs / outbox / parse_status SoT | **Reject** — SQLite INDEX (plan 11) |

**Topology:** SQLite = ops SoT · Doltgres = optional catalog service (async dual-write) · Terminus/cold IR = graph · CAS = blobs · desktop = sqlite snapshots only.

See [01-doltgres §9R](./01-doltgres/PLAN.md) and [11-sqlite-index §10.4](../librarification/11-sqlite-index/PLAN.md).

## 4. iroh expansion (what “Phase-2” means)

| Layer | v1 | Phase-2 |
|---|---|---|
| Control plane | HTTP JSON (plan 21) | Same |
| Blob fetch | Presigned GET / INDEX proxy | + `IrohBlobsTransport` multi-provider |
| Catalog / generations | Server `BlobManifest` | **Not** iroh-docs |
| Peer assist | None | LAN desktop↔desktop / office cache; still verify BLAKE3 |
| Notify | Poll / SSE later | Optional gossip “have G” discovery only |

**Spike before productize:** map `ContentHash` ↔ iroh `Hash`; generation closure ↔ HashSeq; numbers vs HTTP range GET.

---

## 5. Materialization stack (locked preference order)

1. **CAS objects + manifest** (always)  
2. **Hardlink / copy-on-read views** and compile **tmpdir** trees  
3. **SOCI-like external archive indexes** (highest ROI lazy path)  
4. **composefs/EROFS** on Linux orch (later)  
5. **App-level virtual tree** in GUI (no kernel FS)  
6. ~~EdenFS / production FUSE desktop~~ **out**

---

## 6. What streamlining does *not* change

- **22-crate planes** (INDEX / REGISTRY / ORCH / SHARED / GUI) stay.  
- **Cold graph-over-IR + hot Terminus** stay.  
- **qdrant-edge** local + Qdrant remote stay.  
- **Manifest want/have** sync stay.  
- Edge tech streamlines **implementations inside crates**, not the DAG.

Optional pieces only: see [22-crate-topology optional backends](../librarification/22-crate-topology/PLAN.md).

---

## 7. Explicit reject list (consolidated)

| Reject | Why |
|---|---|
| EdenFS as product dependency | Unsupported, wrong keying, ops/embed tax |
| Public IPFS as package CDN | Liability + permanence myth + ops |
| iroh-docs as package catalog | Wrong consistency model |
| Doltgres as Terminus or INDEX SoT | Wrong primary model / ops |
| Second hot graph (Neo4j/Falkor/…) | Complexity without tier win |
| Second vector stack | Plan 09 closed |
| CRDT engines as CAS/catalog authority | Wrong mutability model |
| WebTorrent / Syncthing embed | Wrong product surface |
| Moonshot “collapse half the crates” | Score ~0–3/10 in 05 §10 |

---

## 8. Phase / watchlist checklist

| When | Action | Exit |
|---|---|---|
| **Now** | CAS+tmpdir; SOCI-like index design (**07**); `BlobTransport` trait; SQLite INDEX; cold IR graph; qdrant-edge | Specs land in crates |
| **Near** | ArchiveIndex + NdPk + Materializer S-M.1–S-M.7 (07); shape/bucket SubscribeSpec vocabulary | Measured range-lazy wins |
| **Phase-2** | iroh-blobs spike; composefs on Linux pods; optional CID/CAR | Numbers vs HTTP; go/no-go |
| **Watch** | Doltgres 1.0 + A′/B′ product need; Ladybug FFI/license; DoltLite α | Spike or drop |
| **Near (fleet)** | L1 DaemonL3 + CacheScope wiring (06 — **decided**, not stub) | L1 hit metrics |
| **Never default** | Eden, public IPFS, iroh-docs catalog, dual hot graph, dual vector | — |

---

## 9. Surrounding high-ROI (from 05, not duplicated elsewhere)

| Item | Verdict | Plan home |
|---|---|---|
| SOCI/eStargz external indexes | **ADOPT pattern** | 13, 22 |
| LadybugDB | **Optional graph-cold feature** | 20, 07, 22 |
| WASM pure producers | **WATCH→ADOPT** sealed steps | 12, 22 |
| NATS JetStream | **Optional** orch/outbox | 12 |
| Electric/PowerSync | **Borrow words only** | 14 |
| Nessie/Iceberg/LakeFS | **Borrow catalog-commit vocabulary** | 11 publish txn |

---

## 10. Fleet cache (06) — decided

Full ADR: [06-compiler-fleet-cache/PLAN.md](./06-compiler-fleet-cache/PLAN.md). Orch pointer: [12-orchestration § Compiler fleet cache](../librarification/12-orchestration/PLAN.md).

| Layer | Verdict |
|---|---|
| **L0** ORCH/INDEX output CAS | **ADOPT** required |
| **L1** JobKey stage postcard CAS (heart L1/L2 + S3 L3) | **ADOPT** — wire daemon `DaemonL3` |
| **L2** language mutable caches | **REJECT** share across untrusted tenants |
| **L3** toolchain OCI/nix layers | **ADOPT** (already) |
| **L4** in-process warm state | **ADOPT** warm pool only |
| **L5** node-local source hardlink CAS | **ADOPT** |

Prerequisite for fleet L1 hits: toolchain **content** fingerprints (or uniform image digests). Types: `SharedCasConfig`, `CacheScope::{GlobalStage,NodeLocal,Forbidden}`.

---


---

## 10b. Master plan GD crosswalk (edge freezes)

| GD | Freeze |
|---|---|
| **GD-29** | Materializer + ArchiveIndex ADOPT; EdenFS REJECT |
| **GD-30** | BlobTransport ADOPT; HTTP+S3 v1; iroh Phase-2 |
| **GD-31** | Fleet share L0/L1/L3/L5; reject L2 cross-tenant |
| **GD-32** | Doltgres A′/B′ evaluate catalog side-store; not ops/graph SoT |
| **GD-33** | Public IPFS REJECT; optional CID/CAR Phase-2 |
| **GD-34** | Ladybug optional desktop only; cold IR default |
| **GD-35** | composefs/EROFS Phase-2 Linux only |

## 11. Document control

| Field | Value |
|---|---|
| Created | 2026-07-16 |
| Role | Pointer + decision summary for edge-tech session |
| Master integration | LIBRARIFICATION-PLAN.md Part 0 GD-29..35 + §0.6 + §3.5–3.9 / §2.7–2.9 / §16.4–16.6 / §17.5 / §18.3–18.6 / Wave 6 |
| Non-goals | Replace master plan; implement code; procurement sign-off |
| Update rule | When a sibling PLAN flips a verdict, update §2 table + master GD note + linked addendum in the same PR/session |
| 07 status | **ADOPT** — [07/PLAN.md](./07-materializer-archive-index/PLAN.md) landed; master §3.5–3.8 + GD-29 are program freeze; implement from 07 types |
