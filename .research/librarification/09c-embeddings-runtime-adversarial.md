# 09c — Adversarial Review: Qdrant Edge + Embeddings Runtime (CPU / GPU / Abstractions)

**Research date:** 2026-07-17  
**Status:** Adversarial deep-dive + implementation-hardened plan  
**Extends:** `09-vector.md` (store), `09b-retrieval-pipeline-plan.md` (retrieval quality)  
**Does not reopen:** GD-4 store winner (`qdrant-edge`), dual-tier Jina/Voyage policy, SymbolDelta-only embeds  
**Verification policy:** Claims re-checked against live product docs, crates.io, ONNX Runtime EP docs, and fastembed-rs README on **2026-07-17**. Training data is not trusted for versions or EP maturity.

---

## 0. Summary plan (read this first)

### 0.1 One-page ship plan

| Layer | Decision | When | Owner crate (target) |
|---|---|---|---|
| **Store local** | `qdrant-edge` pin exact (`=0.7.2`+); one shard/project; `on_disk` always | Phase A | `vector-local` |
| **Store remote** | `qdrant-client` 1.18; parity collection + premium collection | Phase A / B | `vector-remote` |
| **Trait** | `VectorStore` + `Embedder<M: EmbeddingModel>` (already sketched in registry) **+ `EmbedRole` query/doc axis (09b §3.1b)** | Phase A | `vector-local` / heart brands |
| **Model L0** | `jinaai/jina-embeddings-v2-base-code` 768-d Cosine — **161M params; canonical artifact = `model_quantized.onnx` 161.9 MB int8, sha-pinned on both planes** (09-vector §20.8) | Phase A | fastembed catalog |
| **Model R1** | `voyage-code-3` remote-only separate collection — **pin `output_dimension=1024` + `input_type` explicitly** | Phase B | HTTP embedder (server-side keys) |
| **Placement** | Client = project + budget-admitted hot deps, Stage-1 only; server = full corpus + all neural rerank + **shard bakery**; rules R1–R8 | Phase A–C | **09-vector §20** |
| **Dep vectors** | **Never embedded on client** — server-baked Edge shards, hash-verified install; hot-set admission under `vector.local_budget_bytes` | P8 | bakery + `vector-local` |
| **Rerank** | Local = RRF (+ quant rescore, always-on for quantized shards); server Deep = **`mxbai-rerank-base-v2`** (Apache-2.0) / **Voyage `rerank-2.5`** premium; **ColBERT experimental — license-blocked** (09b §18.3b) | P9 | rerank service |
| **Runtime default** | **fastembed 5.17.x → ort 2.0** CPU EP | Phase A | `vector-local` |
| **Runtime accel** | CoreML EP (Apple Silicon) opt-in auto; CUDA EP optional Linux build | Phase A.5 | feature flags |
| **Canonical CAS vectors** | **CPU EP only** (or single pinned EP) for INDEX parity writes | Always | EmbedStage |
| **Hybrid search** | Qdrant dense ⊕ Tantivy BM25 via RRF — **not** sparse-in-Qdrant v1 | Phase A | SearchPlanner |
| **Quant** | on_disk always; scalar int8 @ N≥50k; TurboQuant bake-off later; no binary Jina | Phase A / E | Edge config |
| **GPU value** | Cold full index + bulk delta only; **query embed stays CPU-fine** | Product rule | SemanticGate |
| **Escape hatches** | LanceDB feature-flag; sidecar Qdrant; raw `ort` session if fastembed EP lag | Risks | flags |

### 0.2 Delivery sequence (hardened)

```text
P0  Trait freeze (+ EmbedRole axis, JinaCodeV2 brand) + remote adapter + parity tests (no Edge yet)
P1  Edge local adapter + payload indexes + crash/WAL tests + multi-window lock
P2  EmbedStage: EmbedTextBuilder v2 + embed_key + CPU fastembed Jina
    + canonical int8-artifact quality gate (recall@10 Δ ≤ 1 pt vs f32, else fp16)
P3  SymbolDelta wiring + content-hash skip + progress UX
P4  Hybrid RRF with Tantivy; SemanticGate concurrency caps; routing table (09-vector §20.5)
P5  Quant ladder + rescore-on-quantized-shards + idle compact(); memory ceiling harness
P6  CoreML EP path (macOS): numeric equivalence gate vs CPU
P7  Voyage premium remote collection + quality_mode UX (keys server-side)
P8  Shard bakery (INDEX) + hot-set admission + dep-shard install/evict (09-vector §20.3–20.4)
P9  Deep mode: server cross-encoder rerank service (mxbai bake-off vs bge-v2-m3;
    Voyage rerank-2.5 premium) + progressive client display
P10 Optional CUDA Linux pack; CodeRankEmbed bake-off; TurboQuant eval; ColBERT experiment
    (only if a license-clean code ColBERT appears)
```

### 0.3 Non-negotiable invariants (I1–I12)

| ID | Invariant |
|---|---|
| I1 | Embed only **sealed generation** SymbolDelta symbols (dirty trees never durable-embed). |
| I2 | `embed_key` includes model + recipe + facet hashes; equal key ⇒ skip infer. |
| I3 | Local primary = open code bi-encoder (Jina unless bake-off wins). |
| I4 | Remote parity = same model geometry; premium = **separate** collection. |
| I5 | Never fuse scores across embedding spaces (no mean(Jina, Voyage)). |
| I6 | Desktop: on_disk always; scalar ≥50k; no ColBERT Stage-1; no binary Jina v1. |
| I7 | HNSW = Stage-1; neural rerank = server Stage-2 (cross-encoder default; ColBERT experimental, `m=0` if ever). |
| I8 | **CPU embed always works**; accelerators only speed bulk work. |
| I9 | Multimodal IR deferred; multi-rep `sig`/`body` is still unimodal text. |
| I10 | `optimize()`/`compact` is idle/threshold — never per-symbol. |
| I11 | **CAS / INDEX parity vectors are produced on a single canonical EP (CPU) with the single sha-pinned weights artifact (int8 ONNX).** |
| I12 | `tool_digest` fingerprints model weights + recipe + **ort/ONNX package identity**, not ambient GPU presence. |
| I13 | The client never embeds dependency corpora — baked shards or remote route (09-vector §20). |
| I14 | Store budgets enforced by hot-set admission; narrowed scope is labeled, never silent. |
| I15 | Only license-cleared models ship — CI deny-list; CC-BY-NC (jina-reranker-v2, jina-colbert-v2) forbidden (09b §18.3b). |
| I16 | Vendor/API parameters pinned explicitly (Voyage dims/input_type, fastembed batch, HNSW, weights sha); an unpinned default is a bug. |

### 0.4 What this document adds beyond 09 / 09b

1. **Adversarial attack surface** on every major claim (store, model, runtime, GPU, traits).  
2. **Full CPU + GPU execution matrix** (ORT EPs, threading, batching, memory, packaging).  
3. **Library / abstraction survey** ranked for *our* trait shapes, not generic RAG blogs.  
4. **Failure modes, bit-identity, thermal, binary-size, and legal** constraints for a signed desktop app.  
5. **Hardened summary plan** (§0) and **implementation playbook** (§8).

---

## 1. Adversarial review of the current plan

Method: for each decision in 09 + 09b, invent the strongest counter-argument a skeptical staff eng / SRE / security reviewer would raise, then **steelman or patch**. Severity: **Blocker / High / Med / Low**.

### 1.1 Store: qdrant-edge as primary

| Attack | Severity | Steelman / patch |
|---|---|---|
| **Beta format thrash** — 0.7.x may rewrite on-disk layout; users lose indexes on upgrade | **High** | Pin exact crate; store `format_version` + model_id in `schema.json`; atomic dir swap rebuild path; CI migration fixtures per Edge minor. Do **not** claim “SQLite durability maturity” yet. |
| **In-process crash = whole GUI death** — native panic/segfault in Edge kills GPUI | **High** | Isolate Edge writes on a **dedicated OS thread / actor**; catch Rust panics at actor boundary; document sidecar fallback (`NUDOX_QDRANT_SIDECAR=1`) as real product path for “hardened” installs, not only “if beta blocks.” |
| **API ≠ qdrant-client** — dual adapters forever | **Med** | Already planned via `VectorStore`. **Patch:** freeze a *minimal* filter AST (`Eq`, `Any`, `Must`) and refuse the rest; do not chase full Qdrant filter expressiveness in v1. |
| **No background optimizers** — forgotten `optimize()` → tombstone bloat, latency death | **High** | Make compact **automatic** with telemetry counters (deleted_ratio, upserts_since_compact); not “remember to call.” Unit test that delete 30% without compact still functions but warns. |
| **Score divergence Edge vs server** under different HNSW/quant | **Med** | Never mix unlabeled lists. Parity harness: same vectors, **same HNSW params**, no quant first; then quant as separate suite. |
| **Downloads / maturity low** vs LanceDB | **Med** | Accept risk; Lance remains feature-flag. **Patch:** time-box Edge beta review (e.g. re-eval at GA or after 2 more minors). |
| **Snapshot sync to server is under-specified** | **Med** | Product may never need Edge→server snapshot (INDEX workers re-embed from IR). **Clarify:** sync path is **optional**; default remote path is **re-embed from CAS/IR**, not ship Edge shards. Avoid designing two truth sources. |
| **Multi-window concurrent writers** | **High** | Single-writer actor per shard path; multi-window → shared actor or file lock; test two windows open same project. |

**Verdict:** Winner stands. Risk is **ops + process isolation**, not ANN quality. Elevate sidecar from “emergency” to **documented isolation mode**.

### 1.2 Model: jina-embeddings-v2-base-code

| Attack | Severity | Steelman / patch |
|---|---|---|
| **Not proven SOTA on *our* languages** (Rust monikers, Go paths, TS) | **High** | Phase E bake-off vs CodeRankEmbed is **release-blocking for “best local quality” claims**, not for MVP ship. Ship Jina with measured fixture suite; swap only with versioned collection. |
| **161M is still heavy** for “lite” installs (HF-verified 2026-07-17 — earlier drafts said 137M; ONNX f32 is **641.5 MB**, fp16 321 MB, int8 **161.9 MB**) | **Med** | **Ship only the canonical int8 artifact** (09-vector §20.8) — it is also the parity artifact, so there is exactly one file to download. Feature-gate ONNX + models; download-on-first-semantic-search; never bundle f32. |
| **8K context unused** if we truncate bodies to 512–1024 tokens | **Low** | Intentional; body window is quality/cost trade. Document recipe budget in `tool_digest`. |
| **Voyage premium makes local feel “dumb”** | **Med** | UX: quality_mode labels; never silent score mix. Local wins on latency/privacy/offline. |
| **Code catalog models lag** (Qwen3-Embed via candle in fastembed) | **Low** | Escape hatch: custom ONNX via `ort` / `try_new_from_user_defined`. Do not block on newest HF models. |

**Verdict:** Model choice correct for **license + fastembed presence + size**. Quality freeze requires eval, not blog claims.

### 1.3 Same-model local/remote policy

| Attack | Severity | Steelman / patch |
|---|---|---|
| **INDEX could serve better model cheaper at scale** | **Med** | Premium collection already covers this. Parity collection is for Routed Ready + Edge identity. |
| **Bit-identical vectors impossible across EP / ORT versions** | **High** | **I11/I12:** canonical CAS writes on CPU EP; cosine-equivalence tests (max abs diff / cosine ≥ 1−ε) for GPU-accelerated **local-only** reindex paths that do not publish to INDEX. |
| **Recipe drift desktop vs worker** | **Blocker** if missed | Single shared crate for `EmbedTextBuilder`; golden text fixtures in CI both planes. |

**Verdict:** Policy is correct. The real bug is **recipe/EP non-determinism**, not dual models.

### 1.4 Retrieval: hybrid Tantivy + dense; ColBERT later

| Attack | Severity | Steelman / patch |
|---|---|---|
| **RRF without calibration** can bury good dense hits | **Med** | Cap BM25 contribution; eval identifier hard cases (09b §11). Expose “semantic only / keyword only” debug modes. |
| **Double index cost** (Tantivy + Edge) | **Med** | Accept for quality; do **not** also store Qdrant sparse BM25 (09b correct). |
| **ColBERT storage explosion** if product defaults Deep on | **High** | Deep mode **opt-in**; multivector only remote; threshold body tokens. |
| **Named multi-rep 2–3× storage** | **Med** | Phase C gated on eval lift ≥ measurable UX; not free. |

**Verdict:** Pipeline shape is sound. Gate storage-heavy phases hard.

### 1.5 Incrementality / CommitGate

| Attack | Severity | Steelman / patch |
|---|---|---|
| **Mass body_hash storms** (formatters, codegen) melt laptops | **High** | Rate-limit EmbedStage; priority queue (open file / query path first); cancel; never block UI. Thermal throttle (SemanticGate). |
| **Moniker rename re-embeds** while SymbolId stable | **Low** | Correct per 09b; ensure UI doesn’t flash “rebuilding all” for renames. |
| **Partial seal + kill -9** | **High** | Idempotent stage_traces PRIMARY KEY; reconcile on open (count vs heads). Already sketched — **implement before GA**. |

**Verdict:** Design good; **productization of throttle + reconcile** is the gap.

### 1.6 Memory budget &lt;500 MB

| Attack | Severity | Steelman / patch |
|---|---|---|
| **Users measure “Activity Monitor for nudox” = store + ONNX + UI** | **High** | Document **three budgets**: UI, vector store (&lt;500 MB), embedder (150–300 MB, unloadable). Telemetry separate gauges. |
| **Hot-deps policy undefined** → monorepo always >1M local vectors | ~~**Blocker**~~ **RESOLVED 2026-07-17** | Budget-driven hot-set admission algorithm frozen in **09-vector §20.4** (score/cost density under `vector.local_budget_bytes`; bakery-published `ram_estimate`; labeled remote routing for cold packages). |
| **HNSW graph RAM ignored in rough math** | **Med** | Benchmark real RSS; m/ef defaults matter. Prefer payload-local graphs on INDEX multitenant. |

**Verdict:** Math in 09/09b is directionally right; **hot-deps policy is the real ceiling.**

### 1.7 Trait design (`VectorStore` / `Embedder`)

| Attack | Severity | Steelman / patch |
|---|---|---|
| **async_trait + Edge sync** = spawn_blocking tax | **Med** | Dedicated store actor thread may be cleaner than per-call spawn_blocking. |
| **FilterClause too thin** for real product filters | **Med** | Start thin; add `Match`/`Range` only when UI needs them. |
| **Brand `EmbeddingModel` in registry is OpenAI/E5 today** — plan says Jina | **High** | Catalog **must** grow `JinaCodeV2` brand matching 09b; live code is ahead on *type safety*, behind on *model choice*. |
| **No `EmbedRuntime` capability surface** | **High** | See §4 — need `AccelKind`, session lifecycle, batch limits on the trait, not buried in fastembed. |

**Verdict:** Type branding is excellent. Missing **runtime capability / lifecycle** abstraction.

### 1.8 Adversarial scorecard (plan health)

| Area | Score /10 | Notes |
|---|---|---|
| Store choice (Edge) | 8 | Right engine; beta + isolation risks underweighted |
| Model choice | 8 | Right default; eval gate incomplete |
| Dual-tier remote | 9 | Strong |
| Incrementality | 9 | Design complete; ops polish needed |
| Memory story | 7 | Needs hot-deps + multi-budget telemetry |
| GPU story (pre-09c) | **4** | Thin §20; now expanded here |
| Abstraction libraries | **3** | Named fastembed/ort only; expanded §3 |
| Crash/reconcile | 7 | Sketched, not acceptance-hard |
| Packaging / legal ORT | 5 | Needs signed-app pass |
| **Overall plan robustness** | **7.5 → target 9 after P0–P6** | |

---

## 2. Embedding compute: problem geometry

### 2.1 Workloads (what actually burns cycles)

| Workload | Tokens/call | Batch | Frequency | Latency budget | Best device |
|---|---|---|---|---|---|
| **Query embed** | ~8–64 | 1 | Every semantic search | &lt;30 ms p95 | CPU (often) or warm GPU |
| **Delta re-embed** | ~128–1024 | 8–64 | Per sealed commit | Seconds OK | CPU or GPU |
| **Cold project index** | ~128–1024 | 32–256 | Rare | Minutes + progress | **GPU if available** |
| **INDEX worker bulk** | large | 64–512 | Continuous | Throughput $ | **GPU fleet** |
| **Voyage API** | n/a | vendor | Premium | Network | Remote only |

**Critical insight:** For a **161M** encoder (int8 ONNX), **single-query** latency on modern laptop CPU is already interactive. GPU wins **throughput** (cold index, INDEX), not “search feels AI-magic.” Product messaging must not promise “requires GPU.”

### 2.2 Where time goes in one embed

```text
text → tokenize (CPU, HF tokenizers) → tensors → encoder forward → pool/normalize → f32[768]
         ~5–15%                      ~~~~~~~~~~ bulk ~~~~~~~~~~     ~1%
```

GPU only accelerates the forward pass. If tokenize + H2D/D2H dominate (tiny batches), GPU **regresses**. Therefore:

- **Query batch=1:** prefer CPU unless session already warm on GPU and transfer is cheap (unified memory Apple Silicon helps).  
- **Bulk batch≥32:** GPU / ANE / CUDA typically wins when graph partitions cleanly.

### 2.3 Numerical identity vs semantic identity

| Goal | Requirement |
|---|---|
| **CAS embed_key hit** | Same *inputs* → skip model; no float issues |
| **INDEX parity publish** | Same model weights + recipe; vectors within ε of reference CPU run |
| **Local-only accelerated reindex** | Cosine(v_gpu, v_cpu) ≥ 1−ε on fixture (e.g. ε=1e-3 to 1e-2); **do not** claim bit-identical |
| **Cross-model** | Never comparable |

**Frozen policy (I11):** Any vector written to a **parity collection** or used as **cross-machine CAS blob** is produced with:

```text
EP = CPUExecutionProvider (default ORT)
intra_op threads = pinned config in tool_digest
ORT / onnxruntime build id = pinned
model file sha256 = pinned
```

Accelerated EPs may write **local Edge only** under a local `accel_note` payload field for debugging — or better: **always run CPU for durable writes**, use GPU only when user explicitly chooses “fast reindex (non-parity)” — **recommended simpler rule: durable embeds always CPU; GPU optional for “preview speed” only if product needs it.**

**Recommended product rule (simplest correct):**

> **All durable symbol vectors (local Edge + INDEX) are produced on the CPU EP.**  
> Accelerators may be used only for **experimental / rebuild-with-accept-rebuild** paths, or for **query** embedding if cosine-equivalent.

Rationale: cold index is rare; correctness &gt; 2–5× one-time speedup until measured and gated.

**Pragmatic hybrid (if cold index UX demands GPU):**

> GPU for cold index → write local Edge with `embed_ep=coreml|cuda` payload.  
> Background **CPU re-embed** to replace points for parity when idle (or mark corpus `local_only_ready` vs `parity_ready`).

Prefer the simple rule until UX research forces hybrid.

---

## 3. Libraries & abstractions (survey)

Goal: maximize **correctness under `Embedder` + `EmbeddingModel` brands**, minimize binary size / build hell, keep EP flexibility.

### 3.1 Ranked recommendations for nudox

| Rank | Library | Role | License | Use? |
|---|---|---|---|---|
| **1** | **`fastembed` 5.17.x** | Batteries-included embed + rerank; model download/cache; EP passthrough | Apache-2.0 | **Yes — default local embedder** |
| **1b** | **`ort` 2.0.x** | ONNX Runtime Rust; EP registration | Apache-2.0 | **Yes — transitive + escape hatch** |
| **2** | **Our `Embedder` / `EmbeddingModel` traits** (registry/vector-local) | Brand-safe app boundary | — | **Yes — only public surface** |
| **3** | **`tokenizers` (HF)** | Fast encoding (via fastembed) | Apache-2.0 | Transitive |
| **4** | **`qdrant-edge` / `qdrant-client`** | Store, not embed | Apache-2.0 | Yes (09) |
| **5** | **Raw `ort` Session** | Custom ONNX (CodeRankEmbed export, int8 models) | Apache-2.0 | Escape hatch crate feature |
| **6** | **`candle-core` / candle** | Pure Rust / Metal for models without ONNX (Qwen3 path in fastembed features) | Apache/MIT | Feature-gated only; not default |
| **7** | **Burn (+ wgpu/CUDA)** | Framework w/ ONNX→Rust codegen; multi-backend | Apache/MIT | **Watch only** — poor fit for stock HF embed ONNX today |
| **8** | **Python fastembed / sentence-transformers sidecar** | Dev parity scripts | — | Tests/scripts only |
| **Reject** | FAISS FFI as embed runner | Wrong layer | — | No |
| **Reject** | llama.cpp for a 161M encoder | Possible but awkward vs ONNX | MIT | Only if bench crushes ORT on Apple (unlikely default) |
| **Reject** | torchtch / libtorch in GUI | Binary size / ABI hell | — | No |

### 3.2 fastembed-rs — deep fit analysis

**Sources:** [github.com/Anush008/fastembed-rs](https://github.com/Anush008/fastembed-rs) · crates.io `fastembed` **5.17.3** (2026-07-15)

**Strengths for us:**

- Catalog already includes **`jinaai/jina-embeddings-v2-base-code`**.  
- Sync API (no forced Tokio) — good for worker threads.  
- Uses **ort** + **HF tokenizers**.  
- `with_execution_providers(...)` for ORT EPs.  
- **DirectML** feature documented for Windows GPU.  
- Quantized model variants for several families (`…Q` enums).  
- Rerankers present — **but audit licenses: 2 of the 4 catalog rerankers (jina-reranker-v2-base-multilingual, and jina-colbert-v2 outside the catalog) are CC-BY-NC and must never ship (09b §18.3b); `bge-reranker-v2-m3` is the Apache-clean in-catalog option.**  
- Sparse + BGE-M3 joint (dense/sparse/colbert) available if we ever experiment — **not** default.  
- Cache dirs: `FASTEMBED_CACHE_DIR` / `HF_HOME`.  
- User-defined models via path APIs.

**Weaknesses / traps:**

| Trap | Mitigation |
|---|---|
| Sync `embed` on GUI thread freezes app | Always call from Embed worker pool |
| Default batch 256 may OOM on 8K-ish inputs | Cap batch by token budget (e.g. 32–64 for code windows) |
| Quantized ONNX + GPU EP can **fail** (BGE-M3Q note: CUDA fails on default quant model) | Prefer **FP32/FP16 ONNX for GPU paths**; int8 ONNX for CPU |
| Model download at first use | Prefetch on feature enable; checksum; offline fail soft |
| Catalog lag / enum coupling | Wrap enum behind our `JinaCodeV2` brand; allow path override |
| Candle feature models (Qwen3) pull different stack | Never default; size bomb |

**Adapter sketch:**

```rust
/// Production local embedder: fastembed over ort, branded M.
pub struct FastembedOrt<M: EmbeddingModel> {
    // TextEmbedding behind once_cell / mutex — ort sessions often !Sync across concurrent Run
    inner: Mutex<fastembed::TextEmbedding>,
    model_id: ModelId,
    accel: AccelKind,
    _m: PhantomData<M>,
}

impl Embedder for FastembedOrt<JinaCodeV2> {
    type Model = JinaCodeV2;
    async fn embed_batch(&self, texts: &[&str], purpose: EmbeddingPurpose)
        -> Result<Vec<Embedding<Self::Model>>, EmbedError>
    {
        // map purpose → optional prefixes if model needs them
        // spawn_blocking: fastembed is sync
        // validate each row with Embedding::from_vec
    }
}
```

### 3.3 ort (pykeio) — EP surface

**Sources:** [crates.io/crates/ort](https://crates.io/crates/ort) · [onnxruntime EP docs](https://onnxruntime.ai/docs/execution-providers/) · pykeio ort EP guide

**Execution providers relevant to nudox:**

| EP | Platform | Cargo feature (typical) | Role |
|---|---|---|---|
| **CPU** | All | default | **Canonical durable** |
| **CoreML** | macOS / iOS | `coreml` | Apple GPU + ANE |
| **CUDA** | Linux/Windows NVIDIA | `cuda` | Workstation / INDEX workers |
| **TensorRT** | NVIDIA | `tensorrt` | INDEX max throughput (not desktop default) |
| **DirectML** | Windows | `directml` (fastembed feature) | Future Windows GUI |
| **OpenVINO** | Intel | `openvino` | Optional Intel laptops |
| **XNNPACK** | mobile/WASM | `xnnpack` | Not desktop primary |
| **WebGPU** | browsers | — | Out of scope for GPUI native |
| **ROCm / MIGraphX** | AMD | varies | INDEX only if fleet has AMD |

**Registration pattern (conceptual ort 2.x):**

```rust
// Prefer list order = priority; always end with CPU fallback
Session::builder()?
  .with_execution_providers([
      CoreML::default().build(),  // or CUDA
      CPU::default(),
  ])?
  .commit_from_file("jina-code.onnx")?;
```

**Desktop packaging rules:**

1. Default binary: **CPU-only ORT** (smallest, most portable).  
2. macOS “full” build: CPU + CoreML.  
3. Optional Linux `.deb`/Nix: CPU + CUDA matching fleet CUDA version matrix (**version hell** — document supported CUDA or use bundled).  
4. Never link TensorRT into GUI.

### 3.4 Candle / Burn / llama.cpp — when not to

| Stack | Attractive because | Why not default |
|---|---|---|
| **Candle + Metal** | Pure Rust narrative; HF ecosystem | Per-model glue; larger maintenance; fastembed already covers Jina via ONNX |
| **Burn + wgpu** | One backend many GPUs | ONNX import via codegen awkward for large transformers (community reports); overkill for inference-only |
| **llama.cpp Metal** | Extreme throughput on some benches (short text) | Different model format; embedding pooling/task heads must match Jina exactly or **geometry breaks** |

**Independent bench signal (2025–2026 community):** [rust-embedding-bench](https://github.com/jerrythomas/rust-embedding-bench) style comparisons show **ort** often wins single-query latency; llama.cpp Metal can win raw throughput on tiny models — **not transferable to “use llama.cpp for Jina code” without a validated conversion.**

### 3.5 Higher-level “vector app” abstractions (usually wrong layer)

| Project | Claim | Fit |
|---|---|---|
| LangChain / LlamaIndex Rust ports | Pipelines | Too heavy; wrong product |
| `lancedb` embed helpers | Store+embed | Runner-up store only |
| Python Qdrant + fastembed | DX | Server scripts OK |
| **Our stack** | `Embedder` + `VectorStore` + `SearchPlanner` + `SemanticGate` | **Correct granularity** |

**Do not** adopt an external “RAG framework.” Adopt **thin adapters** over fastembed/ort/qdrant.

### 3.6 Recommended crate layout (abstractions)

```text
vector-core/          # traits: VectorStore, Embedder, EmbeddingModel, SearchFilter
                      # types: SymbolId, VectorPoint, embed_key helpers (pure)
vector-local/         # qdrant-edge adapter; FastembedOrt; EmbedStage; actor
vector-remote/        # qdrant-client; HttpEmbedder (OpenAI-compat + Voyage)
vector-eval/          # fixture parity, recall@k, EP equivalence (dev/CI)
```

**Capability type (missing from 09 sketch — add):**

```rust
#[derive(Clone, Debug)]
pub struct EmbedRuntimeInfo {
    pub model_id: ModelId,
    pub accel: AccelKind,          // Cpu | CoreMl | Cuda | DirectMl | ...
    pub durable_canonical: bool,   // true only for CPU parity path
    pub max_batch: usize,
    pub max_seq_len: usize,
    pub weights_sha256: [u8; 32],
    pub ort_package_id: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelKind { Cpu, CoreMl, Cuda, DirectMl, Other }

pub trait Embedder: Send + Sync {
    type Model: EmbeddingModel;
    fn runtime(&self) -> EmbedRuntimeInfo;
    // embed / embed_batch as today...
}
```

---

## 4. CPU path — full engineering

### 4.1 Why CPU is the product spine

- Works on every target (darwin/linux, aarch64/x86_64).  
- Deterministic enough for parity with pinned threads.  
- No driver install.  
- Smaller binary (no CUDA libs).  
- Query embed already fast for 161M-class models (int8 ONNX).

### 4.2 ORT CPU tuning knobs

| Knob | Guidance |
|---|---|
| `intra_op_num_threads` | Default: `min(4, physical_cores/2)` for GUI coexistence; full cores only for headless INDEX |
| `inter_op_num_threads` | 1 for single model session |
| `graph_optimization_level` | ORT_ENABLE_ALL for release |
| Memory arena | Enable for bulk; watch RSS |
| Batch size | 16–64 symbols for desktop; 64–256 INDEX |
| Sequence length | Cap recipe body window (512–1024 tokens) — biggest lever |
| Quantized ONNX | CPU int8/fp16 ONNX if quality gate passes — **separate tool_digest** |

### 4.3 Threading model (desktop)

```text
GPUI UI thread
    │
    ▼
EmbedScheduler (async)
    │  mpsc jobs: {texts[], embed_key[], priority}
    ▼
spawn_blocking pool (1–2 threads)  ← ORT CPU sessions often not free-threaded
    │
    ▼
FastembedOrt::embed_batch
    │
    ▼
VectorStore actor (single writer) → EdgeShard update
```

**Rules:**

1. **One** ORT session for Jina (or mutex). Concurrent `Run` is a common footgun.  
2. Do not share session across processes.  
3. Prefer queue + batch coalesce (50–100 ms) during bulk commits.  
4. Priority: interactive query &gt; open-file symbols &gt; background cold index.

### 4.4 SIMD / ISA expectations

- ORT CPU EP uses optimized kernels (MLAS etc.) — we do **not** hand-roll GEMM.  
- Ensure release builds enable appropriate target CPUs in packaging (x86-64-v2 baseline vs native — product decision).  
- Apple Silicon: ARM NEON path via ORT; CoreML is separate (§5).

### 4.5 CPU performance budget (targets to measure, not marketing)

| Metric | Target (dev laptop class) |
|---|---|
| Query embed p95 (≤64 tokens) | &lt; 25 ms |
| Batch 32 × ~256 tokens | &gt; 30–80 symbols/s (measure) |
| Cold 50k symbols | Progress UI; cancel; estimate from measured rate |
| RSS embedder loaded | ≤ 300 MB (161.9 MB int8 weights + tokenizer + ORT arena + activations at batch ≤ 32); **0 when idle-unloaded** |

Publish real numbers in `vector-eval` before GA copy.

### 4.6 CPU failure modes

| Failure | Mitigation |
|---|---|
| OOM mid-batch | Shrink batch; spill; fail stage without seal |
| Thread oversubscription vs rayon/UI | Cap ORT threads via SemanticGate |
| Corrupt model file | Checksum; redownload; disable semantic |
| Tokenizer panic on invalid UTF-8 | Pre-validate; lossy boundary only if recipe says so (prefer hard fail) |

---

## 5. GPU & accelerator path — full engineering

### 5.1 Platform matrix

| Platform | Primary accel | Secondary | Ship posture |
|---|---|---|---|
| **Apple Silicon macOS** | **CoreML EP** (GPU and/or ANE) | CPU | Opt-in auto after equivalence tests |
| **Intel Mac** | CPU (CoreML limited) | — | CPU |
| **Linux NVIDIA** | **CUDA EP** | TensorRT on INDEX only | Optional build / Nix flake feature |
| **Linux AMD** | CPU (ROCm EP experimental) | — | CPU default |
| **Windows** (future) | DirectML | CUDA | Feature `directml` when GUI ships Win |
| **INDEX k8s** | CUDA / TRT workers | CPU fallback nodes | First-class fleet config |

### 5.2 CoreML EP (macOS) — deep notes

**Sources:** [ONNX Runtime CoreML EP](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html)

**Capabilities:**

- Targets: `CPUOnly`, `CPUAndGPU`, `CPUAndNeuralEngine`, `ALL`.  
- Formats: `NeuralNetwork` (older) vs **`MLProgram`** (Core ML 5+ / macOS 12+) — prefer MLProgram on modern macOS.  
- **Dynamic shapes** allowed by default but **may hurt performance**; static shapes preferred when possible.  
- **Graph partition:** unsupported ops fall back to CPU — **fragmented graphs can be slower than pure CPU**.  
- **Model cache directory** critical: first compile can take **seconds to minutes**; without cache, every process start recompiles.  
- `ProfileComputePlan` for diagnosing ANE vs GPU vs CPU placement.  
- `AllowLowPrecisionAccumulationOnGPU` for fp16 accumulate tradeoffs.

**Nudox CoreML policy:**

```text
1. Session providers: [CoreML(MLProgram, MLComputeUnits=ALL|CPUAndGPU), CPU]
2. Set ModelCacheDirectory under $DATA_ROOT/ort-coreml-cache/<model_sha>/
3. First-run UX: "Preparing accelerator…" once; never block UI without progress
4. Equivalence gate: cosine ≥ 1-ε vs CPU on fixture suite before enabling auto
5. If partition slow-path detected (telemetry: gpu_ratio low + latency worse): fall back CPU
6. Durable vectors: prefer CPU (I11) unless product accepts hybrid §2.3
```

**ANE caveats:**

- Great for sustained efficiency; may not win short single queries.  
- Some transformer ops may not map → CPU fallback.  
- Thermal: long cold index may throttle — respect SemanticGate + system thermal state if available.

### 5.3 CUDA EP (Linux / Windows NVIDIA)

**Sources:** [CUDA EP](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html) · [TensorRT EP](https://onnxruntime.ai/docs/execution-providers/TensorRT-ExecutionProvider.html)

| Topic | Guidance |
|---|---|
| Desktop GUI | **Optional** — do not require CUDA toolkit on every Linux user |
| Packaging | Nix flake input / optional dynamic libs; document CUDA major (e.g. 12.x) |
| INDEX workers | CUDA default on GPU nodes; TensorRT only if engine build amortized |
| Provider order | `[TensorRT?, CUDA, CPU]` on INDEX; `[CUDA, CPU]` on workstation |
| FP16 | Prefer fp16 ONNX on GPU if quality gate passes |
| VRAM | 161M encoder is light (~0.5–1.5 GB class with overhead); still cap concurrency=1 on 4GB GPUs |
| Multi-GPU | Not needed for embed desktop; INDEX can pin device ordinal |

**TensorRT:**

- Longer startup (engine build/cache).  
- Excellent steady throughput for INDEX.  
- **Not** for first interactive desktop open without engine cache.

### 5.4 DirectML (Windows)

fastembed documents `features = ["directml"]` + `ort::ep::DirectML`. When Windows GUI exists:

- Register DirectML + CPU.  
- Disable memory pattern / parallel execution as fastembed docs require for DirectML.  
- Same equivalence + durable CPU policy.

### 5.5 When GPU is a *net loss*

| Scenario | Why |
|---|---|
| Batch size 1, cold GPU | Context init + transfer &gt; compute |
| Fragmented CoreML graph | Sync tax CPU↔ANE |
| Competing with game/IDE GPU load | Preemption latency |
| Battery / low-power mode | Prefer CPU; SemanticGate |
| Quantized model incompatible with EP | Runtime error or silent CPU |

**Auto policy state machine:**

```text
accel = detect()
if low_power or user_pref == ForceCpu: CPU
elif cold_index && accel.ok && equivalence_passed: try Accel
elif query && session_warm && measured_query_gain > 1.2: Accel
else: CPU
on_error: permanent fallback CPU for process + log
```

### 5.6 INDEX (server) GPU plan

| Item | Decision |
|---|---|
| Workers | Dedicated embed deployments with NVIDIA T4/A10-class or better |
| Batch | Large dynamic batches; continuous queue |
| Models | Jina parity ONNX on GPU OK **if** published vectors match CPU reference within ε **or** entire INDEX generation uses same EP consistently |
| Voyage | API — no local GPU |
| Isolation | Embed workers ≠ query pods; scale independently |
| Observability | symbols/s, batch size, GPU util, EP fallback rate |

**INDEX consistency rule:** Pick **one** of:

1. **CPU-only durable** everywhere (simplest), or  
2. **GPU durable with golden tests** per model export + locked ORT/CUDA versions.

Do not mix CPU and GPU published vectors in one collection without ε validation.

### 5.7 smolvm / passthrough note

`SMOLVM-PLAN.md` notes CUDA/GPU passthrough as future for embedding workloads. That path is **fleet/sandbox**, not GPUI. Desktop uses host CoreML/CUDA directly; smolvm GPU is for sealed remote producers if ever needed — **out of v1 desktop scope**.

---

## 6. End-to-end data path (hardened)

### 6.1 Write path

```text
CommitGate sealed generation
  → SymbolDelta (added/removed/changed)
  → for each symbol × vector_name:
        compute embed_key
        if stage_traces has key: skip
        else queue EmbedJob
  → EmbedScheduler batches by priority
  → Embedder (CPU canonical)
  → validate dim + non-NaN + optional L2 normalize
  → VectorStore.upsert (Edge actor)
  → stage_traces insert
  → maybe schedule compact
  → when delta complete: mark corpus Ready facet
```

### 6.2 Query path

```text
query text
  → Embedder.embed (CPU or warm accel; query vectors need not be CAS-published)
  → VectorStore.search (filter DepSet / language / kind)
  → optional RRF with Tantivy
  → optional remote Routed if not Ready
  → label sources; never fuse Voyage with Jina scores
```

### 6.3 Normalization & distance

| Rule | Value |
|---|---|
| Metric | Cosine |
| Storage | Prefer **L2-normalized** f32 at write if model doesn’t already | 
| Score UI | Similarity in [0,1] after consistent mapping |
| NaN/Inf | Reject batch |

### 6.4 Payload (defense in depth)

```text
symbol_id, language, package, kind, path,
content_hash / embed_key,
model_id, recipe_id,
embed_ep,           // "cpu" | "coreml" | ...
tool_digest,        // short hex
generation_id
```

---

## 7. Binary size, packaging, legal, security

### 7.1 Size stack

| Component | Order of magnitude | Notes |
|---|---|---|
| qdrant-edge | ~10–15 MB class (blog ~11 MB) | Accept |
| ONNX Runtime CPU | **tens of MB** | Largest native dep |
| + CoreML build | modest | Uses system frameworks |
| + CUDA | **huge** / external | Optional pack only |
| Jina ONNX weights | **161.9 MB** (canonical int8; fp16 321 MB fallback; f32 641.5 MB never ships) | **Download on demand**, sha256-pinned, not in base dmg |
| fastembed / tokenizers | moderate | |

**Product:** base app without weights; semantic pack downloads model + checksum into Application Support.

### 7.2 Security

- Model download: **HTTPS + sha256 pin** in catalog; no arbitrary URL from untrusted project files.  
- ORT is native code — treat model files as **untrusted input** (ONNX is a graph; keep ORT updated).  
- No GPU peer DMA assumptions beyond ORT.  
- Multi-tenant INDEX: never trust client-supplied vectors without authz (existing gate).

### 7.3 License

- Jina-code Apache-2.0 — OK.  
- ORT Apache-2.0 — OK.  
- Voyage — API ToS / commercial.  
- Redistribute notices for native deps in About/Licenses.

---

## 8. Implementation playbook

### 8.1 Phase checklist (acceptance-bound)

| Phase | Done when |
|---|---|
| **P0** | Remote `VectorStore` + brands `JinaCodeV2` (768) + `VoyageCode3` (1024); **`EmbedRole { Query, Document }` added to `Embedder::embed/embed_batch`** (Jina ignores, Voyage → `input_type`, E5 → prefixes — 09b §3.1b); golden Embedding dim tests |
| **P1** | Edge open/upsert/search/delete/compact behind the single-writer store actor; kill -9 recover; multi-window lock; `schema.json` carries `format_version` + `quant_profile` |
| **P2** | CPU fastembed Jina over the **sha-pinned `model_quantized.onnx`**; int8-vs-f32 quality gate run once (recall@10 Δ ≤ 1 pt on 09b §11 fixtures → freeze int8, else fp16 + re-budget); EmbedTextBuilder fixtures (incl. the §3.1b truncation algorithm) byte-identical desktop/INDEX |
| **P3** | SymbolDelta only; embed_key skip ≥ 99% on no-op commit; doc-only edit ⇒ exactly 1 infer + 1 upsert (09b §16.4) |
| **P4** | RRF hybrid; SemanticGate concurrency=1 default; **routing decision table 09-vector §20.5 implemented + integration-tested row by row** |
| **P5** | 200k RSS store &lt; 500 MB harness green; quant ladder auto; **rescore=true, oversampling=2.0 on every quantized shard** (09-vector §20.6); cross-shard merge equals all-f32 control ordering exactly |
| **P6** | CoreML equivalence suite; auto policy with CPU fallback; durable writes stay CPU-canonical (I11) |
| **P7** | Voyage collection + quality_mode; keys server-side only; `input_type`/`output_dimension`/`output_dtype` pinned in code (I16); no score fusion |
| **P8** | **Bakery**: idempotent per `edgepack_key`, artifact in CAS + manifest with `ram_estimate`; client install = fetch → BLAKE3 verify → unpack → `EdgeShard::load`; **hot-set admission + eviction under `vector.local_budget_bytes`**; R2 proof: dep search works with the embedder never loaded (09-vector §20.10) |
| **P9** | Rerank service (mxbai-rerank-base-v2 vs bge-reranker-v2-m3 bake-off frozen; Voyage rerank-2.5 premium path); Deep p95 ≤ 1.2 s at k=100 with progressive Stage-1 render; **CI model-license deny-list green (I15)** |
| **P10** | CUDA optional; CodeRankEmbed bake-off; TurboQuant decision; ColBERT experiment only if license-clean code ColBERT exists |

### 8.2 Evaluation suite (expand 09b §11)

| Suite | Pass criteria |
|---|---|
| **EP equivalence** | median cosine(v_cpu, v_accel) ≥ 0.999 (tune) on 1k symbols |
| **Parity Edge/server** | top-10 Jaccard ≥ 0.9 same vectors |
| **Filter correctness** | zero cross-language leaks on fixture |
| **Incremental** | doc-only edit re-embeds 1 symbol not N |
| **Perf CPU** | query embed p95 under budget |
| **Perf CoreML bulk** | ≥1.5× CPU on cold 5k or disable auto |
| **Memory** | three gauges under caps (store ≤ 350 MB target / 500 MB cap; embedder ≤ 300 MB loaded, 0 idle; misc ≤ 100 MB — 09-vector §20.8) |
| **Offline** | no network → semantic works if weights present; cold-dep scope omitted **with explicit label** (I14) |
| **Placement (09-vector §20.10)** | R2 proof (deps searchable, embedder never loaded); budget/eviction proof; cross-shard merge exactness; routing rows; hedged-merge labeling; bakery idempotence; same weights sha on both planes |
| **License** | CI deny-list rejects any CC-BY-NC model id in catalog or rerank-service config (I15) |

### 8.3 Config surface (user + internal)

```text
semantic.enabled = true
semantic.model = jina-v2-code
semantic.weights_artifact = model_quantized.onnx@<sha256>   # canonical, both planes (I11)
semantic.accel = auto | cpu | coreml | cuda
semantic.durable_ep = cpu          # frozen default
semantic.batch_size = 32           # overrides fastembed's 256 default (I16)
semantic.ort_intra_threads = 4
semantic.unload_idle_ms = 120000
semantic.coreml_cache = <data>/ort-coreml-cache
semantic.rerank.self_host = mixedbread-ai/mxbai-rerank-base-v2   # Apache-2.0 (09b §18.3b)
semantic.rerank.premium = voyage/rerank-2.5
vector.on_disk = true
vector.quant = auto                # ladder (project shard); dep shards always qp1 int8
vector.rescore_quantized = true    # frozen — cross-shard comparability (09-vector §20.6)
vector.local_budget_bytes = 350000000   # hot-set admission ceiling (09-vector §20.4)
vector.compact_deleted_ratio = 0.2
depshards.enabled = true
depshards.origin = <INDEX url>     # only trusted origin; artifacts BLAKE3-verified
```

### 8.4 Telemetry (must-have)

- `embed.symbols_per_sec{ep=}`  
- `embed.batch_size`  
- `embed.ep_fallback_total`  
- `embed.queue_depth`  
- `vector.rss_bytes` / `embedder.rss_bytes`  
- `vector.compact_duration`  
- `search.latency_ms{source=local|index-jina|index-voyage}`  
- `search.remote_ratio` (fraction of dense queries the working set could not answer — hot-set health)  
- `hotset.admitted_packages` / `hotset.evictions_total`  
- `depshard.installs_total` / `depshard.verify_failures_total`  
- `rerank.latency_ms{model=}` / `rerank.timeouts_total`  

---

## 9. Adversarial “what still can kill us” (residual risks)

| Risk | Likelihood | Impact | Residual mitigation |
|---|---|---|---|
| Edge beta data loss on upgrade | M | H | Rebuild from IR always possible; never sole copy of truth |
| ORT CVE in shipped dylib | L | H | Update cadence; SBOM |
| CoreML worse than CPU on Jina graph | M | M | Auto benchmark gate |
| CUDA packaging support burden | H | M | Nix optional; don’t promise universal Linux GPU |
| ~~Hot-deps policy never productized~~ **Resolved** — admission frozen 09-vector §20.4 | — | — | Residual: enforce the budget harness in CI so it never regresses |
| Recipe drift desktop/INDEX | M | H | Shared crate + CI goldens |
| Users enable GPU durable writes → parity break | M | H | I11 forced CPU durable |
| Voyage cost blowup | M | M | CAS + delta only + budgets |
| Team builds second vector stack (Lance) early | L | H | Feature-flag discipline |
| **CC-BY-NC model ships by accident** (2 of 4 fastembed rerankers are non-commercial) | M | H | I15 CI deny-list on model ids; license table 09b §18.3b is the gate for additions |
| **Bakery staleness** after `edge_format_version` / model / recipe bump — clients remote-route en masse | M | M | Bakery re-bakes hot packages first (by global admission popularity); `search.remote_ratio` alarm |
| **Weights-artifact split-brain** (desktop int8 vs INDEX f32) | L | H | I11/I12: one sha-pinned artifact both planes; CI asserts equal `tool_digest` |

---

## 10. Decision matrix: runtime stacks

Weights: correctness (×3), desktop UX (×2), INDEX throughput (×2), binary size (×2), maint (×2), EP breadth (×1).

| Stack | Corr | UX | INDEX | Size | Maint | EP | **Total** |
|---|---|---|---|---|---|---|---|
| **fastembed + ort CPU** | 15 | 10 | 6 | 6 | 10 | 3 | **50** |
| **fastembed + ort CPU+CoreML** | 14 | 10 | 6 | 6 | 8 | 4 | **48** |
| **ort raw + custom ONNX** | 15 | 8 | 8 | 6 | 6 | 5 | **48** |
| **Candle Metal Jina port** | 10 | 8 | 4 | 8 | 4 | 3 | **37** |
| **Burn wgpu** | 8 | 6 | 4 | 6 | 4 | 4 | **32** |
| **Python sidecar** | 12 | 4 | 10 | 4 | 4 | 5 | **39** |
| **llama.cpp convert** | 6 | 6 | 6 | 8 | 3 | 3 | **32** |

**Winner:** fastembed + ort, CPU canonical, CoreML/CUDA as **accelerators behind feature flags**.

---

## 11. Cross-doc patches (apply with this review)

| Doc | Patch | Status |
|---|---|---|
| `09-vector.md` | Point to 09c for runtime/GPU; elevate sidecar isolation mode; **placement plane added as §20** (bakery, hot-set, budgets, canonical artifact) | **Done 2026-07-17/18** |
| `09b` | §3.1b model I/O correctness; §18.3b license audit; cross-encoder Deep default; rescore amendment; I13–I16 | **Done 2026-07-18** |
| `LIBRARIFICATION-PLAN` §11 | One-liner: durable embeds CPU-canonical on the pinned int8 artifact; client/server placement per 09-vector §20; GPU bulk optional per 09c | Pending |
| Catalog `model/catalog.rs` | Add `JinaCodeV2` (768) + `VoyageCode3` (1024) brands; add `EmbedRole` to `Embedder` (09b §3.1b) | Impl when coding (P0) |
| `SMOLVM-PLAN` | GPU passthrough remains fleet-only footnote | Pending |

---

## 12. Source index (key anchors)

| Topic | URL |
|---|---|
| Qdrant Edge | https://qdrant.tech/documentation/edge/ |
| qdrant-edge crate | https://crates.io/crates/qdrant-edge |
| fastembed-rs | https://github.com/Anush008/fastembed-rs |
| fastembed crate | https://crates.io/crates/fastembed |
| ort crate | https://crates.io/crates/ort |
| ORT execution providers | https://onnxruntime.ai/docs/execution-providers/ |
| CoreML EP | https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html |
| CUDA EP | https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html |
| TensorRT EP | https://onnxruntime.ai/docs/execution-providers/TensorRT-ExecutionProvider.html |
| jina-embeddings-v2-base-code | https://jina.ai/models/jina-embeddings-v2-base-code/ |
| Qdrant quant | https://qdrant.tech/documentation/manage-data/quantization/ |
| Python FastEmbed GPU | https://qdrant.github.io/fastembed/examples/FastEmbed_GPU/ |
| Burn | https://github.com/tracel-ai/burn |
| Candle | https://github.com/huggingface/candle |

---

## 13. Executive summary

The 09 / 09b plan correctly picks **qdrant-edge + Jina-code + same-model parity + Voyage premium + SymbolDelta incrementality**. Adversarial review finds the **largest holes were not the ANN engine**, but:

1. **Embedding runtime under-specified** (CPU tuning, GPU partition traps, durable EP identity).  
2. **Hot-deps / multi-budget memory** product policy incomplete.  
3. **Process isolation** for native Edge/ORT underrated.  
4. **Abstraction layer** stops at store/embed traits — needs **`EmbedRuntimeInfo` + accelerator policy**.  
5. Live code brands **E5/OpenAI**, plan brands **Jina** — catalog must converge.

**Libraries that matter:** `fastembed` + `ort` (+ our traits). Everything else is escape hatch or distraction.

**GPU strategy:** CoreML/CUDA are **throughput accelerators** for bulk work and INDEX; **CPU remains the correctness and query spine**. Prefer **CPU for all durable vectors** until an explicit parity program says otherwise.

**Placement (2026-07-18 hardening):** the client serves Stage-1 over *project + budget-admitted hot deps only*; dependency vectors arrive as **server-baked Edge shards** (client never embeds a dep — I13); all neural rerank is server-side. Full frozen split + budgets: **09-vector §20**.

**Verified corrections (2026-07-17/18):** Jina v2-code is **161M** params (not 137M); ONNX f32 is 641.5 MB so the **canonical artifact is the 161.9 MB int8 file, sha-pinned on both planes** (quality-gated once in P2); voyage-code-3 parameters must be pinned explicitly (I16); **jina-reranker-v2 and jina-colbert-v2 are CC-BY-NC** → Deep-mode Stage-2 is a **cross-encoder** (mxbai-rerank-v2 Apache-2.0 self-host, Voyage rerank-2.5 premium), ColBERT demoted to experimental (I15).

**Ship sequence:** P0–P5 correctness, placement routing and memory → P6 CoreML gated → P7 Voyage → P8 bakery + hot-set → P9 Deep rerank → P10 CUDA/bake-offs.

---

*End of 09c. Store details remain in 09-vector.md; retrieval quality in 09b; this file owns runtime, accelerators, adversarial residual risk, and the hardened summary plan.*
