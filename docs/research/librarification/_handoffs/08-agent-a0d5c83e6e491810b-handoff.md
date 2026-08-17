# Handoff: Research incremental computation
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a0d5c83e6e491810b.jsonl`
- Agent id: `agent-a0d5c83e6e491810b`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/08-incremental.md`
- Existing report: False (0 lines)
- Tools used: 8 ({'ToolSearch': 1, 'Agent': 7})
- Files/greps touched: 0
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll research this exhaustively across all 7 dimensions in parallel, then synthesize the report. Let me launch parallel research forks.

### fragment

All 7 research agents are running in parallel. I'll wait for their results and then synthesize the full report.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify current versions/status with WebSearch/WebFetch; cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform being restructured around DEEP INCREMENTALITY. Pipeline stages: acquire source → per-language producer emits typed IR (symbols with signatures/docs/references) → IR serialized to content-addressed blobs (BLAKE3 hashes exist in the `heart` crate) → derived artifacts: tantivy text index entries, vector embeddings (per symbol), TerminusDB graph documents, rendered docs, tree-sitter trees. THE REQUIREMENT: when a developer COMMITS a change (git commit is the invalidation gate — uncommitted edits trigger nothing), only the symbols that actually changed flow through downstream stages: embeddings are regenerated ONLY for changed symbols, index entries updated only for changed symbols, graph publishes only changed documents. This must hold for every component, locally (embedded in a desktop app) and remotely (k8s fleet).

YOUR MISSION — exhaustive research on incremental-computation architecture:
1. Salsa (rust-analyzer's engine): 2026 state (salsa 3.x?), core model (queries, memoization, red-green revalidation, durability, interning, LRU), what it costs (memory, single-process assumption), whether salsa fits a PERSISTENT cross-process pipeline (databases serialized to disk? salsa's persistence story in 2026 — verify) or whether it's only intra-session.
2. Build-system theory: "Build Systems à la Carte" taxonomy (rebuilder strategies: verifying traces, constructive traces; suspending vs restarting schedulers) — map nudox's pipeline onto it; early cutoff (a stage whose output hash is unchanged stops downstream propagation) — this is the key property; which strategy gives it with minimal machinery.
3. Content-addressed memoization in build tools: Bazel/Buck2 action digests + remote cache (RE API), Nix input-addressed vs content-addressed derivations, Turborepo/turbo-engine task hashing, Gradle build cache. Extract the pattern: stage function + canonical input digest → cached output digest; what canonicalization pitfalls (ordering, timestamps, absolute paths, float nondeterminism) they guard against.
4. Incremental indexing in real code-intel systems: how rust-analyzer, Zoekt, Sourcegraph precise indexes, GitHub handle per-commit index updates (document-level? symbol-level?); tantivy's model for this (delete-by-term + re-add; segment merging costs).
5. Incremental embedding pipelines: publicly documented architectures (Cursor's Merkle-tree sync IS documented — study it: client computes Merkle tree over files, server diffs trees, only changed files re-embedded; verify details 2026), other RAG-pipeline incrementality patterns (LlamaIndex/LangChain ingestion caches keyed by content hash), dedup via content hash across users/projects.
6. Differential/streaming approaches (differential dataflow, Materialize, salsa alternatives like adapton, incremental (Jane Street ocaml), comemo, cached crates) — quick assessment of what's actually applicable vs overkill.
7. Git as the change feed: efficient computation of "which files changed between generation N-1 and N" via gitoxide (gix) tree diff; mapping file-level diff → symbol-level diff (re-run producer on changed files? producers are package-level — research per-language partial-compilation feasibility: rust-analyzer incremental, OXC per-file, go/types per-package...); commit-gated batching (debounce until commit; squash intermediate states).

SYNTHESIS REQUIRED — design the incremental spine for nudox:
- A `Generation` model: inputs (source tree hash per file via git), stage graph (producer → IR → {render, embed, index, graph}), per-stage canonical digests, early cutoff at symbol granularity (per-symbol content hash computed from canonicalized IR).
- The delta contract between generations: SymbolDelta {added, removed, changed(id, old_hash, new_hash), unchanged} — where it's computed, how each downstream consumes it (tantivy delete+add, vector upsert/delete, terminus publish, render patch).
- Persistence: what the "trace store" is (sqlite table of stage digests), so incremental works across process restarts on both desktop and server.
- Recommend: build on salsa vs hand-rolled digest/trace store (justify; the pipeline is coarse-grained and cross-process — a hand-rolled constructive-trace store in sqlite is likely right, but argue it), with concrete Rust types/trait sketch (e.g., trait Stage { type In; type Out; fn digest(&In)->Digest; fn run(&In)->Out }).

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1200 lines, URLs inline) to /Users/philocalyst/Projects/Backend/docs/research/librarification/08-incremental.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) top concrete recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)
