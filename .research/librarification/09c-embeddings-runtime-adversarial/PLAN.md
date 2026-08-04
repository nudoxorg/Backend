# 09c — Summary Plan: Embeddings Runtime (CPU / GPU / Abstractions)

**Authoritative full document:** [`../09c-embeddings-runtime-adversarial.md`](../09c-embeddings-runtime-adversarial.md)  
**Siblings:** store + placement [`../09-vector.md`](../09-vector.md) (§20 = client/server split) · retrieval [`../09b-retrieval-pipeline-plan.md`](../09b-retrieval-pipeline-plan.md)  
**Date:** 2026-07-17 (placement + license hardening 2026-07-18)

---

## Ship decisions (frozen)

| Layer | Decision |
|---|---|
| Local store | `qdrant-edge` (pin exact); one mutable project shard + read-only baked dep shards; on_disk always |
| Remote store | `qdrant-client` 1.18; parity + premium collections |
| **Placement** | Client = project + budget-admitted hot deps, Stage-1 only; server = full corpus + all neural rerank + **shard bakery** (09-vector §20, rules R1–R8) |
| Model L0 | `jina-embeddings-v2-base-code` 768-d — **161M**; canonical artifact **`model_quantized.onnx` 161.9 MB int8, sha-pinned on both planes** |
| Model R1 | `voyage-code-3` remote-only (separate collection); pin `output_dimension=1024` + `input_type` explicitly |
| Durable embeds | **CPU EP canonical (I11)** on the single pinned artifact |
| Accelerators | CoreML (macOS) / CUDA (optional Linux, INDEX) — gated |
| Hybrid | Qdrant dense ⊕ Tantivy BM25 (RRF); no sparse-in-Qdrant v1 |
| Quant | project shard: scalar int8 @ N≥50k; dep shards: always int8 (qp1); **rescore always on quantized shards** (cross-shard comparability) |
| Rerank | Local = RRF only; server Deep = **mxbai-rerank-base-v2** (Apache-2.0) / **Voyage rerank-2.5** premium; **ColBERT experimental — jina-colbert-v2 is CC-BY-NC** |
| Dep vectors | **Client never embeds deps (I13)** — server-baked Edge shards, BLAKE3-verified install; hot-set admission under `vector.local_budget_bytes` (09-vector §20.4) |
| Isolation | Single-writer Edge actor; sidecar = documented isolation mode |
| Hot-deps | ~~monorepo GA blocker~~ **resolved** — budget-driven admission frozen (09-vector §20.4) |

## Implementation sequence (P0–P10)

```text
P0  Trait freeze + brands (JinaCodeV2, VoyageCode3) + EmbedRole query/doc axis
P1  Edge local + payload indexes + kill-9 + multi-window lock
P2  EmbedStage: EmbedTextBuilder v2 + embed_key + CPU fastembed
    + int8-artifact quality gate (recall@10 Δ ≤ 1pt vs f32)
P3  SymbolDelta wiring + hash skip + progress UX
P4  Hybrid RRF + SemanticGate + routing table (09-vector §20.5)
P5  Quant ladder + rescore-on-fanout + idle compact + memory harness
P6  CoreML equivalence gate + auto policy
P7  Voyage premium + quality_mode UX (keys server-side)
P8  Shard bakery + hot-set admission + dep-shard install/evict
P9  Deep mode: server cross-encoder rerank + progressive display
P10 Optional CUDA pack; CodeRankEmbed bake-off; TurboQuant; ColBERT experiment
```

## Invariants I1–I16

See full doc §0.3. Critical new ones:

- **I11** Durable parity vectors → CPU EP + single sha-pinned int8 artifact  
- **I12** `tool_digest` pins ORT package id + weights sha, not ambient GPU  
- **I13** Client never embeds dependency corpora (baked shards or remote route)  
- **I14** Budgets via hot-set admission; narrowed scope always labeled  
- **I15** License deny-list: CC-BY-NC models (jina-reranker-v2, jina-colbert-v2) never ship  
- **I16** Vendor/API params pinned explicitly; an unpinned default is a bug  

## Libraries

| Use | Library |
|---|---|
| Default embedder | `fastembed` 5.17.x (batch overridden 256→32) |
| ONNX runtime | `ort` 2.x |
| App boundary | our `Embedder` + `EmbeddingModel` + `EmbedRole` + `EmbedRuntimeInfo` |
| Server rerank | mxbai-rerank-base-v2 (own ONNX export) / bge-reranker-v2-m3 (fastembed, fallback) |
| Escape | raw `ort` Session / custom ONNX |
| Reject as default | Candle, Burn, llama.cpp, Python sidecar, FAISS; CC-BY-NC rerankers |

## Adversarial residual (top)

1. Edge beta format + in-process native crash → format_version + sidecar mode  
2. Recipe/EP/artifact drift desktop vs INDEX → shared crate + goldens + equal `tool_digest` CI  
3. CC-BY-NC model ships by accident → I15 CI deny-list  
4. Bakery staleness after format/model bump → hot-packages-first re-bake + `search.remote_ratio` alarm  
5. GPU marketed before equivalence suite → false quality/speed claims  

---

*Do not fork this summary — edit the full 09c document and keep this file as the one-page card.*
