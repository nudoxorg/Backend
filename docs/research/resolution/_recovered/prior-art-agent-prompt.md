You are doing deep external research (WebSearch/WebFetch encouraged, plus your own knowledge) for a design decision in a documentation-backend project. Do NOT explore the local repo beyond what's stated here — your job is prior art and design theory.

**The problem**: The project compiles packages in 7 languages (Rust, TypeScript, Python, Go, Java, C#, Nix) into a language-neutral semantic documentation IR via per-language compiler "oracles" (rust-analyzer, OXC/TS, Pyrefly, go/types, javac doclet, Roslyn, snix). Separately it has a tree-sitter layer with per-language declarative specs that extracts identifier occurrences (definitions/references/imports) from raw source. The goal: take the tree-sitter representation of an ENTIRE ARBITRARY CODEBASE (e.g., a downstream consumer repo, example folders, test suites) and RESOLVE every occurrence to symbols in the semantic IR — to power "example usage of this function", "instances/implementations of this type", cross-repo references. It must scale to any future language with bounded per-language effort, and must work when the consumer codebase cannot be compiled (missing deps, partial code, snippets in docs).

Research and report in exhaustive depth on:

1. **Stack graphs / scope graphs** (GitHub's stack-graphs Rust crate, Douglas Creager's work; Eelco Visser's scope graph theory, Néron/van Antwerpen papers). How do they achieve language-agnostic incremental name resolution from tree-sitter parses via declarative .tsg (tree-sitter-graph) rules? What are the per-language authoring costs, which languages GitHub shipped (and which they never managed), why "precise code navigation" via stack graphs was hard for some languages (Python vs TypeScript vs Java), current maintenance status of the stack-graphs project (is it abandoned/archived as of 2025-2026?), and its handling of: imports/re-exports, method receivers requiring type info, overloads, generics. What are honest limits — what CAN'T scope graphs resolve without type inference?

2. **SCIP and LSIF** (Sourcegraph): the SCIP symbol grammar (scheme, package, descriptors — exact syntax and how it encodes disambiguators/overloads), scip-* indexers per language (which reuse real compilers: scip-java/semanticdb, scip-typescript, scip-python, scip-clang...), how cross-repository resolution works (symbol strings as join keys against package registries), and how Sourcegraph blends "precise" (compiler-based) with "search-based" (ctags-like heuristic) code intel including their fallback ranking. Also: SCIP's approach to "find usages across all of open source" and how docs.rs / grep.app / cs.opensource.google / Sourcegraph implement "usage examples" surfacing.

3. **Kythe and Glean** (Google/Meta): schema-level design of a language-neutral node/edge store for cross-references; VName identity scheme in Kythe; Glean's Angle schemas and per-language indexers; what these teach about symbol-identity design for a multi-language IR join.

4. **Tree-sitter native facilities**: locals queries (@local.definition/@local.reference/@local.scope) — real capabilities and limits; tags queries (tree-sitter tags for ctags-style def/ref); tree-sitter-graph DSL; the tree-sitter "highlight + locals" resolution model. How far can pure syntax get for each of: local variables, file-level scope, imports, member access, method calls on typed receivers?

5. **The "oracle re-use" alternative**: instead of syntax-only resolution, run each language's real resolver over the consumer codebase (rust-analyzer for Rust, Pyrefly/pyright for Python, tsserver/OXC resolver for TS, gopls/go-types for Go, Roslyn workspaces for C#, javac for Java) and map results into the IR. Research: which of these work acceptably on BROKEN/PARTIAL code (rust-analyzer and Roslyn are famously error-tolerant; what about go/types, javac?), on code without dependencies fetched, and headless/batch cost. Also research "compile-free" typed resolution efforts: pyright's stub-based inference, tsc's noResolve modes, Java's ECJ partial binding recovery, tree-sitter+type-stub hybrids.

6. **Hybrid/tiered designs**: prior art for a confidence-tiered resolution pipeline: tier 1 exact (compiler oracle), tier 2 scope-graph precise-syntactic, tier 3 heuristic global-name match (import-anchored), tier 4 text/token match — with confidence stored per edge. GitHub's actual shipped design ("fuzzy" jump-to-def via tree-sitter tags + "precise" via stack-graphs for some languages) and Sourcegraph's precise+fallback blend are the canonical examples; get details on how they rank/merge and how they present uncertainty in the UI. Also look at how SourceGraph/GitHub handle version skew (consumer repo depends on foo@1.2 but index has foo@2.0).

7. **Usage-example mining specifically**: prior art on extracting GOOD example snippets (not just occurrence lists): e.g., "examples" panels in docs.rs (how are they generated?), Codota/TabNine's example mining, API usage-pattern mining literature (MAPO, UP-Miner style), how to pick representative call-sites (dedup by AST shape, prefer test/doc code, window extraction around the call).

Return a LONG structured report (data for planning, not a summary): per topic — how it works, concrete formats/grammars (quote the SCIP symbol grammar, tsg rule snippets), per-language effort estimates, honest failure modes, maintenance/ecosystem status as of 2025-2026 with sources, and a final section: "design implications for a 7-language treesitter→IR resolver" as a bulleted list of hard-won lessons (e.g. 'nobody resolves method receivers without type info; every system that tried X did Y'). Cite URLs for non-obvious claims.

===== PROMPT =====

Run the "deep-research" workflow.

Deep research harness — fan-out web searches, fetch sources, adversarially verify claims, synthesize a cited report.

When the user wants a deep, multi-source, fact-checked research report on any topic. BEFORE invoking, check if the question is specific enough to research directly — if underspecified (e.g., "what car to buy" without budget/use-case/region), ask 2-3 clarifying questions to narrow scope. Then pass the refined question as args, weaving the answers in.

Phases:
- Scope: Decompose question (from args) into 5 search angles
- Search: 5 parallel WebSearch agents, one per angle
- Fetch: URL-dedup, fetch top 15 sources, extract falsifiable claims
- Verify: 3-vote adversarial verification per claim (need 2/3 refutes to kill)
- Synthesize: Merge semantic dupes, rank by confidence, cite sources

Invoke: Workflow({ name: "deep-research", args: "Deep research on name resolution systems for documentation backends: stack graphs, SCIP/LSIF, Kythe/Glean, tree-sitter locals/tags, oracle-based resolution, hybrid tiered pipelines, and usage-example mining. The system compiles packages in 7 languages (Rust, TypeScript, Python, Go, Java, C#, Nix) into a language-neutral semantic IR, then needs to resolve tree-sitter occurrences in arbitrary consumer codebases (which may not compile) to symbols in that IR.\n\nResearch exhaustively on:\n\n1. Stack graphs / scope graphs: GitHub's stack-graphs Rust crate, Douglas Creager's work, Eelco Visser's scope graph theory. How do they achieve language-agnostic incremental name resolution from tree-sitter parses via declarative .tsg rules? Per-language authoring costs, which languages GitHub shipped, why it was hard for Python vs TypeScript vs Java, current maintenance status (is it abandoned/archived as of 2025-2026?), handling of imports/re-exports/method receivers/overloads/generics. What CAN'T scope graphs resolve without type inference?\n\n2. SCIP and LSIF (Sourcegraph): the SCIP symbol grammar exact syntax (scheme, package, descriptors, disambiguators/overloads), scip-* indexers per language (which reuse real compilers), how cross-repository resolution works, how Sourcegraph blends \"precise\" vs \"search-based\" code intel, fallback ranking. SCIP's approach to \"find usages across all of open source\" and how docs.rs/grep.app/Sourcegraph implement usage examples.\n\n3. Kythe and Glean (Google/Meta): VName identity scheme in Kythe, Glean's Angle schemas and per-language indexers, what these teach about symbol-identity design.\n\n4. Tree-sitter native facilities: locals queries (@local.definition/@local.reference/@local.scope), tags queries, tree-sitter-graph DSL. How far can pure syntax get for: local variables, file-level scope, imports, member access, method calls on typed receivers?\n\n5. Oracle re-use alternative: which language oracles work on broken/partial code (rust-analyzer, Roslyn error-tolerance, go/types, javac, ECJ), compile-free typed resolution (pyright stub-based, tsc noResolve modes, Java ECJ partial binding recovery).\n\n6. Hybrid/tiered designs: confidence-tiered resolution pipeline (tier 1 exact compiler, tier 2 scope-graph, tier 3 heuristic import-anchored, tier 4 text match). GitHub's shipped design (fuzzy tree-sitter tags + precise stack-graphs), Sourcegraph's precise+fallback blend, how they rank/merge/present uncertainty. Version skew handling.\n\n7. Usage-example mining: docs.rs examples panel generation, Codota/TabNine mining, MAPO/UP-Miner style API usage pattern literature, picking representative call-sites (dedup by AST shape, prefer test/doc code, window extraction).\n\nReturn a LONG structured report with: how each system works, concrete formats/grammars (quote the SCIP symbol grammar, tsg rule snippets), per-language effort estimates, honest failure modes, maintenance/ecosystem status as of 2025-2026 with source URLs, and a final section \"design implications for a 7-language treesitter→IR resolver\" as bulleted hard-won lessons." })

===== PROMPT =====

[SYSTEM NOTIFICATION - NOT USER INPUT]
This is an automated background-task event, NOT a message from the user.
Do NOT interpret this as user acknowledgement, confirmation, or response to any pending question.

<task-notification>
<task-id>ab2ed38334b9ddea0</task-id>
<tool-use-id>toolu_01DP2iz8gEqj1K1sfcCiwoZ6</tool-use-id>
<output-file>/private/tmp/claude-501/-Users-philocalyst-Projects-Backend/17cc2883-6b32-44ea-abc5-c1c9f07d0bf2/tasks/ab2ed38334b9ddea0.output</output-file>
<status>completed</status>
<summary>Agent "Search angle 4: Tree-sitter native facilities and oracle error-tolerance" came to rest</summary>
<note>A task-notification fires each time this agent comes to rest with no live background children of its own. The user can send it another message and resume it, so the same task-id may notify more than once.</note>
<result>You've hit your session limit · resets 9:20pm (America/New_York)</result>
<usage><subagent_tokens>1477</subagent_tokens><tool_uses>41</tool_uses><duration_ms>182418</duration_ms></usage>
</task-notification>