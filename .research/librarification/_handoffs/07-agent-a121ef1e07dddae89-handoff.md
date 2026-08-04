# Handoff: Research TerminusDB and tiering
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a121ef1e07dddae89.jsonl`
- Agent id: `agent-a121ef1e07dddae89`
- Deliverable: `/Users/philocalyst/Projects/Backend/.research/librarification/07-terminus-tiering.md`
- Existing report: True (870 lines)
- Tools used: 1 ({'Agent': 1})
- Files/greps touched: 0
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

The research agent is running in the background gathering current 2026 data on TerminusDB and admission control algorithms. I'll let you know as soon as it completes with the full report and executive summary.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify current project status/versions with WebSearch/WebFetch; do not trust training data alone. Cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform. Per-language producers lower package source to a typed IR; the IR is lowered to TerminusDB graph documents (model/from_ir/link modules exist) and published. Today every package would go to TerminusDB. THE PLAN: TerminusDB becomes a privilege of only the MOST-USED packages (admission-controlled — the user suggests a leaky-bucket algorithm). For the long tail, graph-specific operations run directly over IR loaded from content-addressed blobs. Additionally, the pipeline becomes incremental: new package generations should publish only changed symbols, with LINEAGE edges linking a symbol's nodes across generations. A schema pipeline exists: LinkML schema.yaml → generated Rust types + TerminusDB schema.json.

YOUR MISSION — two-part exhaustive research:

PART A — TerminusDB deep-dive (2026 state):
1. Project health: current version, release cadence, maintainer activity, license, community size. Is it still the right graph store to bet on in 2026? (Check GitHub terminusdb/terminusdb.)
2. Architecture: succinct data structures / delta encoding internals, the commit graph (layers), branching model — how cheap are branches/commits? Can each package be a branch/db? What are practical limits (number of dbs/branches/commits, layer stack depth, squash/rebase operations)?
3. Write path: document insertion throughput, bulk import performance, JSON document interface vs WOQL, the Rust client story (terminusdb-client-rust? HTTP API? — what does production usage look like from Rust in 2026).
4. Lineage modeling: best practice for "same logical entity across generations" — options: (i) stable document IRI reused across commits on one branch (history = commit log), (ii) explicit lineage edges between version-scoped documents, (iii) TerminusDB's own diff/patch API (terminusdb diff between commits/branches — research its exact capabilities, JSON diff semantics, and whether it can DRIVE incremental publishing). Recommend one, with schema sketch.
5. Query: GraphQL and WOQL capabilities relevant to code graphs — path queries, transitive closure, cross-branch/cross-db queries (can you query across two packages' graphs? federation?). Performance characteristics.
6. Operational: memory footprint, disk layout, backup, running in k8s, multi-tenancy isolation.

PART B — Admission control for the hot tier:
1. Leaky bucket vs token bucket vs sliding window for "promote a package to the Terminus tier once its query rate sustains above threshold": formalize the promotion rule the user's leaky-bucket suggestion implies (per-package bucket filled by graph-query demand, leaks at constant rate; promotion when bucket crosses threshold; demotion/eviction when it drains).
2. Cache-admission literature as sanity check: TinyLFU/W-TinyLFU (Caffeine), LRU-K, ARC — is a frequency-sketch admission (TinyLFU-style, count-min sketch) strictly better than a leaky bucket here? Compare on: burst resistance, one-hit-wonder rejection, memory cost, implementability in Rust (which crates exist: `governor` for rate limiting is already a dependency; moka's TinyLFU; probabilistic-collections; count-min crates — verify 2026 state).
3. Promotion mechanics: what promotion COSTS (bulk-publish a package's full graph + backfill lineage from prior generations), how to make promotion async and idempotent, demotion (drop branch/db? tombstone?), hysteresis to prevent flapping.
4. The cold path it must beat: graph ops over IR blobs loaded on demand. Frame the economic model: promotion is worth it when (query_cost_cold − query_cost_hot) × rate > amortized_publish_cost.

SYNTHESIS: recommend the concrete design — Terminus topology (db/branch per package?), lineage schema, the admission algorithm with parameters, the Rust crates to use, and the fallback graph-over-blobs contract that both tiers implement (sketch the trait).

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1000+ lines, URLs inline) to /Users/philocalyst/Projects/Backend/.research/librarification/07-terminus-tiering.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) top concrete recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)
