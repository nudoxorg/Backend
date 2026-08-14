# Handoff: Research embedded vector search
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a8ea8feee9e8d09e1.jsonl`
- Agent id: `agent-a8ea8feee9e8d09e1`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/09-vector.md`
- Existing report: False (0 lines)
- Tools used: 1 ({'Agent': 1})
- Files/greps touched: 0
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

The research agent is running in the background — it's doing extensive web searches across Qdrant's issue tracker, crates.io, GitHub repos for LanceDB/arroy/usearch/sqlite-vec, embedding model benchmarks, and more. This will take a few minutes given the scope.

I'll let you know as soon as it completes with the full report and executive summary.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify current versions/maintenance status with WebSearch/WebFetch; do NOT trust training data. Cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform. Today semantic search uses a Qdrant SERVER (qdrant-client 1.18 from Rust) storing per-symbol embeddings. THE PLAN: vector search gets EMBEDDED into a GPUI desktop app (Rust, macOS + Linux), backed by LOCAL DISK to keep memory slim, while the remote INDEX keeps serving big-scale search. A well-designed trait must span both (remote Qdrant service ↔ local embedded store), so the implementation choice must support: upsert/delete by stable symbol ID (incremental updates — only changed symbols re-embedded), payload filtering (language, package, kind), disk-resident indexes with bounded memory, and reasonable build times for ~10^4–10^6 vectors per project-with-dependencies.

YOUR MISSION — exhaustive evaluation:
1. Qdrant embedded feasibility (settle this definitively): can qdrant run embedded/in-process in Rust in 2026 (there have long been requests; check qdrant issue tracker/roadmap)? If not, is shipping a qdrant sidecar binary with the desktop app viable (size, lifecycle, ports), and is that better or worse than a true embedded lib?
2. Embedded candidates in Rust — for each: 2026 version, license, maintenance health, index types (HNSW? IVF? DiskANN?), disk-vs-memory residency, filtered search support, incremental upsert/DELETE support (critical — many ANN structures handle deletes poorly), crash safety, and binary-size/build cost:
   - LanceDB (lancedb crate; Lance format, IVF_PQ/HNSW, serverless design)
   - usearch (rust binding; HNSW, views/memory-mapping, filters?)
   - hnsw_rs / instant-distance (bare HNSW libs)
   - arroy (Meilisearch's LMDB-backed ANN — designed for incremental updates on disk)
   - sqlite-vec (sqlite extension; brute-force + quantization; pairs with a sqlite-centric local store)
   - Chroma/other with Rust story if relevant; anything new and credible by 2026.
3. Embedding generation locally: fastembed-rs / ort (ONNX Runtime) / candle for running an embedding model in-process on developer machines; which CODE-specialized embedding models are current best small options in 2026 (e.g., jina-code-v2 successors, nomic-embed-code, voyage-code sizes; open-weights only for local) — dimensions, quality, size, license; CPU vs GPU (Metal on macOS) inference cost per symbol; batching. Also: the remote side can use bigger models — how to keep local/remote embeddings COMPATIBLE (same model shipped both places vs dual-index) — recommend a policy.
4. Storage layout: one collection per project? per language? global with payload filter? Quantization (PQ/SQ/binary) tradeoffs at our scale; memory ceilings (target: <500MB resident for vector search in the GUI).
5. Trait design inputs: sketch `trait VectorStore` (async: upsert(batch of {SymbolId, vector, payload}), delete(ids), search(query_vec, filter, k) → scored ids, snapshot/compact) such that QdrantRemote and EmbeddedLocal both implement it; note where the abstraction leaks (filter language differences, consistency, score normalization across impls) and how to paper over them.

SYNTHESIS: a decision matrix table, a clear WINNER recommendation for the embedded store + the local embedding model, the sidecar-vs-embedded verdict for qdrant, and the compatibility policy between local and remote embeddings.

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1000+ lines, URLs inline) to /Users/philocalyst/Projects/Backend/docs/research/librarification/09-vector.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) the decision matrix conclusions + winner, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)
