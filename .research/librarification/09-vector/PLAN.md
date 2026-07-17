# 09 — Embedded Vector Search for Local REGISTRY (GPUI Desktop)

**Research date:** 2026-07-16  
**Scope:** Embedded/on-device vector databases for nudox’s GPUI desktop app (Rust, macOS + Linux); remote Qdrant INDEX compatibility; local embedding generation; `VectorStore` trait design.  
**Context scale:** ~10⁴–10⁶ vectors per project-with-dependencies; target **&lt;500 MB** resident for vector search in the GUI; incremental upsert/delete by stable symbol ID; payload filters (language, package, kind).

> **Verification policy:** All version numbers, crate status, and product claims below were re-checked against live web sources / crates.io on **2026-07-16**. Training data is not trusted.

---

## 0. Executive verdict (read this first)

| Decision | Recommendation |
|---|---|
| **Embedded store winner** | **`qdrant-edge` (Qdrant Edge) v0.7.2** — true in-process Rust library, disk-resident, same filter/payload model as the remote Qdrant server already in `workspace/registry` and `workspace/server` (`qdrant-client = "1.18"`) |
| **Runner-up** | **LanceDB (`lancedb` 0.31.0)** — best pure-disk columnar alternative if Edge beta risk or dependency politics force a second path |
| **Do not ship** | Qdrant **sidecar binary** as primary (Edge supersedes it); bare `instant-distance` (stale since 2023); raw HNSW alone without a payload store |
| **Local embedding model** | **`jinaai/jina-embeddings-v2-base-code`** (768-d, 137M, Apache-2.0, 8K context) via **fastembed-rs** / ONNX Runtime (`ort`) |
| **Local embedding runner** | **fastembed 5.17.x + `ort`** (CPU default; Metal/CoreML EP optional later) |
| **Remote ↔ local compatibility policy** | **Same model + same dimension + same distance metric in both places** (single-index identity). Dual-index only as an explicit upgrade path with versioned collection names. |
| **Storage layout** | **One Edge shard (or Lance table) per workspace project**, with payload fields `language`, `package`, `kind`, `symbol_id`; optional global “open projects” fan-out. Quantize with **scalar or binary** under the 500 MB RAM ceiling. |

The rest of this document is the exhaustive evidence trail, decision matrix, trait sketch, and risks.

---

## 1. Problem framing for nudox

### 1.1 Current state (codebase)

As of 2026-07-16 the live tree wires **remote Qdrant only**:

- [`workspace/server/Cargo.toml`](../../workspace/server/Cargo.toml) — `qdrant-client = "1.18"`
- [`workspace/registry/Cargo.toml`](../../workspace/registry/Cargo.toml) — `qdrant-client = "1.18"` (read-plane vector store)

Semantic search today assumes a **networked Qdrant server** storing per-symbol embeddings. The librarification plan moves **vector search into the GPUI desktop process**, backed by **local disk**, while the **remote INDEX** continues to serve multi-project / multi-tenant scale.

### 1.2 Hard requirements (from architecture plan)

| Requirement | Why it matters |
|---|---|
| **Upsert + delete by stable symbol ID** | Incremental re-embed of changed symbols only; deletes must not leave zombie hits |
| **Payload filtering** | language / package / kind (and future facets) must participate in ANN, not only post-filter |
| **Disk-resident index, bounded RAM** | Desktop app shares RAM with editor UI, LSP, and embedding model; target **&lt;500 MB** for vector search |
| **Build times acceptable at 10⁴–10⁶ vectors** | Open project → warm index without multi-hour jobs |
| **Trait spanning remote + local** | One application path; remote INDEX for scale, local for offline / private / latency |
| **Crash safety** | WAL / transactional durability — developer machines power-cycle and force-quit |

### 1.3 Soft preferences

- Prefer **Apache-2.0 / MIT** licenses for redistribution inside a commercial desktop binary.
- Prefer **Rust-native** crates (no Python sidecar for core path).
- Prefer **API isomorphism** with existing Qdrant filter vocabulary to avoid dual filter compilers.
- Prefer **small binary delta** on the GPUI app (order tens of MB, not hundreds).

---

## 2. Qdrant embedded feasibility — settled for 2026

### 2.1 Historical situation (pre-Edge)

For years the community asked for an embedded Qdrant. The only practical answers were:

1. **Python client “local mode”** (`QdrantClient(":memory:")` / path) — not a Rust in-process library; unsuitable as the GPUI core path.
2. **Ship a `qdrant` server binary as a sidecar** — start process, bind localhost port, manage lifecycle, ~binary size of full server.
3. **Reimplement against a different embedded ANN** and accept filter/API divergence from remote.

### 2.2 Qdrant Edge (2026) — definitive answer

**Yes: Qdrant can run embedded/in-process in Rust in 2026.**

Primary sources:

- Product docs: [https://qdrant.tech/documentation/edge/](https://qdrant.tech/documentation/edge/)
- Launch-style blog (2026-06-16): [https://qdrant.tech/blog/qdrant-edge-on-device-vector-search/](https://qdrant.tech/blog/qdrant-edge-on-device-vector-search/)
- Quickstart (Python + Rust): [https://qdrant.tech/documentation/edge/edge-quickstart/](https://qdrant.tech/documentation/edge/edge-quickstart/)
- crates.io: [https://crates.io/crates/qdrant-edge](https://crates.io/crates/qdrant-edge) — **v0.7.2**, published **2026-06-01**, license **Apache-2.0**, crate size ~1.6 MB source package
- Maintainer confirmation of Rust crate: [https://github.com/orgs/qdrant/discussions/8190](https://github.com/orgs/qdrant/discussions/8190) (Andrey Vasnetsov / `generall`: “yes” → links `crates.io/crates/qdrant-edge`)
- Upstream monorepo: [https://github.com/qdrant/qdrant](https://github.com/qdrant/qdrant) documents Edge as the in-process path

#### What Edge is

From official docs (paraphrased and quoted concepts):

> Qdrant Edge is a lightweight, embedded vector search engine for **in-process** retrieval with a minimal memory footprint and **no background services**. Think of it as **SQLite, but for vector search**.

Key properties relevant to nudox:

| Property | Edge behavior | Desktop fit |
|---|---|---|
| Process model | In-process library (`EdgeShard`) | No Docker, no sidecar |
| Install footprint | ~**11 MB** claimed in product blog | Acceptable for desktop |
| Storage | Local directory / shard on disk | Project-scoped paths |
| Vectors on disk | `on_disk(true)` in `EdgeVectorParams` | Bounded RAM |
| Payload on disk | `on_disk_payload(true)` | Bounded RAM |
| Filters | Full Qdrant-style `Filter` / field conditions | Matches remote INDEX language |
| Upsert / delete | `UpdateOperation` (upsert points, field indexes, etc.) | Incremental symbol updates |
| Quantization | Scalar, Product, Binary, TurboQuant | Fit under 500 MB |
| WAL | Write-ahead log (default 32 MB segment; configurable in Rust) | Crash safety |
| Optimize | **Synchronous** `optimize()` — **no background optimizers** | GUI-controlled scheduling |
| Sync to server | Snapshots / partial snapshot recover | Local ↔ remote INDEX bridge |
| Status | **Beta** (API may change) | Plan for version pin + abstraction |

#### Rust API surface (from quickstart)

Capable operations needed by REGISTRY:

- `EdgeShard::new` / `load` / drop-flush
- `update(UpdateOperation::… upsert / delete / create_field_index / create sparse vector …)`
- `query(QueryRequest { filter, limit, … })` — nearest + filtered
- `retrieve`, `scroll`, `count`, `facet`
- `optimize`, `flush`
- Snapshot unpack / recover for server sync patterns

This is **not** a thin ANN wrapper: it is the Qdrant engine cut down to a library with explicit lifecycle.

#### Beta caveats (do not ignore)

1. Docs state explicitly: **“Qdrant Edge is in beta. The API and functionality may change.”**  
   [https://qdrant.tech/documentation/edge/](https://qdrant.tech/documentation/edge/)
2. crates.io downloads are still low (~2.4k total as of research date) vs `qdrant-client` (~3M) — early adoption curve.
3. Edge API is **not identical** to `qdrant-client` gRPC types (different crate, `PointStructPersisted`, builders). Trait adapters are still required.
4. No background segment optimizer — the app **must** call `optimize()` (e.g. after bulk reindex idle). This is actually good for desktop (predictable CPU).

### 2.3 Sidecar binary — still possible, no longer preferred

| Axis | Sidecar `qdrant` binary | Qdrant Edge library |
|---|---|---|
| Lifecycle | Spawn, health-check, port bind, SIGTERM on app exit | `Drop` / `close` |
| Ports | Localhost collision risk, firewall noise | None |
| Multi-window / multi-project | One process + multi-collections *or* multi-process mess | One shard dir per project |
| Binary size | Full server binary (tens of MB+) + same for every OS | ~11 MB library footprint claim |
| Crash isolation | Process isolation (pro) | In-process (con for native crashes) |
| Feature parity with cloud Qdrant | Highest | High for single-shard; no clustering |
| Code complexity | Process supervisor + client | Direct library calls |

**Verdict:** Sidecar is a **fallback** if Edge hits a showstopper (license packaging issue, Metal/segfault in-process, or beta API thrash). It is **not** the primary design in 2026. MindWork AI Studio’s Feb 2026 discussion ([#8190](https://github.com/orgs/qdrant/discussions/8190)) is exactly the pre-crate pain; the official answer is now the crate.

### 2.4 Python embedded client — out of scope

Python `QdrantClient(path=…)` remains fine for tests/scripts. It is **not** a path for the GPUI Rust binary.

---

## 3. Embedded candidates (Rust) — full evaluation

For each candidate: version (2026-07-16), license, maintenance, index types, disk vs memory, filters, incremental upsert/delete, crash safety, binary/build cost.

### 3.1 Decision matrix (summary)

| Candidate | Version | License | Maint. | Index | Disk-first | Rich filters | Incr. upsert/delete | Crash safety | Fit for nudox |
|---|---|---|---|---|---|---|---|---|---|
| **qdrant-edge** | 0.7.2 (2026-06) | Apache-2.0 | Official Qdrant; **beta** | HNSW (+ Qdrant segment stack) | Yes (`on_disk`) | **Full Qdrant filters** | **Yes** | WAL + flush | **Winner** |
| **lancedb** | 0.31.0 (2026-07-02) | Apache-2.0 | Very active | IVF_PQ, IVF_HNSW_*, IVF_RQ, … | Yes (Lance format) | SQL-like `where` | Add/delete rows; **index refresh manual in OSS** | Columnar + versions | Strong runner-up |
| **usearch** | 2.26.0 (2026-07-10) | Apache-2.0 | Very active | HNSW | mmap views | Predicate/ban-list style | Yes (rename/delete) | File save/load | Good ANN core; **payload DIY** |
| **arroy** | 0.6.4 (2026-04) | MIT | Meilisearch; slowing vs Hannoy | Random-proj trees (Annoy) | LMDB mmap | RoaringBitmap filter | Incremental tree update | LMDB txns | Solid; filter model thinner |
| **hnsw_rs** | 0.3.4 (2026-02) | MIT/Apache | Solo/small | HNSW | mmap data (not full graph always) | Filter trait | Partial | Dump files | Building block only |
| **instant-distance** | 0.6.1 (**2023-06**) | MIT/Apache | **Stale** | HNSW | In-memory | Minimal | Limited | None first-class | **Reject** |
| **sqlite-vec** | 0.1.10-alpha.4 (2026-05) | MIT/Apache | Active but **pre-v1** | Brute-force (+ quant) | SQLite pages | SQL JOIN filters | Yes (SQL) | SQLite journal | Good ≤~2e5 vecs; weak at 1e6 ANN |
| **Chroma** | n/a as Rust core | — | Python/Go path | HNSW etc. | Varies | Yes (Python) | Yes | Varies | **Reject** for GPUI core |

---

### 3.2 LanceDB (`lancedb` crate)

**Sources:**  
[https://crates.io/crates/lancedb](https://crates.io/crates/lancedb) · [https://docs.lancedb.com/indexing/vector-index](https://docs.lancedb.com/indexing/vector-index) · [https://github.com/lancedb/lancedb](https://github.com/lancedb/lancedb)

| Field | Value |
|---|---|
| Version | **0.31.0** (2026-07-02) |
| License | Apache-2.0 |
| Downloads | ~687k total, ~408k recent — healthy |
| Min Rust | 1.91.0 (per crates.io) |
| Design | Serverless embedded vector DB on **Lance** columnar format |

**Index types (2026 docs):**

- `IVF_PQ`, `IVF_RQ`, `IVF_SQ`, `IVF_FLAT`
- `IVF_HNSW_FLAT`, `IVF_HNSW_PQ`, `IVF_HNSW_SQ` (HNSW is a **sub-index inside IVF partitions**, not a free-floating top-level HNSW alone)
- Binary vectors via `IVF_FLAT` + Hamming

**Disk vs memory:** Core value prop is **disk-based IVF + PQ** with refine-factor re-ranking. RAM stays far below “all vectors hot.” Matches the &lt;500 MB goal at 10⁶ × 768 better than pure in-RAM HNSW.

**Filtered search:** First-class `where(...)` on metadata. Docs warn that **HNSW-backed IVF can show higher latency variance under heavy filters**; prefer `IVF_PQ` / `IVF_RQ` when filters dominate ([docs](https://docs.lancedb.com/indexing/vector-index)).

**Incremental upsert/delete:** Table add/delete is supported. **OSS requires manual `create_index` / refresh** as data changes — Enterprise has automatic indexing. For a desktop app this means:

- Batch symbol updates into a writer
- Periodically rebuild or update ANN index (async job, progress UI)

**Crash safety:** Lance multi-versioned datasets; stronger story than raw HNSW dumps, different from Qdrant WAL semantics.

**Binary / build cost:** Pulls Arrow/object-store ecosystem — **heavier compile** than usearch/arroy. Acceptable for desktop if Edge is blocked; not the lightest.

**Fit:** Excellent general embedded vector DB. **Weaker trait isomorphism** with existing Qdrant remote (filter AST, scores, collection model differ). Use if team wants columnar multimodal future (code screenshots, diagrams) more than Qdrant parity.

---

### 3.3 USearch (`usearch` crate)

**Sources:**  
[https://crates.io/crates/usearch](https://crates.io/crates/usearch) · [https://github.com/unum-cloud/USearch](https://github.com/unum-cloud/USearch) · [https://www.unum.cloud/usearch](https://www.unum.cloud/usearch)

| Field | Value |
|---|---|
| Version | **2.26.0** (2026-07-10) — extremely fresh |
| License | Apache-2.0 |
| Downloads | ~797k total, ~341k recent |
| Design | Single-file HNSW engine; multi-language; mmap serving |

**Index:** HNSW with strong SIMD; claims ~10× FAISS HNSW speed in Unum marketing (treat as vendor-claim; re-benchmark on code embeddings).

**Disk:** Memory-mapped index views — serve large indexes without loading all vectors into process RSS. Good for desktop.

**Filters:** Predicate / external ban-list style during traversal — **not** a structured payload engine. You still need SQLite/JSON for language/package/kind and map to allowed IDs or a callback filter.

**Incremental:** Supports add, rename/relabel, on-the-fly deletions (README feature list).

**Crash safety:** File save/load; not a transactional multi-table DB.

**Fit:** Ideal **ANN kernel** if you already own a SQLite metadata plane (see also research task 11-sqlite-index). **Not** a full drop-in for Qdrant remote semantics. Binary size is excellent.

---

### 3.4 arroy (Meilisearch)

**Sources:**  
[https://crates.io/crates/arroy](https://crates.io/crates/arroy) · [https://github.com/meilisearch/arroy](https://github.com/meilisearch/arroy) · [https://www.meilisearch.com/blog/arroy-filtered-disk-ann](https://www.meilisearch.com/blog/arroy-filtered-disk-ann)

| Field | Value |
|---|---|
| Version | **0.6.4** (2026-04-07) |
| License | MIT |
| Downloads | ~438k total |
| Design | Annoy-inspired **random projection forests** on **LMDB** |

**Strengths:**

- Disk-backed, multi-process readers, concurrent readers during writers (LMDB)
- **Incremental updates** without full rebuild (Meilisearch production path)
- Filter via **RoaringBitmap** of allowed IDs during search
- Binary quantization options in Meilisearch stack
- Distance metrics: Euclidean, Manhattan, Cosine, Dot

**Weaknesses:**

- Not HNSW — different recall/latency profile; README notes better at lower dimensions, “surprisingly well” up to ~1000-d
- Payload filtering is **external** (you compute the bitmap of symbol IDs first)
- Meilisearch March 2026 updates describe **stabilizing an HNSW-backed vector store (“Hannoy”)** and removing the legacy experimental vector store setting — arroy remains a crate but the **engine’s strategic center of gravity is moving to HNSW** ([https://www.meilisearch.com/blog/March-2026-updates](https://www.meilisearch.com/blog/March-2026-updates))
- Last arroy crates.io bump April 2026 — not abandoned, but not the fastest-moving surface

**Fit:** Strong pure-Rust LMDB design if we want **zero Qdrant dependency** and already standardize on LMDB/heed. Weaker strategic bet than Edge or LanceDB for 2027+.

---

### 3.5 hnsw_rs / hnswlib-rs

**Sources:**  
[https://crates.io/crates/hnsw_rs](https://crates.io/crates/hnsw_rs) · [https://github.com/jean-pierreBoth/hnswlib-rs](https://github.com/jean-pierreBoth/hnswlib-rs)

| Field | Value |
|---|---|
| Version | **0.3.4** (2026-02-28) |
| License | MIT / Apache-2.0 |
| Design | Pure Rust HNSW; mmap for data vectors; filter trait |

**Fit:** Excellent for experiments and custom engines. **Missing:** payload DB, WAL, quantization suite, server sync. Use as a research baseline, not the REGISTRY product store.

Note: crates.io also lists other HNSW experiments (e.g. `vector-index` blog crate, 2026) that **lack deletes and persistence** in early versions — avoid for production.

---

### 3.6 instant-distance

| Field | Value |
|---|---|
| Version | **0.6.1** last publish **2023-06-26** |
| Status | **Stale** — do not adopt for new architecture |

Still downloaded transitively in some graphs; **not** maintained for 2026 needs.

---

### 3.7 sqlite-vec

**Sources:**  
[https://github.com/asg017/sqlite-vec](https://github.com/asg017/sqlite-vec) · [https://crates.io/crates/sqlite-vec](https://crates.io/crates/sqlite-vec)

| Field | Value |
|---|---|
| Version | **0.1.10-alpha.4** (2026-05-18) |
| License | MIT / Apache-2.0 |
| Status | **pre-v1** — expect breaking changes (README banner) |
| Design | Pure C SQLite extension; `vec0` virtual tables; float/int8/binary |

**Index model:** Primarily **exact / brute-force** KNN with quantization options — **not** production HNSW/IVF at million scale. Community commentary (2025–2026) places comfortable ranges roughly **10⁵–few×10⁵** vectors before latency pain, depending on dim and hardware.

**Filters:** Best-in-class **SQL** composition with other tables (language, package) — if REGISTRY local store is already SQLite-centric (task 11), this is elegant.

**Deletes / upserts:** Full SQLite transactional semantics.

**Fit:**

- **Yes** for small/medium projects, prototype, or hybrid “metadata in SQLite + ANN elsewhere”
- **No** as sole ANN engine for **10⁶ × 768** under interactive latency without heavy quantization + acceptance of exact-scan costs

Pairs well as **metadata co-store** even if ANN is Edge/Lance.

---

### 3.8 Chroma and others

- **Chroma:** Excellent DX in Python; 2026 comparisons still position it as embedded/dev-friendly, not a first-class **Rust GPUI** dependency. Skip for core.
- **SatoriDB / custom DiskANN blogs:** Interesting single-author experiments; not production candidates without multi-year maintenance.
- **pgvector:** Requires Postgres — wrong shape for offline desktop.
- **Turso/libSQL HNSW DiskANN:** Relevant if the entire local stack is libSQL; indexing can be heavy. Worth a footnote if SQLite path wins storage research, not primary here.

---

## 4. Embedding generation locally

### 4.1 Runtime options (Rust)

| Runtime | Crate | Version (research date) | Pros | Cons |
|---|---|---|---|---|
| **ONNX Runtime via ort** | `ort` | **2.0.0-rc.12** (2026-03) | Industry standard EP (CPU, CoreML, CUDA); mature | Large native lib; EP config complexity |
| **fastembed-rs** | `fastembed` | **5.17.3** (2026-07-15) | Batteries-included models + tokenizers; sync API; uses `ort` | Model catalog may lag HF newest; some models need candle features |
| **Candle** | `candle-core` | **0.11.0** (2026-06-26) | Pure Rust/HF ecosystem; Metal path | Heavier integration per model; more code |

**Recommendation:** **`fastembed` on `ort`** for the default desktop path. Escape hatch: custom ONNX export loaded via `ort` for models not in the enum (e.g. CodeRankEmbed if we export ONNX ourselves).

fastembed model catalog (README, 2026) **already includes**:

- `jinaai/jina-embeddings-v2-base-code` ← **preferred code model**
- `jinaai/jina-embeddings-v2-base-en`
- nomic text models, bge, gte, EmbeddingGemma, Qwen3 embeddings (feature-gated candle), etc.

Source: [https://github.com/Anush008/fastembed-rs](https://github.com/Anush008/fastembed-rs)

### 4.2 Code-specialized open-weight models (2026)

| Model | Params | Dim | Context | License | Local desktop? | Notes |
|---|---|---|---|---|---|---|
| **jina-embeddings-v2-base-code** | 137M | **768** | 8K | **Apache-2.0** | **Yes** (~&lt;300 MB weights class) | In fastembed; strong code/docstring retrieval ([jina.ai model card](https://jina.ai/models/jina-embeddings-v2-base-code/)) |
| **CodeRankEmbed** (nomic-ai) | 137M | 768-class | 8K | **MIT** | Yes (~522 MB reported) | Strong CoRNStack code retrieval; not default in fastembed — ONNX export / candle needed ([HF](https://huggingface.co/nomic-ai/CodeRankEmbed)) |
| **nomic-embed-code** | **7B** | high | long | Apache-2.0 | **No** for default GUI (~26 GB class) | SOTA quality; server/GPU INDEX only ([Nomic announcement](https://www.nomic.ai/news/introducing-state-of-the-art-nomic-embed-code)) |
| voyage-code-3 / voyage-3-large | proprietary | varies | — | commercial | API only | Excellent code quality; **not** open-weights for offline |
| bge-small / MiniLM | 22–33M | 384 | 512 | MIT/Apache | Yes | Too generalist for code-first product quality |

### 4.3 Inference cost sketch (order-of-magnitude)

Assumptions: 768-d Jina code, ~128–512 tokens of symbol context (signature + docstring + short body window), developer laptop CPU (modern Apple Silicon or x86 AVX).

| Scenario | Rough cost |
|---|---|
| Single symbol embed (CPU) | ~5–40 ms depending on length / core |
| Batch 64 symbols | Sub-linear; aim for ONNX batching |
| Full project 50k symbols first index | Minutes, not hours — show progress + cancel |
| Metal / CoreML EP | Often 2–5× faster on Apple Silicon for transformer encoders; validate with `ort` EP tests |

**GUI policy:**

1. Never block the UI thread — dedicated embed worker pool.
2. Incremental: only re-embed symbols whose content hash changed.
3. Persist vectors next to the Edge shard; do not re-embed on every launch.
4. Cap concurrent ONNX sessions to protect editor interactivity.

### 4.4 Compatibility policy: local ↔ remote embeddings

**Problem:** ANN similarity is only meaningful **inside one embedding space**. Mixing Jina-code local with Voyage remote in one index is invalid.

**Policy (recommended):**

| Layer | Rule |
|---|---|
| **Canonical model ID** | String constant e.g. `nudox.embed.v1 = jina-embeddings-v2-base-code@rev` |
| **Collection / shard naming** | Include model id: `symbols__jina_v2_code_768` |
| **Distance** | Cosine (normalize at write if model requires) |
| **Dimension** | Fixed 768 for v1; refuse upsert on mismatch |
| **Remote INDEX** | Runs **the same open model** (or identical ONNX) for the primary collection |
| **Optional dual-index** | `symbols__voyage_code_3` **only** as a separate remote collection for cloud-quality search — **never** merged into local shard |
| **Upgrade path** | New model → new collection name + background re-embed job; keep old readable until cutover |
| **Query routing** | Local queries always use local model; remote queries declare model in request metadata |

**Do not** do “local small model + remote big model” into one fused score without a learned cross-encoder reranker (out of v1 scope). Meilisearch’s composite embedder pattern is interesting for **their** product; for nudox symbol identity, **same-model is simpler and correct**.

---

## 5. Storage layout, quantization, memory ceiling

### 5.1 Collection / shard topology

| Layout | Pros | Cons | Recommendation |
|---|---|---|---|
| **One shard per project** | Delete project = delete directory; natural isolation; easy RAM bound | Multi-project search needs fan-out | **Default** |
| **One global shard + payload project_id** | Single open handle; cross-project search easy | Deletes harder; RAM/disk grows forever | Optional “workspace library” mode |
| **One shard per language** | Smaller specialized indexes | Cross-language search annoying | No |

**Payload schema (minimum):**

```text
symbol_id: string|uuid   # stable across reindexes
language:  keyword
package:   keyword
kind:      keyword       # fn, struct, trait, module, ...
path:      keyword|text  # optional
content_hash: keyword    # for incremental embed
model_id:  keyword       # defense in depth
```

Create **payload indexes** on `language`, `package`, `kind` at shard init (Edge `create_field_index`).

### 5.2 Memory model under &lt;500 MB

Rough math for **1e6 vectors × 768 × f32**:

- Raw vectors alone: 1e6 * 768 * 4 ≈ **3.0 GB** — **cannot** keep hot in RAM.
- Therefore **on_disk vectors + quantization** are mandatory at upper scale.

| Technique | Approx. footprint | Quality |
|---|---|---|
| f32 on disk, mmap pages only | RSS ≈ graph + working set | Best quality |
| **Scalar quant (int8)** | ~1/4 vectors | Small quality loss; great default |
| **Binary / 1-bit** | ~1/32 | Aggressive; use with rescore/refine if available |
| Product quant | Configurable | Strong for Lance IVF_PQ; also in Qdrant |

**Edge knobs:** `on_disk(true)` vectors, `on_disk_payload(true)`, quantization_config, smaller WAL (`wal_options.segment_capacity`), explicit `optimize()` after bulk deletes.

**Lance knobs:** `IVF_PQ` or `IVF_RQ` for compression; `refine_factor` for quality; prefer PQ/RQ under heavy filters.

**Working set budget suggestion:**

| Component | Budget |
|---|---|
| ANN graph / centroids hot | 100–200 MB |
| Quantized vector pages | 50–150 MB |
| Payload indexes | 20–50 MB |
| Embed model weights (if loaded) | 150–300 MB (Jina 137M class) — **count separately from “vector search” if possible; unload when idle** |
| **Vector search RSS target** | **&lt;500 MB** excluding UI |

### 5.3 Scale tiers

| Vectors | Strategy |
|---|---|
| ≤ 20k | Flat or HNSW, f16/f32, little quant needed |
| 20k–200k | HNSW/Edge default + scalar quant optional |
| 200k–1M | **Required** on_disk + quant; scheduled optimize; maybe IVF (Lance) |
| &gt;1M deps | Prefer remote INDEX; local holds **project + hot deps** only |

---

## 6. Trait design: `VectorStore`

### 6.1 Goals

- One application-facing trait implemented by **`QdrantRemote`** (`qdrant-client` 1.18) and **`QdrantEdgeLocal`** (`qdrant-edge`).
- Optional third impl: `LanceLocal` behind feature flag (runner-up / escape hatch).
- Async-friendly for remote; Edge is sync under the hood — wrap with `spawn_blocking` or a dedicated store thread.

### 6.2 Sketch

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;

/// Stable application identity for a symbol embedding row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SymbolId(pub String); // or uuid::Uuid

#[derive(Clone, Debug)]
pub struct VectorPoint {
    pub id: SymbolId,
    pub vector: Vec<f32>,
    pub payload: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchFilter {
    /// Normalized cross-backend filter.
    pub must: Vec<FilterClause>,
}

#[derive(Clone, Debug)]
pub enum FilterClause {
    Eq { key: String, value: Value },
    Any { key: String, values: Vec<Value> },
    // extend carefully; keep intersection with Qdrant + Lance expressible set
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub id: SymbolId,
    pub score: f32, // similarity in [0,1] after normalization policy
    pub payload: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub vector: Vec<f32>,
    pub filter: SearchFilter,
    pub limit: usize,
    pub score_threshold: Option<f32>,
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, points: Vec<VectorPoint>) -> Result<(), StoreError>;
    async fn delete(&self, ids: &[SymbolId]) -> Result<(), StoreError>;
    async fn search(&self, req: SearchRequest) -> Result<Vec<SearchHit>, StoreError>;
    async fn get(&self, ids: &[SymbolId]) -> Result<Vec<VectorPoint>, StoreError>;
    async fn count(&self, filter: Option<SearchFilter>) -> Result<u64, StoreError>;

    /// Best-effort durability barrier.
    async fn flush(&self) -> Result<(), StoreError>;

    /// Compaction / segment merge / index rebuild.
    async fn compact(&self) -> Result<(), StoreError>;

    /// Backend capability flags for UI/feature gating.
    fn capabilities(&self) -> StoreCapabilities;
}

#[derive(Clone, Debug)]
pub struct StoreCapabilities {
    pub filtered_search: bool,
    pub disk_resident: bool,
    pub exact_count: bool,
    pub model_id: &'static str,
    pub backend: &'static str, // "qdrant-remote" | "qdrant-edge" | "lancedb"
}
```

### 6.3 Mapping notes

| Trait method | Qdrant remote | Qdrant Edge | LanceDB |
|---|---|---|---|
| `upsert` | `upsert_points` | `UpdateOperation::upsert_points` | `add` / merge |
| `delete` | `delete_points` | point delete ops | `delete` with predicate |
| `search` | `search_points` / query API | `EdgeShard::query` | `search().where()` |
| `compact` | often automatic | **`optimize()` must be called** | `create_index` / optimize |
| `flush` | wait for client ack | `flush` / drop | dataset commit |

### 6.4 Where the abstraction leaks

| Leak | Mitigation |
|---|---|
| **Filter language** | Compile `SearchFilter` → Qdrant `Filter` *and* → Lance SQL; reject unsupported clauses early |
| **Score semantics** | Document distance metric; convert distance→similarity consistently (cosine: `score = 1 - dist` or use Qdrant’s similarity mode); never compare raw scores across models |
| **Consistency** | Remote: eventual/segment optimizers; Edge: sync after `update` + explicit `optimize`; expose `compact` in idle worker |
| **IDs** | Qdrant allows u64 or UUID; map `SymbolId` string → UUID v5 (namespace nudox) for both backends |
| **Batch size limits** | Chunk upserts (e.g. 256–1024) |
| **Beta Edge API thrash** | Pin crate version; isolate in `nudox-vector-edge` crate so upgrades are one PR |
| **Threading** | Edge may not be `Sync` across arbitrary concurrent writes — use a single-writer actor |

### 6.5 Hybrid routing (local + remote)

```text
query(q):
  if offline or prefer_local:
    return local.search(embed(q))
  if project fully indexed remotely and network ok:
    return remote.search(embed(q))  # same model_id
  else:
    return merge_rrf(local.search(...), remote.search(...))  # optional; same model only
```

For v1 prefer **simple routing** (local project scope → local; org-wide → remote), not RRF fusion, to avoid ranking bugs.

---

## 7. Sidecar vs embedded — final comparison for shipping

| Criterion | Edge embedded | Sidecar Qdrant | Lance embedded |
|---|---|---|---|
| UX | Invisible | Port/process failures | Invisible |
| Ops in desktop | Lowest | Highest | Low |
| Parity with remote filters | **Best** | Best (same server code) | Medium |
| Memory control | Explicit on_disk | Server defaults need tuning | Strong disk design |
| Risk | Beta API | Process mgmt bugs | Index refresh discipline |
| **Ship decision** | **Primary** | Fallback flag `NUDOX_QDRANT_SIDECAR=1` | Secondary impl |

---

## 8. Worked recommendation for GPUI REGISTRY

### 8.1 Architecture diagram (logical)

```text
┌──────────────────────────────────────────────────────────┐
│ GPUI App                                                 │
│  ┌────────────┐   ┌──────────────┐   ┌────────────────┐ │
│  │ Symbol Δ   │──▶│ Embed Worker │──▶│ VectorStore    │ │
│  │ (hash)     │   │ fastembed+ort│   │ trait          │ │
│  └────────────┘   └──────────────┘   │  ├ EdgeLocal   │ │
│                                      │  └ RemoteQdrant│ │
│  ┌────────────┐                      └───────┬────────┘ │
│  │ Search UI  │◀──── scored SymbolIds ───────┘          │
│  └────────────┘                                         │
└──────────────────────────────────────────────────────────┘
         │ snapshots / optional sync
         ▼
   Qdrant Cloud / INDEX (same model_id collections)
```

### 8.2 Implementation phases

1. **Trait + remote adapter** (thin wrap of existing `qdrant-client` 1.18 usage).
2. **Edge local adapter** with one shard per project under `~/Library/Application Support/nudox/…` (macOS) / XDG on Linux.
3. **fastembed Jina code** embed pipeline + content-hash skip.
4. **Payload indexes** + filter UI (language/kind).
5. **Quantization + on_disk** when project crosses ~50k symbols.
6. **Idle `compact()`** scheduler.
7. **(Optional)** Lance feature-flag backend if Edge beta blocks release.
8. **(Optional)** Sidecar emergency path.

### 8.3 What not to do

- Do not embed **nomic-embed-code 7B** in the default desktop install.
- Do not use **different models** for local vs remote primary collections.
- Do not rely on **post-filter only** HNSW (usearch/hnsw_rs) without measuring filter selectivity.
- Do not keep **all dependency symbols** local forever — tier to remote INDEX.
- Do not call Edge `optimize()` on every keystroke upsert.

---

## 9. Decision matrix (scored)

Scoring 1–5 (5 best) for nudox weights: filter parity (×2), incr. delete (×2), disk RAM (×2), maintenance (×1), rust embeddability (×1), build/binary cost (×1).

| Candidate | Filter×2 | Delete×2 | Disk×2 | Maint | Embed | Binary | **Total /45** |
|---|---|---|---|---|---|---|---|
| **qdrant-edge** | 10 | 10 | 10 | 4 (beta) | 5 | 4 | **43** |
| lancedb | 8 | 8 | 10 | 5 | 5 | 3 | **39** |
| usearch + SQLite payload | 6 | 8 | 9 | 5 | 5 | 5 | **38** |
| arroy + payload | 6 | 8 | 9 | 4 | 5 | 4 | **36** |
| sqlite-vec alone | 8 | 10 | 8 | 3 (pre-1) | 5 | 5 | **39*** |
| hnsw_rs alone | 4 | 4 | 5 | 3 | 5 | 5 | **26** |
| instant-distance | 2 | 2 | 2 | 1 | 5 | 5 | **17** |
| qdrant sidecar | 10 | 10 | 7 | 5 | 3 | 2 | **37** |

\*sqlite-vec scores well on paper for small N but **fails interactive latency / ANN quality at 1e6** — cap its role.

**Winner: qdrant-edge.**  
**Runner-up: lancedb.**  
**Composable alt: usearch + sqlite metadata** if product standardizes on SQLite for all local state.

---

## 10. Open questions and risks

1. **Edge beta stability:** Will 0.x break shard on-disk format? Need migration tests and version field in shard dir.
2. **Concurrent access:** Multi-window GPUI — one writer actor per shard mandatory?
3. **SymbolId mapping:** UUID v5 vs string IDs in Qdrant — pick one before shipping data.
4. **Metal EP maturity with ort 2.0 rc:** Validate on M-series before promising GPU speedups.
5. **License distribution of ONNX Runtime + model weights** in signed macOS app — legal pass.
6. **Dependency index size:** How many transitive symbols land local vs remote? Product policy needed.
7. **Meilisearch Hannoy:** If we later want full-text+vector hybrid local, re-evaluate Meilisearch-as-library vs Tantivy (task 10) + Edge.
8. **Delete tombstone bloat:** Edge `optimize` thresholds must be tuned (`deleted_threshold`).
9. **Score calibration** between Edge and remote for identical data — A/B test before mixing results in UI.
10. **Windows** later? All primary crates claim support; still need CI.

---

## 11. Source index (inline URLs already cited; key anchors)

| Topic | URL |
|---|---|
| Qdrant Edge docs | https://qdrant.tech/documentation/edge/ |
| Qdrant Edge quickstart | https://qdrant.tech/documentation/edge/edge-quickstart/ |
| Qdrant Edge blog | https://qdrant.tech/blog/qdrant-edge-on-device-vector-search/ |
| qdrant-edge crate | https://crates.io/crates/qdrant-edge |
| Rust Edge discussion | https://github.com/orgs/qdrant/discussions/8190 |
| qdrant-client 1.18 | https://crates.io/crates/qdrant-client |
| LanceDB crate | https://crates.io/crates/lancedb |
| LanceDB vector indexes | https://docs.lancedb.com/indexing/vector-index |
| usearch | https://crates.io/crates/usearch · https://github.com/unum-cloud/USearch |
| arroy | https://crates.io/crates/arroy · https://github.com/meilisearch/arroy |
| arroy filtered ANN | https://www.meilisearch.com/blog/arroy-filtered-disk-ann |
| Meilisearch Hannoy note | https://www.meilisearch.com/blog/March-2026-updates |
| hnsw_rs | https://crates.io/crates/hnsw_rs |
| instant-distance (stale) | https://crates.io/crates/instant-distance |
| sqlite-vec | https://github.com/asg017/sqlite-vec · https://crates.io/crates/sqlite-vec |
| fastembed-rs | https://github.com/Anush008/fastembed-rs · https://crates.io/crates/fastembed |
| ort | https://crates.io/crates/ort |
| candle-core | https://crates.io/crates/candle-core |
| jina-embeddings-v2-base-code | https://jina.ai/models/jina-embeddings-v2-base-code/ |
| CodeRankEmbed | https://huggingface.co/nomic-ai/CodeRankEmbed |
| nomic-embed-code | https://www.nomic.ai/news/introducing-state-of-the-art-nomic-embed-code |
| Qdrant filtering reference | https://qdrant.tech/documentation/search/filtering/ |
| Qdrant quantization | https://qdrant.tech/documentation/manage-data/quantization/ |

---

## 12. Executive summary (document-local)

nudox needs an **embedded, disk-resident vector store** inside a Rust GPUI desktop app that remains API-compatible with the existing **remote Qdrant INDEX** (`qdrant-client` 1.18). As of **2026-07-16**, that problem has a clear primary answer: **Qdrant Edge** (`qdrant-edge` **0.7.2**, Apache-2.0) — the same engine as server Qdrant, linked **in-process**, with **on-disk vectors/payloads**, **WAL**, **full payload filters**, **upsert/delete**, **quantization**, and **snapshot sync** to a Qdrant server. Sidecar Qdrant is demoted to an emergency fallback. Among non-Qdrant embeds, **LanceDB 0.31.0** is the best full-featured alternative (disk IVF/HNSW family) at the cost of filter/index-management divergence; **usearch** and **arroy** are excellent ANN kernels if metadata lives in SQLite/LMDB; **sqlite-vec** is pre-v1 and brute-force-oriented (fine small-N, not 1e6 ANN); **instant-distance** is abandoned.

Local embeddings should use **`jina-embeddings-v2-base-code` (768-d, Apache-2.0)** through **fastembed-rs + ort**, with optional evaluation of **CodeRankEmbed** if we invest in ONNX export. **Remote and local must share the same model id, dimension, and metric**; dual-index only as an explicit second collection. Storage default: **one Edge shard per project**, scalar/binary quantization as N grows, **&lt;500 MB** RSS via on_disk settings, and idle `optimize()`. Abstract behind `trait VectorStore` with Qdrant-shaped filters as the intersection language.

**Ship Edge + Jina-code + same-model policy.** Keep Lance behind a feature flag. Measure Edge beta format stability before GA.

---

---

## 13. Operational playbook (implementation-ready)

### 13.1 Suggested on-disk layout (per project)

```text
$DATA_ROOT/projects/<project_id>/
  vectors/
    edge-shard/           # EdgeShard path (WAL + segments)
      # managed by qdrant-edge
    MODEL_ID              # text file: jina-embeddings-v2-base-code@<sha>
    schema.json           # payload field docs + dim + distance
  embed-cache/
    content_hash → optional raw vector blobs if needed for rebuild
  state.sqlite            # optional: symbol→hash map for incremental (task 11)
```

`$DATA_ROOT` examples:

- macOS: `~/Library/Application Support/nudox/`
- Linux: `$XDG_DATA_HOME/nudox/` or `~/.local/share/nudox/`

### 13.2 Edge shard init (conceptual Rust)

```rust
// Pin exact version in Cargo.toml:
// qdrant-edge = "=0.7.2"

use qdrant_edge::*;
use std::path::Path;

fn open_project_shard(dir: &Path, dim: usize) -> Result<EdgeShard, Box<dyn std::error::Error>> {
    if dir.join("meta.json").exists() || dir.read_dir()?.next().is_some() {
        // Prefer load; pass wal_options if tuning needed
        return Ok(EdgeShard::load(dir, None)?);
    }
    let config = EdgeConfigBuilder::new()
        .on_disk_payload(true)
        .vector(
            "code",
            EdgeVectorParamsBuilder::new(dim, Distance::Cosine)
                .on_disk(true)
                .build(),
        )
        // Optional: scalar quantization when N is large
        // .quantization_config(...)
        .optimizers(EdgeOptimizersConfig {
            deleted_threshold: Some(0.2),
            vacuum_min_vector_number: Some(1000),
            default_segment_number: Some(2),
            ..Default::default()
        })
        .build();
    let shard = EdgeShard::new(dir, config)?;
    // Create payload indexes BEFORE bulk ingest when possible
    shard.update(UpdateOperation::FieldIndexOperation(
        FieldIndexOperations::CreateIndex(CreateIndex {
            field_name: "language".try_into().unwrap(),
            field_schema: Some(PayloadFieldSchema::FieldType(PayloadSchemaType::Keyword)),
        }),
    ))?;
    // package, kind similarly...
    Ok(shard)
}
```

(Exact type names should be verified against `docs.rs/qdrant-edge/0.7.2` at implementation time — beta APIs shift.)

### 13.3 Incremental re-embed algorithm

```text
on_file_change(paths):
  symbols = parse_affected(paths)
  for s in symbols:
    h = blake3(s.embedding_text())
    if h == stored_hash(s.id): continue
    if s.deleted:
      store.delete([s.id])
      clear_hash(s.id)
    else:
      v = embedder.embed(s.embedding_text())  # batch these
      store.upsert([{id, v, payload}])
      set_hash(s.id, h)
  maybe_schedule_compact()  # if delete ratio high or N updates > threshold
```

**Embedding text recipe (v1 proposal):**

```text
// language: rust
// package: serde
// kind: function
// path: src/lib.rs
// name: deserialize
/// doc comment...
fn deserialize(...) { ... }   // body truncated to token budget
```

Keep recipe **byte-identical** on server INDEX workers.

### 13.4 Compact / optimize policy

| Trigger | Action |
|---|---|
| App idle ≥ 30s after ≥ 5k upserts | `compact()` |
| Deleted fraction estimate ≥ 20% | `compact()` |
| Project open (cold) | optional light compact if last compact &gt; 7 days |
| User “Rebuild semantic index” | full re-embed + new shard dir swap |

Never compact on the UI thread. Show non-modal progress for rebuilds &gt; ~10s.

### 13.5 Failure modes and mitigations

| Failure | Symptom | Mitigation |
|---|---|---|
| Corrupt Edge directory | load fails | Atomic dir swap: write `edge-shard.new`, fsync, rename; keep `edge-shard.bak` |
| Model file missing | embed fails | Download with checksum; offline mode disables semantic search gracefully |
| Dimension mismatch | upsert panic/err | Guard in trait; refuse with actionable error |
| RAM spike during first index | OS jetsam / swap thrash | Smaller batches; on_disk from day 1; limit rayon threads |
| Edge beta format change | can’t load after upgrade | Detect `schema.json` format_version; migrate or rebuild |
| Port conflict (sidecar fallback) | can’t bind 6333 | Use ephemeral port file |
| Partial write crash | missing vectors | WAL recovery on load; re-walk content hashes vs store count |

### 13.6 Binary size / build cost expectations

| Component | Expected impact |
|---|---|
| `qdrant-edge` | Medium-large native code (shared with Qdrant core); blog claims ~11 MB install footprint |
| `ort` + ONNX Runtime | **Largest** — consider dynamic link or feature-gate “semantic search” optional install |
| `fastembed` tokenizers | Moderate |
| `lancedb` (if enabled) | Heavy Arrow stack — keep **feature-flagged** |
| `usearch` | Small |
| `sqlite-vec` | Tiny C amalgamation |

**Product suggestion:** ship semantic search as default on desktop but allow “lite” build without ONNX for CI/dev shells.

### 13.7 Test plan (before locking the choice)

1. **Parity test:** 10k synthetic symbols → Edge local + Qdrant server same vectors → top-10 Jaccard ≥ 0.9 under same HNSW params / no quant.
2. **Filter test:** language=rust AND kind=function must not return other languages (n=50k mixed).
3. **Delete test:** delete 10% IDs; search must not return them after optimize.
4. **Crash test:** kill -9 mid-upsert; reload; no panic; eventual consistency via hash reconcile.
5. **Memory test:** 500k × 768 on_disk + scalar quant; RSS of store process/thread &lt; 500 MB (Instruments / heaptrack).
6. **Latency test:** p95 search &lt; 20 ms local on M-series / modern Linux laptop for 200k vectors, filtered.
7. **Embed throughput:** symbols/sec on CPU for Jina-code; gate first-index UX copy on measurement.

### 13.8 Interaction with sibling research tracks

| Track | Interaction |
|---|---|
| **10-tantivy** | Keyword/BM25 local; Edge also supports sparse/BM25 — prefer **one** hybrid owner to avoid double indexes |
| **11-sqlite-index** | Natural home for content_hash, symbol catalog; sqlite-vec only if ANN stays small |
| **08-incremental** | Drives which symbols re-embed; VectorStore is the sink |
| **13-storage** | Project directory layout and packing |
| **14-client-sync** | Snapshot push/pull of Edge shards vs re-embed from source |
| **03/15-gui** | Progress UX for first index; offline badges |

### 13.9 Anti-recommendations (explicit)

1. **Chroma-in-process for Rust GPUI** — wrong language boundary.
2. **FAISS via C++ FFI** — build hell on macOS/Linux dual target; usearch/Edge already cover the need.
3. **Per-language embedding models** — fragments the index; one multilingual code model is enough for v1.
4. **Storing raw f32 always in RSS “for speed”** — breaks the 500 MB budget past ~1e5–2e5 dims@768.
5. **Silent model upgrade** — always version collections.

---

## 14. Comparison deep-dives (selected)

### 14.1 Why Edge wins over Lance for *this* codebase

nudox already invested in **Qdrant filter semantics and operational knowledge** (`qdrant-client` 1.18 on server + registry). Edge preserves:

- Filter AST familiarity
- Payload index concepts
- Quantization docs shared with server
- Snapshot sync patterns documented at [edge synchronization guide](https://qdrant.tech/documentation/edge/edge-synchronization-guide/)

Lance would force a **second cognitive model** (Lance SQL filters, manual IVF rebuild in OSS, different score tuning). Lance remains the right pick if:

- We abandon Qdrant remote entirely, or
- We need multi-modal columnar blobs co-located with vectors as first-class, or
- Edge beta blocks a release train

### 14.2 Why not “just usearch”

usearch is blazingly maintained (2.26.0 days before this research) and mmap-friendly. It still requires:

- External payload store + filter→ID set compilation
- Custom ID management
- Custom durability story
- Custom sync story to remote Qdrant

That is a **mini vector DB project**. Edge is that project, already done.

### 14.3 sqlite-vec honest scaling

Brute-force distance at dim=768:

- 1e5 vectors ≈ 1e5 * 768 FLOPs per query ≈ 7.7e7 FLOPs — fine on modern CPU with SIMD (low ms–tens of ms)
- 1e6 vectors ≈ 7.7e8 FLOPs — still possible but competes with UI; worse under concurrent embeds

Quantized int8 helps bandwidth; **does not** add graph-based sublinear search. For nudox upper bound (deps-heavy monorepos), plan on **ANN**, not sqlite-vec alone.

### 14.4 Meilisearch arroy → Hannoy signal

March 2026 Meilisearch product notes describe **HNSW-backed store (Hannoy)** as stabilized and legacy experimental vector store removed. Implications:

- arroy crate remains useful LMDB ANN
- Strategic trajectory of its primary consumer is **toward HNSW**, not deeper Annoy-trees
- Betting the company on arroy-as-core is weaker than Edge/Lance in 2026

---

## 15. Remote INDEX compatibility matrix

| Concern | Local Edge | Remote Qdrant 1.18 |
|---|---|---|
| Point IDs | UUID/num | UUID/num — **align** |
| Named vectors | `"code"` | `"code"` — **align** |
| Distance | Cosine | Cosine — **align** |
| Payload keys | language, package, kind | same — **align** |
| Quantization | optional local-only | optional server-only — OK if search paths separate |
| HNSW params m/ef | Edge defaults | Server collection config — **may diverge scores slightly** |
| Hybrid sparse | Edge BM25 optional | Server sparse optional — enable both or neither for hybrid |

**Score divergence under different quant/HNSW params is expected.** UI should not present local and remote hits in one list without labeling source or using RRF with care.

---

## 16. Final checklist for architecture doc consumers

- [x] Qdrant embedded feasibility settled (Edge yes; sidecar fallback)
- [x] Candidates evaluated with 2026 versions
- [x] Winner + runner-up named
- [x] Embedding model + runtime named
- [x] Local/remote compatibility policy stated
- [x] Storage layout + RAM strategy
- [x] `VectorStore` trait sketch + leak notes
- [x] Risks/open questions listed
- [x] URLs inline for verification

---

## 17. One-page decision card (paste into ADRs)

```text
ADR: Local vector search for GPUI REGISTRY
Date: 2026-07-16
Status: Proposed

Decision:
  Use qdrant-edge (>=0.7.2, pin exact) as the embedded VectorStore.
  Use qdrant-client 1.18 for remote INDEX (existing).
  Embed with jina-embeddings-v2-base-code via fastembed/ort.
  Same model_id locally and remotely for primary collections.
  One Edge shard per project; on_disk vectors+payload; scalar quant at scale.
  Abstract behind trait VectorStore; feature-flag lancedb as escape hatch.
  Sidecar Qdrant only if Edge blocked.

Consequences:
  + Filter/upsert parity with server
  + No process supervisor
  - Depend on Edge beta; pin + isolate crate
  - Ship ONNX Runtime (size)
```

---

---

## 18. Edge confirmation (surrounding survey)

**Status:** 2026-07-16 confirmation from [edge-tech/05-surrounding-edge](../../edge-tech/05-surrounding-edge/PLAN.md). Does not reopen §17 ADR.

**Unified map:** [edge-tech/00-DECISIONS.md](../../edge-tech/00-DECISIONS.md)

| Item | Decision |
|---|---|
| **Local VectorStore** | **Stay `qdrant-edge`** (plan winner; pin ≥0.7.2) |
| **Remote** | **Stay `qdrant-client` 1.18** INDEX |
| **Runner-up** | LanceDB remains escape hatch only (feature-flag) |
| **USearch as full store** | **Reject** (ANN kernel only if ever needed) |
| **sqlite-vec as primary** | **Reject** for production ANN scale; micro-watch only |
| **Turbopuffer / SPFresh / new SaaS stack** | **Reject** as desktop embed; no second remote vector product for v1 |
| **New full vector stacks from 05** | **Reject** — do not open a parallel plane |

**Crate impact:** none — [22-crate-topology](../22-crate-topology/PLAN.md) `vector-local` / `vector-remote` unchanged.

---

*End of report 09-vector — research date 2026-07-16.*
