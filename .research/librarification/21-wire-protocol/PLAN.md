# Wire Protocol + Service Surfaces (Librarification)

**Research date:** 2026-07-16  
**Scope:** Unified control-plane and data-plane contracts across GUI → client → INDEX / local REGISTRY; ORCH ↔ compiler pods; INDEX ↔ S3; sealed compiler postcard protocol; error taxonomy; protocol versioning.  
**Audience:** librarification architecture / client, INDEX, ORCH, compiler-wire implementers.  
**Codebase anchors (live):**  
`workspace/server/http/router.rs`, `workspace/server/http/dto.rs`, `workspace/server/authz.rs`, `workspace/server/error.rs`, `workspace/server/compiler_client.rs`, `workspace/compiler/protocol.rs`, `workspace/registry/protocol.rs`, `workspace/registry/blob/mod.rs`, `workspace/heart/error/failure.rs`, `workspace/heart/content.rs`.  
**Prior plans (fragments only — this document is the unified contract):**  
`01-compiler-audit` (§ daemon wire gaps), `02-registry-server-audit` (§7 routes), `12-orchestration` (presign, warm/cold, no blob proxy), `13-storage` (§6 generation manifest), `14-client-sync` (§6 wire sketch).

---

## 0. Problem statement

Plans 01, 02, 12, and 14 each sketch a slice of wire surface:

| Plan | What it specified | What it left dangling |
|---|---|---|
| **01** | Compiler daemon `POST /compile` postcard; incomplete `CompileResponse` (no occurrences/archive) | Dual twin with `registry::protocol`; no fleet I/O model (S3 presign) |
| **02** | Live axum routes (search, packages, admin); authz caps | Flat paths, no `/v1`, no sync plan, no generation API |
| **12** | ORCH admits work, presigns S3, does not proxy blobs | No formal ORCH OpenAPI; no shared job DTO with INDEX |
| **13** | Generation manifest JSON projection + CAS has-batch | Not bound to INDEX versioning / Accept headers |
| **14** | Client want/have sync sketch; dual-backend erase | Not reconciled with live DTOs or compiler wire |

**Result:** implementers cannot build `client`, INDEX, ORCH, or `compiler-wire` without inventing incompatible envelopes. This document is the **single contract** for service surfaces after librarification.

### 0.1 Design goals (ranked)

1. **One vocabulary on the wire** — `heart` identity (`PackageId`, `ContentHash`, `Coordinates`, `ResolutionState`, `Failure`) is the JSON/postcard payload language; no parallel stringly-typed enums.
2. **Control plane small, data plane off-app** — JSON HTTP for resolve/search/status/sync-plan/job-submit; **blobs never stream through INDEX or ORCH** (presigned S3 / local CAS path).
3. **Sealed compiler protocol is complete** — response carries every artifact needed to emit a generation (surface, references, occurrences, archive digests, snapshot).
4. **Desktop lag is first-class** — `protocol_version` negotiation so shipped GUIs survive INDEX upgrades within a window.
5. **Internal stores stay internal** — SQLite schemas, Tantivy segments, Qdrant collections, Terminus WOQL are not public API.
6. **Error taxonomy is one tree** — HTTP `ErrorBody` maps to `heart::Failure` / `FailureKind` / phase so client, INDEX, and ORCH share retry logic.

### 0.2 Non-goals

- gRPC / tonic for v1 (revisit only if multi-language INDEX clients appear).
- Blob proxy through ORCH or INDEX for bulk transfer.
- CRDT / bidirectional collaborative sync.
- Full OpenAPI generation toolchain in-tree day one (Rust types + this doc are normative; utoipa later).
- Replacing postcard for sealed compile (binary IR stays postcard).

---

## 1. Actors and planes

```
┌─────────────┐     in-process      ┌──────────────────────────────┐
│  GUI (GPUI) │ ──────────────────► │  client library              │
└─────────────┘   events/channels   │  (erases INDEX vs REGISTRY)  │
                                    └───────┬──────────────┬───────┘
                         control HTTP/JSON  │              │ local API
                         (read/write)       ▼              ▼
                                    ┌────────────┐  ┌─────────────────┐
                                    │   INDEX    │  │ local REGISTRY  │
                                    │  (remote)  │  │ sqlite + DiskCas│
                                    └─────┬──────┘  └─────────────────┘
                         job submit       │
                         status           │
                                          ▼
                                    ┌────────────┐
                                    │    ORCH    │  (k8s + admission)
                                    └─────┬──────┘
                         warm HTTP / Jobs │  no blob proxy
                                          ▼
                                    ┌────────────┐     presigned
                                    │ compiler   │◄──► S3 cas/
                                    │ pods       │
                                    └────────────┘

Data plane (blobs):
  client ──presigned GET──► S3 cas/{blake3}
  compiler ──presigned GET input / PUT sections──► S3
  INDEX ──server-side put (ingest path)──► S3
  local REGISTRY ──filesystem cas/──► DiskCas
```

### 1.1 Actor responsibilities (normative)

| Actor | Owns | Does not own |
|---|---|---|
| **GUI** | UX, project open, progress display | HTTP paths, SQLite, CAS keys |
| **client** | Dual-backend routing, sync engine, local REGISTRY, remote INDEX calls, embedded trusted compile | k8s, S3 credentials (only short TTL URLs) |
| **INDEX** | Authoritative package catalog + generations for public/untrusted deps; search/graph serving; generation status | Compile execution, long-lived job queue (ORCH), bulk blob bytes |
| **local REGISTRY** | Project-scoped store: synced deps + trusted local compile outputs | Remote truth for untrusted packages |
| **ORCH** | Admit compile jobs, warm/cold path selection, presign I/O, poison/retry policy surface to INDEX | Blob storage, search, Tantivy |
| **compiler pod** | Sealed `generate_with` given inputs; upload outputs to CAS | INDEX schema, tenant auth of end users |
| **S3** | Durable CAS objects | Catalog semantics |

### 1.2 Trust planes on the wire

| Plane | Who compiles | How artifacts arrive |
|---|---|---|
| **Trusted project** | Embedded compiler in-process (no ORCH) | Local DiskCas write; optional later publish-to-INDEX (out of scope v1) |
| **Untrusted deps** | Remote fleet via ORCH | INDEX catalog + presigned CAS pull into REGISTRY |

The client **must not** expose a public “compile untrusted source locally with network” API. Trust is a type-level / capability concern (plan 14); wire surfaces below reflect that split: `POST /v1/jobs` is INDEX/ORCH for untrusted; local compile is an in-process trait, not an HTTP route on INDEX.

---

## 2. Current live inventory (what we replace or wrap)

### 2.1 INDEX / server HTTP today (`workspace/server/http/router.rs`)

| Method | Path | Handler | Notes |
|---|---|---|---|
| POST | `/search` | search | JSON; symbol search |
| POST | `/search/semantic` | search | Semantic plane |
| POST | `/packages/search` | search | Package Tantivy |
| POST | `/expand` | search | Graph expand + session |
| GET | `/symbols/:id` | search | Symbol by id |
| GET | `/sessions/:id` | search | Session graph |
| POST | `/packages` | indexing | Ensure/index package |
| GET | `/packages/:id` | indexing | Lifecycle state |
| POST | `/packages/:id/sync` | indexing | Force re-check (server-side) |
| GET | `/healthz` | health | Liveness |
| GET | `/readyz` | health | Readiness + degraded backends |
| GET | `/metrics` | health | Prometheus |
| POST | `/admin/packages/:id/verify` | admin | AdminCap |
| POST | `/admin/packages/:id/rebuild` | admin | AdminCap |

**Body limits:** read plane 2 MiB; write/admin 64 KiB ceiling (`router.rs:31–36`).  
**Authz:** allow-all Principal → ReadCap / WriteCap / AdminCap (`authz.rs`); deny arm wired but unused.  
**No** `/v1` prefix, **no** protocol negotiation header, **no** sync plan, **no** generation manifest route, **no** CAS has-batch.

### 2.2 Compiler wire twins (postcard)

Identical twins (byte-layout contract comments):

- `workspace/compiler/protocol.rs`
- `workspace/registry/protocol.rs`

```
CompileRequest  { coordinates, toolchain, files: Vec<FileBytes> }
CompileResponse::Ok { surface, references, identifiers }
CompileResponse::Err { kind: String, message: String }
```

**Gaps (01):** no `OccurrenceSet`, no `SourceArchive` digests, no `BlobInfo`/`snapshot`, no `protocol_version`, error `kind` is free string not `FailureKind`.  
**Transport:** `POST /compile`, `Content-Type: application/x-postcard` (`compiler_client.rs:11–12`).  
**Daemon body ceiling:** 256 MiB (01 audit).

### 2.3 BlobManifest (storage identity, not yet sync API)

`workspace/registry/blob/mod.rs:46–66`:

```
BlobManifest {
  package: PackageId,
  files: NonEmpty<FileEntry>,  // path + ContentHash
  ir_ref: ContentHash,
  references_ref: ContentHash,
  toolchain: Toolchain,
}
```

Two hashes (must remain distinct on any wire projection — plan 13):

| Hash | Role | Encoding |
|---|---|---|
| **generation_hash** (Hash ①) | Freshness / same snapshot? | bespoke `identity_bytes` |
| **manifest_hash** (Hash ②) | CAS key of postcard bytes | `blake3(postcard(manifest))` |

### 2.4 Failure vocabulary (heart)

`workspace/heart/error/failure.rs`:

- `Phase`: Acquiring | Extracting | Compiling | Emitting  
- `Failure { attempts, phase, message, cause?, at }`  
- `FailureKind`: Transient | SourceUnavailable | Malformed | Timeout | Unsafe | Internal  
- `ResolutionState`: Unindexed | Progressing | Stored | Failed | DeadLettered  

`ServerError` (`server/error.rs`) projects to HTTP status but is **not** a stable public JSON schema today (handlers return status + opaque body in places). Librarification needs a stable `ErrorBody`.

---

## 3. Protocol layering model

### 3.1 Three envelopes

| Envelope | Media type | Used for |
|---|---|---|
| **Control JSON** | `application/vnd.nudox.v{N}+json` (preferred) or `application/json` with `X-Nudox-Protocol: N` | INDEX, ORCH, client control plane |
| **Compiler postcard** | `application/x-postcard` + `X-Nudox-Compiler-Protocol: N` | ORCH/INDEX → compiler pod; never GUI |
| **CAS raw bytes** | `application/octet-stream` (or store native) | Presigned S3 GET/PUT; integrity = BLAKE3 key |

### 3.2 Version integers

| Namespace | Constant | Current design value | Bump when |
|---|---|---|---|
| Control plane | `PROTOCOL_VERSION` | **1** | Breaking JSON field/semantics change |
| Compiler sealed | `COMPILER_PROTOCOL_VERSION` | **2** (v1 = live incomplete Ok; v2 = full artifacts) | Postcard layout break of CompileRequest/Response |
| Generation manifest | `nudox.generation/1` format string | **1** | Manifest schema break (orthogonal to control plane if careful) |
| Producer/CAS stages | existing `PRODUCER_VERSION`, `occ-v1`, etc. | as today | Pipeline identity (not HTTP) |

**Rule:** control-plane `protocol_version` and compiler `COMPILER_PROTOCOL_VERSION` are **independent**. A client on control v1 can still rely on INDEX which talks compiler v2 internally.

### 3.3 Negotiation (`/v1/hello` + headers)

Every INDEX (and ORCH) response should be interpretable under a negotiated version:

**Request headers (client → INDEX):**

```
Accept: application/vnd.nudox.v1+json
X-Nudox-Protocol: 1
X-Nudox-Client: desktop/0.x.y
Authorization: Bearer <token>   # when auth enabled
```

**`GET /v1/hello` response:**

```json
{
  "service": "index",
  "protocol_min": 1,
  "protocol_max": 1,
  "features": ["search", "sync.plan", "jobs.submit", "cas.presign", "sync.iroh"],
  "generation_format": "nudox.generation/1",
  "hash_algo": "blake3",
  "time": "2026-07-16T12:00:00Z"
}
```

`sync.iroh` is **optional** (Phase 2). v1 INDEX may omit it; clients that do not understand it ignore unknown feature strings.

**Client algorithm:**

1. Call `/v1/hello` (or parse headers from first response).
2. If client’s supported range ∩ `[protocol_min, protocol_max]` is empty → hard fail with GUI “update required”.
3. Else use `min(client_max, server_max)` for subsequent calls; send that as `X-Nudox-Protocol`.
4. Unknown JSON fields: **ignore** (serde `deny_unknown_fields` **off** on client decode of server → client).
5. Server **may** reject unknown fields on write requests if they change semantics; prefer ignore + log for soft fields.

**Compatibility window policy:**

- Support at least **N−1** control protocol on INDEX while desktop ships N.
- Deprecate fields for ≥ **6 months or 2 desktop releases**, whichever longer.
- Hash ① generation encoding is **more sacred** than control JSON: never silent-break.

---

## 4. Control-plane API sketch (INDEX HTTP/JSON)

Base path: **`/v1`**. Live flat routes migrate under `/v1` with temporary aliases for one release (see §12 migration).

### 4.1 Route table (normative v1)

#### Discovery & ops

| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/v1/hello` | none / optional | Protocol negotiation |
| GET | `/healthz` | none | Liveness (k8s) — keep unversioned |
| GET | `/readyz` | none | Readiness + degraded |
| GET | `/metrics` | scrape auth | Prometheus |

#### Catalog / resolve

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/resolve` | ReadCap | Coordinates → package ids + generations + sync pointers |
| GET | `/v1/packages/{package_id}` | ReadCap | Lifecycle + generation pointer |
| GET | `/v1/packages/{package_id}/generations/latest` | ReadCap | Latest Stored generation manifest view |
| GET | `/v1/packages/{package_id}/generations/{generation_hash}` | ReadCap | Specific generation |
| POST | `/v1/packages` | WriteCap | Ensure package exists / enqueue indexing (maps live `POST /packages`) |
| POST | `/v1/packages/{package_id}/reindex` | WriteCap | Force re-check (maps live `.../sync`) |

#### Search & graph (read plane)

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/search` | ReadCap | Symbol search (literal) |
| POST | `/v1/search/semantic` | ReadCap + SemanticGate | Semantic search |
| POST | `/v1/packages/search` | ReadCap | Package search |
| POST | `/v1/expand` | ReadCap | Graph expand |
| GET | `/v1/symbols/{symbol_id}` | ReadCap | Symbol document |
| GET | `/v1/sessions/{session_id}` | ReadCap | Session |
| PUT | `/v1/sessions/{session_id}` | WriteCap | Merge session (if multi-replica sessions retained) |

#### Sync control plane (client ↔ INDEX)

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/sync/plan` | ReadCap | Want/have → missing hashes + presigned URLs (+ optional Phase-2 `providers.iroh`) |
| POST | `/v1/cas/has` | ReadCap | Batch existence probe (no URLs) |
| POST | `/v1/cas/presign` | ReadCap | Explicit presign for known hashes (scoped) |

#### Jobs (untrusted compile admission via INDEX → ORCH)

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/jobs` | WriteCap | Submit compile / ensure generation |
| GET | `/v1/jobs/{job_id}` | ReadCap | Job status |
| POST | `/v1/jobs/{job_id}/cancel` | WriteCap | Best-effort cancel |

#### Admin

| Method | Path | Auth | Purpose |
|---|---|---|---|
| POST | `/v1/admin/packages/{package_id}/verify` | AdminCap | Integrity verify |
| POST | `/v1/admin/packages/{package_id}/rebuild` | AdminCap | Force rebuild |
| POST | `/v1/admin/packages/{package_id}/poison` | AdminCap | Mark poisoned / denylist |
| POST | `/v1/admin/packages/{package_id}/unpoison` | AdminCap | Clear poison |

### 4.2 Body limits (revised)

| Plane | Ceiling | Rationale |
|---|---|---|
| Read control | 2 MiB | Search snippets, expand payloads |
| Write control | 256 KiB | Larger than today’s 64 KiB to allow resolve batch of many coordinates; still no source tarballs |
| Sync plan | 1 MiB | have[] can be large; cap hash count server-side (e.g. 50k) |
| Compiler postcard | 256 MiB | Existing daemon ceiling; fleet prefers S3 input refs (see §6) |

**Never** accept source archives on INDEX public JSON APIs in v1 — ingest stays server-side crawler/ORCH.

### 4.3 DTO sketches (Rust — normative for implementers)

Shared crate sketch: `nudox-wire` (or `heart` feature `wire`). `HashHex` = lowercase 64-char BLAKE3 hex. Serde: snake_case tags; clients **ignore** unknown fields on decode.

```rust
//! nudox_wire::v1 — control-plane types (sketch; enough to implement)

use chrono::{DateTime, Utc};
use heart::Toolchain;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type HashHex = String; // 64 hex chars; InvalidHash if not

#[derive(Serialize, Deserialize)]
pub struct HelloResponse {
    pub service: ServiceName, // index | orch
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub features: Vec<String>,
    pub generation_format: String, // "nudox.generation/1"
    pub hash_algo: String,         // "blake3"
    pub time: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
pub struct CoordinatesDto {
    pub ecosystem: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ResolveRequest {
    pub packages: Vec<CoordinatesDto>,
    #[serde(default)]
    pub toolchain: Option<Toolchain>,
    #[serde(default)]
    pub ensure: bool, // enqueue if missing
}

#[derive(Serialize, Deserialize)]
pub struct ResolveResponse {
    pub members: Vec<ResolvedPackage>,
    #[serde(default)]
    pub dep_set_id: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
pub struct ResolvedPackage {
    pub package_id: Uuid,
    pub coordinates: CoordinatesDto,
    pub state: ResolutionStateDto,
    #[serde(default)]
    pub generation_hash: Option<HashHex>,
    #[serde(default)]
    pub manifest_hash: Option<HashHex>,
}

/// Wire mirror of heart::ResolutionState + Poisoned extension (plan 12).
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResolutionStateDto {
    Unindexed { needed: bool },
    Progressing { phase: PhaseDto },
    Stored { generation_hash: HashHex },
    Failed { failure: FailureDto },
    DeadLettered { failure: FailureDto },
    Poisoned { failure: FailureDto },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseDto { Acquiring, Extracting, Compiling, Emitting }

#[derive(Serialize, Deserialize)]
pub struct FailureDto {
    pub attempts: u32,
    pub phase: PhaseDto,
    pub message: String,
    #[serde(default)]
    pub kind: Option<FailureKindDto>, // Transient|SourceUnavailable|Malformed|Timeout|Unsafe|Internal
    pub at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
pub struct GenerationRef { pub package_id: Uuid, pub generation_hash: HashHex }

#[derive(Serialize, Deserialize)]
pub struct SyncPlanRequest {
    pub want_from: Vec<GenerationRef>,
    #[serde(default)]
    pub have: Vec<HashHex>,
    #[serde(default = "default_plan_limit")]
    pub limit: u32, // default 512
    #[serde(default)]
    pub cursor: Option<String>,
    /// Phase 2: client's iroh EndpointId so INDEX can bind a download grant.
    /// Omit for HTTP-only clients. Encoding: lowercase hex of 32-byte key, or
    /// documented base32 form — pick one in implementation and stick to it.
    #[serde(default)]
    pub client_endpoint_id: Option<String>,
}
fn default_plan_limit() -> u32 { 512 }

#[derive(Serialize, Deserialize)]
pub struct SyncPlanResponse {
    pub want: Vec<HashHex>,
    /// v1 primary: presigned HTTPS GETs (kept for compatibility).
    pub urls: Vec<PresignedBlob>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub manifests: Vec<GenerationManifestDto>, // see §5
    /// Phase 2: multi-path provider set. v1 clients ignore unknown fields.
    /// When present, `providers.http` should mirror `urls` (migration alias).
    #[serde(default)]
    pub providers: Option<SyncProviders>,
}

/// Optional multi-transport provider list on sync/plan (Phase 2).
/// Control plane remains HTTP; this only describes **where** to pull CAS bytes.
/// Full semantics: edge-tech/02-iroh §11; client trait: 14-client-sync §6.6.
#[derive(Serialize, Deserialize)]
pub struct SyncProviders {
    /// Same objects as top-level `urls` during dual-write migration.
    #[serde(default)]
    pub http: Vec<PresignedBlob>,
    /// Omit or null when iroh is disabled, no healthy providers, or client
    /// did not send `client_endpoint_id`.
    #[serde(default)]
    pub iroh: Option<IrohProviderSet>,
}

#[derive(Serialize, Deserialize)]
pub struct IrohProviderSet {
    pub endpoints: Vec<IrohEndpointHint>,
    /// Optional BlobTickets for sneakernet/LAN bootstrap — not primary ACL.
    #[serde(default)]
    pub tickets: Vec<String>,
    /// INDEX-signed capability: hash allowlist + client endpoint + expiry.
    pub grant: Option<IrohDownloadGrant>,
}

#[derive(Serialize, Deserialize)]
pub struct IrohEndpointHint {
    pub endpoint_id: String,
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// Lower sorts first (in-region preferred).
    #[serde(default)]
    pub priority: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct IrohDownloadGrant {
    pub v: u32,
    pub client_endpoint_id: String,
    pub hashes: Vec<HashHex>,
    pub exp: DateTime<Utc>,
    /// Detached signature over canonical grant bytes (INDEX grant key).
    pub sig: String,
}

#[derive(Serialize, Deserialize)]
pub struct PresignedBlob {
    pub hash: HashHex,
    pub url: String,
    pub method: String, // "GET" for desktop; "PUT" only on compiler grants
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub size_hint: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct CasHasRequest { pub hashes: Vec<HashHex> }
#[derive(Serialize, Deserialize)]
pub struct CasHasResponse { pub missing: Vec<HashHex> }

#[derive(Serialize, Deserialize)]
pub struct JobSubmitRequest {
    pub coordinates: CoordinatesDto,
    #[serde(default)]
    pub toolchain: Option<Toolchain>,
    #[serde(default)]
    pub priority: JobPriority, // interactive | bulk
    #[serde(default)]
    pub input_hash: Option<HashHex>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPriority { Interactive, #[serde(other)] Bulk }

#[derive(Serialize, Deserialize)]
pub struct JobSubmitResponse {
    pub job_id: Uuid,
    pub package_id: Uuid,
    pub status: JobStatusDto,
    #[serde(default)]
    pub generation_hash: Option<HashHex>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobStatusDto {
    Queued { position_hint: Option<u32> },
    Running { phase: PhaseDto },
    Succeeded { generation_hash: HashHex },
    Failed { failure: FailureDto },
    Poisoned { failure: FailureDto },
    Canceled,
}

/// Evolved from live SearchRequestDto + optional generation scope.
#[derive(Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub ecosystems: Vec<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub session: Option<Uuid>,
    #[serde(default)]
    pub scope: Option<SearchScope>,
}

#[derive(Serialize, Deserialize)]
pub struct SearchScope {
    #[serde(default)]
    pub generations: Vec<GenerationRef>,
    #[serde(default = "default_true")]
    pub allow_remote: bool,
}
fn default_true() -> bool { true }
// Search hit bodies remain domain-shaped (Scored symbols); pin via protocol_version only.
```

### 4.4 Ensure / package add (compatibility with live)

Live `POST /packages` + `AddPackageDto` becomes:

```
POST /v1/packages
{ "ecosystem", "name", "version", "origin"? }
→ { package_id, state, enqueued }
```

Semantics unchanged: idempotent ensure; enqueue if not Stored/fresh.  
`POST /v1/packages/{id}/reindex` replaces `POST /packages/:id/sync` (name clash with **client sync** — rename is intentional).

### 4.5 Auth hooks

| Layer | Mechanism | Notes |
|---|---|---|
| Principal extraction | Bearer JWT / trusted-proxy header → `TenantId` | Live: anonymous only |
| Capability mint | `authorize_read/write/admin` | Keep ZST caps; policy becomes real |
| Semantic search | `SemanticGate` quota token | Unchanged concept |
| Presigned URLs | SigV4 (or S3-compatible) short TTL | **No** long-lived S3 keys on desktop |
| Admin | separate AdminCap + optional mTLS in cluster | ORCH↔INDEX service auth distinct from end-user |

**Service-to-service (ORCH ↔ INDEX, compiler callback):**

```
Authorization: Bearer <service-token>
X-Nudox-Service: orch|compiler
```

Not end-user Principal. INDEX writer API for job terminal states uses service auth + idempotency key = `input_hash` / `job_id`.

---

## 5. Data plane: content-addressed blobs

### 5.1 Invariants

1. **CAS key = `blake3(raw_logical_bytes)`** of the user-visible content (source file, postcard IR, postcard occurrences, …). Compression envelopes, if any, are storage encoding — plan 13: hash is on logical bytes **or** store uncompressed and compress only on the wire; pick one and document. **v1 recommendation:** store **raw** CAS objects as today; optional zstd transfer later without changing keys.
2. **Client verifies** every downloaded object: `ContentHash::of_bytes(bytes) == expected`.
3. **No INDEX proxy** for bulk GET in the steady state. INDEX may offer a small debug `GET /v1/cas/{hash}` behind admin for ops only — not the desktop hot path.
4. **Presign TTL:** default 15 minutes; sync engine re-plans on 403/expired.
5. **PUT presigns** are issued only to compiler pods (ORCH), never to desktop clients for INDEX CAS writes in v1.

### 5.2 Generation manifest (sync-facing) — link 13

JSON view (control plane), distinct from postcard `BlobManifest` stored in CAS:

```jsonc
{
  "format": "nudox.generation/1",
  "package_id": "…uuid…",
  "coordinates": {
    "ecosystem": "rust",
    "origin": "crates.io",
    "name": "serde",
    "version": "1.0.210"
  },
  "generation_hash": "<hex Hash ①>",
  "manifest_hash": "<hex Hash ②>",
  "toolchain": { /* heart::Toolchain */ },
  "sections": {
    "ir": { "hash": "<hex>", "size": 12345 },
    "references": { "hash": "<hex>", "size": 4000 },
    "occurrences": { "hash": "<hex>", "size": 8000 },
    "archive_index": { "hash": "<hex>", "size": 1200 }
  },
  "files": [
    { "path": "src/lib.rs", "hash": "<hex>", "size": 1234 }
  ],
  "created_at": "2026-07-16T00:00:00Z",
  "signature": null
}
```

**Extensions vs live `BlobManifest`:**

| Field | Live BlobManifest | Sync GenerationManifestDto |
|---|---|---|
| files[] | yes | yes |
| ir_ref | yes | sections.ir |
| references_ref | yes | sections.references |
| occurrences | **missing** | **required for dual-store** (01 gap) |
| archive / file digests | files only | optional `archive_index` section = postcard `SourceArchive` |
| generation_hash / manifest_hash | computed off-struct | explicit on wire |
| grammar pins | no | optional metadata (Hash ① impact — only if re-analysis invalidates) |

**Closure for Ready generation (client commit gate):**

```
closure(G) = {
  postcard BlobManifest bytes (manifest_hash),
  sections.ir, sections.references,
  sections.occurrences (if present in format ≥1.1),
  sections.archive_index (if present),
  every files[].hash
}
```

Partial downloads must not mark Ready (plan 14 commit gate).

### 5.3 Sync plan protocol (want/have)

```
client:
  for each generation in DepSet not Ready:
    ensure manifest local (from Resolve or SyncPlan.manifests)
    have := local CAS keys
    want := closure(G) \ have
    POST /v1/sync/plan { want_from, have, limit }
    for each url in response.urls:
      GET url → verify → stage
    when closure complete → atomic commit → Ready
```

Pagination: large packages return `next_cursor`; client loops.  
**Single-flight:** per `(package_id, generation_hash)` and per `hash` (heart `SingleFlight`).

### 5.4 Optional pack endpoint (v1.x, not blocking)

```
GET /v1/packs/{generation_hash}  → seekable zstd pack of missing members
```

Only when HTTP/2 request overhead dominates. Not required for first desktop ship.

### 5.5 Local REGISTRY surface (not HTTP)

The client exposes **traits**, not axum routes, for local store:

```rust
#[async_trait]
pub trait LocalRegistry {
    async fn get_blob(&self, hash: ContentHash) -> Result<Option<Bytes>, RegistryError>;
    async fn put_blob(&self, hash: ContentHash, bytes: Bytes) -> Result<(), RegistryError>;
    async fn generation(&self, id: PackageId) -> Result<Option<GenerationRecord>, RegistryError>;
    async fn commit_generation(&self, manifest: &GenerationManifestDto, staged: &StagedClosure)
        -> Result<SyncedWitness, RegistryError>;
}
```

GUI never talks HTTP to local REGISTRY. Optional localhost debug server is **non-normative**.

---

## 6. Compiler sealed protocol (unify twins + complete artifacts)

### 6.1 Crate: `compiler-wire` (or `nudox-compiler-wire`)

**Single source of truth.** Delete drift risk between:

- `workspace/compiler/protocol.rs`
- `workspace/registry/protocol.rs`

Dependents: `compiler-daemon`, `compiler-core` (encode helpers), INDEX/ORCH client, registry emit path.

### 6.2 Transport modes

| Mode | When | Request body |
|---|---|---|
| **Inline files** (legacy/dev) | Local daemon, small packages | `CompileRequest::Inline { files }` |
| **CAS refs** (fleet) | k8s pods | `CompileRequest::Cas { input_tarball_hash, get_url, put_prefix, … }` |

Fleet **must not** ship multi-hundred-MB JSON/postcard source through ORCH. Plan 12: presign GET input + PUT sections.

### 6.3 Normative types (compiler protocol v2)

Postcard is **not** protobuf: any field layout change is breaking → full v2 bump. Keep `WireFile` / `WireReference` / `WireTarget` and ReferenceKind u8 table **byte-identical** to live v1.

```rust
//! compiler_wire — postcard, COMPILER_PROTOCOL_VERSION = 2
pub const COMPILER_PROTOCOL_VERSION: u32 = 2;
pub const POSTCARD_CONTENT_TYPE: &str = "application/x-postcard";

pub struct CompileEnvelope {
    pub protocol_version: u32, // must match server
    pub request: CompileRequest,
}

pub enum CompileRequest {
    /// Dev/local: today's shape (coordinates, toolchain, files: Vec<FileBytes>).
    Inline { coordinates: Coordinates, toolchain: Toolchain, files: Vec<FileBytes> },
    /// Fleet: input already in object store; pod never receives bulk via ORCH.
    Cas {
        coordinates: Coordinates,
        toolchain: Toolchain,
        input_hash: [u8; 32],
        input_get_url: String,
        output: OutputGrants, // SectionPut[] + optional completion_callback/token
        deadline_unix_ms: Option<u64>,
    },
}

pub enum SectionRole { Ir, References, Occurrences, ArchiveIndex, Manifest }

pub enum CompileResponse {
    Ok {
        protocol_version: u32,
        surface: Vec<u8>,              // postcard ir::entry::Index
        references: Vec<WireFile>,     // v1 layout preserved
        occurrences: Vec<u8>,          // NEW: postcard OccurrenceSet
        archive: WireSourceArchive,    // NEW: path/hash/size digests
        identifiers: Vec<String>,
        snapshot: [u8; 32],            // NEW: generation stamp input
        uploaded: Vec<UploadedSection>,// Cas mode: role+hash+size after PUT
    },
    Err {
        protocol_version: u32,
        kind: String,                  // machine category; migrate to enum
        message: String,
        failure_kind: FailureKindWire, // → heart::FailureKind
        phase: PhaseWire,              // Compiling typically
        retryable: bool,
    },
}
```

### 6.4 Error kind mapping (compiler → heart)

| Wire kind | FailureKind | Retryable |
|---|---|---|
| parse/malformed_source | Malformed | no |
| sandbox_denied | Unsafe | no |
| timeout | Timeout | yes |
| oom | Transient once, then Internal | once |
| input_fetch / upload_failed | Transient | yes |
| toolchain_missing / internal | Internal (or Transient by policy) | policy |

### 6.5 Daemon routes & out-of-band

| Method | Path | Notes |
|---|---|---|
| GET | `/health` | Liveness |
| POST | `/compile` or `/v1/compile` | postcard envelope; 400 on version mismatch |

**Out of compile response:** graph/Terminus docs, Tantivy docs, raw source bytes (digests only).  
**Unify steps:** (1) extract crate, dual re-export; (2) ship v2 Ok with occurrences/archive/snapshot; (3) BlobBuilder gains occurrences; (4) fleet `Cas` mode; Inline only for dev/small.

---

## 7. ORCH API (submit / status / poison; no blob proxy)

ORCH is a **separate service** (plan 12) with its own `/v1/hello` (`service: orch`). Clients normally hit **INDEX** `POST /v1/jobs`; INDEX forwards to ORCH. Direct ORCH access is for operators and INDEX only.

### 7.1 ORCH routes

| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/v1/hello` | none | protocol negotiation |
| GET | `/healthz` | none | liveness |
| GET | `/readyz` | none | kube + INDEX writer reachable |
| POST | `/v1/compile` | service | Admit one compile (warm try + optional Job) |
| POST | `/v1/compile/batch` | service | Bulk cold admit |
| GET | `/v1/jobs/{job_id}` | service | Status |
| POST | `/v1/jobs/{job_id}/cancel` | service | Cancel |
| POST | `/v1/jobs/{job_id}/poison` | admin service | Force poison |
| POST | `/v1/internal/complete` | compiler service | Compiler callback (optional) |
| GET | `/metrics` | scrape | Prometheus |

### 7.2 Admit request (ORCH)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchCompileRequest {
    pub coordinates: CoordinatesDto,
    pub toolchain: Toolchain,
    /// Content hash of sealed input (idempotency key).
    pub input_hash: HashHex,
    pub priority: JobPriority,
    /// If input not yet in CAS, client/INDEX must stage first.
    pub input_present: bool,
    #[serde(default)]
    pub resource_class: Option<String>, // "small" | "medium" | "large"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchCompileResponse {
    pub job_id: Uuid,
    pub path: ExecPath,
    pub status: JobStatusDto,
    /// Presigned URLs issued for this attempt (compiler only; not returned to GUI).
    #[serde(default)]
    pub grants: Option<CompilerGrantsDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecPath {
    Warm,
    ColdJob,
    DedupHit,
}
```

### 7.3 What ORCH never does

| Forbidden | Why |
|---|---|
| Stream IR/source through ORCH HTTP | Memory & bandwidth bomb; plan 12 |
| Long-term job history store | INDEX sqlite is SoT |
| Fair-share scheduling | Kueue |
| End-user search | INDEX |

### 7.4 Completion callback (optional)

```
POST /v1/internal/complete
{
  "job_id": "...",
  "input_hash": "...",
  "result": "succeeded" | "failed" | "poisoned",
  "generation_hash": "...",
  "manifest_hash": "...",
  "sections": [ { "role", "hash", "size" } ],
  "failure": { ... FailureDto ... }?
}
```

Idempotent on `job_id`. INDEX writer updates `parse_status` / generation rows. Cold path may skip callback if Job watcher reconciles from S3 + exit codes alone — pick **one** primary (recommend: watcher primary, callback optimizes warm latency).

### 7.5 Poison semantics on the wire

```
POST /v1/jobs/{id}/poison
{ "reason": "string", "failure_kind": "unsafe" }
```

Effects:

1. ORCH cancels in-flight if possible.
2. INDEX `ResolutionState` → `Poisoned` / `DeadLettered`.
3. Subsequent admits for same coordinates/input_hash rejected until `unpoison`.

---

## 8. Error taxonomy alignment with `heart::Failure`

### 8.1 Public ErrorBody (all control-plane errors)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Stable machine code for clients (not HTTP status alone).
    pub code: ErrorCode,
    /// Human message safe for UI.
    pub message: String,
    /// Retry classification.
    pub retryable: bool,
    /// Optional structured failure (package pipeline).
    #[serde(default)]
    pub failure: Option<FailureDto>,
    /// Correlation.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Protocol that produced this error.
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // 4xx
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    PayloadTooLarge,
    ProtocolUnsupported,
    InvalidHash,
    GenerationIncomplete,
    Poisoned,
    RateLimited,
    // 5xx
    Internal,
    DependencyUnavailable, // degraded backend
    Timeout,
}
```

### 8.2 HTTP status mapping

| ErrorCode | Status | FailureKind (if any) |
|---|---|---|
| BadRequest | 400 | Malformed |
| Unauthorized | 401 | — |
| Forbidden | 403 | Unsafe (policy) |
| NotFound | 404 | SourceUnavailable (sometimes) |
| Conflict | 409 | — |
| PayloadTooLarge | 413 | — |
| ProtocolUnsupported | 400 / 406 | — |
| InvalidHash | 400 | Malformed |
| GenerationIncomplete | 409 | — |
| Poisoned | 422 or 409 | Unsafe |
| RateLimited | 429 | Transient |
| Timeout | 504 | Timeout |
| DependencyUnavailable | 503 | Transient |
| Internal | 500 | Internal |

### 8.3 ServerError → ErrorBody projection

Replace ad-hoc strings in `IntoResponse for ServerError` with:

```rust
impl From<&ServerError> for ErrorBody {
    fn from(e: &ServerError) -> Self {
        // map variants → ErrorCode + retryable via heart::Retryable
        // attach FailureDto when Registry path carries Failure
    }
}
```

Compiler `CompileResponse::Err` always converts to `Failure` before INDEX persistence:

```
Failure {
  attempts,
  phase: Compiling,
  message,
  cause: Some(ErrorDetails::Message(kind)),
  at: Utc::now(),
}
```

Expand `ErrorDetails` over time (structured causes) without breaking FailureDto wire (optional `cause` JSON).

### 8.4 Client library error surface

```rust
pub enum ClientError {
    Protocol { code: ErrorCode, body: ErrorBody },
    Transport(reqwest::Error),
    Integrity { expected: ContentHash, actual: ContentHash },
    Local(RegistryError),
    Offline { needed: Vec<GenerationRef> },
}
```

`Retryable` impl: Protocol.retryable || Transport timeout || Integrity → no.

---

## 9. Compatibility for lagging desktop clients

### 9.1 Matrix

| Client | INDEX | Behavior |
|---|---|---|
| protocol 1 | max 1 | OK |
| protocol 1 | max 2 (min 1) | Client stays on 1; server accepts 1 |
| protocol 1 | min 2 | `/v1/hello` fail → force update |
| protocol 2 | max 1 | Client downgrades if it implements 1, else fail |

### 9.2 Additive evolution rules

1. **Add optional field** → no version bump if servers fill defaults and old clients ignore.
2. **Remove field** → bump `protocol_min` after deprecation window.
3. **Change field meaning** → new field name + deprecate old; then bump.
4. **New route** → advertise in `features[]`; old clients ignore.
5. **Generation format** bump (`nudox.generation/2`) can happen **without** control protocol bump if clients key off `format` string inside manifest.

### 9.3 Feature flags in hello

```
features: [
  "search",
  "search.semantic",
  "sync.plan",
  "sync.occurrences",
  "jobs.submit",
  "cas.presign",
  "cas.has",
  "sessions"
]
```

Client disables UI affordances when features missing (e.g. no semantic if gate/feature off).

### 9.4 Live route aliases

During migration:

| Live | Alias period | Canonical |
|---|---|---|
| `POST /search` | 1 release | `POST /v1/search` |
| `POST /packages` | 1 release | `POST /v1/packages` |
| `GET /packages/:id` | 1 release | `GET /v1/packages/{id}` |
| `POST /packages/:id/sync` | 1 release | `POST /v1/packages/{id}/reindex` |

Unversioned routes set `X-Nudox-Protocol: 1` implicitly and log deprecation metrics.

---

## 10. Internal vs exposed

### 10.1 Exposed (public or service contracts)

| Surface | Audience |
|---|---|
| INDEX `/v1/*` JSON | Desktop client, automation |
| ORCH `/v1/*` JSON | INDEX, operators |
| Compiler postcard `/compile` | ORCH, local dev |
| Presigned S3 URLs | Client (GET), compiler (GET/PUT) |
| ErrorBody, HelloResponse, manifests | All clients |
| `heart` identity types on wire | Shared |

### 10.2 Internal (never public HTTP)

| Subsystem | Why internal |
|---|---|
| SQLite schema / SQL | Migration freedom; dual INDEX/REGISTRY |
| Postgres (while present) | Being removed from orchestration |
| Tantivy segment files | Local only; rebuildable |
| Qdrant collection layout | Swappable vector backend |
| Terminus WOQL / document schema | Hot-tier only; cold = IR |
| sea_query DDL | Server boot only |
| Outbox / sink_watermarks tables | Materializer plumbing |
| k8s Job objects | Ephemeral; GC’d |
| NATS subjects (if added) | Optional bus |
| Embedding model HTTP to Ollama | Server-side only |
| `ptr/{uuid}` object key scheme | Storage layout (13); client uses hashes |
| Authz cap constructors | Process-internal |

### 10.3 Grey zone (service, not desktop)

| Surface | Notes |
|---|---|
| INDEX writer API for ORCH completion | Service token |
| Admin rebuild/verify/poison | AdminCap |
| Debug CAS GET | Admin only |

---

## 11. End-to-end sequences

### 11.1 Desktop opens project with remote deps

```
GUI → client.project.resolve(path)
client → local: parse manifest, trusted members
client → INDEX POST /v1/resolve { packages: deps, ensure: true }
INDEX → { members: [… Stored | Progressing …] }
client → for Stored: SyncEngine.ensure(generation)
client → POST /v1/sync/plan { want_from, have [, client_endpoint_id] }
client → BlobTransport.fetch(want):
           optional providers.iroh → IrohBlobsTransport
           residual → GET providers.http / urls (presign)
         → verify BLAKE3 → commit local
client → search: RoutedQuery (local if Ready else remote POST /v1/search)
GUI ← progress events + results
```

### 11.2 Missing generation / compile

```
client → INDEX POST /v1/jobs { coordinates, priority: interactive }
INDEX → ORCH POST /v1/compile { input_hash, … }
ORCH → warm compiler POST /compile (postcard Cas)
compiler → S3 GET input; compile; S3 PUT sections
compiler/watcher → INDEX complete
client polls GET /v1/packages/{id} or GET /v1/jobs/{id}
client → sync plan → pull → Ready
```

### 11.3 Trusted local compile (no INDEX job)

```
GUI → client.compile_trusted(SourcePath<Trusted>)
client → embedded generate_with → LocalRegistry.commit
(no ORCH, no presign)
```

### 11.4 Poison

```
admin → INDEX POST /v1/admin/packages/{id}/poison
INDEX → ORCH poison + state Poisoned
client resolve → state Poisoned → GUI error, no infinite retry
```

---

## 12–13. Migration + OpenAPI surface map

Migration (surfaces only): **M0** wire crates → **M1** hello+ErrorBody+`/v1` aliases → **M2** generations+sync/plan+presign → **M3** compiler v2 → **M4** ORCH jobs / drop PG queue → **M5** remove unversioned routes. Types land incrementally; no monorepo build required for this design.

OpenAPI schemas = Rust types in §4.3 / §6.3 / §8 (utoipa later).

INDEX operationIds: `hello`, `resolve`, `getPackage`, `getGeneration`, `ensurePackage`, `reindexPackage`, `search` (+ semantic/packages), `expand`, `syncPlan`, `casHas`, `submitJob`/`getJob`/`cancelJob`, `admin*`. Default error media: `application/vnd.nudox.v1+json` → `ErrorBody`. ORCH subset: hello, compile, jobs status/cancel/poison, internal complete.

---

## 14. Client abstraction: INDEX vs REGISTRY

**GUI never selects backend.** Shared `nudox-wire` types; local REGISTRY synthesizes the same DTOs from SQLite.

```rust
#[async_trait]
pub trait Catalog {
    async fn resolve(&self, req: ResolveRequest) -> Result<ResolveResponse, ClientError>;
    async fn status(&self, id: PackageId) -> Result<PackageStatusResponse, ClientError>;
}
#[async_trait]
pub trait BlobSource {
    async fn plan(&self, req: SyncPlanRequest) -> Result<SyncPlanResponse, ClientError>;
    async fn has(&self, hashes: &[ContentHash]) -> Result<Vec<ContentHash>, ClientError>;
}
// RoutedCatalog { local, remote, sync }:
//   resolve = merge Ready local + remote remainder
//   search  = local if SyncedWitness else remote; ensure_background always
```

---

## 15. Security & testing (wire-relevant)

**Security:** desktop GET presigns only, scoped to generation closures (no bucket-wide hash oracle); `resolve(ensure=true)` needs WriteCap/EnsureCap; compiler `input_get_url` allowlisted/ORCH-minted (SSRF); callback tokens job-scoped; admin poison audited via `request_id` + tenant.

**Tests:** golden postcard v2 + ReferenceKind pin; JSON round-trip + ignore-unknown; INDEX contract (hello/resolve/sync.plan/errors); ORCH dedup + no blob bodies; client integrity refusal + protocol_min hard-fail; CI desktop N−1 vs INDEX N.

---

## 16. Recommendations (priority)

1. **`compiler-wire` + `nudox-wire` first** — kill twin drift and GUI DTO forks.
2. **`/v1/hello` + `ErrorBody` first** on INDEX — cheapest compat win.
3. **Compiler v2: occurrences + archive + snapshot** — unblocks REGISTRY closure (01 gap).
4. **`POST /v1/sync/plan` + presign only** — never proxy blobs through INDEX/ORCH. Phase 2 may add optional `providers.iroh` (still no blob proxy; see edge-tech/02-iroh §11).
5. **Rename live `.../sync` → `reindex`** — free “sync” for client pull.
6. **ORCH thin:** submit/status/poison/complete; Kueue schedules; INDEX history.
7. **Postcard compile / JSON control** — do not unify codecs.
8. **Hash ① more sacred than HTTP fields** — breaking-change policy.
9. **All pipeline failures via `Failure`/`FailureKind`** — retire free-string compile kinds.
10. **Never expose SQLite/Tantivy/Qdrant/Terminus** on public routes.

---

## 17. Open questions / risks

1. Who stages fleet `input_hash` — crawler only, or desktop PUT for private packages?
2. `Poisoned` as new `ResolutionState` vs alias of `DeadLettered`?
3. Occurrences required in generation format 1 (preferred) or 1.1?
4. INDEX multi-writer story (single-writer API vs Litestream) for job completion.
5. Sessions multi-device HTTP vs local-only desktop?
6. Semantic search always remote without local vectors?
7. Postcard v1/v2 dual-stack duration on daemon?
8. Compression envelope vs CAS key (plan 13) before size_hint hardens.
9. Public CDN hash URLs vs always-presign for OSS INDEX?
10. Rate limits on `sync/plan` and `resolve(ensure)`?

---

## 18. Executive summary

Librarification needs one wire contract for **GUI → client → (INDEX | REGISTRY)** plus **ORCH → compiler → S3**. Live code has unversioned JSON routes (`server/http/router.rs`), twin incomplete postcard compile protocols (`compiler/protocol.rs` ≈ `registry/protocol.rs`), and no want/have sync API.

**Normative planes:** (1) **Control** HTTP/JSON `/v1` with hello negotiation; (2) **Data** BLAKE3 CAS via presigned URLs (v1) with optional Phase-2 `providers.iroh` / `BlobTransport` (edge-tech/02-iroh) — still never proxy blobs through INDEX/ORCH; (3) **Compiler** single postcard v2 with full artifacts; (4) **ORCH** thin admit/status/poison. Errors use `ErrorBody` aligned with `heart::Failure`. Internals (SQLite, Tantivy, Qdrant, Terminus, k8s Jobs) stay off the public wire. Implement: wire crates → hello/errors → sync/plan → compiler v2 → ORCH → drop aliases. Plans 01/02/12/13/14 are background, not competing APIs.

---

## 19. Live → future map & checklist

| Live | Future |
|---|---|
| `server/http/router.rs` + `dto.rs` | INDEX `/v1` + `nudox-wire` |
| `server/error.rs` / `authz.rs` | ErrorBody projection; real Principal + service auth |
| `server/compiler_client.rs` | ORCH-internal; desktop uses jobs API |
| `compiler/protocol.rs` + `registry/protocol.rs` | single `compiler-wire` |
| `registry/blob/mod.rs` | + occurrences; GenerationManifestDto |
| `heart/error/failure.rs` | FailureDto source of truth |
| `gui/src/backend.rs` | `client` library |

**Implementer checklist:** shared control + compiler crates; `/v1/hello`; `ErrorBody` everywhere; closure-scoped presigns; client BLAKE3 verify; compile Ok full artifacts; ORCH no blob proxy; Poisoned path; no store credentials on desktop; deprecation windows in release notes.

**Glossary:** INDEX (remote catalog), REGISTRY (local store), ORCH (admit), generation (Hash ①), manifest (section list), closure (Ready hash set), want/have (sync diff), control/data/compiler planes.

---

*End of 21-wire-protocol design. No master plan; no build.*
