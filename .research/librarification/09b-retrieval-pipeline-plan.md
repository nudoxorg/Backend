# 09b — Optimal Retrieval Pipeline: IR-Native Dual-Tier Vectors

**Research date:** 2026-07-16 (deepened same day)  
**Status:** Proposed plan (extends `09-vector.md` + master GD-4 / §12 + `08-incremental.md`)  
**Scope:** Choose among bi-encoders, hybrid sparse, multi-representation named vectors, Matryoshka, late interaction (ColBERT), and commercial code models — then lock a pipeline that fits **nudox’s IR + tree-sitter + moniker/part-hash architecture**, with **high-quality local** and **highest-quality remote** under **desktop and INDEX memory bounds**.

> **Compatibility:** This plan does **not** replace qdrant-edge / qdrant-client. It specifies *what* we embed, *which* models/vectors live where, and *how* search stages compose. Store choice remains GD-4.  
> **Sibling docs:** store selection → `09-vector.md`; incremental spine → `08-incremental.md`; identity → §6 / RFC-19; assembly → `_master/plan-part3.md` §12.  
> **Deep sections:** §16 incrementality composition · §17 memory-saving mechanisms · §18 HNSW & rerank · §19 multimodal IR · §20 local GPU · §21 glossary.

---

## 0. Executive decision

| Tier | Role | Model / technique | Storage |
|---|---|---|---|
| **L0 Local primary** | Offline / private / low-latency semantic search | **Bi-encoder:** `jinaai/jina-embeddings-v2-base-code` (768-d, Cosine) via fastembed+ort | Edge: named dense `"sym"` + scalar quant + on_disk |
| **L0 Local lexical** | Exact-ish identifiers / error strings / rare tokens | **Tantivy BM25** (`IdentTokenizer`) — already planned §11 | Not in Qdrant by default (single hybrid owner) |
| **L1 Local optional fields** | Intent-split: “find similar signatures” vs “find similar implementations” | **Named multi-rep** dense vectors `"sig"` / `"body"` (same model, different IR text) — Phase 2 | Edge named vectors; same 768-d |
| **R0 Remote parity** | Syncable with Edge; DepSet search before local Ready | **Same Jina model + same recipe** as L0 | Host collection `symbols__jina_v2_code_768` |
| **R1 Remote premium** | Highest code-retrieval quality | **`voyage-code-3`** (default 1024-d MRL; int8/binary quant) | Separate host collection `symbols__voyage_code3_1024` — **never fused into local scores** |
| **R2 Remote rerank (optional)** | Precision@k when user/agent pays latency | **ColBERT multivector MaxSim** (or Voyage/Cohere cross-encoder later) on top-K from R0/R1 | Multivector field, **HNSW m=0**, used only as Stage-2 |
| **Not in v1 desktop** | — | Full BGE-M3 multi+dense (too heavy / not code-first), SigLIP (no multimodal IR), nomic-embed-code 7B | — |

**One-line product rule:**  
Local = **IR-structured bi-encoder + Tantivy hybrid**. Remote = **same bi-encoder for parity** + **Voyage code premium** + **optional late-interaction rerank**. Storage always **quantized / on_disk** at scale; ColBERT never first-stage indexes every token under HNSW.

---

## 1. Why “document chunking” thinking is the wrong default here

Most 2026 retrieval literature optimizes **RAG over prose chunks**. Nudox is different:

| RAG-default unit | Nudox unit |
|---|---|
| Arbitrary ~512–2k token chunks | **`ir::Entry` / symbol** (function, record, trait, module, …) |
| One blob of text | **Part-structured** material: `sig` / `body` / `doc` / `ref` (+ moniker, kind, path) |
| Re-chunk on file edit | **Part-hash early cutoff** (`embed_key`) — re-embed only changed facets |
| Occurrences = noise | **OccurrenceSet** is the graph/reference SoT; embeddings consume *definitions*, not every use-site |

Tree-sitter + IR give us **already-normalized structure**:

- **Signature material** → `sig_hash` / pretty signature from IR (`Function`, `Record`, …)
- **Body material** → tree-sitter token stream already defined for `body_hash` (comments stripped, identifiers kept)
- **Docs** → `doc_hash` canonicalization
- **Identity** → `LineageMoniker` + kind (never put version in embed text for lineage continuity; package pin lives in payload)

**Implication:** The highest-leverage “bleeding edge” for us is **multi-representation named vectors over IR fields**, not first shipping token-level ColBERT for every symbol. ColBERT still wins as **remote Stage-2** when a single pooled vector is too lossy (long bodies, natural-language → code).

---

## 2. Route catalog — fit for nudox

### 2.1 Bi-encoders (single dense vector)

| Model | Dim | Context | Local? | Code-specialized | Verdict |
|---|---|---|---|---|---|
| **jina-embeddings-v2-base-code** | 768 | 8K | Yes (fastembed, Apache-2.0) | Yes | **Local + remote parity primary** |
| **CodeRankEmbed** | ~768 | 8K | Yes (MIT; ONNX export needed) | Yes (strong CoRNStack) | Phase-1.5 bake-off vs Jina; winner becomes L0 |
| **BGE-M3** | 1024 dense + sparse + multi | 8K | Heavy (~568M) | General, not code-first | **Server-only hybrid source if we ever drop Tantivy sparse**; not desktop default |
| **voyage-code-3** | 2048/1024/512/256 MRL | large | API only | Yes (SOTA code) | **Remote premium R1** |
| **voyage-3.5** | MRL 256–2048 | 32K class | API only | General/tech, not code-specific | Secondary for docs packages; code path prefers voyage-code-3 |
| **Cohere embed-v4** | MRL + native int8/binary | large | API only | Multilingual enterprise | Optional R1 alt for non-code tenants; not primary |
| **nomic-embed-code 7B** | high | long | No (desktop) | Yes | INDEX GPU-only science project; not product path |

**Policy:** Bi-encoder remains Stage-1 everywhere. Quality differentiation is **which** bi-encoder and **what text** it sees — not abandoning bi-encoders.

### 2.2 Hybrid sparse (BM25 / learned sparse)

| Approach | Local | Remote | Notes |
|---|---|---|---|
| **Tantivy BM25** over symbol docs | Yes (planned) | Yes | **Owns keyword** for v1 — IdentTokenizer, language-aware identifiers |
| **Qdrant sparse BM25** (Edge + server) | Possible | Possible | Duplicate of Tantivy unless we drop one; Edge demo uses sparse well |
| **BGE-M3 sparse / SPLADE** | Costly | Strong | Only if Tantivy is insufficient for NL queries |

**Decision:** SearchPlanner hybrid = **dense (Qdrant) ⊕ BM25 (Tantivy)** via RRF. Do **not** also store BM25 sparse in Qdrant for v1 (double index). Revisit Edge sparse only if we ship a “vectors-only lite” build without Tantivy.

### 2.3 Multi-representation (named vectors, not ColBERT)

Qdrant named vectors let one point hold several dense embeddings with independent HNSW.

| Named vector | IR source | Query intent |
|---|---|---|
| `sym` | Full recipe (header + moniker + sig + doc + body window) | Default semantic search |
| `sig` | Signature-only recipe (kind, name, params, returns) | “APIs like this”, type-shape search |
| `body` | Body token window only | Implementation similarity / clone-ish |

**Why this beats one fused blob:** a docs-only edit changes `doc_hash` → re-embed `sym` only if recipe includes docs; a signature change re-embeds `sig`+`sym` but not necessarily `body` if body tokens unchanged. Maps 1:1 onto part-hash fan-out (GD-28).

**Phase:** v1 ships **`sym` only**. Phase 2 adds `sig`/`body` when eval shows intent split is worth storage (~2–3× vectors).

### 2.4 Late interaction (ColBERT / MaxSim)

| Property | Reality for nudox |
|---|---|
| Quality | Excellent soft keyword+context; great NL→code |
| Qdrant | Native multivector + `MAX_SIM`; **disable HNSW (`m=0`)** for multivectors — use as **rerank only** |
| Storage | ~tokens × 128-d per symbol body → **10²–10³×** denser than bi-encoder |
| Edge | Technically possible; **not default** under 500 MB budget at 10⁵–10⁶ symbols |
| Fit | **Remote Stage-2** on top-K (e.g. 50–100) from R0/R1; optional “deep search” mode |

**Anti-pattern:** Indexing ColBERT multivectors under HNSW for first-stage retrieval at registry scale.

### 2.5 Matryoshka (MRL)

| Model | MRL | Use |
|---|---|---|
| voyage-code-3 / voyage-3.5 / Cohere v4 | Yes | **Remote:** store 1024 (or 512) int8 for ANN; keep full float on disk or re-score top-N with full dims if API returns both |
| jina-v2-code | Not a first-class MRL product story | Use **scalar/binary quant in Qdrant**, not ad-hoc truncation, unless we measure truncation retention |

**Remote shortlist+rerank (MRL-style):**  
Index 512-d or 1024-d quantized for HNSW → prefetch top 100 → rescore with full 2048 float if stored (Voyage supports requesting dims). Do **not** invent MRL for Jina by blind truncation without a quality gate.

### 2.6 Quantization (storage at the edge and INDEX)

Qdrant (server **and** Edge as of Edge docs 2026) supports Scalar, Product, Binary, and **TurboQuant** (v1.18+; random-rotation then compress). Original vectors remain on disk for rescoring when quant is enabled.

| Technique | Compression | Local Edge (Jina 768) | Host Jina | Host Voyage |
|---|---|---|---|---|
| **on_disk** vectors+payload | RAM → working set | **Always** desktop default | Cold tiers | Cold tiers |
| **Scalar int8** | ~4× | **Default N ≥ ~50k**; rescoring optional | **Default** | Or API-native int8 ingest |
| **TurboQuant bits4** | ~8× | **Eval candidate** (Qdrant now prefers over scalar when not L1); measure on code corpus | Eval at INDEX scale | Eval |
| **Binary 1-bit** | ~32× | **Avoid** for 768-d unless oversampling+rescore proven | Only with rescore | Strong (quant-aware model); rescore top-K |
| **Binary 1.5/2-bit** | 24×/16× | Maybe later | Maybe | Prefer over 1-bit for mid dims |
| **Product quant** | up to 64× | No (slow distance, quality risk) | Last resort | Last resort |
| **MRL dim truncate** | linear in dim | N/A for Jina product path | N/A | **Yes** — store 1024 (or 512) of 2048 |

**Recommended placement modes (Qdrant “memory modes”):**

1. **Desktop default:** original on disk + quantized in RAM (`on_disk=true`, quant `always_ram=true`) — best latency/RAM for GUI.  
2. **Tiny project:** all f32 on disk, no quant until N crosses threshold.  
3. **INDEX huge:** quant in RAM, HNSW on disk or quant-only hot path with oversampling.

**Budget target (local):** vector-search RSS **&lt;500 MB** excluding ONNX weights; unload embed model when idle.  
**Full policy:** §17.

### 2.7 Vision-language (SigLIP 2) / multimodal

Out of scope until **multimodal IR** exists (definition §19). No mixing image/text collections until monikers can name non-code artifacts.

---

## 3. IR → embed surface (the real quality lever)

### 3.1 `EmbedTextBuilder` (deterministic, versioned)

```text
recipe_id = "nudox.embedtext.v2"

// --- header (always) ---
// language: {lang}
// package: {package_stem}          // version-stripped for stability
// kind: {kind}                     // fn | struct | trait | ...
// moniker: {lineage_moniker_scip}
// path: {relative_path}
// name: {unqualified_name}

// --- signature block (sig facet) ---
{normalized_signature_pretty}

// --- documentation block (doc facet) ---
{canonical_doc_text}                // empty if none

// --- body block (body facet) ---
{tree_sitter_token_window}          // body_hash-compatible normalizer;
                                    // truncate to token budget (default 512–1024 tokens)
```

**Rules:**

1. **Byte-identical** on desktop EmbedStage and INDEX workers for the same `recipe_id` + inputs.
2. Uses **part-hash normalizers** from §6.3 so `body` text agrees with `body_hash` identity (no dual “pretty for humans / different for embed”).
3. Does **not** include branch names, absolute machine paths, or generation ids in the embed text (those are payload).
4. Optional **query-side prefixes** if the model family needs them (e.g. some E5-style models); Jina-code and voyage-code use model-documented input formatting — pin in `tool_digest`.

### 3.2 Facet recipes (multi-rep)

| Vector name | Includes | Re-embed when |
|---|---|---|
| `sym` | header + sig + doc + body window | any of sig/doc/body/moniker/kind changes |
| `sig` | header + sig only | sig / moniker / kind |
| `body` | header-lite + body window | body_hash |

### 3.3 Frozen `embed_key` (replaces vague GD-28 key)

```text
embed_key(vector_name) =
  BLAKE3(
    "nudox.embed_key.v1",
    recipe_id,
    model_id,           // e.g. jina-embeddings-v2-base-code@sha
    vector_name,        // "sym" | "sig" | "body"
    dim,                // 768 | 1024 | ...
    metric,             // cosine
    sig_hash | ∅,       // included if recipe uses sig
    body_hash | ∅,
    doc_hash | ∅,
    moniker_bytes,
    kind_tag
  )
```

- CAS stores raw dense vector blobs under `embed_key` → cluster-wide dedup for public packages.
- Lineage embed-reuse (GD-7) still requires edge confidence ≥ 0.95 **and** matching `embed_key` inputs (identical hashes).
- Model bump → new `model_id` → global invalidation by design (`tool_digest`).

### 3.4 What we do **not** embed as primary points

| Artifact | Role in retrieval |
|---|---|
| Every `Occurrence` reference span | Graph / “find references”; not a vector row (would explode N and pollute ANN) |
| Whole files as default | Optional **file-level** secondary points later for “module overview” queries |
| Dirty working tree | Never durable (GD-15 / GD-28) |

---

## 4. End-to-end pipeline architecture

```text
                         ┌─────────────────────────────────────┐
  git sealed gen         │  compiler-core / generate           │
  ─────────────────────► │  IR Index + OccurrenceSet + hashes  │
                         └──────────────┬──────────────────────┘
                                        │ SymbolDelta (part hashes)
                                        ▼
                         ┌─────────────────────────────────────┐
                         │  EmbedTextBuilder (recipe v2)       │
                         │  embed_key per named vector         │
                         └──────────────┬──────────────────────┘
                                        │
              ┌─────────────────────────┼─────────────────────────┐
              ▼                         ▼                         ▼
     ┌────────────────┐      ┌────────────────────┐     ┌──────────────────┐
     │ Local Embed    │      │ INDEX Embed workers│     │ CAS embed cache  │
     │ Worker         │      │                    │     │ by embed_key     │
     │ jina + ort     │      │ jina (R0)          │     └──────────────────┘
     │ SemanticGate   │      │ voyage-code-3 (R1) │
     └───────┬────────┘      │ optional ColBERT   │
             │               └─────────┬──────────┘
             ▼                         ▼
     ┌────────────────┐      ┌────────────────────┐
     │ Edge VectorStore│      │ Host Qdrant        │
     │ collection/shard│      │ coll Jina (parity) │
     │ named: sym [sig]│      │ coll Voyage (prem) │
     │ quant + on_disk │      │ optional colbert MV│
     └───────┬────────┘      └─────────┬──────────┘
             │                         │
             └────────────┬────────────┘
                          ▼
               ┌──────────────────────┐
               │ SearchPlanner        │
               │ 1. dense ANN         │
               │ 2. Tantivy BM25      │
               │ 3. RRF fuse          │
               │ 4. optional R2 rerank│
               │ 5. DepSet / payload  │
               └──────────────────────┘
```

### 4.1 Write path (EmbedStage) — SymbolDelta only

EmbedStage is a pure **§7 Stage** consumer of `SymbolDelta` (`08-incremental.md` §8.3). It never walks the full project. Full algorithm and part-hash matrix: **§16**.

```text
# Only runs after generation seal (GD-15). Dirty WT → no durable embeds.
for each symbol in delta.removed:
  store.delete(symbol_id); clear stage_traces / payload keys

for each symbol in delta.added ∪ delta.changed:   # NOT unchanged
  for each vector_name in active_set:               # v1: {sym}
    k = embed_key(vector_name, parts, model, recipe)
    # L1: stage_traces hit (stage_id, input_digest=k, tool_digest) → skip model
    # L2: CAS has vector blob for k → upsert from CAS without re-infer
    # L3: lineage reuse if conf ≥ 0.95 and same k (GD-7)
    if need_infer:
      text = EmbedTextBuilder.build(symbol, vector_name)
      vec  = embedder.embed(text)                   # only changed symbols
      CAS.put(k, vec); traces.put(...)
    store.upsert(Point { id: uuid_v5(symbol_id), vectors, payload… })

maybe_schedule_compact()   # idle; never per-symbol optimize
```

**Remote R1:** same loop with `voyage-code-3` → separate collection; `embed_key` includes that model_id. INDEX workers also only process package-level SymbolDelta for sealed generations.

### 4.2 Query path

```text
query(q, scope, quality_mode):

  // Lexical
  bm25 = tantivy.search(q, filter=scope)           # always cheap

  // Dense Stage-1
  if quality_mode == Local || offline:
      qv = local_jina.embed(q)
      dense = edge.search(qv, using="sym", filter=scope)
  else if quality_mode == Parity || !premium_enabled:
      qv = remote_jina.embed(q)                   # or local jina if same model
      dense = host.search(coll_jina, qv, filter=scope)
  else: # Premium
      qv = voyage.embed(q, input_type=query, dim=1024)
      dense = host.search(coll_voyage, qv, filter=scope)

  fused = RRF(dense, bm25)

  // Optional Stage-2 late interaction (remote, quality_mode == Deep)
  if deep && remote_ok:
      fused = colbert_maxsim_rerank(q, fused.top(100))

  return fused.top(k) with source labels
```

**Routing defaults (align GD-4 Routed):**

| Condition | Dense source |
|---|---|
| Offline / prefer_local / local Ready | Edge Jina |
| Online, local not Ready | Host Jina (parity) |
| Online, user/org “Best quality” | Host Voyage |
| Cross-org public package search | Host Jina or Voyage; never require Edge |

Never RRF-fuse Jina scores with Voyage scores into one unlabeled list.

### 4.3 Edge ↔ host (unchanged store topology, refined data)

| Shard | Contents | Sync |
|---|---|---|
| **Mutable Edge** | Trusted project `sym` (Jina) | Local-only by default |
| **Immutable Edge** | Hot deps `sym` (Jina) | Partial snapshots from **Jina host collection only** |
| **Host Jina** | All INDEX packages | Authoritative for untrusted deps |
| **Host Voyage** | Premium INDEX packages | No Edge mirror (model mismatch) |

Partial snapshots only make sense for the **parity model**. Premium Voyage stays query-remote.

---

## 5. Collection schemas

### 5.1 Local Edge (per project)

```text
vectors:
  sym:  size=768, distance=Cosine, on_disk=true
  # phase 2:
  # sig:  size=768, distance=Cosine, on_disk=true
  # body: size=768, distance=Cosine, on_disk=true
quantization: scalar int8 (enable when N ≥ ~50k)
payload indexes: language, package, kind  (keyword)
payload fields: symbol_id, moniker, content_hash, generation_id,
                model_id, recipe_id, embed_key_sym, ...
```

### 5.2 Host — parity (Jina)

```text
collection: symbols__jina_v2_code_768
vectors: sym 768 Cosine
hnsw: payload_m=16, m=0          # per-tenant graphs when filtering by package
payload: package_id is_tenant=true, language, kind, moniker, generation_id, ...
sharding: custom optional; hot packages → dedicated shards (tiered multitenancy)
quantization: scalar int8 default; binary experiment at multi-hundred-M points
```

### 5.3 Host — premium (Voyage)

```text
collection: symbols__voyage_code3_1024
vectors: sym size=1024 Cosine     # MRL; optional second named full_2048 on disk only
quantization: int8 or binary (model is quant-aware)
same multitenancy payload as Jina
# NO Edge snapshot sync
```

### 5.4 Host — optional ColBERT rerank field (same points or side collection)

```text
# Preferred: side-by-side named multivector on premium or parity points
colbert: size=128, multivector MAX_SIM, hnsw m=0, on_disk=true
# Query: prefetch dense limit=100 → query using=colbert
```

Only materialize ColBERT for symbols whose body token count exceeds a threshold (e.g. ≥ 64 tokens) to control storage.

---

## 6. Memory & cost model

### 6.1 Local (1e5 symbols, `sym` only, 768-d)

| Representation | Approx size |
|---|---|
| f32 raw | 1e5 × 768 × 4 ≈ **300 MB** |
| int8 scalar | ≈ **75 MB** vectors |
| HNSW graph | tens of MB (depends on m/ef) |
| ONNX Jina weights | ~150–300 MB (unload when idle) |
| ColBERT for all | **multi-GB** — reject as default |

**Policy:** on_disk + int8 by default above 50k; hold project + **hot deps only** on Edge.

### 6.2 Host public INDEX (1e7 symbols, order-of-magnitude)

| Config | Ballpark vectors storage |
|---|---|
| Jina 768 f32 | ~30 GB |
| Jina 768 int8 | ~7.5 GB |
| Voyage 1024 int8 | ~10 GB |
| Voyage 1024 binary | ~1.25 GB (+ rescore quality tradeoff) |
| ColBERT all symbols | often **prohibitive** → thresholded subset or top packages only |

### 6.3 Embed compute

| Path | Cost profile |
|---|---|
| Local Jina CPU | ~5–40 ms/symbol; batch 32–64; first index minutes for 50k |
| Local Metal EP | 2–5× when validated |
| Remote Voyage API | $$$ at full registry rebuild; mitigated by **CAS embed_key** + sealed-only deltas |
| ColBERT | Stage-2 only on ≤100 candidates |

---

## 7. SearchPlanner integration (with existing planes)

| Plane | Owner | Stage |
|---|---|---|
| Exact / identifier | Tantivy | always |
| Semantic dense | Qdrant Edge or Host | Stage-1 |
| Graph / “who implements” | Terminus hot + graph-cold IR | parallel, not RRF’d blindly |
| Fusion | SearchPlanner RRF(dense, bm25) | default |
| Rerank | ColBERT MaxSim or cross-encoder | Deep mode |
| Scope | DepSet + payload filters | **must** apply on every ANN query |

UI labels results: `local` | `index-jina` | `index-voyage` | `hybrid`.

---

## 8. What we reject (with reasons)

| Idea | Why not (for us) |
|---|---|
| Voyage as local primary | No offline, no open weights, no Edge parity snapshots |
| BGE-M3 as desktop default | 568M + not code-specialized; sparse overlaps Tantivy |
| ColBERT first-stage HNSW | RAM/insert disaster at 10⁶ symbols |
| One global undivided HNSW over all packages | Noisy; use `is_tenant` + DepSet filters |
| Mixing Jina and Voyage scores | Invalid geometry without learned fusion |
| Embedding every occurrence | N explodes; wrong unit |
| Whole-file Voyage 32k as default unit | Breaks symbol identity, lineage, incremental part-hashes |
| Silent model upgrades | Always new collection + dual-read cutover |

---

## 9. Phased delivery (slots into existing S12 / waves)

### Phase A — MVP (align existing S12.1–S12.5)

1. `EmbedTextBuilder` v2 + frozen `embed_key`  
2. Local Jina `sym` → Edge; remote Jina `sym` → host  
3. Tantivy hybrid RRF in SearchPlanner  
4. Payload filters + package multitenancy on host  
5. Parity harness (same vectors → Jaccard@10 ≥ 0.9 Edge vs host)  
6. Scalar quant + on_disk thresholds  

*Acceptance:* offline semantic search works; **one-symbol doc edit → embedder invocations == 1 and Edge upserts == 1** (§16.4); unchanged symbols never enter the worker.

### Phase B — Remote premium

1. Host collection Voyage `sym` 1024 + int8  
2. Quality mode in client (`parity` | `premium`)  
3. CAS cache for Voyage keys  
4. Cost dashboards (tokens / package / day)  

*Acceptance:* human eval set (code NL queries) shows premium ≥ parity on nDCG; never mixed unlabeled.

### Phase C — Multi-rep named vectors

1. Add `sig` / `body` named vectors (same Jina)  
2. Intent routing: signature-shaped queries → prefetch `sig`  
3. Part-hash-driven partial re-embed  

*Acceptance:* signature-only rename doesn’t re-embed `body`; eval improves on “find similar API” tasks.

### Phase D — Late interaction (optional Deep mode)

1. ColBERT multivector on host for long-body symbols  
2. Prefetch dense → MaxSim rerank  
3. Gate behind SemanticGate + latency budget  

*Acceptance:* Deep mode p95 &lt; 300 ms on warm INDEX for k=10 after dense prefetch; storage growth &lt; X% of package corpus.

### Phase E — Bake-offs / escape hatches

1. CodeRankEmbed vs Jina local A/B  
2. Binary quant on Voyage at scale  
3. Lance feature flag remains GD-4 escape hatch  

---

## 10. Crate / type surface (implementation map)

| Component | Crate | Notes |
|---|---|---|
| `EmbedTextBuilder` | `moniker` or `vector-local` | Pure; no Qdrant |
| `embed_key` hashing | `moniker` / heart ContentHash | Domain-separated tags |
| `Embedder` trait brands | `vector-local` / registry runtime | Extend catalog: `JinaCodeV2`, `VoyageCode3` |
| `QdrantEdgeLocal` | `vector-local` | Named vectors `sym`… |
| `QdrantRemote` | `vector-remote` | Multi-collection router |
| `SearchPlanner` | server/client-core | RRF + quality mode |
| `SemanticGate` | existing | CPU + optional API budget |
| Tantivy path | `text-search` | Unchanged owner of BM25 |

Wire protocol: **never** expose collection names or Qdrant filters; expose `quality_mode` + opaque search API only (GD-9).

---

## 11. Evaluation plan (gate each phase)

| Suite | Measures |
|---|---|
| **Fixture parity** | Edge vs host Jina top-10 Jaccard |
| **NL→code** | Hand-labeled queries over Rust/TS/Go fixtures |
| **Identifier hard cases** | Rare symbol names → BM25 must surface; dense must not drown them after RRF |
| **Incremental correctness** | Part-hash matrix: doc-only / sig-only / body-only edits |
| **Memory** | 200k × 768 on_disk int8 RSS &lt; 500 MB (store only) |
| **Latency** | Local p95 search &lt; 20 ms @ 200k; premium remote p95 &lt; 150 ms network+ANN |
| **Lineage T6** | Embed assist scores calibrated on residual pairs (thresholds in §6) |

---

## 12. Decision matrix (scored for *this* architecture)

Weights: code quality (×3), local offline (×3), IR/part-hash fit (×3), memory (×2), ops simplicity (×2), remote SOTA (×2).

| Route | Code | Offline | IR fit | Mem | Ops | Remote SOTA | Notes |
|---|---|---|---|---|---|---|---|
| Jina bi-encoder + Tantivy hybrid | ●●● | ●●● | ●●● | ●●● | ●●● | ●● | **Core path** |
| IR multi-rep named vectors | ●●● | ●●● | ●●●● | ●● | ●● | ●● | Phase C force-multiplier |
| Voyage-code-3 premium | ●●●● | ○ | ●●● | ●●● (quant) | ●● | ●●●● | R1 only |
| BGE-M3 all-in-one | ●● | ● | ●● | ● | ●● | ●●● | Redundant with Tantivy; heavy |
| ColBERT Stage-2 | ●●●● | ○/● | ●● | ● | ● | ●●●● | Deep mode only |
| ColBERT Stage-1 | ●●●● | ○ | ● | ○ | ○ | ●●●● | Reject |
| MRL truncate Jina blindly | ● | ●●● | ●● | ●●●● | ●●● | ● | Need evidence first |
| Voyage MRL+quant | ●●●● | ○ | ●●● | ●●●● | ●● | ●●●● | R1 storage strategy |
| SigLIP multimodal | ○ | ○ | ○ | ●● | ● | ●● | Future |

**Winner composition:** rows 1 + 3 + (2) + (5 as optional) — not a single model.

---

## 13. ADR-ready summary

```text
ADR: IR-native dual-tier retrieval pipeline
Date: 2026-07-16
Status: Proposed
Supersedes: single-vector “embed the symbol string” vagueness in §12
Extends: 09-vector.md (store), GD-4, GD-28, §6 part hashes

Decision:
  1. Primary unit of embedding = sealed IR symbol, not file chunks or occurrences.
  2. Deterministic EmbedTextBuilder v2 + embed_key from part hashes + model id.
  3. Local Stage-1: jina-embeddings-v2-base-code (768) on qdrant-edge; hybrid with Tantivy.
  4. Remote Stage-1 parity: same Jina collection (Edge sync / Routed).
  5. Remote Stage-1 premium: voyage-code-3 (1024 MRL, int8/binary) separate collection.
  6. Stage-2 optional: ColBERT MaxSim multivector (HNSW off) for Deep mode only.
  7. Phase 2: named multi-rep vectors sig/body aligned to part-hash fan-out.
  8. Quantization + on_disk mandatory at scale; ColBERT never default first-stage.

Consequences:
  + Highest practical local quality under offline + RAM constraints
  + Highest remote quality without poisoning local geometry
  + Incremental re-embed correctness via part hashes
  - Two remote collections + quality_mode UX
  - Voyage cost must be controlled via CAS + sealed deltas
  - ColBERT storage limited to thresholded subset
```

---

## 14. Open questions (bounded)

1. **CodeRankEmbed vs Jina** on our fixture languages — run Phase E before freezing L0 forever.  
2. **Voyage dim default:** 1024 vs 512 for INDEX cost — decide after one package-scale cost model.  
3. **Whether Deep ColBERT is product-visible** or agent-only.  
4. **File-level secondary points** for “explain this module” — separate recipe, not v1.  
5. **Edge multi-vector support parity** with server for Phase C — verify on pinned qdrant-edge version before coding multi-rep local.

---

## 15. Source anchors

| Topic | URL |
|---|---|
| Qdrant multivector / MaxSim | https://qdrant.tech/documentation/tutorials-search-engineering/using-multivector-representations/ |
| Qdrant vectors (named, multi, sparse) | https://qdrant.tech/documentation/manage-data/vectors/ |
| Qdrant quantization (scalar / binary / TurboQuant) | https://qdrant.tech/documentation/manage-data/quantization/ |
| Qdrant multitenancy / is_tenant | https://qdrant.tech/documentation/manage-data/multitenancy/ |
| Qdrant Edge docs (named vectors, quant, schema mutate) | https://qdrant.tech/documentation/edge/ |
| Qdrant Edge sync | https://qdrant.tech/documentation/edge/edge-synchronization-guide/ |
| voyage-code-3 MRL + quant | https://blog.voyageai.com/2024/12/04/voyage-code-3/ |
| ONNX Runtime CoreML EP | https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html |
| HNSW paper | https://arxiv.org/abs/1603.09320 |
| jina-embeddings-v2-base-code | https://jina.ai/models/jina-embeddings-v2-base-code/ |
| Prior store decision | `.research/librarification/09-vector.md` |
| Incremental spine | `.research/librarification/08-incremental.md` |
| Part hashes / embed_key | `.research/librarification/_master/plan-part2.md` §6.3 |
| IR / occurrences | `workspace/ir/` |

---

## 16. Incrementality composition (fully qualified)

### 16.1 Goal statement (acceptance)

> **Local durable embeddings are produced only for symbols in `SymbolDelta.added ∪ SymbolDelta.changed` on a sealed generation.** Unchanged symbols never enter the embedder. Dirty working trees never write durable vectors (GD-15 / GD-28).

This is not aspirational — it is the same contract as Tantivy and Terminus fan-out in `08-incremental.md` §8.3.

### 16.2 Layers of early cutoff (all must hold)

```text
L0  CommitGate: no generation → no EmbedStage
L1  Package dirtiness: only dirty packages re-produce IR
L2  SymbolDelta: only added/removed/changed symbol ids
L3  Part hashes: EmbedStage input_digest uses embed_key (facets), not whole content_hash only
L4  stage_traces: (stage_id, input_digest, tool_digest) → skip run()
L5  CAS: same embed_key → reuse vector blob (cross-package on INDEX)
L6  Lineage: conf ≥ 0.95 + same embed_key → copy without re-infer (GD-7)
```

**Critical refinement vs naive `08` sketch:** hashing only `content_hash` as embed input_digest is **too coarse** once we have multi-rep vectors and recipe versioning. Prefer:

```text
input_digest = embed_key(vector_name)   # §3.3
tool_digest  = BLAKE3(model_id, recipe_id, ort_build, dim, metric, quant_policy_id?)
stage_id     = "embed.v2.sym" | "embed.v2.sig" | "embed.v2.body" | "embed.v2.voyage_sym"
```

Quantization policy is a **store** property, not part of `tool_digest` for the raw vector blob (raw f32 in CAS; store may quantize on upsert). If we ever store only quantized bytes in CAS, include quant id in the key.

### 16.3 SymbolPartHashes alignment

`08-incremental.md` currently sketches `SymbolPartHashes { sig, body, refs }`. Master §6.3 freezes **also** `doc` and `embed_key`. For EmbedStage partial invalidation, heads must carry:

```rust
pub struct SymbolPartHashes {
    pub sig: ContentHash,    // sig-v1
    pub body: ContentHash,   // body-v1  (tree-sitter token normalizer)
    pub doc: ContentHash,    // doc-v1
    pub refs: ContentHash,   // ref-v1  (graph; not usually in embed_key)
    // embed_key is derived, not stored as independent producer output —
    // but may be cached on the point payload for skip without recompute
}
```

**Fan-out matrix (local, v1 `sym` only):**

| Change in parts | Tantivy | Embed `sym` | Graph (ref) |
|---|---|---|---|
| doc only | reindex text | **re-embed** | skip |
| body only | reindex | **re-embed** | skip if refs same |
| sig only | reindex | **re-embed** | maybe |
| refs only | maybe | **skip** (if recipe ignores refs) | rewrite |
| moniker/kind | reindex | **re-embed** | rewrite |

**Phase C multi-rep:**

| Change | `sig` vec | `body` vec | `sym` vec |
|---|---|---|---|
| doc only | skip | skip | re-embed |
| body only | skip | re-embed | re-embed |
| sig only | re-embed | skip | re-embed |

### 16.4 Worked example (one-symbol doc edit)

```text
Gen N-1:  foo::bar  doc_hash=D0 body=B sig=S  → embed_key_sym=K0  vector V0 in Edge
User edits doc comment only, commits.
Gen N:    foo::bar  doc_hash=D1 body=B sig=S  → embed_key_sym=K1 ≠ K0

SymbolDelta.changed = [foo::bar] with old_parts/new_parts
EmbedStage:
  removed: none
  for foo::bar, vector sym:
    traces miss on K1
    CAS miss on K1
    run embedder once → V1
    upsert point (same PointId, new vector + payload embed_key=K1)
  optimize: not yet (below threshold)

All other symbols: not in delta → zero embed work
```

**Acceptance test (S7.6 / S12.4):** assert embedder invocation count == 1 and Edge upsert count == 1 for this scenario.

### 16.5 Deletes and renames

| Event | Vector action |
|---|---|
| Symbol removed | `delete(point_id)`; clear traces for old keys (or leave orphan traces — GC later) |
| Moniker rename, same body/sig/doc | New moniker in recipe → new `embed_key` → re-embed; old point id is **stable SymbolId** (uuid v5 of identity), not moniker string — point id stable, vector content changes |
| Lineage Identical (T0) across gens | Same embed_key → stage_traces/CAS hit → **no infer**; optional no-op upsert |
| Mass reformat (body_hash storm) | Rate-limit / progress UI; still correct, expensive by definition |

Point IDs: **UUID v5(namespace_nudox, symbol_id)** stable across re-embeds (09-vector §6.4). Payload holds moniker for filters/display.

### 16.6 What is *not* in the local incremental path

| Work | Why not “re-embed all local symbols” |
|---|---|
| Opening a project | Load Edge shard from disk; no embed |
| Switching branches to same commit | Same generation id → no-op |
| Pulling hot deps | Immutable Edge **snapshot** of precomputed Jina vectors; no local Voyage; no local re-embed of crates.io |
| Query-time | Embed **one query string** only (not corpus) |
| Premium Voyage | INDEX-only; never blocks local delta path |
| ColBERT materialization | Host optional; not required for local Ready |

### 16.7 Interaction with `optimize()` / compaction

Edge has **no background optimizers** — app calls `optimize()` / trait `compact()`.

| Trigger | Relation to incrementality |
|---|---|
| After ≥5k upserts + idle 30s | Amortized; not per-symbol |
| Deleted fraction ≥ ~20% | Tombstone GC |
| Never on each EmbedStage item | Would destroy incremental UX |

Compaction does **not** re-run the embedder; it only merges segments / drops deleted points.

### 16.8 INDEX (remote) incrementality

Same SymbolDelta semantics for package generations published to INDEX:

- Workers embed only changed symbols for sealed package versions.  
- CAS `embed_key` → cluster-wide dedup (Blackbird-style): two packages with identical normalized symbol text + model share one blob.  
- Voyage and Jina are **separate** keys (`model_id` in embed_key).  
- Host multitenant collection upserts are point-id stable; payload `generation_id` / package version for GC of old package versions (product policy).

### 16.9 Failure / crash composition

| Failure | Recovery |
|---|---|
| Kill -9 mid-embed batch | Generation stays `running` or partial traces; re-seal re-runs missing traces only (idempotent PRIMARY KEY) |
| Embedder OOM | Stage fails symbol batch; do not seal; retry with smaller batch |
| Edge WAL recovery | Points durable after flush; reconcile count vs symbol_heads on open |
| Model file missing | Offline semantic disabled; do not mark corpus Ready |

### 16.10 Explicit non-goals for v1 incrementality

- Live embed of dirty buffer (preview gens are separate TTL space if ever productized).  
- Incremental ColBERT token-matrix updates on desktop.  
- Re-embedding because quant config changed (re-quant from stored f32 / CAS, no model).

---

## 17. Memory-saving mechanisms — decisions

### 17.1 Two different “memory” problems

| Problem | What grows | Fix |
|---|---|---|
| **A. Index RAM** | HNSW + hot vectors for N symbols | on_disk, quant, tenant graphs, hot-deps only |
| **B. Embedder RAM** | ONNX weights ~150–300 MB for Jina-class | load on demand; unload idle; one session |
| **C. Disk** | f32 originals + WAL + snapshots | acceptable; cheaper than RAM |
| **D. Multivector** | ColBERT matrices | never default local Stage-1 |

A and B must be budgeted **separately**. The 500 MB target is **A** (vector search RSS), not A+B.

### 17.2 Mechanism-by-mechanism (qualified)

#### on_disk (vectors + payload)

- **What:** mmap / disk-primary storage; RSS ≈ graph + working set, not full f32 corpus.  
- **Edge:** first-class `on_disk(true)` in `EdgeVectorParams` / `on_disk_payload`.  
- **Decision:** **Always on for desktop Edge** from day one (even small projects — avoids RAM cliffs as deps grow).  
- **Incrementality:** orthogonal; upserts still point-level.

#### Scalar quantization (int8)

- **What:** f32 → uint8 per dimension; ~4× vector memory; SIMD-friendly; Qdrant reports typically **&lt;1%** quality loss.  
- **Rescore:** usually optional for scalar (defaults differ from binary).  
- **Decision:** enable when project+hot-deps **N ≥ ~50_000** or measured RSS &gt; budget. Below that, skip for simpler debugging.  
- **always_ram=true** for quantized vectors while originals stay on_disk — Qdrant hybrid mode.

#### TurboQuant (bits4 default, as of Qdrant 1.18)

- **What:** random rotation + multi-bit compress; up to 8× at bits4 with recall competitive with scalar in vendor benches; asymmetric (stored compressed, query full precision).  
- **Edge:** docs claim all Qdrant quant methods including TurboQuant.  
- **Decision:** **Phase E bake-off** against scalar on a code fixture before making it desktop default. If recall@10 within 1–2 pts of scalar at half the RAM, adopt bits4 as default at N ≥ 50k.  
- **Not** blocking Phase A (scalar is the known-good).

#### Binary quantization

- **What:** 1 bit/dim (~32×); best for high-dim centered embeddings (OpenAI-ada 1536, Cohere 4k class in Qdrant’s published tests).  
- **768-d Jina:** **high risk** without oversampling + rescore of originals on disk (latency hit).  
- **Voyage-code-3:** model is **quant-aware**; binary 1024 with rescoring is a supported cost knob (Voyage blog).  
- **Decision:** local Jina → **no binary v1**. Host Voyage → optional with `oversampling ≥ 2` and `rescore=true`.

#### Product quantization

- **What:** high compression, slower non-SIMD distance, larger quality loss.  
- **Decision:** reject for interactive desktop; INDEX last resort only.

#### Matryoshka truncation

- **What:** use first k dims of a trained nested embedding (Voyage: 2048⊃1024⊃512⊃256).  
- **Not** the same as “chop Jina to 256 and hope.”  
- **Decision:** Voyage INDEX stores **1024-d** (balance); 512 if cost dominates; keep ability to rescore with higher dim if stored. Local Jina stays 768 full + quant.

#### Corpus tiering (product policy, not a Qdrant knob)

- Local Edge holds **project + hot deps** only.  
- Transitive cold packages: remote INDEX query or omit.  
- This is the largest “memory save” at monorepo scale — larger than binary vs scalar.

#### Avoided “saves”

| Idea | Why not |
|---|---|
| Drop HNSW entirely (flat scan) | Fine ≤~20k; fails interactive at 1e6 |
| Shared global Edge for all projects | RAM unbounded; delete isolation worse |
| ColBERT everywhere with HNSW | Memory explosion |

### 17.3 Frozen local config ladder

```text
N < 20k:     on_disk, no quant, default HNSW
20k–50k:     on_disk, scalar optional
N ≥ 50k:     on_disk + scalar int8 (always_ram quant), rescore off by default
N ≥ 200k:    same + consider TurboQuant bits4 after eval; shrink hot-dep set
always:      unload embedder after idle T; SemanticGate concurrency cap
```

### 17.4 Host INDEX config ladder

```text
Jina parity coll:  scalar or TurboQuant bits4; package is_tenant; m=0 payload_m=16
Voyage premium:    1024-d; int8 or binary+rescore; no Edge sync
ColBERT field:     m=0, on_disk, only body_tokens ≥ threshold
```

---

## 18. HNSW and rerank (fully qualified)

### 18.1 What HNSW is

**Hierarchical Navigable Small World** (Malkov & Yashunin, 2016) is a **graph-based approximate nearest neighbor (ANN)** index. Qdrant’s default dense index is HNSW-family.

**Exact kNN:** distance(query, every vector) — O(N·d) — too slow at 10⁵–10⁶.  
**HNSW:** multi-layer proximity graph:

```text
Top layers: sparse long-range links  → fast coarse navigation
Bottom layer: dense local links      → refine neighbors
Search: greedy walk from entry point downward → ~log N steps
```

**Important parameters (conceptual):**

| Param | Role | Tradeoff |
|---|---|---|
| **m** | Max edges per node | Higher m → better recall, more RAM/build time |
| **ef_construction** | Candidate width while **building** | Higher → better graph, slower index |
| **ef** (search) | Candidate width while **querying** | Higher → better recall, slower query |

**Approximate** means top-k may miss a true neighbor; product eval uses recall@k / Jaccard vs exact on fixtures.

### 18.2 HNSW in our deployments

| Surface | HNSW use |
|---|---|
| Edge `sym` (Jina) | **On** — Stage-1 local search |
| Host Jina with `is_tenant` | Prefer **payload-local graphs**: `m=0`, `payload_m>0` so each package has its own graph (multitenancy docs) — unfiltered global search is slower (acceptable; we always filter DepSet) |
| ColBERT multivector field | **`m=0` (HNSW off)** — store matrices for MaxSim **rerank only** (Qdrant multivector tutorial) |
| Quantized search | HNSW over quantized codes; optional **rescore** with full vectors |

### 18.3 What rerank is

**Two-stage retrieval:**

```text
Stage 1 — cheap, high recall
  HNSW dense ANN  and/or  BM25
  → candidate set C (|C| = 50–200)

Stage 2 — expensive, high precision  ("rerank")
  Score only c ∈ C with a stronger model
  → final top k (10)
```

**Rerankers we may use:**

| Reranker | Input | When |
|---|---|---|
| **None** | — | Default local + parity |
| **RRF fusion** | dense list + BM25 list | Default hybrid (not a neural rerank; rank fusion) |
| **ColBERT MaxSim** | query token matrix × doc token matrix | Deep mode, remote |
| **Cross-encoder** | (query, doc) pair scores | Future alternative to ColBERT |
| **Quant rescore** | full-precision vectors on shortlist | After binary/TurboQuant aggressive modes |

Rerank does **not** re-embed the corpus. It may embed the **query** once more in a late-interaction space.

### 18.4 Why ColBERT is Stage-2 only

- Storage: ~tokens × 128-d per symbol vs one 768-d vector.  
- HNSW on every token vector is RAM/insert hostile at registry scale (Qdrant tutorial guidance).  
- MaxSim on |C|=100 is fine; MaxSim on N=10⁷ is not interactive.

### 18.5 Query path with labels (no mixed geometries)

```text
candidates = Stage1(mode)
if hybrid: candidates = RRF(candidates, bm25)
if deep && remote: candidates = ColBERT_rerank(query, candidates)
return labeled(candidates)  // source: local | index-jina | index-voyage
```

Never: average(Jina_score, Voyage_score).

---

## 19. Multimodal IR — what we mean

### 19.1 Definition

**Unimodal IR (today):** language-agnostic **code structure** — `ir::Entry` kinds, types, docs, tree-sitter-derived bodies, `OccurrenceSet` spans. All embeddable material is **text/tokens**.

**Multimodal IR (future):** the intermediate representation also names **non-text artifacts** as first-class entries with stable identity (monikers), for example:

- Architecture diagrams / images linked to modules  
- UI screenshots linked to components  
- PDFs or notebooks as package members  

Those entries would carry content hashes and could be embedded with a **vision–language** model (e.g. SigLIP 2, Jina v4 multimodal) into vectors stored beside code vectors (named vector `image` vs `sym`) for joint retrieval.

### 19.2 Why it is out of scope now

1. No IR kind / moniker grammar for assets.  
2. No producer pipeline (tree-sitter does not parse PNGs).  
3. No product requirement to search diagrams offline in v1.  
4. Mixing SigLIP and Jina spaces without a shared embedding is invalid (same as Jina vs Voyage).

### 19.3 What is *not* multimodal

- Code + docstrings + comments in one text recipe → still **unimodal text**.  
- Hybrid dense + BM25 → two **text** retrieval channels.  
- Multi-rep `sig`/`body` → multiple text views, still unimodal.

---

## 20. Local GPU / accelerator viability

### 20.1 Scope

Local embedding model is **~137M** Jina-code (or similar CodeRankEmbed), not a 7B LLM. Workloads:

| Workload | Frequency | GPU value |
|---|---|---|
| Query embed (1 string) | Every semantic search | Low — CPU already ms-class |
| Delta re-embed (1–100 symbols) | Each commit | Nice — still fine on CPU |
| Cold full index (10⁴–10⁵) | Rare | **High** — minutes → faster |
| Continuous dirty-buffer embed | Not in product | N/A |

### 20.2 Stack

```text
fastembed → ONNX model → ort (ONNX Runtime)
                         ├─ CPU EP          (required, default, CI)
                         ├─ CoreML EP       (macOS: GPU + Apple Neural Engine)
                         ├─ CUDA EP         (optional Linux NVIDIA builds)
                         └─ DirectML        (future Windows)
```

CoreML EP docs: requires recent macOS; can target CPUAndGPU / CPUAndNeuralEngine / ALL. Graph partitioning: unsupported ops fall back to CPU — **fragmented graphs can be slower than pure CPU**. Must benchmark the actual Jina ONNX export.

### 20.3 Viability assessment

| Platform | Verdict |
|---|---|
| **Apple Silicon + CoreML** | **Viable and recommended as opt-in/auto** after parity numeric check vs CPU. Expect ~2–5× on clean encoder graphs; not guaranteed until measured. |
| **CPU-only laptop** | **Fully supported product path** — never require GPU. |
| **NVIDIA Linux workstation** | Viable for power users if we ship CUDA-enabled ort; not default CI matrix. |
| **Matching Voyage locally on GPU** | **Out of scope** — Voyage is API; local GPU only accelerates open models. |

### 20.4 Product rules

1. **CPU path always correct** — golden vectors from CPU for CAS identity when `tool_digest` pins EP-agnostic ONNX (float tolerance: store f32 from one canonical EP, or accept tiny diffs only inside non-parity paths).  
2. **Canonical embed for parity CAS:** prefer **CPU EP** (or single pinned EP) when writing vectors that must match INDEX Jina workers bit-for-bit; if bit-identical is impossible across EP, define **cosine equivalence** tests instead of memcmp.  
3. **SemanticGate** budgets concurrent sessions (default 1) and respects low-power / thermal.  
4. **Unload** model after idle (e.g. 60–120s) to free 150–300 MB.  
5. Feature-gate CoreML in builds if binary size hurts.

### 20.5 Incrementality interaction

GPU does **not** change *which* symbols embed — only *how fast* `stage.run` is for symbols already in the delta. A GPU without SymbolDelta still wastes work; SymbolDelta on CPU still meets the product invariant.

---

## 21. Glossary (plan-local)

| Term | Meaning |
|---|---|
| **Bi-encoder** | Model maps text → one dense vector; query and doc encoded separately; similarity = cosine/dot |
| **Cross-encoder** | Model scores (query, doc) jointly; strong rerank, no independent doc index |
| **Late interaction / ColBERT** | Token-level vectors; MaxSim at query time |
| **MaxSim** | Σ_i max_j sim(q_i, d_j) over token vectors |
| **Named vector** | Multiple dense fields per Qdrant point (`sym`, `sig`, …) |
| **Multivector** | Matrix of vectors per field (ColBERT); not the same as named vectors |
| **HNSW** | Graph ANN index for Stage-1 dense search |
| **Rerank** | Stage-2 rescoring of a shortlist |
| **RRF** | Reciprocal Rank Fusion — merge rankings without score calibration |
| **MRL** | Matryoshka Representation Learning — nested prefix dims |
| **Quantization** | Compress vector components for RAM/speed |
| **Rescore** | Re-rank quant candidates with full-precision vectors |
| **Oversampling** | Fetch quant_top = oversampling × limit before rescore |
| **embed_key** | BLAKE3 cache key for a (symbol facet × model × recipe) vector |
| **SymbolDelta** | added/removed/changed set between sealed generations |
| **Multimodal IR** | IR that includes non-text artifacts as first-class entries (§19) |
| **Parity collection** | Host Jina index identical in geometry to local Edge |
| **Premium collection** | Host Voyage (or other) — not Edge-synced |

---

## 22. Cross-doc patches required (checklist)

| Doc | Change |
|---|---|
| `08-incremental.md` | Extend `SymbolPartHashes` with `doc`; EmbedStage `input_digest` = `embed_key`; vectors row cites this §16 |
| `_master/plan-part3.md` §12 | Cite `09b`; freeze dual-tier + embed_key + quant ladder + incrementality acceptance |
| `09-vector.md` | Point to 09b for retrieval quality; quant default = scalar + on_disk; TurboQuant eval |
| `00-ASSEMBLY-SPEC` GD-4 | Optional one-line: retrieval pipeline detailed in 09b; premium Voyage is remote-only second collection |

---

## 23. Frozen invariants (quick card)

```text
I1  Embed only SymbolDelta symbols on sealed gens (local + INDEX workers).
I2  embed_key includes model + recipe + facet hashes — skip when equal.
I3  Local primary model = open code bi-encoder (Jina v2 code unless bake-off wins).
I4  Remote parity = same model; premium = separate collection (Voyage-code-3).
I5  Never mix scores across embedding spaces without labeled UI / learned fusion.
I6  Desktop: on_disk always; scalar (≥50k); no ColBERT Stage-1; no binary Jina v1.
I7  HNSW = Stage-1; ColBERT MaxSim = optional Stage-2 with m=0.
I8  CPU embed always works; CoreML/CUDA accelerate delta/full index only.
I9  Multimodal IR deferred; text multi-rep (sig/body) is not multimodal.
I10 optimize()/compact is idle/threshold — never per-symbol with embed.
```

---

*End of 09b — optimal IR-native dual-tier retrieval plan (deepened).*
