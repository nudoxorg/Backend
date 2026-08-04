# 03 — Existing System: Ground Truth for IR Storage → IR-Native VCS

> **Status (Rev 3.2):** Ground-truth research context — feeds `design/IR-NATIVE-VCS-DESIGN.md`. Its Hash ①/② split and outbox bug (§1.5, C1) are adopted as K12; Terminus-hot posture as K17. Note the design is **greenfield** for the IR plane: the live postcard `Index` documented here is context, not a migration source (K1/K28).

**Date:** 2026-07-16  
**Workspace:** `/Users/philocalyst/Projects/Backend`  
**Purpose:** Ruthless ground-truth brief for replacing whole-blob IR (`ir_ref` CAS section) with a Pijul-inspired IR-native VCS. Live code is authoritative; research docs are cited only where they correctly describe the tree or correctly name planned gaps.

---

## 0. One-sentence reality

**Today, IR is a single opaque postcard-serialized `ir::entry::Index` blob, content-addressed under `BlobManifest.ir_ref`, never versioned at symbol grain, never used as the production graph path (Terminus is), and never synchronized via want/have (no client sync API exists).**

Everything below elaborates that fact with file:line citations.

---

## 1. Exact current IR storage path

### 1.1 Pipeline (live)

```
source archive (tar.gz)
  → ingest → BlobBuilder (files only)
  → server compile phase → compiler daemon POST /compile (postcard)
  → CompileResponse::Ok.surface = postcard(Index)
  → BlobBuilder::set_ir(surface_bytes)
  → BlobBuilder::set_references(ReferenceSet from CST wire)
  → finalize → BlobManifest + PendingSection[]
  → emit: put_section* + put_manifest + outbox fan-out
  → global_store record_stored(identity_bytes generation stamp)
```

Evidence:

- Extract: `workspace/server/coordination/indexing.rs:100–127` (`ingest_archive` → `BlobBuilder`).
- Compile + attach IR: `indexing.rs:129–179` — `surface` bytes from daemon become `ir_bytes`; `builder.set_ir(ir_bytes)`.
- Daemon serializes Index as postcard: `workspace/compiler/bin/compiler_daemon.rs:210–214`  
  comment: *“JSON cannot represent `HashMap<NudoxPath, Entry>` map keys … postcard is the project wire format”*.
- Emit: `workspace/registry/blob/emit.rs:41–72`.
- Stored snapshot (Hash ①): `indexing.rs:192–210` — `ContentHash::of_bytes(&manifest.identity_bytes())`.

### 1.2 `BlobManifest` fields (live, complete)

```46:66:workspace/registry/blob/mod.rs
pub struct BlobManifest {
    pub package: PackageId,
    pub files: nonempty::NonEmpty<FileEntry>,  // path + BLAKE3 + size
    pub ir_ref: ContentHash,                   // serialized Index
    pub references_ref: ContentHash,           // ResolvedReference set
    pub toolchain: Toolchain,
}
```

**There is no `occurrences_ref`.** Research 04 and master plan GD-10 plan to add it; live code does not have it. OccurrenceSet is produced in `generate_with` (`workspace/compiler/generate/mod.rs:115–119`) but **dropped on the daemon wire** — `CompileResponse::Ok` only carries `surface`, `references` (CST-era), `identifiers` (`workspace/compiler/protocol.rs:73–85`).

### 1.3 How IR is serialized today

| Layer | Codec | Evidence |
|---|---|---|
| **Durable IR section in CAS** | **postcard** of `ir::entry::Index` | Daemon: `postcard::to_allocvec(&generated.surface)` (`compiler_daemon.rs:213–214`); server stores those bytes as-is via `set_ir` (`indexing.rs:175`); `BlobManifest.ir_ref` is BLAKE3 of those bytes (`creation.rs:90–95`) |
| **Producer in-process CAS** | postcard of `ProducerOutput` / surface stages | `workspace/compiler/compile/producer/runtime.rs:136–151, 168–185` |
| **Oracle / some producers** | JSON decode path still exists | `producer/mod.rs` (`decode_index_json` grep hit at `:536`) |
| **Manifest envelope** | postcard of `BlobManifest` | `manifest_cas_key` / `put_manifest` (`blob/mod.rs:159–161`, `store.rs:176–177`) |
| **Reference section** | bespoke wire mirror → postcard | `ReferenceSet::encode` (`blob/mod.rs:222–238`) — *not* derived serde on `ResolvedReference` |
| **Compile protocol** | postcard request/response | `server/compiler_client.rs:11–12, 63–79` |
| **IR type derives** | `serde` under feature; **no active facet attributes** | `workspace/ir/Cargo.toml:14–17`; types use `#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]` |
| **JSON** | **not** used for Index on the blob path | Explicitly rejected for map keys (`compiler_daemon.rs:210–212`) |

**Compression:** none on CAS sections today. Research 13 plans zstd + trained dicts for IR (`13-storage` §4.4); live `Store::put_section` stores raw bytes verified as `blake3(bytes)==key` (`store.rs:122–146`). Local `DiskCas` wraps `blake3(value) ‖ value` (`heart/cache/disk.rs:156–172`) — still uncompressed payload.

### 1.4 CAS layout (registry object store)

From `workspace/registry/store.rs:4–20, 97–116`:

| Key | Contents |
|---|---|
| `cas/{blake3-hex}` | immutable sections: source files, IR postcard, ReferenceSet postcard, serialized manifest |
| `ptr/{package-uuid}` | 32 raw bytes = **manifest CAS key** (Hash ②), not generation stamp |

`put_manifest` (`store.rs:166–189`):

1. `manifest.validate()`
2. `hash = manifest.manifest_cas_key()` = `blake3(postcard(manifest))`
3. put manifest bytes at `cas/{hash}`
4. put `hash.as_bytes()` at `ptr/{package-id}`

### 1.5 Two hashes (MUST NOT unify)

Documented at length in `blob/mod.rs:68–106`, pinned by golden tests `workspace/registry/tests/blob_hash_pins.rs`:

| | Hash ① generation stamp | Hash ② manifest CAS key |
|---|---|---|
| Input | `identity_bytes()`: sorted (path, file-hash) + `ir_ref` + `references_ref` + postcard(toolchain), length-prefixed (`blob/mod.rs:117–138`) | full `postcard::to_allocvec(manifest)` |
| Role | “same logical snapshot?” / `ResolutionState::Stored { hash }` / audits | address in `cas/` + `ptr/` leaf |
| Golden (fixture) | `940fa8fa…dc3e` | `41ff362a…2abe` |

**Live bug / contradiction (emit vs store):**  
`blob/emit.rs:60–65` feeds **Hash ②** into `outbox.append(package, generation, …)` because `put_manifest` returns the CAS key.  
`indexing.rs:193–210` records **Hash ①** via `record_stored(..., snapshot, ...)`.  
Audits recompute Hash ① (`server/save/blobs.rs:135–139`).  
→ **Outbox generation keys and Stored snapshot hashes are not the same value domain.** Any IR-VCS work must pick one SoT and stop conflating them (research docs always say Hash ① is generation; emit currently violates that for outbox).

### 1.6 What is *not* stored

| Artifact | Status |
|---|---|
| Live tree-sitter trees | Never (by design; `blob/mod.rs:22–25`, `generate/cst.rs`) |
| OccurrenceSet | Produced but **not** in BlobManifest / compile Ok |
| GraphCorpus / cold graph postcard | Not stored; GD-8 says re-project on miss |
| Symbol-level IR patches / history | None |
| zstd envelopes on remote CAS | None |

---

## 2. Exact types: Index, Entry, NudoxPath, ContentHash

### 2.1 `ContentHash` — `workspace/heart/content.rs:9–26`

```rust
pub struct ContentHash([u8; 32]); // BLAKE3-256
// of_bytes, from_bytes, as_bytes, hex, builder()
```

Also hosts `JobKey` (`content.rs:49–93`):  
`JobKey = H(len‖producer ‖ len‖toolchain ‖ len‖source ‖ len‖dep_lock)`  
`with_tag(tag) = H(jobkey ‖ len‖tag)` for cst / occ / archive child keys.

`Freshness::compare(recorded, recomputed)` (`content.rs:105–120`).

### 2.2 `NudoxPath` — `workspace/ir/entry.rs:15–24`

```rust
pub enum NudoxPath {
    External { path: PathBuf, dependency: String },
    Local(PathBuf),
}
```

- **Only inter-entry pointer** inside IR.
- Often a **single PathBuf component** embedding `::` FQNs (research 04 §1.2; producers).
- **No package version** inside the path.
- Graph linker re-splits on `::` / `.` (`compiler/graph/symtab.rs`).

### 2.3 `Index` — `workspace/ir/entry.rs:26–35`

```rust
pub struct Index {
    pub root_ids:        Vec<NudoxPath>,
    pub entries_by_path: HashMap<NudoxPath, Entry>, // rustc_hash::FxHashMap
}
```

- Flat table, **not** nested tree. Nesting reconstructed via `members` on payloads.
- Comment about “stable integer IDs” (`entry.rs:28–29`) is **stale** — keys are `NudoxPath`.
- Roots filled by pipeline heuristic: Local paths with one component and no `"::"` (`ir/pipeline/pipeline.rs`, research 04).

### 2.4 `Entry` / `Symbol<T>` — `workspace/ir/kind.rs`

**Shell** (`kind.rs:94–116`): `name`, `path: NudoxPath`, `aliases`, `visibility`, `documentation`, `deprecation?`, `doc_links?`, `inner: T`.

**Entry** (`kind.rs:150–204`), serde `tag = "kind", content = "value"`:

| Variant | Payload |
|---|---|
| Module | `Module { members }` |
| RecordType | `Record` |
| Info | `String` |
| UnionType | `Vec<Type>` |
| TraitDef / TraitImpl | protocols |
| SumType | `SumType` |
| Function | `Function` |
| TypeAlias | `TypeAliasBody` |
| Constant / Variable / Field / Event | `TypedBinding` |
| Macro / PrimitiveType | `()` |

**No `SymbolId` inside IR.** Identity is package-local `NudoxPath` only. UUID `SymbolId` is heart-layer (`heart/identity/symbol.rs:15–47`): UUIDv5 of `instance_token ‖ EntryUri` (PackageId + path segments).

### 2.5 Sibling syntax corpus (not inside Entry)

- `Occurrence` / `OccurrenceSet` — `ir/syntax/occurrence.rs` (beside surface).
- `ResolvedReference` — legacy flat, still what lands in `references_ref`.
- `FunctionBody` yoke tree+source still exists (`syntax/body.rs`) but graph body-path refs retired.

### 2.6 Identity stack (three systems — do not collapse)

| Layer | Key | Version-stable? |
|---|---|---|
| IR | `NudoxPath` | No version in path |
| Graph IRI | `Symbol/{lang}%2F{pkg}%2F{fq}` | **Yes** (version via PackageVersion.declares) |
| heart SymbolId | UUID instance-salted | **No** across PackageId versions |

Master plan 0.3 identity stack + moniker layer is **planned**, not live IR.

---

## 3. Cold-path graph-over-IR: live vs planned

### 3.1 Live (production)

**There is no cold GraphStore implementation.**

- Trait: `GraphStore` at `workspace/registry/runtime/graph/mod.rs:72–97` — `get_occurrences`, `get_references`, `are_related`.
- **Only impl:** `Graph<Live>` Terminus WOQL over `Relation` documents (`mod.rs:591–639`).
- Writes: `insert_symbols` (`mod.rs:442–494`) inserts **thin** heart::Symbol docs — not full GraphCorpus edges. Relation model assumed by reads is **not** populated by full linked_data emit on this path (known wiring gap, research 20 §1.6).
- Expansion: pure `expand_via` + `outgoing_edges` WOQL (`expansion.rs`, `mod.rs:564–587`).
- Server search/graph composition assumes Terminus-backed graph (`server/search/mod.rs` anchors in research 20).

IR **is** stored and audited (`get_section(manifest.ir_ref)` in `save/blobs.rs:168–173`) but **not decoded for graph queries**.

### 3.2 Planned cold path (research 20 / GD-8 / GD-25)

Design (not implemented):

```
graph call → TieredGraphRouter
  Hot ∧ ready → Terminus Graph<Live>
  else → ColdGraph: CAS get(ir_ref [+ occ]) → postcard decode Index
         → from_ir::project → flatten to (SymbolId, RelationKind, SymbolId)
         → adjacency indexes → GraphStore
```

MVP: **re-project always** from IR (option A); optional lean `ColdGraphBlob` later (option C/D).  
No persisted full GraphCorpus postcard in MVP (GD-8).  
Home: `runtime/graph/cold/` or `graph-cold` crate; projector stays `compiler/graph/from_ir.rs:903+`.

`from_ir::project` exists and is pure enough (`from_ir.rs:903–957`) — used by linked_data emit, not by registry runtime.

### 3.3 Cost model (research only)

Cold miss ~5–25 ms small / ~100–500 ms large; warm moka hit sub-ms. Terminus simple WOQL ~3–15 ms RTT. Aligns with “most packages never pay Terminus.”

---

## 4. Terminus hot tier: when IR is NOT used

### 4.1 Live

| Concern | Reality |
|---|---|
| Graph reads | **Always Terminus** if graph is used — no tier router |
| Admission control | **None** — only test stub `usage_is_tracked_for_tiering` (`server/tests/initialization_flow.rs:122–129`) |
| `StoreLinks.graph` | Bitmap bit “materialized in Terminus” (`metadata/mod.rs:32–38`) — tracks fan-out completion, not hot/cold |
| Terminus store of IR | Terminus holds **graph documents**, not postcard Index |
| crates.io terminus-store | Banned for production (research 07; GD-13) — HTTP Document/WOQL to server v12 |

**When IR is not used today for graph:** essentially **always for production graph ops**. IR is durability/reproducibility root of truth for rebuild; graph path never loads it.

**When IR *is* used:** compile/generate, blob emit, integrity audit, (future) rebuild of derived sinks; linked_data emit projects IR→JSON-LD at compile time for Terminus-shaped docs, but runtime `insert_symbols` is a thinner path.

### 4.2 Planned (07 / GD-13)

- Terminus = **hot only**: leaky-bucket scorer + CMS doorkeeper.
- Warming/Cooling packages answer from **cold** until promote ready / after soft demote.
- Demotion pins cold first; hot probe failure → cold failover.
- Incremental hot publish: symbol set-diff from IR, Document API `overwrite=true`.
- DB-per-package, one commit per generation.

**IR-VCS implication:** cold path + any VCS of IR must remain the **always-correct** fallback; Terminus never becomes SoT for symbol content.

---

## 5. Client sync: want/have CAS protocol (planned, not live)

### 5.1 Live

| Component | Status |
|---|---|
| GUI backend | Pure HTTP remote (`workspace/gui/src/backend.rs`) — no local REGISTRY |
| `/v1/sync/plan` | **Does not exist** — routes are flat `/search`, `/packages`, … (`21-wire` inventory) |
| Want/have | **Not implemented** |
| Dual INDEX/REGISTRY | Naming exists only in research + master plan; live monolith is registry+server+postgres |
| Blob transport | Server-side object_store; no presigned client CAS API |

### 5.2 Planned contract (14 + 21 + GD-20 + GD-30)

**Unit of atomicity:** one package **generation** = manifest + full section **closure**.

```
closure(G) = {
  manifest_hash (Hash ②),
  ir_ref, references_ref, [future occurrences_ref],
  all FileEntry.hash
}
```

**Protocol sketch (21 / 14):**

```
POST /v1/sync/plan
  body: { have: [ContentHash], want_from: [{ package, generation }] }
  → { want: [ContentHash], urls: { hash → presigned_url }, expires_at }

client: want = closure(G) \ local_have
client: GET each want URL via BlobTransport (v1 HttpPresign; Phase-2 iroh)
client: verify blake3; stage; atomic commit → GenerationStatus::Ready
```

- **Not** CRDT / ElectricSQL / PowerSync / cr-sqlite (GD-20).
- Routing: if all DepSet members Ready → local; else remote + background SyncEngine; offline = Ready subset only.
- Control plane HTTP/JSON `/v1`; data plane **never** proxies blob bytes through INDEX/ORCH.
- Generation visibility all-or-nothing (14 §3.2 staging model).

**IR-VCS interaction:** if IR becomes multi-object (patches/hunks), **closure definition must expand** while still presenting a single generation stamp (Hash ①) to the sync engine. Want/have remains hash-set difference over whatever leaf objects the generation commits.

---

## 6. Gaps an IR-native VCS would close

1. **Whole-Index monolith**  
   Any symbol change rewrites and rehashes the entire Index postcard → no cross-version IR dedupe at symbol grain; patch releases pay full IR rewrite.

2. **No symbol-level history**  
   Lineage is planned via monikers + part-hashes (GD-7) and Terminus commit log (hot only). Cold lineage requires loading two full Indexes and set-diffing. VCS of IR would give constructive history without Terminus.

3. **No early-cutoff boundary inside IR storage**  
   Incremental spine (08 / GD-14) wants per-symbol digests → skip embed/index/publish. Storage still treats IR as one blob; JobKey CAS is package/stage grain (`generate/mod.rs:106–121`), not symbol grain.

4. **OccurrenceSet missing from generation**  
   Cold graph Reference quality depends on occurrences (20 §1.6). VCS design should not bake in CST-only `references_ref` as the sole ref corpus.

5. **Graph cannot serve long tail offline**  
   Without cold path + partial IR fetch, desktop REGISTRY cannot answer graph offline for non-hot packages even if source/IR blobs sync.

6. **HashMap/HashSet non-determinism**  
   Index postcard order is non-canonical (research 04 §2.4). VCS content addressing at symbol grain needs canonical encoding of each unit.

7. **Dual reference pipelines**  
   CST ResolvedReference vs OccurrenceSet (04 §3.5). VCS SoT should be OccurrenceSet + surface IR.

8. **No client partial IR fetch**  
   Want/have is all-or-nothing on section hashes. IR-VCS could expose finer hashes inside generation for progressive symbol pages — but must not break atomic generation Ready semantics.

9. **Terminus write/read model mismatch**  
   Full GraphCorpus edges vs thin Symbol docs. Unifying around IR as SoT + one lowerer (GD-8) is cleaner if IR history is first-class.

10. **Three snapshot definitions**  
    - BlobInfo.snapshot = files only (`blob_info.rs:30–46`)  
    - Hash ① = files + ir + refs + toolchain  
    - Hash ② = postcard manifest  
    - BlobBuilder.provisional_generation = arrival-order running digest (`creation.rs:64–70, 137–139`)  
    IR-VCS must not add a fourth without deprecating the others.

---

## 7. What MUST be preserved

### 7.1 Dual-hash discipline (Hash ① ≠ Hash ②)

Pinned tests and module docs. Generation identity must survive **serde field adds** to BlobManifest (or successor). CAS key must equal stored bytes. **Never unify.**

### 7.2 Content addressing = BLAKE3-256 (`ContentHash`)

All CAS integrity is re-hash on read (`store.rs:138–145`). Client sync assumes trustless-at-hash.

### 7.3 Generation as sync atomicity unit

Ready only after full closure verified (14). Partial IR hunks may exist in staging; product API must not see mixed generations.

### 7.4 Dual planes: INDEX (remote) + REGISTRY (local)

GD-1/GD-27. Same logical schema; different backends (S3 vs disk). IR-VCS objects must be storable on both.

### 7.5 Trees never in generation identity

GD-11. Source + reparse; optional ephemeral projection cache only.

### 7.6 IR as cold SoT for graph

GD-8/GD-25/GD-34. Terminus and optional Ladybug are accelerators. Demotion must not lose graph fidelity.

### 7.7 Identity layers

Do not collapse NudoxPath / moniker / SymbolId / ContentHash (0.3 + GD-7). VCS keys should align with **content** (part hashes) and **coordinate** (moniker/NudoxPath), not store-join UUIDs alone.

### 7.8 JobKey stage CAS semantics

Producer version ‖ toolchain ‖ source ‖ dep_lock still gate re-produce. IR-VCS does not replace compile isolation; it versions **outputs**.

### 7.9 Outbox / sink disposability

Derived indexes (tantivy, vectors, terminus) rebuildable from blobs + catalog (GD-14). IR-VCS must remain enough to rebuild.

### 7.10 Compile wire completeness (target)

Compiler protocol v2 must return full artifacts (GD-9): surface + occurrences + archive digests + snapshot — not drop occurrences as v1 does.

---

## 8. File path inventory — load-bearing storage touchpoints

### 8.1 IR types

| Path | Role |
|---|---|
| `/Users/philocalyst/Projects/Backend/workspace/ir/entry.rs` | Index, NudoxPath |
| `.../ir/kind.rs` | Entry, Symbol\<T\> |
| `.../ir/function.rs`, `record.rs`, `module.rs`, `protocols.rs`, `parameter.rs`, `ty.rs`, `generics.rs`, `primitives.rs` | payloads |
| `.../ir/syntax/occurrence.rs`, `types.rs`, `walker.rs`, `body.rs` | occurrences / refs / bodies |
| `.../ir/pipeline/pipeline.rs` | Collected → Indexed |
| `.../ir/Cargo.toml` | serde/facet features |

### 8.2 Content addressing & cache

| Path | Role |
|---|---|
| `workspace/heart/content.rs` | ContentHash, JobKey, Freshness |
| `workspace/heart/cache/disk.rs` | DiskCas envelope |
| `workspace/heart/cache/mod.rs`, `tiered.rs`, `stampede.rs`, `single_flight.rs` | Cas trait, L1/L2/L3 |
| `workspace/heart/identity/symbol.rs` | SymbolId, EntryUri |
| `workspace/heart/identity/package.rs` (via PackageId) | package UUID identity |
| `workspace/heart/connection.rs` | Cold/Live typestate |

### 8.3 Blob / registry store

| Path | Role |
|---|---|
| `workspace/registry/blob/mod.rs` | BlobManifest, dual hash, ReferenceSet codec |
| `workspace/registry/blob/creation.rs` | BlobBuilder, set_ir, set_references |
| `workspace/registry/blob/emit.rs` | put sections + put_manifest + outbox |
| `workspace/registry/store.rs` | cas/ + ptr/ object store |
| `workspace/registry/ingest/mod.rs` | archive → BlobBuilder |
| `workspace/registry/metadata/mod.rs` | StoreLinks |
| `workspace/registry/metadata/hash.rs` | re-export / generation stamp docs |
| `workspace/registry/protocol.rs` | CompileRequest/Response twin |
| `workspace/registry/index/mod.rs` | postgres GlobalPackage / generation |
| `workspace/registry/schema/queries.rs` | SQL generation/outbox |
| `workspace/registry/tests/blob_hash_pins.rs` | golden Hash ①/② |
| `workspace/registry/tests/blob_store_roundtrip.rs` | CAS integrity |
| `workspace/registry/tests/blob_assembly.rs` | builder sections |

### 8.4 Compiler generate / graph

| Path | Role |
|---|---|
| `workspace/compiler/generate/mod.rs` | generate_with stages |
| `workspace/compiler/generate/surface.rs` | Index production |
| `workspace/compiler/generate/cst.rs` | CstSet (transient trees) |
| `workspace/compiler/generate/occurrences.rs`, `resolve.rs` | OccurrenceSet (occ-v1) |
| `workspace/compiler/generate/source_archive.rs` | per-file BLAKE3 archive |
| `workspace/compiler/generate/blob_info.rs` | file-only snapshot fold |
| `workspace/compiler/generate/parse_cache.rs` | source tree hash for JobKey |
| `workspace/compiler/generate/linked_data/emit.rs` | GraphCorpus → JSON-LD |
| `workspace/compiler/compile/producer/runtime.rs` | postcard CAS for stages |
| `workspace/compiler/protocol.rs` | wire twin |
| `workspace/compiler/bin/compiler_daemon.rs` | postcard surface emit |
| `workspace/compiler/graph/from_ir.rs` | project() |
| `workspace/compiler/graph/link.rs`, `symtab.rs`, `model.rs` | IRI / linker / documents |

### 8.5 Server / graph runtime

| Path | Role |
|---|---|
| `workspace/server/coordination/indexing.rs` | extract/compile/emit orchestration |
| `workspace/server/save/blobs.rs` | verify_blobs, rebuild_from_blobs |
| `workspace/server/save/mod.rs` | derived store rebuild integrity |
| `workspace/server/compiler_client.rs` | postcard HTTP to daemon |
| `workspace/server/search/**` | search + graph expand consumers |
| `workspace/server/http/router.rs` | live routes (no sync) |
| `workspace/registry/runtime/graph/mod.rs` | GraphStore + Terminus |
| `workspace/registry/runtime/graph/expansion.rs` | BFS |
| `workspace/registry/runtime/graph/resolution.rs` | pure cross-version diff |
| `workspace/registry/runtime/graph/structure.rs` | structure assemble |

### 8.6 Research / plan SoT

| Path | Role |
|---|---|
| `LIBRARIFICATION-PLAN.md` | frozen GDs, dual INDEX/REGISTRY, GD-8/10/11/13/20/27 |
| `.research/librarification/04-ir-audit{,/PLAN}.md` | IR shape audit |
| `.../07-terminus-tiering{,/PLAN}.md` | hot admission |
| `.../08-incremental{,/PLAN}.md` | constructive traces / SymbolDelta |
| `.../13-storage{,/PLAN}.md` | CAS layout, compression, sync manifest |
| `.../14-client-sync{,/PLAN}.md` | want/have SyncEngine |
| `.../20-graph-over-ir{,/PLAN}.md` | ColdGraph design |
| `.../21-wire-protocol{,/PLAN}.md` | /v1 + closure + compiler v2 |

---

## 9. Contradictions: research docs vs live code

| # | Claim (research / master plan) | Live code | Severity |
|---|---|---|---|
| C1 | Generation stamp = Hash ① everywhere | Outbox uses Hash ② from `put_manifest` return (`emit.rs:60–65`); Stored uses Hash ① (`indexing.rs:193`) | **High** — dual generation domains |
| C2 | `occurrences_ref` on BlobManifest (GD-10, 04 gap list as “to land”) | Field absent; compile Ok drops OccurrenceSet | High for cold graph fidelity |
| C3 | Cold graph-over-IR is default (GD-25) | No ColdGraph; only Terminus GraphStore | High — design vs runtime |
| C4 | Terminus hot-only with leaky-bucket | No admission; graph materialization is fan-out to all sinks | Medium |
| C5 | INDEX sqlite + S3; REGISTRY local sqlite + cas | Live server still postgres-backed index (`registry/index`, schema queries) | High for migration |
| C6 | Client want/have SyncEngine | No routes, no client crate, GUI pure remote | High |
| C7 | Compiler postcard v2 full artifacts | v1: surface + CST refs + identifiers only (`protocol.rs:73–85`) | High |
| C8 | BlobInfo/snapshot includes IR identity | `BlobInfo::assemble` **ignores** surface/cst for hash — files only (`blob_info.rs:30–34`) while Hash ① includes ir_ref | Medium — dual snapshot stories |
| C9 | Graph insert = GraphCorpus Relation parity | `insert_symbols` thin docs only; WOQL expects Relation class | High — hot path incomplete |
| C10 | Index “stable integer IDs” comment | Keys are NudoxPath | Doc only |
| C11 | Facet encoding path | Feature declared; no `#[facet]` on IR types | Low |
| C12 | zstd IR dictionaries (GD-12) | Raw CAS bytes | Medium (planned) |
| C13 | GD-15 generation id = blake3(project‖repo‖commit) | Live generation = Hash ① of package blob identity (registry packages, not git commits) | Conceptual — different product surfaces |
| C14 | Postgres exits (GD-2) | Live registry index/search still on postgres | Program-level |
| C15 | Research 13 §5.3 still mentions “Postgres (server already uses postgres)” as remote catalog | Master plan freezes sqlite INDEX | Research vs plan conflict; plan wins |
| C16 | `registry::protocol` deleted (GD-26) | Still present as twin of compiler protocol | Pending |
| C17 | StoreLinks.graph = Terminus admission | Acts as outbox materialization bit | Semantic overload risk |

---

## 10. Implications for a Pijul-inspired IR-native VCS

### 10.1 What “replace blob-stored IR” means in this codebase

Today:

```
ir_ref → single postcard(Index) object
```

Target direction (inferred from gaps + GD-8/14/20):

```
generation commits a set of IR atoms (symbols / modules / patches)
  addressed by content hash
  composable across package versions
  projectable to GraphStore without Terminus
  still committed under one Hash ① generation stamp for sync
```

### 10.2 Non-negotiable integration points

1. **`BlobManifest` successor** must keep dual-hash discipline; `ir_ref` may become `ir_root` / patch set hash / tree hash, but identity_bytes must remain schema-stable.
2. **Closure for sync** must list every IR atom hash the generation needs for Ready (or a single pack + TOC — ArchiveIndex GD-29).
3. **`from_ir::project` input** must still be reconstructible as an `Index` (or GraphCorpus) for cold path — VCS is storage, not a second IR algebra unless project is rewritten.
4. **Daemon wire** must stop treating surface as one opaque `Vec<u8>` only if clients need streaming atoms; alternatively daemon still emits full Index and INDEX rewrites into VCS on ingest (transitional).
5. **Outbox generation field** must be fixed to Hash ① before multi-generation VCS history multiplies the bug.

### 10.3 What VCS should not own

- Tree-sitter live trees  
- Tantivy / vector segments  
- Terminus layers (hot cache only)  
- Untrusted compile execution (ORCH)  
- Package coordinate catalog (MetaStore)

### 10.4 Minimum viable “IR-VCS-shaped” intermediate (if full Pijul is far)

Even without full patch algebra:

1. Canonical per-entry postcard (sorted keys) + content hash per `NudoxPath`  
2. Generation = merkle over sorted (path, entry_hash) + toolchain + occ_root  
3. CAS leaves = entry atoms; optional whole-Index cache blob for fast cold project  
4. SymbolDelta = set ops on entry hashes between generations  

This alone closes gaps 1–3 and feeds GD-14 early cutoff without inventing a second product.

---

## 11. Condensed ground-truth checklist for implementers

- [x] IR storage path is **postcard `Index` under `ir_ref`**  
- [x] Serialization is **postcard/serde**, not JSON, not facet-live  
- [x] Types: `ContentHash` BLAKE3-32; `NudoxPath`; flat `Index`; tagged `Entry`  
- [x] Cold graph: **designed, not coded**  
- [x] Hot Terminus: **only live GraphStore**; IR unused on query path  
- [x] Want/have: **spec only**  
- [x] Dual Hash ①/②: **sacred**, but outbox currently mis-keyed  
- [x] Preserve dual INDEX/REGISTRY, generation atomicity, tree non-persistence, IR-as-SoT  
- [x] Fix occurrences on wire + manifest before cold graph parity  
- [x] Inventory of storage touchpoints listed in §8  

---

## 12. Word-count note / method

This brief was produced by reading live sources under `workspace/{ir,heart,registry,compiler,server}` and research `04,07,08,13,14,20,21` plus root `LIBRARIFICATION-PLAN.md`, cross-checking every storage claim against file contents rather than research prose. Where research and code disagree, **code wins** and the disagreement is recorded in §9.

**Bottom line for IR-VCS design:** you are not replacing a sophisticated IR version control layer — you are replacing **one opaque CAS blob per package generation**, while the rest of the system already assumes content-addressed sections, dual generation/CAS hashing, and a not-yet-built cold graph + sync plane that both desperately want **finer-grained, deterministic, reconstructible IR units**.
