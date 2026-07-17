# Handoff: Research GUI references and views
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a6dc766403742dddc.jsonl`
- Agent id: `agent-a6dc766403742dddc`
- Deliverable: `/Users/philocalyst/Projects/Backend/.research/librarification/15-gui-references.md`
- Existing report: False (0 lines)
- Tools used: 1 ({'ToolSearch': 1})
- Files/greps touched: 0
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll launch a deep research agent to handle this exhaustive multi-topic investigation in parallel threads.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent doing exhaustive external research for a major architecture plan. Today is 2026-07-16 — verify current status with WebSearch/WebFetch; cite URLs inline.

CONTEXT: nudox is a multi-language code-intelligence / documentation platform. Its desktop GUI (crate "lindsey", built on GPUI — Zed's UI framework — plus the longbridge gpui-component widget library) is becoming THE start and end of nudox for developers: the single interface where a developer assigns "projects" (their own code, trusted, compiled locally) whose third-party dependencies are delegated to a remote INDEX and progressively synced to a local store. Embedded inside the GUI process: tantivy text search, vector search, a local sqlite catalog, IR blobs + source + tree-sitter trees on disk. The pipeline is incremental (per-commit symbol-level updates) and symbols have LINEAGE across versions. The GUI needs a FULL refactor with new views. Current views (small prototype): graph_view, symbol_view, search_panel, local_index_panel, log_panel, settings.

YOUR MISSION — exhaustive research for the GUI refactor:
1. Reference products — study each deeply for information architecture, navigation model, and offline/storage design: DevDocs (devdocs.io — docset format, offline IndexedDB storage, fuzzy search UX, keyboard-first nav), Dash (macOS — docset ecosystem, sqlite docset format, search ranking, annotations/snippets), Zeal, Velocity; JetBrains Quick Documentation + external doc viewers; Xcode documentation browser (DocC archives — their JSON render format is interesting prior art for IR→rendered docs!); rustdoc's generated site + docs.rs UX (search index format, source view, "jump to def" links); Hoogle (type-directed search UX!) — nudox has typed IR, type-directed search is a differentiator; Sourcegraph's UI (code nav, references panel); GitHub code view (symbols sidebar, fuzzy file finder). For each: what to copy, what to avoid.
2. The view catalog for nudox — design the full set with wireframe-level ASCII sketches and interaction notes: (a) Project manager / onboarding (pick a project dir → detect workspace/path deps → trust boundary visualization: THIS is trusted, THESE 247 deps are delegated; per-dep sync status), (b) omni-search (cmd-K: fuzzy symbol search fusing text+vector+type-directed results, as-you-type <10ms local, filters by language/package/kind), (c) symbol page (rendered signature + docs + source snippet via tree-sitter + references/callers + implementations + VERSION TIMELINE from lineage: "this function changed in v2.3: signature changed" — a lineage/history view no competitor has), (d) package browser (module tree, API surface, version picker, diff-between-versions view driven by symbol lineage), (e) dependency graph view (exists as prototype), (f) sync/jobs dashboard (what's syncing, what's compiling locally, incremental pipeline status per commit), (g) settings (trust, storage quota/GC, remote endpoint, model for embeddings), (h) anything the references suggest we're missing (annotations? playgrounds? pinning?). Prioritize them.
3. GPUI + gpui-component capability check (verify 2026 state from repos: zed-industries/zed gpui, longbridge/gpui-component): available widgets (virtualized lists/tables, tree view, tabs/docks, input, markdown rendering?, code editor component with syntax highlighting? webview escape hatch?), text rendering with syntax highlighting in GPUI (Zed does it — what's reusable as a crate in 2026), how Zed structures a multi-pane workspace app in GPUI (workspace/pane/item patterns to imitate), async data loading patterns (Entity + spawn + notify), theming. Identify the gaps where nudox must build custom widgets (graph canvas? timeline?) and the cost.
4. Architecture for the refactor: recommended module structure (a store/model layer wrapping the `client` library with GPUI Entities; view modules per surface; a command palette/action system GPUI-style; navigation/history (back/forward like a browser — Dash/DevDocs teach this)), keyboard-first design (GPUI keymaps), and how search stays <10ms (debounce, local index, streaming results into a virtualized list).

SYNTHESIS: the prioritized view roadmap (MVP set vs later), the module structure, the widget gap list with build-vs-adopt calls, and the top UX principles distilled from references (with the lineage/timeline + type-directed search differentiators fleshed out most).

DELIVERABLE:
- Write the FULL report (dense markdown, 500-1200 lines, URLs inline) to /Users/philocalyst/Projects/Backend/.research/librarification/15-gui-references.md
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) the view roadmap + top recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)
