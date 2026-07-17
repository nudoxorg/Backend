# Handoff: Research symbol identity academic
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a47e40599f5b47dcf.jsonl`
- Agent id: `agent-a47e40599f5b47dcf`
- Deliverable: `/Users/philocalyst/Projects/Backend/.research/librarification/05-symbol-identity-academic.md`
- Existing report: False (0 lines)
- Tools used: 1 ({'Agent': 1})
- Files/greps touched: 0
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll conduct exhaustive academic research on program-element identity and matching across versions. Let me start with web searches across all the key areas simultaneously.

### fragment

Research agent is running in the background, doing exhaustive web searches across all 9 topic areas (origin analysis, entity mapping, tree differencing, refactoring detection, clone genealogy, fingerprinting, embedding-based matching, bipartite matching, and evaluation datasets). I'll report back when it completes with the executive summary, recommended pipeline, and open questions.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive ACADEMIC literature research for a frontier architecture decision. Today is 2026-07-16 — verify recency with WebSearch/WebFetch; do not trust training data alone. Cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence platform. Per-language compiler frontends lower package source into a shared IR of typed symbols (functions, types, modules, with signatures, docs, references). Each published version of a package produces a new IR "generation". The platform is becoming deeply INCREMENTAL: when a new generation arrives, only symbols that actually changed should be re-embedded (vector search), re-indexed (tantivy), and re-published (TerminusDB graph, which stores lineage links across generations).

THE RESEARCH QUESTION (the centerpiece of the whole plan): "Under what reasonable interpretation can we say that two symbols, across generations of a package, are THE SAME symbol?" We want the most ambitious defensible answer — a layered identity/matching scheme with graceful degradation: exact identity → renamed → moved → signature-evolved → split/merged → truly new/deleted. False merges are worse than false splits (a wrong lineage link corrupts history; a missed link just loses continuity).

YOUR MISSION — exhaustive academic survey of program-element identity and matching across versions:
1. Origin analysis: Godfrey & Zou "Using origin analysis to detect merging and splitting of source code entities" and successors. What signals (name, signature, body metrics, call relations), what accuracy?
2. Entity mapping/matching literature: UMLDiff (Xing & Stroulia), Kim & Notkin function mapping ("When functions change their names"), S. Kim et al. origin detection, AURA, diff-based matching. Extract each algorithm's features + thresholds + reported precision/recall.
3. Tree differencing: GumTree (Falleri et al.) and modern successors (2020s: hyperparameter-tuned GumTree, MTDiff, IJM, DiffSitter-adjacent work); tree edit distance complexity; how AST diffing yields element-level mappings; language-agnostic operation over tree-sitter CSTs.
4. Refactoring detection: RefactoringMiner (Tsantalis, latest version/state 2026), RefDiff, RefDetect — rename/move/extract/inline detection accuracy; are these Java-only or generalized; what of their statement-level matching transfers to a language-agnostic IR.
5. Clone genealogy / code lineage: clone genealogies (Kim et al.), code provenance/lineage tracking, "code element histories" in MSR (e.g., CodeShovel, CodeTracker — check their 2024-2026 state, accuracy vs git log -L), FinerGit.
6. Fingerprinting: winnowing/MOSS document fingerprints, simhash/minhash for near-duplicate detection, locality-sensitive hashing applied to code, semantic hashing of ASTs, Merkle-tree hashing of normalized ASTs.
7. Embedding-based matching: neural code-similarity (CodeBERT-era → 2026 state) used for element matching across versions — is anyone doing identity-via-embedding-distance and how reliable is it as a TIE-BREAKER (not primary signal)?
8. Bipartite matching formulation: stable matching / Hungarian assignment over similarity matrices for unmatched-set resolution; thresholding strategies to prefer false-split over false-merge; split/merge (1→N, N→1) handling in the literature.
9. Evaluation: what ground-truth datasets exist (manually validated histories); what precision is achievable per tier (name-equal ~100%, rename ~90-95%?, move ~85%?...). Give literature-grounded numbers.

SYNTHESIS REQUIRED: end the report with a concrete recommended matching pipeline for nudox — ordered tiers with the exact features each tier uses (assume available per symbol: fully-qualified path, kind, normalized signature from typed IR, doc text, normalized body tokens from tree-sitter, intra-package reference edges), the matching algorithm per tier, thresholds to start with, expected precision, and how confidence should be recorded on lineage edges (so downstream consumers can filter by confidence).

DELIVERABLE:
- Write the FULL report (dense markdown, 600-1200+ lines, URLs cited inline) to /Users/philocalyst/Projects/Backend/.research/librarification/05-symbol-identity-academic.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) the recommended tier pipeline as bullets, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)
