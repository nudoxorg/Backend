# iroh (n0 / number0) as Sync/Transport Layer for nudox INDEX ↔ Local REGISTRY

> **Status (LIBRARIFICATION-PLAN Rev 2, 2026-07-16):** iroh-blobs is **PROMOTED** — it is now THE sole client data plane for v1 (GD-9/GD-30), not a Phase-2 spike. `IrohBlobsTransport` is the only production `BlobTransport` impl; `HttpPresignTransport`, `AutoBlobTransport`, and all "residual HTTPS" fallback rules in §11.3 are deleted from the binding design. The `nudox-iroh-provider` regional fleet serves CAS objects under INDEX-signed `DownloadGrant`s over self-hosted relays; S3 remains the durable SoT; the control plane stays HTTP; compiler pods keep direct S3 IAM and are not iroh clients; iroh-docs remains rejected for the catalog (GD-30).

**Research date:** 2026-07-16  
**Status:** 2026 reality check after iroh 1.0 (shipped 2026-06-15)  
**Scope:** Whether nudox should adopt iroh core / iroh-blobs / iroh-docs / iroh-gossip as the INDEX↔desktop REGISTRY data plane (or any slice of it), vs staying on HTTP control plane + presigned S3 blob GETs.  
**Cross-links:** Plan 13 storage (BLAKE3 CAS, generation manifests), Plan 14 client-sync (want/have, remote-serves-while-syncing), Plan 21 wire-protocol (control JSON + CAS raw + no blob proxy), sibling edge-tech `04-ipfs-and-cas`.  
**Codebase anchors:** `workspace/heart/content.rs` (`ContentHash` / BLAKE3), `workspace/registry/blob/mod.rs` (`BlobManifest`), `workspace/heart/cache/` (Tiered CAS), `workspace/gui/src/backend.rs` (today: pure HTTP).  
**Sources verified 2026-07-16:** iroh.computer docs & blogs, crates.io/docs.rs, n0-computer GitHub, TechTimes / byteiota 1.0 coverage.

---

## 0. Executive summary (read this first)

### 0.1 One-paragraph verdict

**Augment later, do not replace the v1 data plane.** iroh 1.0 (2026-06-15) is a mature dial-by-Ed25519-key QUIC networking library with excellent NAT traversal, ~95% direct-path traffic in production, and a first-class **BLAKE3 verified-streaming** protocol (`iroh-blobs`) that is philosophically aligned with nudox CAS. That alignment is real and rare. It does **not** justify ripping out HTTP+presigned S3 for INDEX→desktop package sync in v1: INDEX is a classic server-authoritative, multi-tenant, catalog-heavy product; clients need CDN-grade bulk blob delivery, short-TTL auth, and operational knobs that S3 already provides. The correct stance is **keep Plan 21 control plane (HTTP/JSON) and Plan 13/14 CAS-over-HTTP data plane as the product default; treat iroh-blobs as a Phase-2 optional transport for LAN peer assist, edge caches, and offline/sneakernet-shaped flows**—once protocol adapters map nudox `ContentHash` ↔ iroh `Hash` and generation manifests ↔ `HashSeq`/`Collection` metadata.

### 0.2 Decision matrix (quick)

| Option | Verdict for nudox | Why |
|---|---|---|
| **Replace S3/presign data plane with iroh-blobs end-to-end** | **Reject for v1** | INDEX must stay multi-tenant HTTP product; S3/CDN is the right bulk store; iroh-blobs production quality still flagged as “prefer 0.35” on latest canary line; ops/relay cost for every desktop client is real |
| **Replace control plane with iroh-docs / gossip** | **Reject** | Catalog is server-authoritative; CRDT multi-writer is dead weight (Plan 14); search/graph stay on INDEX HTTP |
| **Use iroh only as QUIC dial fabric (no blobs)** | **Defer** | Useful for custom internal RPC later; not needed for package sync |
| **iroh-blobs as optional multi-source downloader** | **Phase 2 / strong candidate** | Multi-provider fan-in, verified ranges, resume, HashSeq ≈ generation closure — maps cleanly onto want/have |
| **iroh-gossip for “package updated” notifications** | **Maybe later** | Nice for live INDEX→client generation-ready events; HTTP long-poll / SSE / push is simpler day-1 |
| **iroh-docs for generation metadata** | **Reject as primary** | Wrong consistency model (LWW multi-author KV); use server-emitted `BlobManifest` |
| **LAN / peer-assist CAS (desktop↔desktop, office cache)** | **Strong Phase-2 fit** | Exact sendme/LAN use case; reduces INDEX egress; still verify BLAKE3 |
| **Self-host n0 relays for enterprise** | **Ops option only** | Dedicated relays for production; public n0 relays are rate-limited hobby tier |
| **Borrow Bao / BLAKE3 verified streaming ideas without iroh** | **Yes always** | Already compatible; optional bao-tree outboards on large IR blobs later |

### 0.3 Recommended stance in one line

**HTTP JSON control plane + presigned S3 CAS for v1; pin iroh 1.x as the preferred *optional* P2P/LAN blob transport when multi-source and NAT-hard peer assist matter; do not adopt iroh-docs as the package catalog.**

### 0.4 Mapping to nudox product semantics

| nudox concept | iroh analogue | Fit |
|---|---|---|
| INDEX (authoritative untrusted deps) | Provider node(s) + external catalog | Partial — catalog stays HTTP |
| Local REGISTRY | Client with FsStore + DiskCas | Good for local retention |
| Generation manifest | Collection / HashSeq + metadata blob | Good structural fit |
| `ContentHash` (BLAKE3-256) | `iroh_blobs::Hash` (BLAKE3 root) | **Excellent** — same hash family |
| Want/have missing hashes | `Get` / `GetMany` + local bitfields / `fetch` | Excellent |
| Presigned S3 GET | Not an iroh concept | Keep S3 as primary provider |
| Client subscribe to INDEX packages | Gossip topic or docs subscribe, or **poll sync-plan over HTTP** | HTTP better for v1 |
| Offline after sync | Local store takeover | Matches Plan 14 UX contract |
| Untrusted package integrity | Hash verification on stream | Matches; authz still INDEX/IAM |

---

## 1. What is iroh today (2026)

### 1.1 Product pitch

From [iroh.computer](https://www.iroh.computer/) and the [What is iroh?](https://docs.iroh.computer/what-is-iroh) docs:

> **IP addresses break, dial keys instead.** iroh is a modular networking stack written in Rust that establishes authenticated, end-to-end encrypted **QUIC** connections between endpoints identified by **Ed25519 public keys** (`EndpointID`), with relay-assisted NAT traversal and automatic path migration.

Core promises:

1. **Reachability:** dial a peer by key no matter where it moves.
2. **Best path:** prefer direct P2P; fall back to encrypted relay when hole punching fails.
3. **Composability:** thin core + ALPN-routed application protocols (blobs, gossip, docs, or custom).

It is an **embedded library**, not a daemon users must install (though relay binaries exist). Language bindings: Rust (primary), plus Python / Node.js / Swift / Kotlin via `iroh-ffi` restored at 1.0.

### 1.2 Company / maintainership

| Item | Detail |
|---|---|
| **Org** | **n0 / number0** — [n0.computer](https://n0.computer), product site [iroh.computer](https://www.iroh.computer) |
| **Legal entity** | N0, Inc. (copyright headers on crates through 2026) |
| **Business model** | Open-source core + **Iroh Services** (managed relays, metrics, net diagnostics, billing) |
| **Public infra** | Free community relays + `dns.iroh.link` / pkarr DNS (rate-limited; no production SLA) |
| **License** | **MIT OR Apache-2.0** dual (core and major protocols) |
| **Community** | Discord, Reddit r/iroh / r/rust, FOSDEM 2026 talks, sendme clones ecosystem |

### 1.3 1.0 milestone (critical for adoption timing)

**Iroh 1.0 shipped 2026-06-15** ([blog: Iroh 1.0 — Dial Keys, not IPs](https://www.iroh.computer/blog/v1); [road to 1.0](https://www.iroh.computer/blog/the-road-to-iroh-1-0)).

Claims and commitments (verify against primary sources):

| Claim | Detail |
|---|---|
| Wire + API stability | v1 endpoint can talk to any other v1 endpoint (language-agnostic wire) |
| Pre-history | 65+ pre-1.0 releases over ~4 years; major pivots (IPFS-compat → QUIC-only → batteries-included → “just connections”) |
| Production scale | Public relays: **>200M endpoints created in 30 days** (n0 claim); industry coverage cites ~200M endpoint connections/month; **~95% of data** on direct paths |
| Support | Major: Full Support 1 year; Minor: 3 months; public relays track latest major; **0.35x public relay support through 2026-12-31** |
| MSRV | crates list **Rust ≥ 1.91.0** (as of mid-2026 crate pages) |
| Core crate version | **iroh ~1.0.2** on crates.io (published ~2026-07-06 per crawl) |

**Implications for nudox:** the historical “iroh rewrites every quarter” risk is **materially reduced for the networking core**. Protocol crates (blobs/docs/gossip) still version on a **0.x / 0.10x track** and are **not** as stable as `iroh` 1.x — treat them as semi-independent products.

### 1.4 Architecture stack

```
┌─────────────────────────────────────────────────────────────┐
│ Application / Protocols (ALPN-routed)                         │
│   iroh-blobs | iroh-gossip | iroh-docs | custom ALPNs         │
├─────────────────────────────────────────────────────────────┤
│ Router — accept loop, dispatch by ALPN                        │
├─────────────────────────────────────────────────────────────┤
│ Endpoint — EndpointID (Ed25519), connect/accept, path mgmt    │
├─────────────────────────────────────────────────────────────┤
│ Address lookup — DNS/Pkarr, optional Mainline DHT, mDNS       │
├─────────────────────────────────────────────────────────────┤
│ QUIC + TLS 1.3 (raw public keys) via n0's noq (quinn fork)    │
│   multipath, NAT traversal extension, 0-RTT options           │
├─────────────────────────────────────────────────────────────┤
│ Transport — UDP default; optional Tor / BLE / custom          │
├─────────────────────────────────────────────────────────────┤
│ Relays — WebSocket home relay; hole-punch assist + fallback   │
└─────────────────────────────────────────────────────────────┘
```

**Key design choices (from road-to-1.0 history):**

| Choice | Rationale |
|---|---|
| Ed25519 EndpointID = address + identity | Stable as IPs change; mutual auth baked in |
| QUIC only (no multi-transport in core) | Performance, stream mux, standards alignment |
| TLS raw public keys (RFC 7250) | Real QUIC/TLS, not Noise-as-QUIC-lookalike |
| ALPN for protocol selection | Zero per-stream multistream-select tax |
| Relays (Tailscale-inspired) | Reliability over p2p purity; ~9/10 conditions direct |
| Pkarr-format address records | Signed home-relay publication via DNS or DHT |
| Protocols as separate crates (since ~0.28) | Core stays small; blobs/docs/gossip evolve independently |
| noq multipath | Seamless path migration without hacks |

### 1.5 Core crates map (2026)

| Crate | Role | Version signal (mid-2026) | Notes |
|---|---|---|---|
| **`iroh`** | Endpoint, Router, relays client, address lookup | **1.0.x** stable | Primary dependency |
| **`iroh-base`** | Shared types (`EndpointId`, `RelayUrl`, …) | 1.0.x | |
| **`iroh-relay`** | Relay client + server binary | 1.0.x | Self-host path |
| **`iroh-dns-server`** | DNS/Pkarr lookup server | in-repo | n0 runs `dns.iroh.link` |
| **`iroh-tickets`** | Ticket encode/decode (endpoint, custom) | 1.0.x | Optional convenience |
| **`iroh-blobs`** | BLAKE3 content-addressed transfer | **0.103.x** (canary); **0.35** “production quality” note on docs.rs | **Most relevant to nudox CAS** |
| **`iroh-gossip`** | Epidemic broadcast trees (HyParView + PlumTree) | **0.101.x** | Pub/sub topics |
| **`iroh-docs`** | Multi-dimensional LWW-ish docs / replicas | **0.101.x** | Meta-protocol over blobs+gossip |
| **`iroh-metrics`** | Metrics helpers | 1.x | |
| **`iroh-ffi`** | Language bindings | restored at 1.0 | |
| **`iroh-net` (historical)** | Old name; **merged / retired** into `iroh` | Do not depend | |
| **`noq`** | n0 QUIC implementation (quinn fork) | 1.0.x | Multipath |

**Important naming history:** pre-0.28 monorepo; post-0.28 “let them have crates”; pre-1.0 canary series 0.90–0.99; `NodeId` → `EndpointId` rename; `Discovery` → **AddressLookup**.

### 1.6 Tickets, pkarr, relays (the three coordination primitives)

#### Tickets

[Tickets docs](https://docs.iroh.computer/concepts/tickets): serializable tokens packing **what + where**:

- Prefix + base32(postcard payload)
- Types: `EndpointTicket`, `BlobTicket` (hash + format + endpoint addr), `DocTicket` (namespace + capability)
- Good for QR / copy-paste / short sessions
- **Contain IP addresses** when full dial info is embedded; reusable (not one-shot auth tokens)
- **n0 recommendation:** if you have a coordination server, prefer storing **EndpointIDs** and let address lookup refresh dial details — tickets go stale

**nudox mapping:** INDEX is exactly a coordination server. Prefer:

```
INDEX returns: { endpoint_id, generation_hash, content_hashes[], optional relay_hint }
not: long-lived BlobTickets as the primary auth mechanism
```

Tickets *can* bootstrap LAN peer-assist or offline “airdrop package” UX later.

#### Pkarr / DNS address lookup

[Address lookup](https://docs.iroh.computer/concepts/address-lookup): endpoints publish signed records (EndpointID + home relay URL) via pkarr format to n0 DNS (`iroh-dns-server`) or Mainline DHT (BEP 44). Dial-by-ID only works when lookup is enabled (default with `presets::N0`).

#### Relays

[Relays docs](https://docs.iroh.computer/concepts/relays):

1. Assist hole punching (exchange candidate addresses).
2. Fallback encrypted proxy when direct fails.
3. Stateless; cannot read e2e payload; **can** see metadata (IPs, timing, volume).

| Tier | Use |
|---|---|
| Public n0 relays | Dev / hobby; rate-limited; no SLA |
| Dedicated / Iroh Services | Production; auth by API key tokens; isolation |
| Self-host `iroh-relay` | Full control; enterprise airgap |

**n0 claims:** ~9/10 conditions achieve direct; ~95% of transferred bytes go direct in aggregate telemetry.

### 1.7 Historical pivots (why stability claims matter)

Condensed from [The road to iroh 1.0](https://www.iroh.computer/blog/the-road-to-iroh-1-0):

1. **IPFS-compatible rust stack** → too heavy for mobile; blob path suboptimal.
2. **QUIC + BLAKE3 “krakensync”** for Delta Chat history transfer → real product use.
3. **Drop IPFS multiaddr/multihash** for Ed25519 + BLAKE3-only.
4. **Hole punching + relays** (Tailscale-inspired).
5. **Batteries-included toolkit** (docs abstraction, FFI) → API surface grew.
6. **Narrow to “just connections”** (~0.28): split blobs/gossip/docs into repos.
7. **0.35 long-term line + 0.9x canaries** toward 1.0.
8. **noq multipath** rewrite under the hood.
9. **1.0** wire+API stability declaration.

**For nudox risk register:** core networking risk is now moderate-low; **protocol crate churn risk remains medium** (blobs docs still warn latest line is not “production quality” vs 0.35).

---

## 2. iroh-blobs / Bao / BLAKE3 — CAS fit analysis

### 2.1 Content addressing model

From [Blobs protocol docs](https://docs.iroh.computer/protocols/blobs) and [docs.rs/iroh-blobs](https://docs.rs/iroh-blobs/latest/iroh_blobs/):

| Concept | Definition |
|---|---|
| **Blob** | Opaque byte sequence, no embedded metadata |
| **Link / Hash** | 32-byte **BLAKE3 root hash** of the blob |
| **HashSeq** | Blob whose payload is a concatenation of 32-byte links (length % 32 == 0) |
| **Collection** | Ordered sequence of hashes; by convention element[0] is a **metadata blob** describing the rest |
| **Outboard** | Bao-style external BLAKE3 tree metadata for verified streaming (blob bytes stay pure) |
| **Provider / Requester** | Serve vs fetch roles; a node can be both |
| **ALPN** | `iroh_blobs::ALPN` registered on Router |

Protocol shape:

1. Requester opens QUIC stream to provider.
2. Sends request describing hashes + byte/chunk ranges.
3. Provider responds with **BLAKE3 verified streams** (Bao encoding) on the same stream.
4. Receiver verifies **incrementally** — corrupt chunks rejected before full download completes.

### 2.2 Bao verified streaming (why it matters)

[BLAKE3 paper §6.4](https://github.com/BLAKE3-team/BLAKE3-specs) + [bao crate](https://github.com/oconnor663/bao): tree hash enables:

- **Streaming verify** while downloading (video/file use case that motivated Bao).
- **Range requests** with only the tree slices needed to authenticate the range.
- **Resume** after interruption without re-trusting unauthenticated partials.

iroh details ([blobs docs](https://docs.iroh.computer/protocols/blobs)):

- Default BLAKE3 chunk size **1 KiB** → ~**6% outboard overhead**.
- Chunk size can change **without changing root hash** (outboard recalculated).
- Chunk-group granularity for ranges historically **16 KiB / 16 chunks** (protocol docs).
- Outboards stored as external metadata; original blob remains the canonical CAS object.

n0 previously forked BLAKE3 for “guts” access; as of 2025 they moved to upstream BLAKE3 **hazmat** API ([blog](https://www.iroh.computer/blog/blake3-hazmat-api)).

### 2.3 Hash function compatibility with nudox

| Property | nudox `ContentHash` | iroh-blobs `Hash` | Compatible? |
|---|---|---|---|
| Algorithm | BLAKE3 | BLAKE3 | **Yes** |
| Digest size | 32 bytes | 32 bytes | **Yes** |
| Semantics | Content root of blob | Content root of blob | **Yes if same domain separation / no salt** |
| Encoding on wire | typically hex in JSON / raw in CAS keys | base32 in tickets; raw in protocol | Map at boundary |
| Chunking for identity | Whole-blob hash (Plan 13) | Whole-blob root hash | **Aligned** — chunk size is transfer-only |

**Critical invariant:** as long as both sides hash the **exact same byte sequence** with plain BLAKE3-256 root (no keyed hash, no personalization), `ContentHash` and iroh `Hash` are **byte-identical**. Plan 13 already stores `cas/{blake3}`; an iroh provider can serve those same files if outboards are generated or recomputed.

**Action if adopting:**

1. Audit `heart/content.rs` for any domain separator / prefix before BLAKE3.
2. Provide `From<ContentHash> for iroh_blobs::Hash` (and reverse) as a thin newtype conversion.
3. Prefer generating outboards offline at INDEX ingest for large IR/archives; small source files can compute on the fly.

### 2.4 Collection / HashSeq vs BlobManifest

nudox generation (Plan 13/14):

```
BlobManifest {
  package,
  files: [(path, ContentHash), ...],
  ir_ref, references_ref, toolchain, ...
}
generation_hash = identity stamp
manifest_hash   = blake3(postcard(manifest))  // CAS key of wire blob
```

iroh collection convention:

```
HashSeq = [ meta_hash, h1, h2, ... hn ]
meta_blob = application-defined JSON/postcard describing paths, sizes, roles
```

**Mapping sketch:**

| nudox | iroh-blobs |
|---|---|
| postcard `BlobManifest` as CAS object | metadata blob (collection[0]) **or** standalone blob referenced by INDEX |
| section hashes (files, IR, refs) | remaining HashSeq entries **or** GetMany set |
| generation stamp | application field in meta; not iroh-native |
| commit generation when all hashes local | tags / bitfields complete + app-level catalog Ready |

**Recommendation:** Do **not** force every generation into a single HashSeq if INDEX already returns a structured manifest over HTTP. Use:

- HTTP: authoritative manifest + want/have plan.
- iroh: `GetMany` over the missing hash set (0.90+ feature) — **exactly** the multi-small-blob case n0 calls out.

From [iroh-blobs 0.90 features](https://www.iroh.computer/blog/iroh-blobs-0-90-new-features):

- **`GetMany`:** set of hashes + per-hash ranges; sequential abort if missing; ideal for many small blobs.
- Parallel multiple `Get`s: better for few large blobs (cheap QUIC streams).
- **`Observe`:** bitfield stream of remote availability (want/have-ish).
- **`Push` / `PushMany`:** upload path (needs access control).
- **`Downloader` + `ContentDiscovery`:** multi-provider fan-in by EndpointId list.

### 2.5 Progressive download, resume, verification

| Capability | iroh-blobs | nudox need |
|---|---|---|
| Progressive verify | Yes (Bao) | Strong yes for large IR / archives |
| Byte ranges | Yes | Single-file from large blob; archive member later |
| Resume partial | Yes (bitfields track gaps in new store) | Required for desktop flaky networks |
| Multi-provider | Downloader + discovery | Phase 2 LAN + INDEX |
| Progress events | `DownloadProgress` stream | Map to `JobProgress` / GUI |
| Partial generation visibility | App-level (tags/bitfields ≠ Ready) | Plan 14 staging model still required |

**Plan 14 still owns commit semantics.** iroh gives you authentic bytes; nudox decides when a generation is `Ready` for local takeover.

### 2.6 Store implementations

| Store | Use |
|---|---|
| `MemStore` | Tests / small tools |
| `FsStore` (`fs-store` feature, redb + reflink-copy) | Desktop / server persistent |
| Custom via store traits | Bridge to nudox `DiskCas` |

**Integration risk:** dual storage (iroh FsStore **and** nudox DiskCas) is wasteful. Prefer:

**Option A (recommended):** Keep nudox DiskCas as SoT; implement a thin provider that reads from DiskCas / object_store and serves Bao streams (custom or adapter).  
**Option B:** Use FsStore as L2 and mirror into DiskCas on commit (more moving parts).  
**Option C:** Make DiskCas store outboards alongside blobs (`cas/{hash}` + `cas/{hash}.outboard`) without full iroh store.

### 2.7 Production-quality caveat (read carefully)

docs.rs for latest iroh-blobs (0.103 as of research crawl) states:

> **NOTE: this version of iroh-blobs is not yet considered production quality. For now, if you need production quality, use iroh-blobs 0.35**

Meanwhile `iroh` core is 1.0. **Version skew is real:**

| Track | Meaning for nudox |
|---|---|
| iroh 1.0 + blobs 0.35 | Conservative; may lag features (GetMany, bitfields) |
| iroh 1.0 + blobs 0.10x canary | Features; higher break risk; still 0.x |
| Wait for iroh-blobs 1.0 | Ideal adoption gate for deep integration |

**Verdict on blobs maturity:** algorithmically excellent and battle-used (sendme, Delta Chat lineage); **crate semver/stability not yet co-equal with iroh 1.0**. Plan Phase-2 spikes, not v1 critical path.

### 2.8 Fit score: iroh-blobs ↔ nudox CAS

| Criterion | Score (1–5) | Note |
|---|---|---|
| Hash compatibility | **5** | BLAKE3 root |
| Want/have multi-blob | **5** | GetMany + Observe |
| Large blob streaming | **5** | Bao + ranges |
| Many small source files | **4** | GetMany designed for this; still care about connection setup |
| Multi-tenant INDEX SaaS | **2** | Not the primary design center; needs authz glue |
| CDN/S3 economics | **2** | iroh is peer/provider, not object store |
| Offline local takeover | **5** | Local store after sync |
| Ops simplicity vs S3 | **2** | Relays, endpoint keys, provider fleet |
| **Overall as v1 primary** | **2.5** | Reject as primary |
| **Overall as Phase-2 transport** | **4.5** | Strong |

---

## 3. iroh-docs / documents / author keys

### 3.1 What it is

From [Documents docs](https://docs.iroh.computer/protocols/documents) and [iroh-docs GitHub](https://github.com/n0-computer/iroh-docs):

- Multi-dimensional **key-value document** (aka **Replica**).
- Identity: **NamespaceId** = public key of namespace keypair (**write access gate**).
- Entry identity: `(namespace, author, key)`.
- Entry value: **BLAKE3 content hash** (+ size, timestamp); bytes live in **iroh-blobs**.
- **Author** keypair signs entries; multiple authors allowed; semantics app-defined.
- Sync algorithm: **range-based set reconciliation** (Meyer 2022 style fingerprints).
- Live updates via **iroh-gossip**.
- Share via **DocTicket** with `ShareMode::Read` / `Write`.
- Conflict policy: effectively **last-write-wins per key** (timestamp + author ordering in practice) — a CRDT-ish set of signed entries, not a rich text CRDT like Automerge/Loro.

Stack:

```
iroh-docs  →  metadata + reconciliation
iroh-blobs →  content bytes
iroh-gossip → live invalidation / join
```

### 3.2 Multi-writer model

| Role | Capability |
|---|---|
| Namespace secret | Create document; issue write tickets |
| Author keys | Sign inserts; many authors per doc |
| Read ticket holders | Sync & read |
| Write ticket holders | Insert/update entries |

This is **collaborative local-first document** machinery (see n0 tauri-todos example), not package registry semantics.

### 3.3 Fit for package generation metadata?

| Requirement (nudox) | docs fit? |
|---|---|
| INDEX is sole author of untrusted package generations | Multi-writer is surplus |
| Immutable generation once published | LWW mutable keys fight immutability |
| Strong catalog queries (search packages, resolution) | redb replica ≠ Postgres/sqlite catalog |
| Multi-tenant isolation | Namespace-per-tenant possible but awkward vs OAuth/IAM |
| Progressive sync of **blobs** | blobs protocol does this without docs |
| Client never authors dependency IR | Clients should not be write-capable authors |

**Verdict: reject iroh-docs for package generation / catalog.** Plan 14 already concluded CRDTs are overkill. Server-emitted `BlobManifest` + HTTP catalog is the correct model.

### 3.4 Where docs *might* fit (non-v1, non-core)

| Use case | Plausibility |
|---|---|
| Collaborative annotations on code (multi-user notes) | Possible product later; orthogonal to REGISTRY |
| Shared team “project pin set” offline | Possible; still often better as app CRDT (Loro/Automerge) over iroh transport |
| Distributed config for edge nodes | Maybe for internal infra |

**Do not** put generation manifests, IR pointers, or package metadata primarily in iroh-docs.

### 3.5 Automerge protocol note

iroh ecosystem also documents **Automerge-over-iroh** as a first-class composition ([docs Automerge protocol](https://docs.iroh.computer/protocols/automerge)). n0 explicitly prefers “robust transport + best-of-breed CRDT” over expanding docs into a full CRDT suite. Same conclusion for nudox: if collaborative editing ever appears, use a real CRDT + iroh-or-HTTP transport; not docs-as-package-db.

---

## 4. Subscription model

### 4.1 How peers “subscribe” in the iroh world

| Mechanism | What you get | Delivery semantics |
|---|---|---|
| **iroh-gossip topic** | Broadcast messages to topic members | Epidemic; eventual; redundant; no broker |
| **iroh-docs `doc.subscribe()`** | LiveEvent stream (InsertRemote, ContentReady, …) | App-level; built on gossip + reconciling |
| **Polling / re-fetch** | Client dials provider periodically | Simple; app-defined |
| **Blob Observe** | Remote bitfield changes for a hash | Content availability, not catalog |
| **Tickets** | Bootstrap join, not continuous sub | One-shot dial info |

Gossip ([docs](https://docs.iroh.computer/connecting/gossip)):

- TopicId = 32 bytes (recommend SHA-256 of namespaced string).
- Bootstrap peers required to join swarm.
- HyParView membership + PlumTree broadcast trees.
- Scales to “a few thousand peers” on one topic (docs guidance); use scoped topics for larger systems.
- **Not** Kafka/NATS delivery guarantees.

### 4.2 Compare to “client subscribed to INDEX packages”

nudox product (Plan 14):

```
Developer opens project
  → resolve DepSet {(P_i, G_i)}
  → INDEX serves queries immediately
  → background SyncEngine pulls missing CAS for each generation
  → on Ready, local REGISTRY takes over
```

This is **server-authoritative pull of an immutable closure**, not multi-peer collaborative sync.

| Approach | Fit |
|---|---|
| HTTP `GET /v1/sync/plan` + long-poll/SSE “generation ready” | **Best v1** — matches Plan 21 |
| Client poll manifest ETag / generation stamp | Good simple fallback |
| iroh-gossip topic per package or per tenant | Works for push notify; must still pull blobs; bootstrap/ops cost |
| iroh-docs per package | Wrong model |
| NATS / Redis pubsub at INDEX | Fine server-side; not desktop embed |

**Subscription recommendation:**

1. **v1:** HTTP control plane carries resolve + sync-plan + optional SSE `package.generation.ready`.
2. **Phase 2:** optional gossip topic `nudox.pkg.{package_id}` or tenant-scoped topic for edge caches announcing “I have generation G” (provider discovery), not as source of truth.
3. **Never** treat gossip as integrity boundary — still verify BLAKE3.

### 4.3 Provider discovery for multi-source

iroh-blobs `ContentDiscovery` trait returns a stream of provider EndpointIds. Sources can be:

- Static list from INDEX (`GET /v1/blobs/{hash}/providers`).
- Gossip announcements.
- Experimental content tracker (iroh-experiments).
- LAN mDNS neighbors.

This is the **interesting** subscription-adjacent feature for nudox: INDEX can return “official provider EndpointIds + S3 URLs”; client Downloader tries LAN first, then INDEX provider, then S3 HTTP.

---

## 5. Provider / node roles for INDEX vs desktop

### 5.1 Role sketch

```
                    ┌──────────────────────┐
                    │  INDEX control plane │  HTTP/JSON (Plan 21)
                    │  catalog, search,    │
                    │  auth, sync-plan     │
                    └──────────┬───────────┘
                               │ issues:
                               │  - presigned S3 URLs (v1 primary)
                               │  - optional EndpointId of blob providers
                               ▼
┌──────────────┐    blobs ALPN     ┌────────────────────────────┐
│ Desktop      │◄─────────────────►│ INDEX blob provider fleet  │
│ REGISTRY     │                   │ (iroh endpoints wrapping   │
│ iroh client  │                   │  object_store / DiskCas)   │
│ + DiskCas    │                   └────────────────────────────┘
└──────┬───────┘
       │ optional LAN
       ▼
┌──────────────┐
│ Peer desktop │  same hashes, mutual assist
│ or office    │
│ cache node   │
└──────────────┘
```

| Actor | iroh role | Notes |
|---|---|---|
| INDEX API servers | Usually **not** heavy blob providers | Keep “no blob proxy” (Plan 21) |
| INDEX **provider workers** | iroh-blobs Provider over S3/DiskCas | Optional scale-out; separate from query path |
| S3 | Not an iroh peer | Remains primary durable store |
| Desktop | Requester (+ optional Provider for LAN assist) | Fs/DiskCas after commit |
| Compiler pods | Unrelated to iroh for v1 | Keep presigned PUT/GET |

### 5.2 Can INDEX “be a provider”?

Yes, technically: run Endpoint + BlobsProtocol with store backed by object_store. Operationally:

| Concern | Assessment |
|---|---|
| Scaling providers | Horizontal; each has EndpointId; INDEX advertises set |
| Cold start / key management | Persistent SecretKey per provider; rotate carefully |
| Multi-tenant authz | **Must** gate Get by capability (see §6) — default blobs Get is often open |
| Egress cost | Direct P2P from provider VM still burns provider bandwidth; S3+CDN may be cheaper at scale |
| NAT | Providers should be public / well-relayed; easy |

**Better hybrid:** S3 for bulk; iroh providers only for (a) LAN caches, (b) regions where CDN is weak, (c) peer assist.

### 5.3 NAT traversal reliability

| Scenario | Expected behavior |
|---|---|
| Home consumer NAT ↔ public provider | High direct success; relay fallback |
| Corporate symmetric NAT / firewall | Often **relay-only**; rate limits on public relays hurt |
| Desktop ↔ desktop same LAN | mDNS / local path; excellent |
| Airplane offline after sync | No network needed for local Ready generations |
| Browser WASM client | Supported directionally; not nudox GUI path (GPUI native) |

**Enterprise risk:** if many desktops are relay-bound, you **must** run dedicated relays (cost + privacy). Do not ship production dependency on public n0 relays.

### 5.4 Relay costs

| Mode | Cost driver |
|---|---|
| Direct P2P | Nearly free (n0’s pitch: lower cloud egress) |
| Relay fallback | Operator pays bandwidth; public = rate-limited |
| Dedicated Iroh Services | Subscription + usage (see n0 billing docs) |
| Self-host | Infra + ops |

For **INDEX→client package sync of multi-MB IR**, relying on relays as the common path is **economically worse** than S3+CDN. iroh wins when **direct** or **LAN**.

### 5.5 Offline local reads after sync

Fully compatible with Plan 14:

1. Pull closure via any transport (HTTP or iroh).
2. Verify hashes.
3. Commit generation in sqlite + DiskCas.
4. Route queries locally; no Endpoint needed until next sync.

iroh is offline-irrelevant post-commit — same as S3 path.

---

## 6. Security / auth

### 6.1 Transport security (iroh core)

| Property | Mechanism |
|---|---|
| Mutual authentication | Ed25519 EndpointIDs in TLS raw public keys |
| Confidentiality | QUIC/TLS 1.3 e2e |
| Relay blindness to payload | Yes |
| Relay metadata exposure | IPs, timing, volumes |
| ALPN visibility | ClientHello ALPN not encrypted (ECH future) |
| Optional PQ | Post-quantum **key exchange** optional in 1.0; signatures still Ed25519 |

[Security & privacy docs](https://docs.iroh.computer/deployment/security-privacy): public relays OK for hobby; production → dedicated infra; direct connections **reveal IPs to peers** (expected).

### 6.2 Capability tickets vs product authz

| iroh primitive | Authz meaning | Enough for nudox? |
|---|---|---|
| Knowing EndpointId | Can attempt dial | No — not authorization |
| BlobTicket | Locator + hash | **Integrity** of content id, not access control |
| DocTicket Read/Write | Namespace capability | For docs only |
| Provider event hooks | Allow/deny by EndpointId / request | **Must use** if providers face untrusted clients |
| Dedicated relay API keys | Relay access, not blob ACL | Orthogonal |

**Untrusted packages integrity model (nudox):**

1. **Integrity:** BLAKE3 hash is the trust root for bytes (same as S3 path).
2. **Authenticity of “this hash is package P generation G”:** INDEX signature / TLS + catalog row — **not** provided by iroh-blobs alone.
3. **Authorization to download:** INDEX-issued short-lived capability (today: presigned URL; iroh path: signed grant listing allowed hashes + expiry + client EndpointId).

**Recommended iroh authz pattern for Phase 2:**

```
1. Client authenticates to INDEX (existing session / device key).
2. INDEX returns SyncPlan {
     generation,
     hashes: [...],
     providers: [EndpointId...],
     grant: signed { client_endpoint, hashes|manifest, exp }
   }
3. Client dials provider; on Get, provider verifies grant (hook)
   OR client uses only S3 presign and skips iroh for authenticated bulk.
4. Client verifies every byte against BLAKE3.
```

Without step 3, an open blobs provider is a **public CAS mirror** — fine for open-source public packages, **bad** for private tenants.

### 6.3 Multi-tenant isolation

| Layer | Isolation mechanism |
|---|---|
| Catalog | INDEX DB row-level / tenant_id (existing direction) |
| S3 | Bucket prefixes + IAM + presign scope |
| iroh provider | Per-request grant; do **not** share one open provider across tenants without checks |
| Endpoint identity | One client EndpointId per install; bind grants to it |
| Gossip topics | Namespace topic IDs with cryptographic unpredictability |

**Anti-pattern:** one global HashSeq of all packages on one open provider.

### 6.4 Comparison to presigned S3 security

| | Presigned S3 | iroh-blobs + grant |
|---|---|---|
| Time-boxed access | Native | App-level grant |
| Scope to object key | Native | Hash allowlist |
| CDN cache | Easy | N/A |
| Revocation | Short TTL | Short TTL + provider deny list |
| Industry familiarity | High | Low |
| Integrity | Client must hash (nudox will) | Streaming verify built-in |

Presign remains the **path of least regret** for v1 private packages.

---

## 7. Performance

### 7.1 Large binary blobs (source archives, IR)

| Feature | Benefit |
|---|---|
| QUIC streams | Multiplex without head-of-line HTTP/1 issues; cheap parallel Gets |
| Bao verify | Pipeline download ∥ verify; fail fast on corruption |
| Ranges | Fetch IR section without full archive |
| Multipath | Migrate Wi-Fi→Ethernet without restart |
| Resume / bitfields | Laptop lid close resilience |

Expected: **competitive with or better than HTTPS** on direct path for large sequential transfers; **worse** if stuck on congested shared relays.

### 7.2 Many small blobs (source files)

| Strategy | Guidance |
|---|---|
| `GetMany` ordered hash list | Designed for this (0.90+) |
| Pack small files into seekable archive CAS objects | Plan 13 still recommends per-file CAS for dedupe; optional pack for cold transfer |
| HTTP/2 or HTTP/3 to S3 | Extremely optimized for small GETs + CDN edge |
| Connection setup | iroh dial + hole punch adds RTTs vs warm CDN TCP/TLS session reuse |

**At registry scale (thousands of tiny files per generation):** S3+CDN or a **single packed transfer blob** often beats naive per-file P2P. Hybrid: pack for network, unpack into per-file CAS locally (dedupe still works if pack members are hashed individually and stored split).

### 7.3 Resume & parallelism

- Bitfields track sparse partials (post-0.90 store rewrite).
- Downloader `SplitStrategy` can parallelize across providers.
- Multi-provider re-planning mid-fetch (design goal of blobs rewrite).

Map to Plan 14 `SingleFlight` per `ContentHash` and per generation.

### 7.4 Rough performance posture vs alternatives

| Workload | Winner (typical) |
|---|---|
| 500 MB IR, home broadband, public INDEX | S3+CDN or direct iroh provider ≈ similar; CDN often wins TTFB |
| Same, two devs on office LAN, one seeded | **iroh LAN peer assist** wins egress + latency |
| 10k files × 2 KB | HTTP/2 batch or packed archive; pure per-hash P2P mediocre |
| Flaky mobile network | Bao resume strong; also doable with HTTP Range |
| Corporate firewall | HTTPS 443 CDN >> iroh UDP/relay (unless enterprise relays + policy) |

### 7.5 Metrics to collect in a spike

1. Time-to-first-byte generation manifest (HTTP baseline).
2. Full generation sync wall time: S3 only vs iroh provider vs hybrid.
3. % direct vs relayed bytes (iroh metrics).
4. CPU: Bao outboard gen at ingest vs verify at client.
5. Disk amplification: outboard % for IR sizes.
6. Dial latency distribution (p50/p95) desktop→provider.

---

## 8. Rust integration concerns

### 8.1 Async runtime: Tokio is mandatory

iroh / noq / iroh-blobs examples and deps are **Tokio**-centric ([Async Rust challenges in iroh](https://www.iroh.computer/blog/async-rust-challenges-in-iroh) historically explains Tokio choice via quinn).

| Desktop stack choice | Implication |
|---|---|
| Already on Tokio (axum INDEX, many Rust GUIs) | Natural fit |
| smol-first / async-std legacy | **Conflict risk** — Tokio reactors don’t run inside smol without bridging |
| GPUI app with mixed model | Run iroh on a dedicated Tokio runtime thread; channel to UI |

**Practical pattern for GPUI desktop:**

```
Main UI thread / GPUI
    ↕ channels / SyncEvent
Background Tokio runtime (client crate owns it)
    - reqwest INDEX HTTP
    - optional iroh Endpoint + Downloader
    - DiskCas commits
```

Do **not** try to drive iroh from a pure smol executor.

### 8.2 Dependency weight

iroh pulls: QUIC (noq), rustls, relay WS client, address lookup, etc. Acceptable for a desktop code-intel app; measure:

- compile time impact on `client` crate
- binary size delta
- always-on UDP + relay WebSocket battery/network usage (idle Endpoint)

Idle endpoints maintain **home relay WebSocket** — battery/laptops: consider lazy Endpoint spin-up only when syncing.

### 8.3 API stability 2025–2026 (careful documentation)

| Layer | Stability |
|---|---|
| **iroh 1.x wire** | Stable by policy; major bump required for wire break |
| **iroh 1.x Rust API** | Stable intent; minor adds allowed |
| **iroh 0.35** | Maintenance / public relays until end of 2026 |
| **iroh-blobs 0.35** | “Production quality” recommendation on docs.rs |
| **iroh-blobs 0.90–0.103** | Feature-rich canary; expect breaks until 1.0 |
| **iroh-docs / gossip 0.10x** | Coupled to each other; still 0.x |
| **Historical churn** | NodeId rename, crate splits, relay WS-only, multipath — **real** |

**Pinning policy if adopted:**

```
iroh = "1"
iroh-blobs = "=0.35"   # until blobs 1.0, OR
# iroh-blobs = "0.103" with cargo update discipline + integration tests
```

Run compatibility tests in CI between client and provider versions.

### 8.4 Embedding checklist

- [ ] Persist `SecretKey` for stable EndpointId (or ephemeral if privacy-preferring; then grants bind differently)
- [ ] Configure dedicated relays for production builds (not public preset alone)
- [ ] Feature-flag entire iroh stack (`client` crate `p2p` feature)
- [ ] Graceful degrade to HTTP-only if Endpoint bind fails (UDP blocked)
- [ ] Shutdown Router cleanly on app quit
- [ ] Metrics: dial success, direct ratio, bytes by path
- [ ] No iroh in compiler pods v1

### 8.5 MSRV / edition

crates advertise high MSRV (1.91 as of mid-2026) and edition2024 appeared in canary notes. Align workspace MSRV before depending.

---

## 9. Comparison matrix (nudox sync transports)

### 9.1 Contenders

| Tech | One-line |
|---|---|
| **HTTP + presigned S3** | Plan 21 default: control JSON + CAS GET |
| **iroh + iroh-blobs** | Dial-by-key QUIC + Bao BLAKE3 streams |
| **IPFS / libp2p / Bitswap** | Public swarm CAS; see `04-ipfs-and-cas` reject-as-primary |
| **BitTorrent** | Swarm file distribution; great for bulk ISOs, weak multi-tenant authz |
| **rsync / zsync** | Delta over mutable trees; wrong for immutable CAS |
| **Git protocol** | Want/have + packs; great prior art, heavy as product transport |
| **NATS** | Server message bus; not blob store; fine for control events |

### 9.2 Full comparison

| Criterion | HTTP+S3+presign | iroh-blobs | IPFS/Bitswap | BitTorrent | rsync | Git pack | NATS |
|---|---|---|---|---|---|---|---|
| Content integrity | Client BLAKE3 | Streaming Bao BLAKE3 | CID (often SHA-256) | Piece hashes | Checksums optional | SHA-1/256 objects | App-level |
| Matches nudox BLAKE3 CAS | Yes | **Native** | Optional multihash | No | No | No | N/A |
| Want/have missing set | App HTTP | GetMany / fetch | Bitswap | Have bitfield | File list | Native | N/A |
| Multi-tenant private pkgs | **Excellent** | Needs grants | Poor public | Poor | SSH ACL | SSH/HTTP ACL | Auth subjects |
| CDN / cheap bulk | **Excellent** | Weak | Gateways dying | Trackers/DHT | No | No | No |
| NAT / P2P desktop | N/A (client-server) | **Excellent** | Variable | Good | Needs reachability | Needs reachability | Client-server |
| LAN peer assist | Manual only | **Natural** | Possible | Natural | Possible | Possible | No |
| Ops complexity | Low (S3) | Medium (relays, keys) | High | Medium | Low | Medium | Medium |
| Offline after sync | Yes | Yes | Yes if pinned | Yes if complete | Yes | Yes | N/A |
| Catalog / search | HTTP INDEX | No | No | No | No | No | Events only |
| Resume | HTTP Range | Bitfields | Session-dependent | Standard | Partial | Pack oriented | N/A |
| Corporate firewall | **Best** (443) | UDP/WS may struggle | Often blocked | Often blocked | 22/873 | 443 smart-HTTP | 443 possible |
| CRDT multi-writer | No (good) | docs optional (unneeded) | No | No | No | No | No |
| Maturity for SaaS registry | **Highest** | Medium | Medium ecosystem, wrong fit | High for public bulk | High niche | High for source | High for messaging |
| v1 recommendation | **Primary** | Optional Phase 2 | Reject primary | Reject | Reject | Steal want/have ideas only | Optional notify |

### 9.3 Narrative comparisons

#### vs HTTP+S3+presign (the incumbent plan)

**Keep S3 as durable CAS and default pull path.** iroh does not replace object storage; at best it becomes another **retrieval transport** in front of the same bytes. Control plane stays HTTP (resolve, search, jobs, sync-plan). iroh never carries Tantivy queries or ORCH job admission.

#### vs IPFS/libp2p

Sibling report `04-ipfs-and-cas` already rejects public IPFS as package distribution. iroh is **what n0 became after leaving IPFS compatibility** — lighter, BLAKE3-first, dial-by-key, no DHT required. If nudox ever wants P2P, **iroh is the better P2P bet than Kubo**. That still doesn’t make P2P the default data plane.

#### vs BitTorrent

Excellent swarming for immutable public artifacts; weak private multi-tenant story; piece size / torrent files parallel HashSeq but ecosystem is file/swarm oriented, not library-embedded CAS for app-specific IR. iroh-blobs is closer to a library than libtorrent for embedding in a Rust desktop client.

#### vs rsync

rsync shines on mutable directory trees with rolling checksums. nudox generations are **immutable hash closures** — set difference on hashes beats rolling checksums. rsync also needs SSH/daemon reachability without the NAT story.

#### vs Git protocol

**Best prior art for want/have** (Plan 14 already maps this). Do not speak git wire on the open internet for IR blobs; optionally reuse pack-like batching as an application encoding over HTTP or iroh streams.

#### vs NATS

Use NATS (or Redis, or SSE) **inside INDEX infrastructure** for “generation published” fanout to API nodes. Do not put NATS clients in every desktop for blob sync. NATS ≠ CAS.

---

## 10. Verdict and integration sketch

### 10.1 Final verdict

| Decision | Detail |
|---|---|
| **Replace data plane?** | **No** for v1 |
| **Augment data plane?** | **Yes, Phase 2** optional transport |
| **Replace control plane?** | **No** |
| **Adopt iroh-docs?** | **No** for packages |
| **Adopt iroh-gossip?** | Optional notify / provider discovery later |
| **Adopt iroh core only?** | Only if building custom protocols; not required alone |
| **Borrow ideas without crate?** | Yes — want/have, Bao outboards optional |

### 10.2 What stays HTTP JSON (Plan 21)

- `/v1/hello` protocol negotiation
- Auth session / device registration
- Package resolve, search, graph, symbols
- Sync plan: want/have hash lists, generation status
- Job submit / ORCH status
- Admin verify/rebuild
- Presign issuance for S3
- Optional: list of iroh provider EndpointIds + grants

### 10.3 What could move to iroh data plane (Phase 2)

- Fetch missing CAS blobs from provider EndpointIds
- LAN peer assist among desktops / office cache
- Optional live “content ready” gossip (non-authoritative)
- Sneakernet: BlobTicket export/import of a generation for airgapped machines

### 10.4 Concrete integration sketch

```
                    ┌─────────────────────────────────────┐
                    │           GUI (GPUI)                │
                    └─────────────────┬───────────────────┘
                                      │ events
                    ┌─────────────────▼───────────────────┐
                    │         client library              │
                    │  SyncEngine | QueryRouter | Cas     │
                    └──────┬───────────────┬──────────────┘
           control HTTP    │               │ data plane (feature-flagged)
                           ▼               ▼
                    ┌────────────┐   ┌─────────────────────────────────┐
                    │   INDEX    │   │  BlobTransport trait            │
                    │  catalog   │   │   ├─ HttpS3Transport (default) │
                    │  search    │   │   └─ IrohBlobsTransport (opt)  │
                    └─────┬──────┘   └─────────────────────────────────┘
                          │ presign / provider list
                          ▼
                    ┌────────────┐     ┌──────────────┐
                    │    S3      │     │ iroh provider│
                    │ cas/blake3 │     │ workers      │
                    └────────────┘     └──────────────┘
```

**Trait sketch (illustrative, not normative code):**

```rust
#[async_trait]
trait BlobTransport: Send + Sync {
    async fn fetch_many(
        &self,
        want: &[ContentHash],
        ctx: FetchCtx, // grant, progress, cancel
    ) -> Result<(), FetchError>;
}

struct HttpS3Transport { /* presigned URLs from INDEX */ }
struct IrohBlobsTransport { endpoint, downloader, providers: Vec<EndpointId> }

struct TieredBlobTransport {
    primary: HttpS3Transport,
    assist: Option<IrohBlobsTransport>, // try LAN/providers first for subset
}
```

**SyncEngine loop (unchanged shape from Plan 14):**

```
for (package, generation) in DepSet:
  manifest = INDEX.get_manifest(package, generation)  // HTTP
  missing = manifest.closure() \ local_cas.has_batch(...)
  transport.fetch_many(missing, ctx)  // S3 and/or iroh
  verify all blake3
  commit generation Ready
```

### 10.5 Migration path

| Phase | Work | Exit criteria |
|---|---|---|
| **0 — v1 ship** | HTTP+S3 only; implement want/have; DiskCas commit | Product works offline after sync |
| **1 — spike** | Feature-flagged iroh Endpoint in client; lab provider serving one generation from DiskCas | Parity test: same hashes via S3 vs iroh |
| **2 — authz** | Signed download grants; provider hooks | Private tenant cannot fetch others’ hashes |
| **3 — multi-source** | INDEX returns provider list; Downloader prefers LAN | Measured egress reduction in office scenario |
| **4 — optional gossip** | Generation-ready + peer announce | Document delivery guarantees (best-effort) |
| **5 — harden** | Dedicated relays; metrics; blobs 1.0 upgrade | Production enterprise offering |

**Do not** block librarification or Plan 21 on iroh.

### 10.6 Anti-goals (explicit)

- Replacing S3 as system of record with iroh stores.
- Putting search/index state in iroh-docs.
- Depending on public n0 relays for paid product SLA.
- Compiler-pod iroh mesh for artifact exchange (presign is enough).
- Exposing unauthenticated open CAS providers on the public internet for private packages.

**Build-ready detail for Phase 2 lives in §11** (BlobTransport, providers list, provider topology, hash parity, outboards, desktop Endpoint lifecycle, failure/fallback, security, metrics, spike plan).

---

## 11. Expanded Phase-2 integration design (build-ready)

This section turns the INDEX→REGISTRY sync recommendation into a concrete design a team can implement without inventing interfaces. **v1 ships HTTP+S3 only.** Phase 2 adds an optional iroh-blobs data-plane path behind the same SyncEngine loop. **Subscription / catalog / resolve stay HTTP forever** (or SSE/long-poll as Plan 14/21 evolve) — iroh never becomes the control plane.

Cross-links (normative peers):

| Plan | What this section extends |
|---|---|
| [14-client-sync](../../librarification/14-client-sync/PLAN.md) § BlobTransport / iroh Phase-2 | SyncEngine owns transport selection; trait lives in client-core |
| [21-wire-protocol](../../librarification/21-wire-protocol/PLAN.md) § sync/plan providers | Optional `providers.http` / `providers.iroh` on `SyncPlanResponse` |
| [13-storage](../../librarification/13-storage/PLAN.md) § cas/ layout | Outboard files next to CAS objects; hash = raw BLAKE3 of logical bytes |

### 11.1 Architecture invariants (do not break)

1. **Control plane = HTTP/JSON** (`POST /v1/sync/plan`, resolve, search, jobs). Unchanged shape from Plan 21.
2. **Data plane = pluggable `BlobTransport`**. v1 implementation is HTTPS presign; Phase 2 adds iroh.
3. **Integrity = BLAKE3 content hash.** Every byte path (S3 or iroh) ends in the same verify-before-commit rule (Plan 14).
4. **S3 remains system of record** for durable CAS. iroh providers *serve* or *cache* the same objects; they do not replace object storage.
5. **Subscription is not iroh.** “Package G is ready / generation advanced” uses HTTP poll, SSE, or later optional gossip for *provider discovery only* — never as catalog authority.
6. **No multi-writer package docs.** iroh-docs is out of scope for generations/catalog (see §3, §10.6).

```
                    ┌─────────────────────────────────────┐
                    │           GUI (GPUI)                │
                    └─────────────────┬───────────────────┘
                                      │ SyncEvent / progress
                    ┌─────────────────▼───────────────────┐
                    │  client (tokio runtime)             │
                    │  SyncEngine → BlobTransport         │
                    │  DiskCas commit → Ready witness     │
                    └──────┬───────────────┬──────────────┘
           control HTTP    │               │ fetch(hashes) → verified bytes
                           ▼               ▼
                    ┌────────────┐   ┌──────────────────────────────────────┐
                    │   INDEX    │   │  BlobTransport                      │
                    │  catalog   │   │   ├─ HttpPresignTransport  (v1)     │
                    │  sync/plan │   │   └─ IrohBlobsTransport    (ph.2)   │
                    └─────┬──────┘   └──────────────────────────────────────┘
                          │ SyncPlanResponse:
                          │   want[], providers.http[], providers.iroh[]
                          ▼
              ┌───────────┴────────────┐
              │                        │
       ┌──────▼──────┐          ┌──────▼──────────────────┐
       │ S3 cas/{h}  │          │ nudox-iroh-provider     │
       │ (SoT)       │◄─read───│ (sidecar / edge fleet)  │
       └─────────────┘          │ serves CAS + outboards  │
                                └─────────────────────────┘
```

### 11.2 `trait BlobTransport` (heart or client-core)

**Home:** prefer `client-core` (or `client` feature-gated module) so `heart` stays free of network crates. If a zero-dep interface is desired for testing, put a minimal trait in `heart` with no iroh/reqwest types — implementations live in `client`.

**Contract (normative sketch):**

```rust
/// Data-plane pull of content-addressed blobs into local CAS.
/// Control plane (sync/plan, resolve) is NOT part of this trait.
#[async_trait]
pub trait BlobTransport: Send + Sync {
    /// Fetch each hash; stream verified logical bytes into `sink`.
    /// Implementations MUST reject incomplete/corrupt data before sink.commit.
    /// Desktop v1/Phase-2: **fetch only** — no `put` required on desktop.
    async fn fetch(
        &self,
        hashes: &[ContentHash],
        ctx: &FetchCtx,
        sink: &dyn BlobSink,
    ) -> Result<FetchReport, FetchError>;

    /// Optional mid-flight cancel; default no-op.
    fn cancel(&self) {}
}

/// Where verified bytes land (DiskCas staging). Transport does not own commit-to-Ready.
pub trait BlobSink: Send + Sync {
    fn begin(&self, hash: ContentHash, size_hint: Option<u64>) -> Result<Box<dyn BlobWrite>, SinkError>;
}

pub trait BlobWrite: Send {
    fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), SinkError>;
    /// Called only after transport has authenticated the full object (hash or Bao).
    fn finalize(self: Box<Self>, expected: ContentHash) -> Result<(), SinkError>;
}

pub struct FetchCtx {
    pub grant: Option<DownloadGrant>,     // INDEX-issued; required for private iroh path
    pub providers: ProviderSet,           // from SyncPlanResponse
    pub progress: ProgressTx,             // maps to SyncEvent / JobProgress
    pub cancel: CancellationToken,
    pub prefer: TransportPreference,      // from config / feature flag
}

pub struct FetchReport {
    pub completed: Vec<ContentHash>,
    pub failed: Vec<(ContentHash, FetchError)>,
    pub bytes_by_path: BytesByPath,       // direct | relay | https — for metrics
}
```

**Rules:**

| Rule | Detail |
|---|---|
| **put not required on desktop** | Clients only pull. Push/upload stays compiler-pod / INDEX ingest (presigned PUT). |
| **No Ready flip inside transport** | SyncEngine commits generation after *all* closure hashes are in DiskCas (Plan 14). |
| **Hash is authority** | Even if Bao streaming verified chunks, final `ContentHash` must match CAS key before `finalize`. |
| **SingleFlight stays above transport** | Coalesce concurrent `fetch` of the same hash in SyncEngine (existing `SingleFlight`). |
| **Idempotent** | Re-fetch of an already-present hash is a no-op success. |

### 11.3 `HttpPresignTransport` (v1) vs `IrohBlobsTransport` (Phase 2)

#### HttpPresignTransport (product default)

| Item | Spec |
|---|---|
| **Input** | `providers.http` (or legacy `urls[]`) from `SyncPlanResponse` — list of `{ hash, url, method: GET, headers?, size_hint? }` |
| **Mechanism** | `reqwest` GET (HTTP/2); optional Range resume later |
| **Verify** | `ContentHash::of_bytes` (or streaming hasher) equals requested hash |
| **Authz** | Short-TTL presigned URL; no long-lived S3 creds on desktop |
| **Deps** | Existing HTTP stack; no iroh, no UDP |
| **When** | Always available; sole path when `sync.transport = http` or iroh disabled/failed |

#### IrohBlobsTransport (Phase 2 optional)

| Item | Spec |
|---|---|
| **Input** | `providers.iroh` — EndpointIds, optional relay hints, tickets, + `DownloadGrant` |
| **Mechanism** | Tokio `Endpoint` + `iroh-blobs` Downloader / `Get` / `GetMany`; Bao verified streams |
| **Verify** | Protocol streaming verify **plus** same CAS key check into DiskCas |
| **Authz** | Provider checks INDEX-signed grant (hash allowlist + client EndpointId + exp); integrity still BLAKE3 |
| **Deps** | `iroh` 1.x + pinned `iroh-blobs` (0.35 production-quality or later 1.0); feature `p2p` / `iroh` |
| **When** | `sync.transport = iroh \| auto` and plan returns iroh providers |

#### Auto / tiered selection

```rust
struct AutoBlobTransport {
    http: HttpPresignTransport,
    iroh: Option<IrohBlobsTransport>,
}

// prefer order for sync.transport = auto:
// 1. LAN / same-site iroh providers (if dialable quickly)
// 2. Regional nudox-iroh-provider
// 3. HTTPS presign (always complete residual missing set)
```

**Partial success policy:** if iroh completes a subset and then stalls (relay limited, dial fail), **automatically continue residual hashes on HTTPS** using the same plan’s `providers.http` entries. Never leave the generation half-stuck because iroh failed if S3 URLs remain valid.

### 11.4 Control plane: `POST /v1/sync/plan` (unchanged role, extended response)

**Request unchanged** (Plan 21):

```json
{
  "want_from": [{ "package_id": "…", "generation_hash": "…" }],
  "have": ["<blake3 hex>", "…"],
  "limit": 512,
  "cursor": null,
  "client_endpoint_id": null
}
```

Phase-2 **optional** request field:

| Field | Type | Purpose |
|---|---|---|
| `client_endpoint_id` | hex/base32 EndpointId, optional | Bind download grant to this client; omit if HTTP-only |

**Response (v1 compatible + Phase-2 extension):**

```json
{
  "want": ["h1", "h2"],
  "urls": [ { "hash": "h1", "url": "https://…", "method": "GET", "size_hint": 1234 } ],
  "expires_at": "2026-07-16T12:05:00Z",
  "next_cursor": null,
  "manifests": [ /* GenerationManifestDto */ ],
  "providers": {
    "http": [
      { "hash": "h1", "url": "https://…", "method": "GET", "size_hint": 1234 }
    ],
    "iroh": {
      "endpoints": [
        {
          "endpoint_id": "…",
          "relay_url": "https://relay.nudox.example",
          "addrs": [],
          "region": "us-east-1",
          "priority": 10
        }
      ],
      "tickets": [],
      "grant": {
        "v": 1,
        "client_endpoint_id": "…",
        "hashes": ["h1", "h2"],
        "exp": "2026-07-16T12:05:00Z",
        "sig": "…"
      }
    }
  }
}
```

**Compatibility rules:**

1. v1 clients read `want` + `urls` only; ignore `providers`.
2. Phase-2 clients prefer `providers.http` if present, else fall back to top-level `urls` (alias during migration).
3. `providers.iroh` may be **null / omitted** when: feature off, no provider fleet in region, client did not send EndpointId, or tenant policy disables P2P.
4. **INDEX still does not proxy blob bytes** (Plan 21 invariant).

See Plan 21 § sync plan DTO extension for field-level wire types.

### 11.5 Provider topology: `nudox-iroh-provider`

#### Role

INDEX API processes stay thin (catalog, auth, sync-plan, presign). Blob serving over iroh is a **sidecar / edge worker**:

```
┌─────────────────────────────────────────────────────────┐
│  INDEX region (e.g. us-east-1)                          │
│  ┌──────────────┐    advertise EndpointIds               │
│  │ INDEX API    │──────────────────────────────────┐    │
│  │ (HTTP only)  │                                  │    │
│  └──────────────┘                                  ▼    │
│  ┌────────────────────────────────────────────────────┐ │
│  │ nudox-iroh-provider  (1..N replicas)               │ │
│  │  - iroh Endpoint + BlobsProtocol                   │ │
│  │  - grant verification hook on Get                  │ │
│  │  - read path: local DiskCas cache → S3 object_store│ │
│  │  - outboards: cas/{hash}.outboard or recompute     │ │
│  └────────────────────────────────────────────────────┘ │
│                          │                              │
│                          ▼                              │
│                   S3 cas/{blake3-hex}                   │
└─────────────────────────────────────────────────────────┘
```

#### Naming & deployment

| Item | Choice |
|---|---|
| **Binary / crate** | `nudox-iroh-provider` (workspace binary; not in desktop critical path) |
| **Colocation** | Same region as S3 + INDEX; optional office “edge cache” appliances later |
| **Scaling** | HPA on CPU/egress; each replica has stable `SecretKey` → stable EndpointId registered in INDEX provider registry |
| **Discovery** | INDEX DB table `iroh_providers(endpoint_id, region, relay_url, weight, healthy)`; sync/plan samples by region + health |
| **Health** | Provider heartbeats to INDEX (or sidecar probes); unhealthy endpoints omitted from plans |

#### Grants / tickets

| Primitive | Use |
|---|---|
| **DownloadGrant (preferred)** | INDEX-signed structure: `{ client_endpoint_id, hash_set or manifest_hash, exp, tenant }` verified by provider on each Get. Coordination-server pattern (INDEX is the coordinator). |
| **BlobTicket** | Optional bootstrap for LAN sneakernet / QR “airdrop this generation”; **not** primary multi-tenant authz (stale dial info; weak ACL). |
| **EndpointTicket** | Rare; prefer EndpointId + address lookup / relay_url from plan. |

Grant verification must run **before** streaming bytes. Open providers are only acceptable for fully public OSS CAS mirrors.

#### Multi-region

1. Client’s INDEX base URL implies region (or `/v1/hello` returns `region`).
2. Plan returns in-region providers first (`priority`); optional cross-region only if local unhealthy.
3. Relays: **dedicated** per major region for production; never depend on public n0 relays for paid SLA.
4. S3 remains global SoT; providers are regional read caches + dial targets.

#### What INDEX API does **not** do

- Accept iroh dials on the query node.
- Embed long-lived BlobTickets as the only auth mechanism.
- Put catalog rows into iroh-docs.

### 11.6 Hash parity checklist

**Goal:** `heart::ContentHash` bytes == `iroh_blobs::Hash` bytes for the same logical CAS object.

| # | Check | Expected |
|---|---|---|
| 1 | Algorithm | BLAKE3 |
| 2 | Digest length | 32 bytes |
| 3 | Domain separation on **content** blobs | **None** — `ContentHash::of_bytes` is raw `blake3::hash(bytes)` ([`workspace/heart/content.rs`](../../../workspace/heart/content.rs)) |
| 4 | Do **not** confuse with `JobKey` | `JobKey::derive` length-prefixes components — **not** a CAS content key |
| 5 | Compression envelope | If Plan 13 stores compressed-on-disk forms, CAS key must be documented as hash of **logical** (pre-envelope) or **stored** bytes — iroh must hash the **same** sequence the CAS key names |
| 6 | DiskCas envelope | If local file is `blake3(value) ‖ value`, strip envelope before exposing to iroh provider; provider serves **raw logical bytes** whose BLAKE3 is the key |
| 7 | Encoding | JSON wire: lowercase hex; iroh tickets: base32; protocol: raw 32 bytes — convert only at boundaries |
| 8 | Chunk size / Bao outboard | Transfer-only; **must not** change root hash |
| 9 | Unit test | `assert_eq!(ContentHash::of_bytes(b).as_bytes(), iroh_blobs::Hash::new(b).as_bytes())` (or equivalent API) for empty, 1B, 1KiB, 1MiB, 16MiB-1 fixtures |
| 10 | Golden vector | Check in a known fixture file used by both Http and Iroh paths in CI |

**Conversion helpers (Phase 2):**

```rust
impl From<ContentHash> for iroh_blobs::Hash { /* copy 32 bytes */ }
impl From<iroh_blobs::Hash> for ContentHash { /* copy 32 bytes */ }
```

If any future change introduces keyed BLAKE3 for content, **Phase 2 is blocked** until mapping is redefined — treat that as a breaking storage change.

### 11.7 Outboard / Bao layout

| Topic | Decision |
|---|---|
| **What** | Bao-style external outboard (~6% at 1 KiB chunks) for verified streaming of large blobs |
| **Where (INDEX / provider)** | Next to object: `cas/{hash}` (or S3 key) + `cas/{hash}.outboard` (or `cas/{hash}.bao`) |
| **Where (desktop)** | Optional: `cas/{hash}.outboard` only if the desktop also *provides* LAN assist; pure requesters can discard outboards after verify |
| **When to generate** | **Large blobs at ingest** (IR, archives, above threshold e.g. ≥ 1 MiB): write outboard in compiler emit / INDEX ingest path. **Small files:** lazy compute on provider first serve or skip Bao and send simple verified blob if protocol allows |
| **Lazy path** | Provider on cache miss: GET from S3 → compute outboard → serve → optionally write outboard back to cache/S3 |
| **S3** | Optional second object `cas/{hash}.outboard`; CDN-cacheable immutable |
| **Does not affect** | Root `ContentHash` / CAS key (outboard is not part of content identity) |

```
# Remote / provider disk (illustrative)
cas/ab/cd/{64hex}            # raw logical bytes (SoT object body)
cas/ab/cd/{64hex}.outboard   # Bao outboard (derived)

# Desktop REGISTRY (Plan 13 layout + optional)
cas/ab/cd/{64hex}.blob       # after sync commit
cas/ab/cd/{64hex}.outboard   # only if acting as LAN provider
```

### 11.8 Desktop Endpoint lifecycle

| Mode | Behavior | Battery / cost | Recommendation |
|---|---|---|---|
| **Always-on** | Endpoint + home-relay WebSocket for app lifetime | Higher idle drain; better peer-assist readiness | Enterprise office cache appliances only |
| **On-demand (default)** | Create Endpoint when SyncEngine needs iroh; shut down after idle timeout (e.g. 60–120s no fetches) | Low idle cost | **Desktop product default** |
| **Lazy during sync only** | Endpoint lives strictly inside `fetch()` scope | Lowest complexity; re-dial cost each sync wave | Acceptable for spike / v1-of-Phase-2 |
| **Disabled** | `sync.transport = http` or build without `p2p` feature | Zero | Default until Phase 2 ships |

**Lifecycle sketch:**

```
SyncEngine.ensure(generation)
  → POST /v1/sync/plan (include client_endpoint_id if iroh enabled)
  → if providers.iroh present && transport allows:
        endpoint = pool.acquire()   // start Endpoint if None
        IrohBlobsTransport.fetch(...)
  → residual missing → HttpPresignTransport.fetch(...)
  → pool.release_idle()             // schedule shutdown timer
  → verify + commit Ready
  // Endpoint NOT required for local query path after Ready
```

**Persistence:** store `SecretKey` under OS keychain / app data for stable EndpointId (improves grant binding + LAN reputation). Offer privacy mode: ephemeral key each session (grants must use short TTL + re-plan).

**Runtime:** dedicated Tokio runtime in `client` (GPUI must not drive iroh). Feature-flag compile: `client` crate `p2p` feature pulls iroh.

### 11.9 Failure modes and automatic fallback

| Failure | Detection | Action |
|---|---|---|
| **UDP blocked / no QUIC** | Endpoint bind or dial timeout; 0 direct paths | Stay on relay if grant+relay allow; **if relay fails or rate-limited → HTTPS presign** |
| **Relay-only path** | Metrics: direct_ratio ≈ 0 | Continue if dedicated relay healthy; else HTTPS. Do not burn public n0 relay quota in prod builds |
| **Provider 404 / missing hash** | Get aborts | Retry other EndpointIds; then HTTPS for that hash |
| **Grant expired / denied** | Provider reject | Re-call `POST /v1/sync/plan`; do not spin |
| **Bao/hash mismatch** | Verify error | Drop partial; retry once alternate path; then Fail generation (never Ready) |
| **Partial iroh success** | Subset completed | Complete residual via HTTPS automatically |
| **Plan urls expired mid-sync** | HTTP 403 | Re-plan; resume missing only |
| **App suspend / lid close** | Cancel token | Staging remains; next ensure resumes (HTTP Range and/or iroh bitfields) |
| **No iroh providers in plan** | `providers.iroh` absent | Silent HTTPS-only (not an error) |
| **Feature off / binary without p2p** | Config | HTTPS-only |

**Invariant:** `sync.transport = auto` must **never** be worse than pure HTTP for eventual completion when S3 presigns are present. iroh is acceleration + LAN assist, not a hard dependency.

```
fetch(want):
  missing = want
  if iroh_enabled and providers.iroh:
    missing = iroh.fetch(missing)  // returns still-missing
  if missing non-empty:
    missing = http.fetch(missing)
  if missing non-empty:
    return Error::Incomplete
  return Ok
```

### 11.10 Security model (Phase 2)

| Layer | Rule |
|---|---|
| **No multi-writer docs** | Package catalog/generations are server-authored only. Do not use iroh-docs. |
| **Integrity = hash** | Untrusted package bytes are safe to pull from any provider **iff** BLAKE3 matches INDEX-stated CAS key. |
| **Authenticity of mapping** | “hash H belongs to package P generation G” comes from INDEX TLS + catalog/manifest — not from the peer. |
| **Authorization** | Private tenants: INDEX grant scoped to **hash set from this sync plan** (or single manifest closure), expiry, and client EndpointId. Provider enforces grant. |
| **Presign still primary ACL for HTTP** | S3 URL signature remains the HTTP authz story. |
| **Untrusted bytes still hash-verified** | LAN peer assist, office cache, or compromised provider cannot inject wrong IR if client verifies. |
| **Open CAS** | Only for public packages deliberately mirrored; never default for private INDEX. |
| **Relay metadata** | Relays see IPs/timing/volume — use dedicated relays for enterprise; document in privacy policy. |
| **Desktop as provider** | Optional LAN assist is a **ToS + license** product decision; default **off** until legal review. If on, still serve only hashes the peer proves via grant/plan, not whole DiskCas browse. |

### 11.11 Metrics and feature flag

#### Feature flag / config

```
# client config (illustrative)
sync.transport = "http" | "iroh" | "auto"   # default: "http" until Phase 2 GA; then "auto"
sync.iroh.idle_shutdown_ms = 90000
sync.iroh.allow_lan_provide = false
sync.iroh.relay_mode = "dedicated" | "n0_public_dev_only"
```

`GET /v1/hello` may advertise `"features": […, "sync.plan", "cas.presign", "sync.iroh"]` so clients know whether to send `client_endpoint_id`.

#### Metrics (client + provider)

| Metric | Labels / notes |
|---|---|
| `sync_fetch_bytes_total` | `path=https\|iroh_direct\|iroh_relay` |
| `sync_fetch_duration_seconds` | histogram per generation / per hash size bucket |
| `sync_transport_fallback_total` | `from=iroh, to=https, reason=…` |
| `iroh_dial_success` / `iroh_dial_fail` | reason |
| `iroh_direct_ratio` | bytes direct / bytes iroh |
| `iroh_grant_reject_total` | provider-side |
| `cas_outboard_hit` / `cas_outboard_miss` | provider |
| `sync_hash_mismatch_total` | should be ~0; page on any sustained rate |

### 11.12 Spike plan (1 week)

**Goal:** prove hash parity and measure whether iroh is competitive with S3 for a realistic package pull — **not** to ship product.

| Day | Work | Exit |
|---|---|---|
| **1** | Hash parity test crate: fixtures empty→16MiB; `ContentHash` ↔ iroh `Hash`; DiskCas raw-bytes audit (envelope strip) | CI test green |
| **2** | Minimal `nudox-iroh-provider` reading one local `cas/` tree; lab EndpointId hardcoded | `sendme`-style pull of one blob |
| **3** | Desktop/dev client: `IrohBlobsTransport` behind feature flag; SyncEngine test harness (no GUI) | Pull generation closure via iroh only |
| **4** | Same closure via `HttpPresignTransport` against S3/MinIO; instrument p50/p95 wall time, CPU, peak RSS | Side-by-side numbers |
| **5** | Failure drill: block UDP (pf/firewall), confirm relay-only and HTTPS fallback; grant expiry re-plan | Fallback matrix documented |
| **6** | 1GB package (or synthetic IR blob set totaling ~1GB): multi-hash GetMany vs parallel HTTPS | p95 vs S3 report |
| **7** | Write spike report: go/no-go for Phase 2 engineering; pin crate versions; open questions §12 updated with data | Decision memo |

**Success criteria (suggest):**

1. Zero hash mismatches across both transports on same fixtures.
2. Direct-path iroh p95 within ~1.2× of same-region S3 for 1GB sequential-ish workload **or** clear LAN win (≥2×) for dual-desktop scenario.
3. Automatic HTTPS fallback succeeds when UDP blocked.
4. No dependency on public n0 relays in the “prod-like” config path.

**Fail / defer if:** blobs API thrash blocks pinning; corporate UDP failure dominates without affordable dedicated relays; no perf or egress win vs CDN.

### 11.13 Explicit non-goals (Phase 2)

| Non-goal | Why |
|---|---|
| **iroh-docs for catalog / generations** | Wrong consistency model; INDEX HTTP owns authority |
| **Gossip as primary subscription** | Catalog events stay HTTP (poll/SSE); gossip at most provider discovery later |
| **Replace S3 SoT with iroh FsStore** | Dual storage tax; ops regression |
| **Desktop `put` / client upload of INDEX CAS** | Compiler/ingest paths only |
| **Compiler-pod iroh mesh** | Presign PUT/GET sufficient (Plan 12/21) |
| **Public n0 relays as production SLA** | Rate limits; no enterprise contract |
| **Blocking v1 librarification on iroh** | Phase 0 ships HTTP+S3 |
| **Open unauthenticated private CAS** | Multi-tenant leak |
| **Browser/WASM as first iroh client** | GPUI native path only for Phase 2 |

### 11.14 Implementation checklist (when green-lit)

- [ ] `BlobTransport` + `HttpPresignTransport` in client (v1 can ship trait with one impl)
- [ ] `SyncPlanResponse.providers` wire fields (Plan 21) — iroh section optional
- [ ] Hash conversion + parity tests in CI
- [ ] Outboard generate-on-ingest for large blobs; layout next to `cas/{hash}`
- [ ] `nudox-iroh-provider` binary + grant verify hook
- [ ] INDEX provider registry + plan sampling by region
- [ ] Feature flag `sync.transport`; default `http`
- [ ] On-demand Endpoint; dedicated relay config for non-dev
- [ ] Metrics + HTTPS fallback path tested
- [ ] Pin `iroh` 1.x + chosen `iroh-blobs`; document upgrade gate to blobs 1.0

---

## 12. Open questions

1. **Hash domain equality:** Confirm `heart::ContentHash` is raw BLAKE3-256 of file bytes with no prefix; document conversion to `iroh_blobs::Hash`. *(Code read 2026-07-16: `of_bytes` is raw BLAKE3 — still need DiskCas envelope + compression policy written down as SoT.)*
2. **Outboard storage policy:** Generate at INDEX ingest for large blobs only, or always compute client-side on first provide? *(§11.7 default: large-at-ingest, small lazy.)*
3. **Desktop EndpointId persistence vs privacy:** Stable ID helps grants & peer assist; ephemeral ID reduces tracking — product choice. *(§11.8 default: stable + optional privacy mode.)*
4. **Enterprise UDP policy:** What fraction of target customers block UDP/QUIC? Need relay-only mode + dedicated relays cost model.
5. **iroh-blobs 1.0 timeline:** Gate deep integration on blobs stable release, or accept 0.35 API forever?
6. **Provider topology:** Sidecar per INDEX region vs separate CAS edge service? *(§11.5 default: regional sidecar `nudox-iroh-provider`.)*
7. **Peer assist legal/ToS:** May desktops redistribute package bytes to LAN peers under package licenses / nudox ToS? *(Default off until review.)*
8. **Battery impact:** Always-on Endpoint vs on-demand during SyncEngine only? *(§11.8 default: on-demand.)*
9. **Comparison spike data:** Need real numbers (§11.12) before productizing Phase 2.
10. **WASM/remote agents:** Any future non-GPUI client that would prefer iroh-ffi over HTTP?
11. **Relay region placement:** If Phase 2 ships, where do we colocate dedicated relays relative to INDEX + S3 regions?
12. **Interaction with packed archives (Plan 13):** If cold transfer uses zip/seekable packs, does iroh fetch pack blob only, or still per-file? *(Prefer: pack is just another CAS hash; per-file CAS remains SoT after unpack.)*
13. **Grant signing key hierarchy:** INDEX root vs per-tenant keys; rotation without breaking in-flight syncs.
14. **Compression vs iroh identity:** Confirm CAS key is always pre- or post-zstd consistently across HTTP and iroh (§11.6 row 5).

---

## 13. Sources (primary)

### Official

- https://www.iroh.computer/
- https://www.iroh.computer/blog/v1 — Iroh 1.0 announcement (2026-06-15)
- https://www.iroh.computer/blog/the-road-to-iroh-1-0 — History & pivots (2026-07-09)
- https://www.iroh.computer/blog/iroh-0-28-let-them-have-crates — Protocol crate split
- https://www.iroh.computer/blog/iroh-blobs-0-90-new-features — GetMany, Observe, Downloader
- https://www.iroh.computer/blog/blake3-hazmat-api — BLAKE3/Bao integration
- https://docs.iroh.computer/what-is-iroh
- https://docs.iroh.computer/concepts/endpoints
- https://docs.iroh.computer/concepts/relays
- https://docs.iroh.computer/concepts/tickets
- https://docs.iroh.computer/concepts/address-lookup
- https://docs.iroh.computer/protocols/blobs
- https://docs.iroh.computer/protocols/documents
- https://docs.iroh.computer/connecting/gossip
- https://docs.iroh.computer/deployment/security-privacy
- https://docs.iroh.computer/about/release-policy
- https://github.com/n0-computer/iroh
- https://github.com/n0-computer/iroh-blobs
- https://github.com/n0-computer/iroh-docs
- https://github.com/n0-computer/iroh-gossip
- https://crates.io/crates/iroh
- https://docs.rs/iroh-blobs/latest/iroh_blobs/

### Secondary / coverage

- https://byteiota.com/iroh-1-0-peer-to-peer-networking/
- https://www.techtimes.com/articles/318490/20260616/peer-peer-library-iroh-10-ships-dial-devices-key-not-ip-address.htm
- https://blog.lambdaclass.com/the-wisdom-of-iroh/
- https://github.com/oconnor663/bao — Bao verified streaming
- https://github.com/BLAKE3-team/BLAKE3-specs — BLAKE3 verified streaming section

### Internal plans

- `docs/research/librarification/13-storage/PLAN.md`
- `docs/research/librarification/14-client-sync/PLAN.md`
- `docs/research/librarification/21-wire-protocol/PLAN.md`
- `docs/research/edge-tech/04-ipfs-and-cas/PLAN.md`

---

## 13. Appendix A — Glossary

| Term | Meaning |
|---|---|
| **Endpoint / EndpointId** | iroh node; Ed25519 public key identity |
| **ALPN** | TLS protocol negotiation string selecting blobs/gossip/docs/custom |
| **Home relay** | Closest relay kept as WebSocket; assists NAT + fallback |
| **Pkarr** | Signed DNS-shaped records for key→relay publication |
| **Ticket** | Compact postcard token: dial info ± content capability |
| **Blob / Hash / HashSeq** | Content-addressed bytes; BLAKE3 root; list of roots |
| **Outboard** | External Bao tree for verified streaming |
| **Provider / Requester** | Serve vs fetch roles in blobs protocol |
| **Namespace / Author** | docs write gate / entry signer |
| **TopicId** | 32-byte gossip channel id |
| **noq** | n0 QUIC stack (quinn multipath fork) |
| **n0 / number0** | Company maintaining iroh |

## 14. Appendix B — Minimal mental model for architects

```
iroh core     = WireGuard-shaped "dial this key" (but QUIC app streams, not just IP tunnel)
iroh-blobs    = HTTP Range + BitTorrent-ish multi-source + Bao verify, addressed by BLAKE3
iroh-gossip   = lightweight P2P pubsub (not NATS)
iroh-docs     = local-first multi-author KV (not a package registry)
nudox needs   = server catalog + immutable CAS pull + local takeover
conclusion    = use iroh-blobs as optional CAS transport; keep HTTP+S3 as default
```

## 15. Appendix C — Spike plan (engineering, ~1–2 weeks)

**Goal:** Prove hash-compatible fetch of one real package generation via iroh vs S3.

1. **Day 1–2:** Confirm BLAKE3 equality; write conversion tests for `ContentHash` ↔ `Hash`.
2. **Day 3–4:** Provider binary: load generation from DiskCas/S3, serve `BlobsProtocol`, print EndpointId.
3. **Day 5–6:** Client feature flag: `IrohBlobsTransport::fetch_many` using GetMany for missing hashes; commit via existing staging path.
4. **Day 7:** Authz prototype: HMAC/Ed25519 grant verified in provider event hook.
5. **Day 8–9:** Benchmarks vs presigned GET (same region, home NAT, office LAN two peers).
6. **Day 10:** Write-up: go / no-go for Phase 2 roadmap with numbers.

**Success criteria:**

- Byte-identical CAS objects vs S3 path.
- Correct failure when grant expires / wrong tenant.
- Documented direct-path ratio and wall-clock delta.
- No regression when feature flag off.

## 16. Appendix D — Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| blobs crate churn before 1.0 | Medium | Feature flag; pin; abstract `BlobTransport` |
| Relay cost / rate limits | High if primary path | Never primary; dedicated relays; prefer S3 |
| UDP blocked in enterprise | High for some segments | HTTP fallback always; relay-only mode |
| Dual store complexity | Medium | Single DiskCas SoT; adapter provider |
| Open provider data leak | Critical | Mandatory grants; no public open CAS for private tenants |
| Battery/idle network | Low–Med | Lazy endpoint; stop on idle |
| MSRV / edition friction | Low–Med | Align workspace before depend |
| Over-scoping into docs/gossip | Med (schedule) | Explicit anti-goals in roadmap |
| Peer assist license issues | Med | ToS + only public packages assist by default |
| Assuming 95% direct applies to *your* fleet | Med | Measure on real customers |

## 17. Appendix E — Alignment with Plans 13 / 14 / 21 (checklist)

| Plan requirement | iroh impact |
|---|---|
| 13: BLAKE3 self-authenticating blobs | **Reinforced** by Bao streaming |
| 13: cross-version dedupe via CAS | Unchanged; transport-agnostic |
| 13: single-file random access | Ranges help; still prefer per-file CAS keys |
| 14: remote serves while syncing | Control plane still INDEX HTTP; iroh only fills CAS |
| 14: local takeover after commit | Unchanged |
| 14: CRDTs overkill | **Confirmed** — reject docs |
| 14: want/have Git/Nix-shaped | GetMany/Observe map cleanly |
| 21: control plane small JSON | Keep; add optional provider fields |
| 21: blobs never proxy through INDEX/ORCH | Still true; providers are separate workers or S3 |
| 21: no gRPC v1 | iroh is not gRPC; optional side channel |
| 21: integrity = BLAKE3 key | Same |

## 18. Appendix F — When to revisit this reject/augment decision

Re-open “iroh as primary data plane” only if **multiple** of the following become true:

1. iroh-blobs reaches **1.0** with production endorsement.
2. Measured **>30%** of sync bytes already candidates for LAN/peer assist.
3. S3 egress becomes a top-3 COGS line item.
4. Product strategy includes **offline mesh / airgap enterprise** as a flagship SKU.
5. UDP/QUIC path is known-good for the majority of paying desktops (or relay economics work).

Until then: **HTTP+S3 primary, iroh optional augment.**

---

## 19. Summary for decision-makers

**iroh 1.0 is real, stable-enough networking infrastructure from a focused company (n0), with a BLAKE3 blob protocol that looks uncannily like what nudox already designed for CAS sync.** That is rare praise. It still loses to **presigned S3 + HTTP control** as the default INDEX→REGISTRY path because registries are multi-tenant, catalog-heavy, CDN-friendly services — not cozy peer networks.

**Do:** keep Plan 21; implement want/have over HTTP; design `BlobTransport` trait; spike iroh behind a flag; consider Phase-2 LAN assist.  
**Don't:** rewrite the data plane around EndpointIds; put packages in iroh-docs; depend on public relays; delay librarification for P2P.

**One-line memo:** *BLAKE3 soulmates, different product geometry — date in Phase 2, don’t marry in v1.*
