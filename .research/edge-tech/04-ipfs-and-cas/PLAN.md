# IPFS / IPLD / Filecoin & Content-Addressed Networks for nudox Blob Storage

> **Status (LIBRARIFICATION-PLAN Rev 2, 2026-07-16):** The public-IPFS and iroh-blobs-as-Phase-2 verdicts in §0.2 are partially superseded: iroh-blobs is now the **v1 production data plane** (not Phase-2, GD-30), so the "iroh Phase-2 optional multi-source" framing is obsolete. The CID/CAR export option (was "Phase-2 optional") is **cut** in Rev 2 — no public IPFS/Bitswap/UnixFS at any phase (GD-33). All other rejects in this doc remain in force.

**Research date:** 2026-07-16  
**Status:** 2026 reality check (not 2018 hype)  
**Scope:** Whether nudox should put packages on the public IPFS network, run a private IPFS Cluster, or only borrow ideas (CID mapping, IPLD codecs, Bitswap-style want/have, CAR packaging).  
**Cross-links:** Plan 13 storage (IPLD/Merkle-DAG mention), Plan 14 client-sync (Bitswap as prior art), dedicated iroh report at `.research/edge-tech/02-iroh/`.  
**Codebase anchors:** `workspace/heart/content.rs` (BLAKE3 `ContentHash`), `workspace/registry/blob/mod.rs` (`BlobManifest`), INDEX object_store, REGISTRY disk CAS.

---

## 0. Executive summary (read this first)

### 0.1 One-paragraph verdict

**Borrow formats and CAS ideas; reject the public IPFS network as primary distribution for nudox packages; treat private IPFS Cluster as a niche enterprise option that loses to S3+CDN+presign for INDEX; keep Filecoin as optional cold archive only if a paying enterprise customer demands it.** nudox already has the right architecture: BLAKE3 CAS + generation manifests + want/have missing hashes. IPFS/IPLD in 2026 is a mature, better-specified ecosystem than 2018, but its network, GC, legal, and multi-small-blob failure modes do not match a package registry for untrusted third-party source and IR.

### 0.2 Decision matrix (quick)

| Option | Verdict for nudox | Why |
|---|---|---|
| **Public IPFS network** as INDEX backend | **Reject** | Permanence ≠ pin; DHT/provider churn; free-gateway dependency dying; legal liability hosting untrusted source; SHA-256 default vs BLAKE3 first-class; latency for millions of small blobs |
| **Private IPFS Cluster** for enterprise INDEX | **Defer / niche** | Real pin orchestration; still Go daemons + ops surface; S3 multi-region + IAM is simpler and enterprise-standard |
| **Filecoin retrieval markets** for cold packages | **Optional cold tier only** | 2026 strategy is paid onchain demand; retrieval still lukewarm/cold; crypto UX wrong for default desktop product |
| **Adopt CID / multihash mapping of BLAKE3** | **Optional interop layer** | Multihash `0x1e` = blake3 exists; map `ContentHash` → CIDv1(raw) if external tools need CIDs |
| **Adopt CAR as transfer pack** | **Maybe later** | Good bulk container for verifiable streams; not required if custom seekable-zstd packs work |
| **Adopt UnixFS trees** | **Reject as primary** | Chunking/layout change CIDs; breaks simple file-level BLAKE3 identity; we want path→hash manifests, not UnixFS HAMT |
| **Adopt IPLD codecs for IR graphs** | **Reject day-1** | postcard + BLAKE3 leafs already form a shallow Merkle DAG; dag-cbor is interoperable but not free in Rust product path |
| **Adopt Bitswap / Graphsync as wire** | **Reject; steal pattern** | Custom HTTP want/have is simpler; HTTP trustless gateway pattern is the 2025–2026 winner for verifiable fetch |
| **Adopt pure CAS ideas (no network)** | **Yes — already planned** | Plan 13/14: generation_hash ≈ root; cas/{blake3}; fetch missing hashes |

### 0.3 Recommended stance in one line

**IPFS-class content addressing without the public swarm: keep BLAKE3 CAS + manifests; optionally emit CIDv1(raw, blake3) for interop; optionally package cold transfers as CAR; never require Kubo on the client; never publish untrusted package source to the open Amino DHT as the product default.**

---

## 1. Product context: what nudox already is

### 1.1 Content model (from Plan 13)

nudox stores per package **generation**:

| Artifact | Role | Access |
|---|---|---|
| Source files | GUI views, re-parse, audit | Random single-file; bulk sync |
| Tree-sitter “resolved trees” | Snippets / go-to-impl | Hot path — **not** persisted as opaque C trees |
| Typed IR | Search, symbols, lineage | Whole-blob + index fan-out |
| Extracted references | Cross-file without CST | With IR or on demand |

Design goals ranked: integrity under BLAKE3 → cross-version dedupe → sync-friendliness → bounded memory → local footprint → single-file random access.

### 1.2 Dual home

| Layer | Backend | Pattern |
|---|---|---|
| **Remote INDEX** | S3-like `object_store` + catalog | Immutable `cas/{blake3}` + manifests |
| **Local REGISTRY** | Disk CAS + sqlite | Pin/LRU GC; progressive fill from INDEX |
| **Sync** | Want/have missing hashes | Manifest-driven set difference (Nix/Git/IPFS-shaped) |

Plan 14 explicitly lists IPFS Bitswap as prior art for want/have, and states it is **relevant only if peer-to-peer edge caches appear; not v1**.

Plan 13 §2.8 already says:

> A generation manifest whose entries are BLAKE3 hashes **is** a shallow Merkle DAG… We do not need full IPLD codecs or CIDs day one; we need the **invariant**: root hash commits the entire generation, and leaves can be fetched/verified independently.

This report stress-tests that claim against 2026 IPFS reality.

### 1.3 The question restated

Should package blobs live **on** IPFS (public or private), or should nudox remain a **CAS product that happens to share conceptual DNA with IPLD**?

Answer up front: **conceptual DNA only**, with a thin optional interop layer.

---

## 2. IPFS status in 2026: org, funding, implementations

### 2.1 Organizational map (important for longevity bets)

The 2015–2022 story was “Protocol Labs builds everything.” By 2024–2026 the stack has **forked into specialized entities**:

| Entity | Role (2026) | Notes |
|---|---|---|
| **Protocol Labs** | Network / research incubator; historical parent of IPFS & Filecoin | Still funds some ecosystem work; not day-to-day Kubo shipping |
| **IPFS Foundation** | Nonprofit for the IPFS project (est. 2024 after ~10 years at PL) | Stewardship, grants, narrative; [ipfsfoundation.org](https://ipfsfoundation.org/) |
| **Interplanetary Shipyard** | Independent maintainers of Kubo, Helia, Boxo, Rainbow, gateways, Desktop, Companion | Primary engineering velocity for “IPFS that works on home hardware”; [ipshipyard.com](https://ipshipyard.com/) |
| **Filecoin Foundation / FIL ecosystem** | Storage markets, FVM, Onchain Cloud | Adjacent but economically separate |

**Implication for nudox:** betting on “Protocol Labs will keep free public infrastructure forever” is already wrong. Public gateways, pinning startups, and vendor APIs have been **sunsetting or rate-limiting** (Infura public IPFS API/gateway deprecation historically; Scaleway pinning shutdown; Cloudflare public gateway sunset; Fleek hosting pivot/discontinuation narratives in 2025–2026 ecosystem posts). Shipyard’s own “Post Gateway World” thesis is that **central free gateways are unsustainable**.

Sources:

- Shipyard 2025 year-in-review: https://ipshipyard.com/blog/2025-shipyard-ipfs-year-in-review/
- Post-gateway strategy: https://ipshipyard.com/blog/2025-a-post-gateway-world/
- Shipyard independence: https://ipshipyard.com/blog/shipyard-hello-world/
- IPFS Foundation about: https://ipfsfoundation.org/about/

### 2.2 Implementations that matter

#### Kubo (Go; formerly go-ipfs)

- Dominant full-node implementation; still the reference for DHT participation, pinning, Desktop.
- **2025 shipped seven major releases** (v0.33–v0.39): Provide Sweep (DHT provider scalability), AutoTLS, HTTP retrieval default alongside Bitswap, Bitswap broadcast reduction, AutoNATv2, UPnP self-healing, gateway resource limits, Pebble datastore option.
- Theme: **self-hosting on residential hardware is finally viable**; old DHT provide path could not announce more than ~5k CIDs before vanishing from DHT; Sweep claims large reductions in lookup load and smooth provides for hundreds of thousands of CIDs.

Source: https://ipshipyard.com/blog/2025-shipyard-ipfs-year-in-review/

#### Helia (JS; successor to js-ipfs)

- Modular browser/Node library ecosystem.
- `@helia/verified-fetch` = `fetch()`-shaped API that verifies CIDs client-side.
- Service-worker gateway (`inbrowser.link`) for trustless browser retrieval without central gateway trust.

#### Boxo (Go library)

- Component library powering Kubo and Rainbow gateways.
- Gateway code lives in Boxo; conformance tests against specs.
- HTTP-based downloading / webseeds work in Boxo is the industrial direction for large-block retrieval.

Repo: https://github.com/ipfs/boxo

#### Rainbow + Someguy

- **Rainbow**: focused HTTP gateway binary (not a full Kubo kitchen sink).
- **Someguy**: delegated routing HTTP API (Routing V1) so light clients avoid full DHT.

#### ipfs-cluster

- Pinset orchestration across a fleet of Kubo (or compatible) daemons.
- Separate private libp2p swarm with `cluster_secret`; CRDT or Raft consensus for global pinset.
- Still the open-source answer to “replicate these CIDs across N nodes with RF=k.”

Docs: https://ipfscluster.io/documentation/deployment/architecture/

### 2.3 What “IPFS is healthy” means in 2026

**Healthy:**

- Specs are better (UnixFS formalized, Kademlia DHT formalized, trustless gateway mature).
- HTTP is first-class for retrieval (not an embarrassing gateway hack).
- Browser verification path exists without trusting `ipfs.io`.
- Self-host DHT provide is less pathological.

**Still hard / not solved:**

- Content permanence without explicit pins or paid pinning/storage deals.
- Public-good free bandwidth.
- Homogeneous performance for **millions of tiny blobs** (package source trees).
- First-class BLAKE3 as the *default* hash (still SHA-256 culture).
- Rust-native full IPFS stack (see §8).

### 2.4 Funding reality check

Do not assume unlimited PL runway for free global CDN. Shipyard solicits sponsorships; public gateways are being **migrated off** as free CDN. For a commercial code-intelligence product, **you will pay for storage + egress** whether that is S3, a pinning vendor, or Filecoin deals. Decentralization does not cancel the bill — it moves who you pay and who you trust for availability.

---

## 3. Addressing: CIDs, multihash, BLAKE3, UnixFS, CAR, trustless retrieval

### 3.1 CID anatomy (CIDv0 vs CIDv1)

A **CID is not a bare file hash**. Per IPFS docs ([content addressing](https://docs.ipfs.tech/concepts/content-addressing/)):

1. Hash the **block** (which may be a UnixFS wrapper / DAG node, not raw file bytes).
2. Combine with multicodec (how to interpret the block).
3. Multihash describes algorithm + digest.
4. Multibase only in the **string** form.

| Version | Appearance | Defaults | Notes |
|---|---|---|---|
| **CIDv0** | `Qm…` base58btc, 46 chars | Implicit dag-pb + sha2-256 | Legacy; still common |
| **CIDv1** | `bafy…` / `bafk…` base32 | Explicit version + codec + multihash | Preferred for browsers/subdomains; required for non-dag-pb |

**Critical for nudox:** even when the multihash uses BLAKE3 of file bytes, a **UnixFS-wrapped** add produces a **different CID** than a raw-leaf single-block CID of the same bytes. Chunk size, DAG layout, codec, and hash algorithm all change the root CID for “the same file.”

Docs explicitly: “Same file, different CIDs” — chunking, layout, codec, CID version, hash algorithm.

### 3.2 Multihash and BLAKE3

| Item | Value |
|---|---|
| Default IPFS hash | **sha2-256** |
| BLAKE3 multihash code | **`0x1e`** (draft in multicodec table; implemented in go-multihash / go-verifcid path since ~2022) |
| Kubo allowlist history | Issues to allow blake3 in verifcid/kubo (`ipfs/kubo#8650`, `ipfs/go-verifcid#13`) |
| Practical status 2026 | **Supported in multiformats tooling**; **not** the network default; interoperability depends on peers accepting the multihash code |

Sources:

- https://docs.ipfs.tech/concepts/content-addressing/
- https://github.com/ipfs/kubo/issues/8650
- https://github.com/ipfs/go-verifcid/issues/13
- multicodec table: blake3 = 0x1e

**nudox implication:** our primary key is **raw BLAKE3 of logical bytes** (`ContentHash`). That is cleaner for package integrity than “whatever UnixFS layout Kubo used today.” If we need a CID:

```
CIDv1 = multibase( base32, 1 || multicodec(raw=0x55) || multihash(0x1e || 32 || blake3_digest) )
```

This maps **1:1** when:

- codec = `raw` (0x55), and
- the blob fits in one block / is not chunked into a UnixFS tree, and
- digest = our existing BLAKE3.

If we ever chunk large blobs for IPFS-compat, **the root CID stops equaling our ContentHash** — which is a deal-breaker for dual-identity systems. Prefer **never dual-identity**: either externalize as raw single-block CIDs, or keep BLAKE3 only and treat CID as a derived export format.

### 3.3 UnixFS

UnixFS is the file/directory format used by `ipfs add`, MFS, most public content. Formal spec landed (Shipyard 2025: UnixFS specification at specs.ipfs.tech).

Properties:

- Files become DAGs of blocks (typically 256 KiB–1 MiB chunks historically; parameters vary).
- Directories become dag-pb listings or HAMT-sharded structures for large dirs.
- Excellent for “share a folder with a root CID.”
- **Poor fit for nudox primary storage:**
  - Cross-version dedupe is at **block** grain of UnixFS, not at our **logical file** grain, unless we carefully control chunking.
  - Directory HAMT rewrites large subtrees when one file changes — opposite of “manifest lists N independent hashes.”
  - Single-file random access is “traverse path in DAG,” not “GET cas/{hash}.”

**Verdict:** do not store package trees as UnixFS roots in INDEX. A generation manifest of path→blake3 is strictly better for package registries.

### 3.4 CAR files (Content Addressable aRchives)

CAR is an IPLD transport format: a header with roots + a sequence of (CID, block) pairs. Media types:

- `application/vnd.ipld.car`
- `application/vnd.ipld.raw` (single block)

Used heavily by **trustless gateways**, Filecoin deals, bulk export/import, and Helia/Kubo tooling.

Strengths for nudox:

- Self-describing verifiable bulk transfer of a generation’s blocks.
- Streaming-friendly; clients verify each block’s multihash.
- Ecosystem tooling (go-car, rust car crates exist at varying maturity).

Weaknesses:

- Block identity is CID-based; to use CAR natively we either (a) wrap BLAKE3 as CIDv1 raw, or (b) invent a non-IPLD CAR-like container (our seekable-zstd pack).
- CAR byte streams are **not necessarily deterministic** across gateways (order/dups parameters matter; trustless gateway spec is careful about this).

**Verdict:** **optional export/import and cold transfer format**, not the local CAS layout. Local CAS stays flat `cas/{blake3-hex}`.

### 3.5 Trustless HTTP gateway (the 2025–2026 winner)

Spec: https://specs.ipfs.tech/http-gateways/trustless-gateway/ (updated 2026-03-05 in crawl).

Core idea: client requests **verifiable** bytes, not gateway-deserialized HTML:

```
GET /ipfs/{cid}?format=raw
Accept: application/vnd.ipld.raw

GET /ipfs/{cid}?format=car&dag-scope=entity
Accept: application/vnd.ipld.car
```

Client verifies multihash of every block. Gateway cannot silently tamper with content-addressed data (it can still withhold, serve incomplete CAR streams, or observe requests).

Important knobs:

| Param | Meaning |
|---|---|
| `dag-scope=block\|entity\|all` | How much of the DAG to stream |
| `entity-bytes=from:to` | Trustless range equivalent for UnixFS files |
| `car-order`, `car-dups` | Determinism / streaming tradeoffs |
| Block size guidance | Clients SHOULD limit blocks to **2 MiB** (Bitswap ecosystem-safe size) |

Kubo 0.36+ enables **HTTP retrieval by default** alongside Bitswap — meaning nodes fetch verifiable blocks over HTTPS from providers that speak the trustless API, including generic CDNs that store raw blocks.

**nudox mapping:** our planned:

```
GET /v1/cas/{blake3} → bytes
POST /v1/cas/has → missing subset
```

is the **same trust model** without CID/codec ceremony:

- Server is untrusted for integrity (client verifies BLAKE3).
- Server is trusted for **availability** (or multi-sourced).
- HTTP/CDN is the transport.

We do not need Bitswap to get “trustless retrieval.” We need **hash verification + immutable URLs**.

### 3.6 Addressing recommendation for nudox

| Layer | Choice |
|---|---|
| Canonical identity | `ContentHash` = BLAKE3-256 of logical bytes |
| Path layout | `cas/{lowercase-hex}` (existing Plan 13) |
| Optional interop | `cid_v1_raw_blake3(hash)` for tools that only speak CID |
| Optional bulk | CAR of raw CIDv1 blocks **or** nudox seekable pack |
| Do not use as primary | UnixFS directory roots for packages |

---

## 4. IPLD codecs & graphs — fit for package manifests / IR?

### 4.1 What IPLD actually is

IPLD (InterPlanetary Linked Data) is a **data model + codec + linking** stack for Merkle-DAGs:

- Links are CIDs.
- Codecs define serialization (dag-cbor, dag-json, dag-pb, raw, …).
- Schemas / selectors exist for typed traversal and Graphsync-style requests.

Resources: https://ipld.io/docs/codecs/

### 4.2 Common codecs

| Codec | Code | Use | Fit for nudox |
|---|---|---|---|
| **raw** | 0x55 | Opaque bytes | Best for CAS leaves |
| **dag-pb** | 0x70 | UnixFS legacy protobuf | Avoid for structured IR |
| **dag-cbor** | 0x71 | Full IPLD model, binary, recommended | OK for **interop manifests** if we ever export |
| **dag-json** | 0x0129 | Human-readable links | Debug only |
| **dag-jose** | — | Signed envelopes | Overkill vs ed25519 over generation_hash |

DAG-CBOR is the “good default” for structured IPLD. It is **not free** for nudox: we already use **postcard** for internal `BlobManifest` and IR; dual codecs mean dual bugs.

### 4.3 Graphs for packages

A nudox generation is naturally:

```
GenerationRoot (generation_hash)
  ├── Manifest (manifest_hash) → list of (path, content_hash, size, codec)
  ├── File leaves (content_hash) × N
  ├── IR blob (ir_hash)
  └── References blob (refs_hash)
```

This **is** a Merkle DAG. IPLD would represent the same with CID links. Benefits of full IPLD:

- Cross-system traversal (any IPLD tool can walk the graph).
- Selectors for partial graph sync (Graphsync).
- Shared tooling (explorers, car-utils).

Costs:

- CID encoding tax on every link.
- Codec lock-in or multi-codec support.
- Graphsync complexity (see §5).
- Mental model split for Rust engineers who just want BLAKE3 keys.

Plan 13 already mapped:

| IPLD concept | nudox |
|---|---|
| Root CID | `generation_hash` |
| Linked blocks | `cas/{blake3}` leaves |
| Structural sharing | unchanged files share hashes across generations |

### 4.4 IR as IPLD graph?

Tempting idea: store IR as dag-cbor nodes with CID links between symbols. Related discussion in Plan 06 (symbol identity industrial) treats IPLD as a **pattern for identity**, not a mandate to use IPLD codecs.

**Reject for day-1 IR storage:**

1. IR is loaded as wholes or large sections for search/index build — not traversed like a web of tiny IPLD nodes.
2. postcard + zstd dictionaries (Plan 13) optimize size/speed for Rust.
3. Partial symbol graphs are better as **explicit graph DB / Terminus** (product architecture) than as IPLD selectors over S3.
4. Changing one symbol would rewrite parent CIDs — fine for purity, painful for “update one field in a 50MB IR blob” unless we design a prolly/tree structure (Dolt-like) — which is a different research track (see doltgres report).

**Adopt as idea:** content-address leaves; root commits children; lineage = new root + shared leaves. That is already the design.

### 4.5 Manifest codec policy

| Audience | Format |
|---|---|
| Internal wire / store | postcard `BlobManifest` (existing) |
| Sync-facing debug | JSON view (Plan 13 sketch) |
| Optional external IPLD export | dag-cbor document with CID links to raw blake3 blocks — **only if a partner requires it** |

Do not make dag-cbor the source of truth.

---

## 5. Sync protocols: Bitswap, Graphsync, trustless HTTP vs custom want/have

### 5.1 Bitswap

Classic IPFS block exchange:

1. Peer announces wants (CIDs).
2. Peers who have blocks send them.
3. Integrity via multihash.

2025 improvements (Kubo): broadcast reduction (track responsive peers; cut broadcast 80–98%, bandwidth 50–95%); HTTP retrieval as co-equal path.

**Why not Bitswap for nudox v1:**

- Requires libp2p stack on client (heavy for desktop app; worse for WASM).
- Peer discovery / DHT / NAT — solved pain for IPFS, pure cost for a registry with a known INDEX origin.
- Want lists of **millions of small CIDs** stress memory and protocol chatter; package trees are exactly this workload.
- Plan 14 already deferred Bitswap to “P2P edge caches someday.”

**What to steal:** the **semantic** of want/have, not the protocol.

### 5.2 Graphsync

IPLD graph synchronization with **selectors** — request a subgraph in one message. Used historically in Filecoin data transfer stacks.

Status reality:

- Spec still incomplete in places (ipld.io Graphsync page notes incompleteness).
- Powerful for deep DAGs; overkill for flat CAS of whole blobs listed in a manifest.
- Go implementations exist; Rust is sparse/niche.
- Trustless CAR-over-HTTP largely **displaced** the need for Graphsync for many HTTP clients (“CAR as transport” talks since ~2023).

**Verdict:** do not build Graphsync. If we need partial DAG later, use **manifest sections + HTTP range/pack**, not IPLD selectors.

### 5.3 Trustless gateway / HTTP providers

As of 2025–2026, the pragmatic stack is:

1. Resolve providers (delegated routing HTTP, DHT, or **hardcoded INDEX**).
2. Fetch raw/CAR over HTTPS.
3. Verify hashes locally.

For nudox, step 1 collapses to “INDEX is the provider” (plus future mirrors). Steps 2–3 are exactly our CAS GET + BLAKE3 verify.

### 5.4 Custom want/have (recommended)

From Plan 13/14:

```
client:  HAVE { local ContentHash set for generation G }
client:  WANT { hashes in Manifest(G) \ HAVE }
server:  stream blobs for WANT
client:  verify blake3; stage; commit generation atomically
```

HTTP sketch:

```
GET  /v1/packages/{id}/generations/{g} → GenerationManifest
POST /v1/cas/has   body: [hash…] → missing subset
GET  /v1/cas/{hash} → bytes
GET  /v1/packs/{generation_hash} → optional bulk pack
```

Comparison:

| Property | Bitswap | Graphsync | Trustless GW | nudox HTTP CAS |
|---|---|---|---|---|
| Integrity | Multihash | Multihash | Multihash | BLAKE3 |
| Discovery | DHT/peers | Peers | Routing V1 / known host | INDEX catalog |
| Partial sync | Per-block wants | Selectors | dag-scope / entity-bytes | Manifest set diff |
| Client weight | High (libp2p) | High | Medium (HTTP+verify) | Low (HTTP+verify) |
| CDN cache | Hard | Hard | Good for immutable | **Excellent** |
| Enterprise auth | Awkward | Awkward | Doable | **Native (OIDC/IAM)** |

### 5.5 When P2P becomes interesting

Only if:

- LAN multi-device REGISTRY sharing without cloud,
- air-gapped enterprise mesh,
- or peer-assisted CDN for popular public packages.

Even then, evaluate **iroh** (dedicated report) or plain **HTTP multi-mirror** before Bitswap. Do not start with public Amino DHT.

---

## 6. Private clusters / IPFS Cluster / dual for enterprise INDEX

### 6.1 What IPFS Cluster does well

- Global pinset with replication factor.
- Orchestrates many Kubo nodes so pins land on enough peers.
- CRDT consensus recommended for dynamic membership.
- Cluster control plane is a **private** libp2p network (`cluster_secret`); IPFS daemons underneath may be private swarm or public.

Architecture: https://ipfscluster.io/documentation/deployment/architecture/

### 6.2 Private IPFS network pattern

Classic enterprise tutorial pattern:

1. Shared swarm key so nodes only talk to each other.
2. Disable public bootstrappers.
3. Cluster for pin RF.
4. Internal gateway for apps.

This gives content addressing + multi-node replication **without** publishing to the world.

### 6.3 Why this is usually the wrong enterprise INDEX for nudox

Enterprise buyers already understand:

- S3 / GCS / Azure Blob
- IAM, CMEK, bucket policies, PrivateLink
- CloudFront / Fastly / Cloudflare CDN
- Presigned URLs, audit logs, Object Lock

IPFS Cluster gives:

- Go daemon zoo (Kubo × N + cluster-service × N)
- Pin vs GC semantics operators must learn
- CID-centric tooling foreign to backend teams
- Weaker story for **fine-grained authz** on package ACLs (private packages)
- Still need a **catalog DB** (Cluster is not a package index)

You can put CIDs in Postgres and pins in Cluster — but then you have invented **S3 with extra steps**, unless you truly need multi-party pinset consensus across untrusted orgs.

### 6.4 When private IPFS Cluster *does* make sense

- Customer mandate: “all artifacts on IPFS” (government / research consortium).
- Multi-org pinset where no single party owns object storage.
- Existing Kubo investment and SRE skill.
- Dual-publish: S3 primary, Cluster pin mirror for partners.

### 6.5 Dual INDEX design (if ever)

```
[Compiler / ingest]
       │
       ▼
  BLAKE3 CAS write ──► S3 primary (authoritative)
       │
       ├─► optional: derive CIDv1 raw, pin via Cluster API
       └─► catalog row: blake3, optional cid, size, package_id
```

Clients always prefer S3/CDN URLs. CID is metadata for partners.

**Default enterprise INDEX:** S3-compatible + CDN + presign. Cluster optional SKU.

---

## 7. Filecoin / retrieval markets — cold package archive economics?

### 7.1 What Filecoin is in 2026

Filecoin is a **storage market with cryptographic proofs** on top of content-addressed data (IPFS-related stack). Network claims ~exbibyte-scale capacity. 2026 official strategy shifts from supply growth to **paid onchain demand**, Filecoin Onchain Cloud, warm storage services, stablecoin payments, flagship institutional clients (archives, research, AI datasets).

Source: https://filecoin.io/blog/the-2026-filecoin-network-strategy

### 7.2 Retrieval reality

Industry analysis consistently classifies Filecoin as **cold / lukewarm**:

- Storage deals + proofs ≠ low-latency CDN.
- Retrieval markets historically under-delivered vs storage; ongoing work (Retriev-style incentive experiments, Onchain Cloud warm storage).
- Fine for “keep a sealed copy of crates.io snapshot for 5 years.”
- Poor for “desktop GUI open `src/lib.rs` from serde 1.0.210 in 20ms.”

### 7.3 Economics for nudox packages

| Tier | Tool | Economics |
|---|---|---|
| Hot IR/source for active packages | S3 + CDN | $$$ egress predictable; ms latency |
| Warm multi-region | S3 IA / GCS Nearline | Lower storage, higher restore |
| Cold compliance archive | Glacier / Filecoin deal | Cheapest storage; minutes–hours retrieval |

Filecoin only wins if:

1. Customer requires decentralized durability proofs, or
2. Multi-decade archive with adversarial deletion resistance, or
3. Grant-funded / web3-native deployment.

Crypto token UX (FIL wallets, deal brokers, SP selection) is **hostile** to default SaaS packaging.

### 7.4 Verdict

**Do not couple nudox core to Filecoin.** Optional connector: export generation CAR → storage deal → store deal IDs in catalog for disaster recovery. Not a sync path for REGISTRY fill.

---

## 8. Rust ecosystem: rust-ipfs, iroh, libp2p-rs

### 8.1 rust-ipfs

- Original `rs-ipfs/rust-ipfs` is **archived / unmaintained** (community discussion 2024).
- Forks exist (`dariusc93/rust-ipfs`, crates.io package updates) but **not** a production peer to Kubo.
- Equilibrium Labs and others attempted full Rust IPFS historically; none is the default industry stack in 2026.

Forum: https://discuss.ipfs.tech/t/status-of-rust-ipfs/18080

**Implication:** embedding a full IPFS node in the nudox Rust desktop client via rust-ipfs is a **research project**, not a dependency choice.

### 8.2 iroh (cross-link; keep short)

n0’s **iroh** is often positioned as “IPFS done right” — carefully:

- Roots in IPFS-adjacent ideas (content addressing, P2P).
- Explicit **new direction**: not wire-compatible with Kubo/Amino as a full implementation.
- Beetle was the IPFS-compat experiment; maintenance deprioritized relative to iroh core.
- Focus: reliable hole-punched connections, blobs, docs/sync apps, production connection scale.

See dedicated report: `.research/edge-tech/02-iroh/`.

**For this report:** if nudox ever wants **P2P edge sync**, evaluate iroh **before** Bitswap/Kubo. Do not assume iroh gives free access to the public IPFS DHT.

### 8.3 rust-libp2p

- Production-grade networking stack (tokio-only migration complete; QUIC/WebRTC/etc.).
- Used by many non-IPFS projects.
- 2025 annual report frames privacy/confidential networking focus into 2026.
- Building “just enough” protocols on rust-libp2p is feasible but **still a product** — not a free Bitswap client with UnixFS.

Source: https://libp2p.io/reports/annual-reports/2025/

### 8.4 Practical Rust path for IPFS interop (if needed)

| Need | Approach |
|---|---|
| Speak CID/multihash | `cid`, `multihash`, `multibase` crates |
| Verify BLAKE3 multihash | `blake3` + multihash wrapper |
| Read/write CAR | `iroh-car` / `forest_car` / community car crates (evaluate maintenance) |
| Fetch from trustless gateway | `reqwest` + verify — **no libp2p** |
| Full swarm node | Shell out to Kubo or run sidecar — avoid in-process |

**Recommended Rust interop stack for optional export:**

```
blake3 → multihash(0x1e) → cid::Cid v1 raw → optional car write
```

No Kubo required.

---

## 9. Failure modes (why “just put it on IPFS” fails for packages)

### 9.1 Content permanence myth

**IPFS does not store forever.** Unpinned blocks are GC’d. When the last pin disappears, content vanishes from the network even if CIDs still circulate on NFT metadata and README badges.

Operational truth:

- Pin locally, or
- Pay a pinning service, or
- Make a Filecoin deal, or
- Run Cluster RF≥N.

nudox INDEX **must** treat permanence as **our S3 lifecycle + backups**, not “the DHT will provide.”

### 9.2 Garbage collection vs package GC

| System | GC unit | Pin unit |
|---|---|---|
| IPFS | Block / DAG | Recursive pin on root CID |
| nudox REGISTRY | BLAKE3 blob | Package generation pin + LRU |
| nudox INDEX | Object lifecycle policies | Catalog reference counts |

Mapping GC is non-trivial: pinning a UnixFS root pins the whole tree; nudox wants **per-blob refcounts** across generations (file shared by 50 versions stays until all generations GC). Flat CAS + refcount is simpler than recursive pin math on DAGs.

### 9.3 Legal liability hosting untrusted package source

Public IPFS nodes that **provide** content advertise and serve bytes to strangers. Hosting a public package INDEX that re-provides arbitrary open-source is similar to crates.io/npm risk **plus**:

- Harder takedown (many peers).
- Association with whatever else is on the same node/gateway (phishing, malware, CSAM historical abuse of public gateways).
- Enterprise customers **will not** accept “we pinned your proprietary monorepo IR to a DHT.”

Even for open source, **nudox should serve from controlled infrastructure** with ToS, abuse pipelines, and the ability to stop providing a hash without “fighting the permanent web” mythology.

Integrity hashing does not equal distribution rights.

### 9.4 Performance: millions of small blobs

Package ecosystems are worst-case for classic IPFS:

- crates.io-scale: huge numbers of small source files.
- DHT provider records do not scale to “every file is a CID” without aggregation.
- Bitswap wantlists explode.
- Blockstore random reads of 2–4KB objects punish spinning disks; even SSD + DHT latency is worse than S3+CDN for hot paths.
- Kubo Provide Sweep improved large pinsets, but a **registry** still wants hierarchical manifests and bulk packs.

nudox design already mitigates:

- Generation manifest lists hashes (one root).
- Optional transfer pack for cold first sync.
- Per-file CAS for dedupe and random access.
- CDN on immutable cas/* keys.

Do not replace that with per-file DHT provides.

### 9.5 Gateway centralization & rate limits

Shipyard traffic analysis (May 2025 sample): public gateways saw huge automated client load (~67% requests) treating gateways as free CDN. Migration plan: rate-limit backends, push browsers to inbrowser.link / verified-fetch.

If nudox clients used `ipfs.io` to fetch packages, we inherit:

- Rate limits / 429s
- Uncertain SLA
- Privacy leakage (gateway sees request patterns)
- Political dependency

### 9.6 Hash algorithm impedance

Network culture = sha2-256. Our culture = BLAKE3. Dual-hashing every blob (store both) wastes CPU and confuses identity. Mapping only at boundaries is fine; dual canonical IDs are not.

### 9.7 Operator complexity

Running Kubo+Cluster well is a specialty. nudox SRE should optimize Postgres + object_store + CDN. Every additional daemon is paging risk.

### 9.8 Determinism / reproducibility traps

`ipfs add` parameters change root CIDs. CIDs-as-package-ids without pinned profiles cause “same source, different identity” disasters. Our BLAKE3-of-logical-bytes avoids that class of bug.

---

## 10. Comparison: S3+CDN+presign vs public IPFS vs private IPFS vs pure CAS ideas

### 10.1 Feature matrix

| Dimension | S3+CDN+presign | Public IPFS | Private IPFS Cluster | Pure CAS ideas only (nudox plan) |
|---|---|---|---|---|
| Integrity | Client hash check (app-level) | Built-in multihash | Built-in | BLAKE3 native |
| Availability SLA | Cloud vendor | Best-effort peers | Your RF pins | Cloud vendor |
| Latency (hot) | Excellent | Variable | Good LAN / mediocre WAN | Excellent |
| AuthZ / private pkgs | Mature IAM | Hard | Network isolation | Mature IAM |
| Cost model | Clear $ | Hidden (your pins + egress) | Your hardware | Clear $ |
| Multi-region | Built-in | Organic | DIY | Built-in |
| Client simplicity | HTTP | libp2p or gateway | gateway/HTTP | HTTP |
| Dedupe | Key = hash | Block/CID | Block/CID | Key = hash |
| Offline desktop | Sync then local | Pin local node | Sync then local | REGISTRY design |
| Legal control | High | Low | High | High |
| Ecosystem cool factor | Low | High | Medium | Medium (Nix-like) |
| Fit for nudox INDEX | **Primary** | Poor | Niche | **Architecture** |
| Fit for REGISTRY local | N/A | Optional daemon | N/A | **Primary** |

### 10.2 Cost sketch (qualitative)

Assume public open-source INDEX traffic similar to docs.rs / crates.io CDN:

- **S3+CDN:** pay storage + egress; cache hit ratio high because hashes immutable.
- **Public IPFS:** still pay for **your** highly-available pinset (or pinning SaaS); clients who use public gateways free-ride on someone else until rate-limited; popular packages may be helped by community pins — **unreliable** for product SLA.
- **Cluster:** fixed cost of N nodes; good if already colocated with other services; bad if you wanted serverless.

### 10.3 Security comparison

| Threat | S3+CDN | Public IPFS | Private Cluster | Pure CAS HTTP |
|---|---|---|---|---|
| Tampered blob | Detect if client verifies | Detect | Detect | Detect |
| Missing blob | 404 / vendor | Peer churn | Ops issue | 404 |
| Data exfiltration listing | IAM | Anyone who has CID | Swarm members | IAM |
| Request metadata privacy | CDN logs | Many observers | Your logs | Your logs |
| Supply chain “wrong package” | Catalog signing | Catalog + CID | Catalog + CID | **generation_hash signatures** |

Content addressing solves **integrity**, not **authenticity of name→hash bindings**. nudox still needs signed manifests / catalog trust (Plan 13 §6.4) regardless of IPFS.

### 10.4 Developer experience

Cargo/npm users expect:

```
registry URL + auth token → download crate
```

Not:

```
install Kubo, wait for DHT, hope CID is provided, debug bitswap
```

Even Helia verified-fetch is a JS story; our client is Rust GPUI.

---

## 11. Detailed verdict for nudox

### 11.1 Adopt network? **No (public). Maybe niche (private).**

| Network | Decision | Conditions |
|---|---|---|
| Public Amino / IPFS mainnet | **Reject** as package distribution | Never default; no product dependency on public gateways |
| Private IPFS Cluster | **Optional enterprise SKU** | Only with customer mandate or multi-org pinset needs |
| Filecoin | **Optional cold archive connector** | Compliance / durability theater / grant requirements |
| P2P edge (iroh/libp2p) | **Future research** | After INDEX HTTP path is excellent; see iroh report |

### 11.2 Adopt formats? **Selective yes.**

| Format | Decision | How |
|---|---|---|
| **CAS / Merkle root invariant** | **Yes (already)** | `generation_hash` + leaf BLAKE3s |
| **Want/have missing hashes** | **Yes (already)** | HTTP batch `has` + GET |
| **CIDv1 raw + blake3 multihash** | **Optional interop** | Export helper; store optional `cid` column |
| **CAR** | **Optional bulk** | Generation export for partners / cold restore |
| **UnixFS** | **No** as primary tree | Manifest paths instead |
| **dag-cbor manifests** | **No** as source of truth | postcard + JSON debug view |
| **Trustless gateway protocol** | **Steal ideas** | Verifiable GET; don’t require full spec |
| **Bitswap / Graphsync** | **No** | Pattern only |

### 11.3 If formats: concrete mapping

#### A. BLAKE3 → CID mapping (normative sketch)

```
inputs:
  digest: [u8; 32]  // ContentHash
outputs:
  mh = multihash_encode(code=0x1e, digest)
  cid = Cid { version: 1, codec: 0x55 raw, hash: mh }
  string = cid.to_string_base32()  // bafk… or similar
```

Round-trip: only for **raw single-block** blobs. Document that UnixFS CIDs are out of scope.

#### B. Generation as shallow DAG (logical, not IPLD-required)

```
generation_hash = blake3( identity_encoding(package, version, toolchain, …) )
manifest_hash   = blake3( postcard(BlobManifest) )
// BlobManifest lists leaf hashes — leaves are independent CAS objects
```

Optional IPLD export:

```
dag-cbor {
  "format": "nudox.generation/1",
  "generation": generation_hash as bytes,
  "manifest": link(CIDv1 raw blake3(manifest_bytes)),
  "files": [ { "path": "...", "link": CIDv1 raw … } ],
  ...
}
```

#### C. CAR pack (optional)

```
for each hash in generation closure:
  write CAR block (CIDv1_raw_blake3(hash), bytes)
roots: [cid(manifest) or synthetic root]
```

Local REGISTRY never needs CAR for normal operation.

#### D. What not to map

- Do not re-encode IR into dag-cbor symbol graphs for storage.
- Do not use IPNS for “latest version” (use catalog + signed pointers; IPNS TTL/propagation is the wrong mutability tool for package latest).
- Do not use MFS as REGISTRY filesystem.

### 11.4 Alignment with Plan 13 / 14

| Plan statement | This report |
|---|---|
| Shallow Merkle DAG without full IPLD day one | **Confirmed** |
| Bitswap only if P2P edge later | **Confirmed; prefer iroh eval first** |
| Sync = hash set difference | **Confirmed; best path** |
| S3 cas/ + CDN | **Confirmed primary** |
| generation_hash ≈ IPLD root CID | **Conceptual only** |

### 11.5 Phased policy (engineering)

**Phase 0 (now):**

- Ship BLAKE3 CAS + manifests + HTTP want/have.
- No IPFS dependencies in product crates.

**Phase 1 (interop toolkit, low priority):**

- `nudox-cid` helper crate: ContentHash ↔ CIDv1 string.
- Optional `cid` field in export APIs for partners.

**Phase 2 (bulk):**

- Evaluate CAR vs seekable-zstd pack for cold generation download.
- Prefer one pack format; CAR wins only if external IPFS tooling is a requirement.

**Phase 3 (only on demand):**

- Private Cluster pin mirror SKU.
- Filecoin cold archive exporter.
- P2P LAN sync experiment (iroh).

---

## 12. Worked scenarios

### 12.1 Public open-source INDEX (default SaaS)

1. Ingest package → compute file/IR BLAKE3 → write S3 `cas/*`.
2. Write Postgres catalog + generation manifest.
3. CDN caches cas/* forever.
4. Client want/have pulls missing hashes into REGISTRY.
5. **No IPFS.**

### 12.2 Enterprise air-gapped

1. Same CAS model on MinIO/local S3.
2. Offline pack export (zstd or CAR) on USB/sneakernet.
3. **No public DHT.** Private Cluster only if they already run IPFS for other data.

### 12.3 “We want decentralization” marketing customer

Honest answer:

- Content addressing **is** the decentralization that matters for integrity.
- Multi-mirror S3 + optional community mirrors with hash verify = pragmatic.
- Public IPFS pin of open packages = optional community mirror, **not** SLA tier.
- Filecoin deal = cold backup brochure feature.

### 12.4 Desktop REGISTRY GC

Use refcount of generation pins + LRU — not IPFS GC. Unpinning a package generation decrements blob refs; zero-ref blobs delete. Simple, testable, no recursive DAG pin walker.

---

## 13. Related systems (boundary clarification)

| System | Relation to this report |
|---|---|
| **Nix binary caches** | Closest operational model (narinfo + NAR + substituters). Prefer over IPFS for mental model. |
| **Git pack want/have** | Sync negotiation prior art. |
| **casync / ostree** | Chunked CDC later optimization. |
| **IPFS/IPLD** | Format + philosophy; not the network product. |
| **Filecoin** | Cold market; optional. |
| **iroh** | Possible future P2P; not IPFS mainnet. See 02-iroh. |
| **Dolt / prolly trees** | Structural sharing for **mutable DB** versions; different layer than blob CAS. See 01-doltgres. |

---

## 14. Risks of *adopting* IPFS incorrectly (anti-patterns)

1. **Dual canonical IDs** (blake3 and sha256 UnixFS CID) without a single source of truth.
2. **Publishing proprietary IR** to any shared swarm.
3. **Depending on ipfs.io** for production sync.
4. **Running Kubo in-process** in the GPUI app.
5. **Using IPNS** for package channels.
6. **Storing one UnixFS root per version** and losing cross-version file dedupe semantics at the product layer.
7. **Assuming pin = permanent** without RF and monitoring.
8. **Replacing catalog auth** with “if you know the CID you can get it” for private packages.

---

## 15. Open questions

1. **Do any design partners require CID-native artifacts** (e.g. already pin research artifacts on IPFS)? If yes, prioritize Phase 1 interop.
2. **Maximum single blob size** in nudox CAS? If we ever exceed ~1–2 MiB leaves and want IPFS interop, chunking policy must not break ContentHash identity (keep logical hash separate from transport chunks).
3. **Will enterprise RF multi-cloud** be solved with object_store multi-backend before anyone asks for Cluster?
4. **CAR vs custom pack:** pick one for cold export once we measure client download CPU (zstd dict packs may beat CAR for homogeneous IR).
5. **Signed catalog vs signed CAR roots** — is generation_hash ed25519 enough for offline air-gap verification without IPLD?
6. **LAN collaborative REGISTRY** (two devs sync from each other): is that a 2027 iroh experiment or never?
7. **Legal:** mirror of crates.io/npm source on our CAS — does dual-publish to public IPFS change DMCA/response process enough to forbid it entirely?
8. **BLAKE3 multihash draft status** — monitor multicodec table status field for `blake3` if we ever emit CIDs widely.
9. **Gateway conformance** — if we implement trustless-gateway-compatible endpoints for interop, do we gain anything over `/v1/cas/{blake3}` for non-IPFS clients? (Probably not.)
10. **Metrics:** define SLIs for INDEX blob GET that would make any P2P mesh need to beat p99 CDN latency before adoption.

---

## 16. Recommendations (actionable)

### 16.1 Product / architecture

1. **Keep S3-like object_store + BLAKE3 CAS + generation manifests as the only INDEX path for GA.**
2. **Keep local REGISTRY disk CAS with pin/LRU; sync via want/have.**
3. **Do not add Kubo/Helia/Cluster as runtime dependencies** of the desktop client or core server.
4. **Document the Merkle-DAG invariant** in architecture docs without requiring IPLD libraries.
5. **Optional:** `ContentHash::to_cid_v1_raw()` in a small utility for export; feature-gated.

### 16.2 Explicit non-goals (write into roadmap)

- Hosting the public package corpus on the Amino DHT.
- Bitswap between REGISTRY instances in v1–v2.
- UnixFS as package layout.
- Filecoin-backed “free perpetual storage” marketing claims.

### 16.3 If leadership wants “web3 storage” checkbox

Ship:

1. Export generation as CAR of raw blake3 CIDs.
2. Script to pin CAR to customer’s Cluster / pinning service.
3. Catalog fields for external CID + pin status.
4. Clear docs: **availability remains customer’s pin RF**.

### 16.4 Monitoring / abuse (even without IPFS)

- Hash-level takedown = delete object + catalog tombstone (CDN purge).
- Much easier than IPFS world; preserve this advantage.

---

## 17. Source index (URLs)

### Status / org / implementations

- https://ipshipyard.com/blog/2025-shipyard-ipfs-year-in-review/
- https://ipshipyard.com/blog/2025-a-post-gateway-world/
- https://ipshipyard.com/blog/shipyard-hello-world/
- https://ipshipyard.com/blog/2025-dht-provide-sweep/
- https://discuss.ipfs.tech/t/work-plans-for-kubo-helia-other-shipyard-ipfs-projects-in-2025/18742
- https://ipfsfoundation.org/
- https://ipfsfoundation.org/about/
- https://github.com/ipfs/boxo
- https://github.com/ipfs/kubo
- https://github.com/ipfs/helia

### Addressing / specs

- https://docs.ipfs.tech/concepts/content-addressing/
- https://specs.ipfs.tech/http-gateways/trustless-gateway/
- https://specs.ipfs.tech/unixfs/
- https://specs.ipfs.tech/routing/kad-dht/
- https://ipld.io/docs/codecs/
- https://ipld.io/docs/codecs/known/dag-cbor/
- https://ipld.io/specs/transport/car/carv1/
- https://github.com/ipfs/kubo/issues/8650
- https://github.com/ipfs/go-verifcid/issues/13

### Cluster / private

- https://ipfscluster.io/documentation/deployment/architecture/

### Filecoin

- https://filecoin.io/blog/the-2026-filecoin-network-strategy
- https://filecoin.io/

### Rust / iroh / libp2p

- https://discuss.ipfs.tech/t/status-of-rust-ipfs/18080
- https://libp2p.io/reports/annual-reports/2025/
- https://iroh.computer/docs/ipfs (conceptual relationship; see 02-iroh report)
- https://github.com/n0-computer/iroh/discussions/955

### Failure modes / permanence

- https://filebase.com/blog/stop-misusing-ipfs-5-real-mistakes-and-how-to-actually-avoid-them/
- Gateway/provider sunsets discussed in ecosystem posts (Infura, Scaleway, Cloudflare, Fleek) — treat free public infrastructure as ephemeral.

### Internal plans

- `.research/librarification/13-storage/PLAN.md` — CAS layout, IPLD mention, sync manifests
- `.research/librarification/14-client-sync/PLAN.md` — want/have, Bitswap deferral
- `.research/librarification/06-symbol-identity-industrial/PLAN.md` — IPLD as identity pattern

---

## 18. Appendix A — Glossary (nudox-oriented)

| Term | Meaning here |
|---|---|
| **CAS** | Content-addressed storage; key = hash(bytes) |
| **CID** | IPFS self-describing content id (version+codec+multihash) |
| **Multihash** | Self-describing hash (code + length + digest) |
| **UnixFS** | IPFS file/directory DAG format |
| **CAR** | Stream/archive of CID-addressed blocks |
| **Bitswap** | IPFS P2P block want/have protocol |
| **Graphsync** | IPLD selector-based graph transfer |
| **Trustless gateway** | HTTP API returning verifiable raw/CAR |
| **Pin** | Mark content exempt from GC / ensure local retention |
| **Amino DHT** | Public IPFS content routing DHT |
| **generation_hash** | nudox root commitment for a package snapshot |
| **ContentHash** | nudox BLAKE3-256 digest type |
| **INDEX** | Remote multi-tenant package intelligence service |
| **REGISTRY** | Local on-disk CAS + catalog for desktop |

---

## 19. Appendix B — Comparison to “IPFS as CDN for npm” historical attempts

Multiple ecosystems experimented with putting package tarballs on IPFS (JS package mirrors, various web3 package managers). Recurring outcomes:

1. **Pinning cost** lands on the maintainer or a foundation grant.
2. **UX falls back to HTTPS gateways**, re-centralizing.
3. **Latency and reliability** lose to traditional CDNs.
4. **Integrity** was the real win — and is achievable with checksums on HTTPS.

nudox should learn the lesson: **ship the integrity model, buy the CDN**.

---

## 20. Appendix C — Minimal pseudocode (interop only)

### C.1 ContentHash to CIDv1 string

```rust
// Pseudocode — not a crate API commitment
fn content_hash_to_cid_v1_raw(hash: &[u8; 32]) -> String {
    let mh = encode_multihash(0x1e, hash); // blake3
    let cid = Cid::new_v1(0x55, mh);       // raw codec
    cid.to_string_of_base(Base32Lower)
}
```

### C.2 Trustless-style fetch without IPFS

```rust
async fn fetch_blob(client: &Http, base: &Url, hash: ContentHash) -> Result<Bytes> {
    let url = base.join(&format!("cas/{}", hash.hex()))?;
    let bytes = client.get(url).send().await?.bytes().await?;
    ensure!(blake3::hash(&bytes) == hash);
    Ok(bytes)
}
```

### C.3 Want/have batch

```rust
async fn sync_generation(local: &Cas, remote: &Index, gen: &Manifest) -> Result<()> {
    let want: Vec<_> = gen.hashes().filter(|h| !local.contains(h)).collect();
    // optional: remote.has(&want) -> still_missing
    for h in want {
        let b = remote.get_cas(h).await?;
        local.put_verified(h, b)?;
    }
    local.commit_generation(gen)?;
    Ok(())
}
```

No Bitswap. No DHT. Same guarantees IPFS gives for integrity.

---

## 21. Appendix D — Decision record (ADR-style)

**Title:** Do not use public IPFS as nudox package blob substrate  
**Status:** Accepted (research recommendation)  
**Date:** 2026-07-16  

**Context:** nudox needs durable, authenticated, sync-friendly storage for source/IR with BLAKE3 identity and enterprise-ready INDEX.

**Decision:** Implement pure CAS over object_store + HTTP want/have. Optionally map to CID/CAR for interop. Reject public network dependency. Defer private Cluster and Filecoin to optional connectors.

**Consequences:**

- Positive: simpler clients, better authz, clear SLAs, BLAKE3-native, CDN-friendly.
- Negative: no free lunch from public peer bandwidth; less “decentralized web” marketing; extra work if a partner is CID-only.
- Mitigations: optional CID export; multi-mirror HTTP; signed manifests.

---

## 22. Final summary

### 22.1 2026 IPFS in one breath

IPFS is **more practical for self-hosting and trustless HTTP retrieval** than ever (Kubo Sweep, HTTP retrieval default, verified-fetch, formal specs), while **free public gateway CDN economics are collapsing** and the org chart has federated (Foundation + Shipyard + Filecoin ecosystem). It is infrastructure you **run and pay for**, not a magical permanent disk in the sky.

### 22.2 What nudox should do

| Do | Don’t |
|---|---|
| BLAKE3 CAS + manifests | Public DHT as INDEX |
| HTTP want/have + verify | Embed Kubo in desktop |
| CDN immutable cas keys | UnixFS package trees |
| Optional CID/CAR export | Bitswap v1 sync |
| S3/IAM enterprise story | Filecoin as hot path |
| Steal Merkle & trustless ideas | Dual hash canons |

### 22.3 Bottom line

**Reject the public network, defer private Cluster, ignore Filecoin for core, adopt the CAS/Merkle/want-have ideas you already planned, and keep CID/CAR as optional boundary formats.** That is the 2026-correct answer for a BLAKE3 content-addressed code intelligence platform.

---

*End of report. No code changes; research only.*
