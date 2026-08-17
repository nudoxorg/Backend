# 09b — Optimal Retrieval Pipeline: IR-Native Dual-Tier Vectors

**Research date:** 2026-07-16 (deepened same day)  
**Status:** Proposed plan (extends `09-vector.md` + master GD-4 / §12 + `08-incremental.md`)  
**Scope:** Choose among bi-encoders, hybrid sparse, multi-representation named vectors, Matryoshka, late interaction (ColBERT), and commercial code models — then lock a pipeline that fits **nudox’s IR + tree-sitter + moniker/part-hash architecture**, with **high-quality local** and **highest-quality remote** under **desktop and INDEX memory bounds**.

> **Compatibility:** This plan does **not** replace qdrant-edge / qdrant-client. It specifies *what* we embed, *which* models/vectors live where, and *how* search stages compose. Store choice remains GD-4.  
> **Sibling docs:** store selection → `09-vector.md`; incremental spine → `08-incremental.md`; identity → §6 / RFC-19; assembly → `_master/plan-part3.md` §12.  
> **Deep sections:** §16 incrementality composition · §17 memory-saving mechanisms · §18 HNSW & rerank · §19 multimodal IR · §20 local GPU (summary; **full runtime in 09c**) · §21 glossary.  
> **Adversarial + CPU/GPU runtime + library abstractions + summary plan:** **`09c-embeddings-runtime-adversarial.md`** (2026-07-17).

---

## 0. Executive decision

| Tier | Role | Model / technique | Storage |
|---|---|---|---|
| **L0 Local primary** | Offline / private / low-latency semantic search | **Bi-encoder:** `jinaai/jina-embeddings-v2-base-code` (768-d, Cosine) via fastembed+ort | Edge: named dense `"sym"` + scalar quant + on_disk |
| **L0 Local lexical** | Exact-ish identifiers / error strings / rare tokens | **Tantivy BM25** (`IdentTokenizer`) — already planned §11 | Not in Qdrant by default (single hybrid owner) |
| **L1 Local optional fields** | Intent-split: “find similar signatures” vs “find similar implementations” | **Named multi-rep** dense vectors `"sig"` / `"body"` (same model, different IR text) — Phase 2 | Edge named vectors; same 768-d |
| **R0 Remote parity** | Syncable with Edge; DepSet search before local Ready | **Same Jina model + same recipe** as L0 | Host collection `symbols__jina_v2_code_768` |
| **R1 Remote premium** | Highest code-retrieval quality | **`voyage-code-3`** (**pin `output_dimension=1024` + `input_type` explicitly** — never rely on API defaults; int8/binary quant) | Separate host collection `symbols__voyage_code3_1024` — **never fused into local scores** |
| **R2 Remote rerank (optional)** | Precision@k when user/agent pays latency | **Cross-encoder Stage-2** on top-100 from R0/R1: self-host **`mxbai-rerank-base-v2`** (Apache-2.0, code-benchmarked) · premium **Voyage `rerank-2.5`** API. **ColBERT MaxSim demoted to experimental** — no license-clean code ColBERT exists (§18.3b) | Rerank service on INDEX; ColBERT (if ever) multivector field, **HNSW m=0**, Stage-2 only |
| **Not in v1 desktop** | — | Full BGE-M3 multi+dense (too heavy / not code-first), SigLIP (no multimodal IR), nomic-embed-code 7B | — |

**One-line product rule:**  
Local = **IR-structured bi-encoder + Tantivy hybrid**. Remote = **same bi-encoder for parity** + **Voyage code premium** + **optional server-side neural rerank** (cross-encoder default; ColBERT experimental — §18.3b). Storage always **quantized / on_disk** at scale; ColBERT never first-stage indexes every token under HNSW.

**Placement:** which plane runs each workload (client vs server), the shard bakery, hot-set admission, and the frozen latency/memory budgets are owned by **09-vector §20** — this doc defines *what* is retrieved and *how well*, not *where*.

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
| **Licensing (verified 2026-07-17)** | **jina-colbert-v2 is CC-BY-NC-4.0 — forbidden in the product**; answerai-colbert-small-v1 is Apache-2.0 but 33M / English-prose / not code-tuned. **There is currently no license-clean, code-capable ColBERT** — this demotes ColBERT from “planned Stage-2” to **experimental**, and makes the cross-encoder the Deep-mode default (§18.3 / §18.3b) |

**Anti-pattern:** Indexing ColBERT multivectors under HNSW for first-stage retrieval at registry scale.

### 2.5 Matryoshka (MRL)

| Model | MRL | Use |
|---|---|---|
| voyage-code-3 / voyage-3.5 / Cohere v4 | Yes | **Remote:** store 1024 (or 512) int8 for ANN; keep full float on disk or re-score top-N with full dims if API returns both |
| jina-v2-code | Not a first-class MRL product story | Use **scalar/binary quant in Qdrant**, not ad-hoc truncation, unless we measure truncation retention |

**Remote shortlist+rerank (MRL-style):**  
Index 512-d or 1024-d quantized for HNSW → prefetch top 100 → rescore with full 2048 float if stored (Voyage supports requesting dims). Do **not** invent MRL for Jina by blind truncation without a quality gate.

**Pinning rule (I16):** the Voyage API accepts `output_dimension` ∈ {2048, 1024, 512, 256} and `output_dtype` ∈ {float, int8, uint8, binary, ubinary} — **always pass both explicitly** (we pin 1024/float for CAS blobs). The blog and API docs disagree on which dimension is “default”; code that relies on a vendor default is a latent geometry-corruption bug.

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

### 3.1b Model I/O correctness (frozen — how vectors are actually produced)

Getting the *text* right (§3.1) is half the recipe; the other half is the model I/O contract. Verified against live model cards / API docs **2026-07-17**.

| Contract | Jina v2-code (L0/R0) | voyage-code-3 (R1) |
|---|---|---|
| Pooling | **Mean pooling over the attention mask** — required by the model card. fastembed does this; a raw-`ort` escape hatch must reimplement it identically or vectors silently diverge | API-internal |
| Normalization | **L2-normalize at write and at query** (belt-and-braces for cosine even where the store normalizes) | verify normalized output |
| Query/doc asymmetry | **None** — no instruction prefixes; same encoder both sides | **`input_type="document"` on the write path, `"query"` on the query path** — Voyage prepends different prompts server-side; omitting `input_type` silently degrades retrieval |
| Dimensions | 768 from the artifact | **pin `output_dimension=1024`** on every call (I16) |
| dtype | f32 out of ONNX | pin `output_dtype="float"` for CAS blobs; store-side quant is separate |
| Context | 8192 (ALiBi) but trained at 512 — recipe budget is 1024 tokens (below); longer windows gated on eval | 32k; the same recipe budget applies for parity of unit |
| Tokenizer | HF `tokenizer.json` from the pinned repo — its sha is part of `tool_digest` | vendor-side |
| Batch limits | fastembed’s default batch is **256 — override to 32** (long code windows OOM) | ≤ 1000 texts **and** ≤ 120k tokens per request — chunk INDEX batches to both caps |
| Weights artifact | **canonical pinned `onnx/model_quantized.onnx` — 161.9 MB int8 — on both planes** (161M params; f32 = 641.5 MB, fp16 = 321 MB are gated fallbacks only; 09-vector §20.8) | n/a |

**Truncation algorithm (deterministic, frozen):**

```text
total_budget = 1024 tokens (model tokenizer, pinned by sha)
1. header + sig : kept whole. Pathological case: if sig alone > 512 tokens
                  (generated code), hard-truncate sig at 512 on a token boundary.
2. doc block    : min(tokens(doc), 256)   # documentation IS embedded — doc facet
3. body window  : all remaining budget, taken from the start of the body-hash-
                  normalized token stream, cut on a token boundary. Deterministic;
                  no sampling, no "smart" selection in v1.
```

**Trait gap in live code (fix in P0):** `Embedder` in `workspace/registry/runtime/vector/embedding.rs` carries `EmbeddingPurpose::{Code, Documentation}` but **no query-vs-document axis**. Voyage (`input_type`) and E5-family (prefixes; `E5Small` is in the live catalog) require one. Add `EmbedRole { Query, Document }` to `embed`/`embed_batch`: Jina adapters ignore it, Voyage maps it to `input_type`, E5 maps it to `"query: "`/`"passage: "` prefixes. `EmbeddingPurpose` maps to *named vectors* (Code → `sym`, Documentation → a future doc facet) — it is not the role axis, and conflating the two would bake the bug into the trait.

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

**Remote R1:** same loop with `voyage-code-3` → separate collection; `embed_key` includes that model_id; every call pins `input_type="document"`, `output_dimension=1024`, `output_dtype="float"` (§3.1b), batched under the 1000-text / 120k-token API caps. Voyage keys live server-side only (09-vector §20 R5). INDEX workers also only process package-level SymbolDelta for sealed generations.

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
  else: # Premium — runs server-side; the client ships query TEXT, never API keys
      qv = voyage.embed(q, input_type="query", output_dimension=1024)
      dense = host.search(coll_voyage, qv, filter=scope)

  fused = RRF(dense, bm25)

  // Optional Stage-2 neural rerank (remote, quality_mode == Deep) — §18.3
  if deep && remote_ok:
      fused = stage2_rerank(q, fused.top(100))   # cross-encoder service;
                                                 # ColBERT only behind experiment flag
      # client renders Stage-1 order immediately; re-ranks in place when this lands

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
| **Immutable Edge** | Hot deps `sym` (Jina) | **Server-baked Edge shards** (bakery, 09-vector §20.3); admission per §20.4; read-only after install |
| **Host Jina** | All INDEX packages | Authoritative for untrusted deps |
| **Host Voyage** | Premium INDEX packages | No Edge mirror (model mismatch) |

Dep-shard distribution only exists for the **parity model** — the bakery is the productized form of the “partial snapshot” idea. Premium Voyage stays query-remote.

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

### 5.4 Host — ColBERT rerank field (**experimental only — license-gated, §18.3b**)

```text
# ONLY behind an experiment flag, and ONLY once a license-clean code ColBERT exists.
# Preferred shape if enabled: side-by-side named multivector on premium or parity points
colbert: size=128, multivector MAX_SIM, hnsw m=0, on_disk=true
# Query: prefetch dense limit=100 → query using=colbert
```

Only materialize ColBERT for symbols whose body token count exceeds a threshold (e.g. ≥ 64 tokens) to control storage. The shipping Deep-mode Stage-2 is the **cross-encoder service** (§18.3), which needs no extra stored vectors at all — a real storage win over ColBERT.

---

## 6. Memory & cost model

### 6.1 Local (1e5 symbols, `sym` only, 768-d)

| Representation | Approx size |
|---|---|
| f32 raw | 1e5 × 768 × 4 ≈ **300 MB** |
| int8 scalar | ≈ **75 MB** vectors |
| HNSW graph | tens of MB (depends on m/ef) |
| ONNX Jina weights | **161.9 MB** canonical int8 (f32 is 641.5 MB — never ship it to desktop; 09-vector §20.8) — unload when idle |
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

### Phase D — Deep mode (server Stage-2 rerank)

1. Rerank service on INDEX: **`mxbai-rerank-base-v2`** (Apache-2.0, 0.5B, code-benchmarked) on GPU workers; input = (query, EmbedText of candidate) pairs over top-100 from Stage-1; bake-off vs `bge-reranker-v2-m3` (Apache-2.0, in fastembed, but 512-token window and not code-tuned) before freezing  
2. Premium wiring: **Voyage `rerank-2.5`** (32k context) behind the same service interface; per-org budget caps  
3. Client: **progressive display** — Stage-1 order renders immediately, list re-ranks in place when Stage-2 lands (09-vector §20.7)  
4. (Experimental, flag off) ColBERT multivector MaxSim for long-body symbols — blocked until a license-clean code ColBERT exists (§18.3b)  

*Acceptance:* Deep end-to-end p95 ≤ 1.2 s at k=100 rerank with progressive Stage-1 render ≤ local/parity budgets; measurable nDCG lift vs Stage-1 on the §11 NL→code suite; CI model-license deny-list green.

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
  6. Stage-2 optional (Deep): server cross-encoder rerank — mxbai-rerank-v2 self-host
     (Apache-2.0) / Voyage rerank-2.5 premium; ColBERT MaxSim experimental only
     (license-blocked as of 2026-07-17; HNSW off if ever enabled).
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
3. **Deep-mode surface:** product-visible or agent-only; and **watch for a license-clean, code-capable ColBERT** to revive the MaxSim experiment (today jina-colbert-v2 is CC-BY-NC and answerai-colbert-small is English-prose — §18.3b).  
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
| ORT all EPs | https://onnxruntime.ai/docs/execution-providers/ |
| fastembed-rs (EP passthrough, DirectML, Jina-code) | https://github.com/Anush008/fastembed-rs |
| 09c adversarial runtime plan | ./09c-embeddings-runtime-adversarial.md |
| HNSW paper | https://arxiv.org/abs/1603.09320 |
| jina-embeddings-v2-base-code | https://jina.ai/models/jina-embeddings-v2-base-code/ |
| Prior store decision | `docs/research/librarification/09-vector.md` |
| Incremental spine | `docs/research/librarification/08-incremental.md` |
| Part hashes / embed_key | `docs/research/librarification/_master/plan-part2.md` §6.3 |
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
| Pulling hot deps | **Baked Edge shard install** — fetch, hash-verify, unpack (09-vector §20.3); no local Voyage; the client never embeds crates.io code (09-vector R2) |
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
project shard (mutable):
  N < 20k:   on_disk, no quant, default HNSW
  20k–50k:   on_disk, scalar optional
  N ≥ 50k:   on_disk + scalar int8 (always_ram quant)
  N ≥ 200k:  same + consider TurboQuant bits4 after eval; shrink hot-dep set
dep shards (baked): ALWAYS scalar int8 (profile qp1 — 09-vector §20.3)
rescore:     ALWAYS ON for quantized shards (oversampling 2.0) — cross-shard
             merges must compare exact f32 scores (09-vector §20.6); an
             unquantized project shard needs no rescore
always:      unload embedder after idle T; SemanticGate concurrency cap
```

> **Amendment 2026-07-17:** the earlier “rescore off by default at int8” guidance is superseded by the cross-shard comparability rule above — with the project shard and baked dep shards at different quant profiles, merged rankings are only valid if every quantized shard rescores against its on-disk f32 originals (sub-ms warm on NVMe at oversampling 2.0).

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
| **Cross-encoder (server)** | (query, doc) pair scores — `mxbai-rerank-base-v2` self-host / Voyage `rerank-2.5` premium | **Deep-mode default** (Phase D) |
| **ColBERT MaxSim** | query token matrix × doc token matrix | **Experimental only** — license-blocked for product (§18.3b) |
| **Quant rescore** | full-precision vectors on shortlist | **Always on quantized shards** (§17.3; 09-vector §20.6) |

Rerank does **not** re-embed the corpus. A cross-encoder needs no stored vectors at all — it reads (query, candidate-text) pairs, so it composes with any Stage-1 and costs zero index storage.

### 18.3b Reranker / late-interaction licensing audit (verified 2026-07-17)

We ship a commercial desktop app and a commercial INDEX. **CC-BY-NC models cannot ship in either plane**, and two of the four rerankers in the fastembed catalog are CC-BY-NC — being in our chosen library’s catalog is *not* license clearance.

| Model | License | Params / notes | Verdict |
|---|---|---|---|
| `jinaai/jina-reranker-v2-base-multilingual` (in fastembed) | **CC-BY-NC-4.0** | 278M; code-capable (CodeSearchNet 71.4 MRR@10); 1024-token window | **Forbidden** — research/eval only, or paid Jina license |
| `jinaai/jina-colbert-v2` | **CC-BY-NC-4.0** | 0.6B; 128/96/64-d tokens; 8k ctx | **Forbidden** — blocks the ColBERT Stage-2 as originally planned |
| `jinaai/jina-reranker-v1-turbo-en` (in fastembed) | Apache-2.0 | English-only, prose | Clean but wrong domain |
| `BAAI/bge-reranker-v2-m3` (in fastembed) | **Apache-2.0** | 0.6B; multilingual general; ~512-token practical window; not code-tuned | **Clean fallback** — usable day one via fastembed |
| `mixedbread-ai/mxbai-rerank-base-v2` / `-large-v2` | **Apache-2.0** | 0.5B / 1.5B; code + 100+ languages, code-search benchmarked; Qwen2.5-based | **Default self-host Stage-2** — needs our own ONNX export or a server runtime (server-side only, so fine) |
| `answerdotai/answerai-colbert-small-v1` | Apache-2.0 | 33M; English prose; not code | Eval baseline only |
| Voyage `rerank-2.5` / `rerank-2.5-lite` | Commercial API | 32k context; instruction-following | **Premium Stage-2** |
| Cohere Rerank 3.5 | Commercial API | multilingual enterprise | Alternative premium; not primary |

**Enforcement (I15):** a CI deny-list of model ids (the two CC-BY-NC entries above, plus any future addition) checked against every `ModelId` in the catalog and every model the rerank service is configured to load. A human adding a model must add its license to the table above first.

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

> **Canonical deep-dive:** [`09c-embeddings-runtime-adversarial.md`](./09c-embeddings-runtime-adversarial.md) §§2–5, 8, 10.  
> This section is the **frozen product summary**; do not re-derive EP details here.

### 20.1 Scope

Local embedding model is **161M** Jina-code (HF-verified; or 137M CodeRankEmbed), not a 7B LLM. Workloads:

| Workload | Frequency | GPU value |
|---|---|---|
| Query embed (1 string) | Every semantic search | Low — CPU already ms-class |
| Delta re-embed (1–100 symbols) | Each commit | Nice — still fine on CPU |
| Cold full index (10⁴–10⁵) | Rare | **High** — minutes → faster (if CoreML/CUDA graph is clean) |
| INDEX fleet bulk | Continuous | **First-class CUDA/TRT** on workers |
| Continuous dirty-buffer embed | Not in product | N/A |

### 20.2 Stack

```text
app Embedder trait (branded EmbeddingModel)
    → fastembed 5.17.x  (default)  or raw ort Session (escape hatch)
         → ONNX model (Jina-code; sha256 pinned)
              → ort (ONNX Runtime 2.x)
                    ├─ CPU EP          (required, default, CI, **durable canonical**)
                    ├─ CoreML EP       (macOS: GPU + ANE; MLProgram + model cache dir)
                    ├─ CUDA EP         (optional Linux/Windows NVIDIA; INDEX workers)
                    ├─ TensorRT EP     (INDEX only; engine cache)
                    └─ DirectML        (future Windows GUI; fastembed feature)
```

**Libraries ranked for us (09c §3):** `fastembed` + `ort` + our `Embedder`/`EmbeddingModel` traits. Candle/Burn/llama.cpp are **not** default. Python sidecar = scripts only.

### 20.3 Viability assessment

| Platform | Verdict |
|---|---|
| **Apple Silicon + CoreML** | **Viable as opt-in/auto** after cosine-equivalence suite vs CPU; require `ModelCacheDirectory` or first-run compile pain; fragmented graphs can **lose** to pure CPU |
| **CPU-only laptop** | **Fully supported product path** — never require GPU |
| **NVIDIA Linux workstation** | Optional CUDA-enabled pack; not default CI matrix |
| **INDEX k8s GPU nodes** | CUDA default; optional TensorRT; lock EP for collection generations |
| **Matching Voyage locally on GPU** | **Out of scope** — API only |

### 20.4 Product rules (hardened 2026-07-17)

1. **CPU path always correct and complete** — no feature requires an accelerator.  
2. **I11 Durable canonical EP = CPU** for vectors written to local Edge *and* INDEX parity collections (simplest correct rule). Accelerators may speed **query** embeds or an explicit “fast rebuild” UX only if labeled non-parity; prefer not shipping hybrid durable paths until needed (full policy 09c §2.3).  
3. **I12 `tool_digest`** fingerprints model weights + recipe + **ORT package identity**, not ambient GPU presence.  
4. **SemanticGate** budgets concurrent sessions (default 1), low-power → force CPU, thermal-aware throttling on cold index.  
5. **Unload** model after idle (e.g. 60–120s) to free 150–300 MB (**separate** from 500 MB store budget).  
6. Feature-gate CoreML/CUDA in builds; default binary is **CPU-only ORT**.  
7. Quantized ONNX (int8) is a **CPU** optimization path; do not assume GPU EPs accept the same file (fastembed BGE-M3Q warning pattern).  
8. **Thread model:** one ORT session + mutex or single embed worker; never free-thread concurrent `Run` without proof.

### 20.5 Incrementality interaction

GPU does **not** change *which* symbols embed — only *how fast* `stage.run` is for symbols already in the delta. A GPU without SymbolDelta still wastes work; SymbolDelta on CPU still meets the product invariant.

### 20.6 `EmbedRuntimeInfo` (trait gap from adversarial review)

```rust
pub struct EmbedRuntimeInfo {
    pub model_id: ModelId,
    pub accel: AccelKind,        // Cpu | CoreMl | Cuda | DirectMl | Other
    pub durable_canonical: bool, // true only on CPU parity path
    pub max_batch: usize,
    pub max_seq_len: usize,
    pub weights_sha256: [u8; 32],
    pub ort_package_id: &'static str,
}
```

Expose via `Embedder::runtime()` so SemanticGate and UI can show “Accelerator: CoreML (query only)” without leaking ORT types across crates.

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
I7  HNSW = Stage-1; neural rerank = server Stage-2 (cross-encoder default;
    ColBERT experimental, m=0 if ever enabled).
I8  CPU embed always works; CoreML/CUDA accelerate bulk/query only when gated.
I9  Multimodal IR deferred; text multi-rep (sig/body) is not multimodal.
I10 optimize()/compact is idle/threshold — never per-symbol with embed.
I11 Durable parity vectors are produced on CPU EP (canonical) — see 09c.
I12 tool_digest pins model + recipe + ORT package id; not ambient GPU.
I13 The client never embeds dependency corpora — server-baked Edge shards or
    remote routing only (09-vector §20).
I14 Local store budgets enforced by hot-set admission; narrowed scope is always
    labeled in the UI, never silent.
I15 Only license-cleared models ship (CI deny-list); CC-BY-NC models
    (jina-reranker-v2, jina-colbert-v2) are forbidden in both planes (§18.3b).
I16 Vendor/API parameters are pinned explicitly — Voyage output_dimension +
    input_type, fastembed batch size, HNSW params, weights-artifact sha.
    An unpinned default is a bug.
```

**Summary ship plan (P0–P8):** **09c §0** — do not maintain a parallel phase list here; 09b Phase A–E remain the *retrieval quality* phases, 09c P0–P8 the *runtime/store implementation* sequence.

---

*End of 09b — optimal IR-native dual-tier retrieval plan (deepened).*
