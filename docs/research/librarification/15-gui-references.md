# GUI References & View Roadmap — nudox / lindsey

> Research date: 2026-07-16  
> Scope: external documentation-browser UX prior art; GPUI / gpui-component 2026 capabilities; full view catalog for the lindsey refactor  
> Companion audit: [03-gui-audit.md](./03-gui-audit.md) (`workspace/gui`, crate `lindsey`)  
> Live code: `workspace/gui/src/` (12 files; GPUI + longbridge gpui-component)

---

## 0. Framing

nudox is becoming a multi-language code-intelligence / documentation platform. The desktop GUI (crate **lindsey**, GPUI + [longbridge/gpui-component](https://github.com/longbridge/gpui-component)) is intended to be **the** start and end of the product for developers:

1. Assign a **project** (local code, trusted, compiled in-process).
2. Third-party dependencies are **delegated** to a remote INDEX and progressively synced into a local store.
3. Inside the GUI process: Tantivy text search, vector search, SQLite catalog, IR blobs + source + tree-sitter trees on disk.
4. Pipeline is **incremental** (per-commit, symbol-level) with **symbol lineage** across versions.

Current prototype views (`workspace/gui/src/`): `graph_view`, `symbol_view`, `search_panel`, `local_index_panel`, `log_panel`, `settings` — all wired by a monolithic `Workspace` with hardcoded flex layout, fixture data in search, and a remote-only `BackendClient` (see 03-gui-audit).

This report answers four questions:

1. What do the best documentation / code-intelligence UIs teach us (copy / avoid)?
2. What is the full view catalog for nudox, with wireframes and priorities?
3. What can GPUI + gpui-component already do in mid-2026, and where must we build custom widgets?
4. How should the refactor be structured (stores, navigation, keyboard-first, <10ms search)?

---

## 1. Reference products — deep study

### 1.1 DevDocs (devdocs.io)

**What it is.** Free, open-source, browser-based multi-docset documentation browser. Maintained under freeCodeCamp ([github.com/freeCodeCamp/devdocs](https://github.com/freeCodeCamp/devdocs)). Live product: [https://devdocs.io/](https://devdocs.io/).

**Information architecture**

- Left: searchable entry list scoped to enabled docsets.
- Center: rendered HTML documentation page.
- Top / command: global fuzzy search across all enabled docsets.
- Settings: enable/disable docsets; offline download of indexes + pages.
- No multi-window workspace; one page at a time with browser-like history.

**Navigation model**

- Keyboard-first: `?` shows shortcuts; arrow keys walk results; Enter opens; Esc clears.
- Fuzzy matching: e.g. `bgcp` → `background-clip` ([devdocs.io](https://devdocs.io/)).
- Scope-to-docset: type docset name/abbreviation then Tab to restrict search.
- Address-bar search integration (browser omnibox).
- Mobile + PWA install path.

**Offline / storage design**

- Downloads documentation into browser storage (IndexedDB object stores for indexes and page bodies).
- Historically: index could be offline while page bodies still required network (older SitePoint writeups); modern offline mode claims full offline for selected docsets ([SitePoint overview](https://www.sitepoint.com/look-devdocs-io/), [devdocs.io](https://devdocs.io/)).
- Storage is **evictable** by the browser under pressure — unsuitable as a model for a native app's durable catalog (see MDN IndexedDB eviction notes).

**What to copy**

| Pattern | Why it matters for nudox |
|---------|--------------------------|
| Instant fuzzy search as primary surface | Search-first is non-negotiable; DevDocs users almost never browse trees |
| Tab-to-scope | Map to package / language / kind filters in omni-search |
| Keyboard-only loop | GPUI keymaps can match this 1:1 |
| Progressive offline of chosen docsets | Mirrors remote INDEX → local store progressive sync |
| Clean empty states ("enable a docset") | Project onboarding should feel like enabling docsets |

**What to avoid**

- Single-page-at-a-time with no tabs — developers need multiple symbols open.
- Browser-evictable storage for the primary catalog.
- HTML-as-source-of-truth without a structured IR underneath (no type-directed search, no lineage).
- No project/trust model — DevDocs is pure third-party docs.

---

### 1.2 Dash (macOS) — Kapeli

**What it is.** The gold-standard offline API documentation browser + snippet manager for macOS. Product: [https://kapeli.com/dash](https://kapeli.com/dash). Docset generation guide: [https://kapeli.com/docsets](https://kapeli.com/docsets). User guide: [https://kapeli.com/dash_guide](https://kapeli.com/dash_guide).

**Information architecture**

- Global hotkey → search field (fuzzy, instant).
- Results grouped by docset / type (Class, Method, Function, …).
- Detail pane renders local HTML from the selected docset.
- Sidebar: installed docsets, search profiles (group docsets by task: "iOS work", "backend", …).
- Snippets manager (orthogonal product surface; high stickiness).
- IDE integrations (Xcode, JetBrains, VS Code, Alfred, Raycast, …).

**Docset format (load-bearing prior art)**

Docsets are Apple Documentation Set–inspired bundles:

```
MyAPI.docset/
├── icon.png
└── Contents/
    ├── Info.plist
    └── Resources/
        ├── Documents/          # HTML pages
        └── docSet.dsidx        # SQLite search index
```

SQLite schema (canonical, from Kapeli docs):

```sql
CREATE TABLE searchIndex(
  id INTEGER PRIMARY KEY,
  name TEXT,
  type TEXT,
  path TEXT
);
CREATE UNIQUE INDEX anchor ON searchIndex (name, type, path);
```

- `name` — search target (class name, method, etc.)
- `type` — one of ~70 Dash types (Class, Method, Function, Trait, Module, …) — [full list](https://kapeli.com/docsets#supportedentrytypes)
- `path` — relative HTML path, optional `#anchor`, or even `http://` URL

Optional: `//apple_ref/cpp/Type/Name` anchors for in-page TOC; `DashDocSetFamily=dashtoc` in Info.plist.

**Other plist knobs:** `dashIndexFilePath` (default page), `DashDocSetFallbackURL` (online redirect), `DashDocSetPlayURL` (playground deep-link, v4+), `DashDocSetDefaultFTSEnabled`, `isJavaScriptEnabled`, plus feed XML (`<version>` + `<url>`, `dash-feed://` scheme). Dash natively opens **Swift DocC archives** and generates docsets from Sphinx, Javadoc, rustdoc, Haddock, GoDoc, etc. ([docsets guide](https://kapeli.com/docsets)).

**What to copy**

| Pattern | nudox mapping |
|---------|---------------|
| SQLite name/type/path index | Local SQLite catalog is already planned; adopt Dash-like type taxonomy for kind filters |
| Instant offline fuzzy search | Tantivy + SQLite prefix should beat Dash latency for symbol names |
| Search profiles | "Search scopes": current project / trusted deps / all synced packages |
| Global hotkey → search | `cmd-K` / `cmd-shift-O` omni-search |
| Snippets / annotations | Later: user notes pinned to symbols (lineage-aware) |
| Docset feeds + version | Remote INDEX package feeds; version pins on packages |
| Playgrounds URL | Optional "Try in playground" for languages that support it |
| IDE integration surface | Deep-links from Zed / VS Code / JetBrains into nudox symbol pages |

**What to avoid**

- HTML-centric storage as primary (nudox has structured IR; HTML is a render target, not the IR).
- Flat type string with no cross-version identity — Dash has no lineage.
- macOS-only assumptions; we ship GPUI cross-platform.
- Snippet manager as v1 scope creep.

---

### 1.3 Zeal

**What it is.** Open-source offline documentation browser (Windows / Linux / macOS). Site: [https://zealdocs.org/](https://zealdocs.org/). Source: [https://github.com/zealdocs/zeal](https://github.com/zealdocs/zeal). Uses **Dash-compatible docsets** (Kapeli feeds).

**IA / navigation**

- Simpler than Dash: docset list, search bar, HTML viewer (Qt WebEngine).
- Fuzzy search over the same SQLite indexes.
- Fewer polish features (no snippets, weaker IDE integration, less refined ranking).

**What to copy**

- Cross-platform consumption of a shared docset ecosystem — validation that a **format** (not a product) can be the interchange layer.
- Lightweight footprint: search + render is enough for a pure docs browser.

**What to avoid**

- Qt WebEngine dependency for every page (nudox should render from IR → native GPUI, not ship a browser).
- "Thin Dash clone" product positioning — nudox's differentiators are project trust, lineage, type-directed search, and embedded indexes.

---

### 1.4 Velocity (Windows)

**What it is.** Paid Windows documentation browser using Dash docsets. Site: [https://velocity.silverlakesoftware.com/](https://velocity.silverlakesoftware.com/). Often listed as discontinued / freemium on alternative directories (2026 status: niche, not innovating).

**Takeaway.** Proves the docset format is the durable asset; the viewer apps are interchangeable. nudox should **not** implement Dash docset import as a primary feature in MVP, but the SQLite name/type/path pattern is the right mental model for the local catalog. Optional later: import user-contributed docsets as a fallback when nudox IR is missing for a language.

---

### 1.5 JetBrains Quick Documentation + external docs

**Sources:** [Viewing reference information](https://www.jetbrains.com/help/idea/viewing-reference-information.html), [Documentation tool window](https://www.jetbrains.com/help/idea/documentation-tool-window.html), [Quick Documentation guides](https://www.jetbrains.com/guide/java/tips/quick-documentation/).

**Model**

| Layer | UX | Latency expectation |
|-------|-----|---------------------|
| Quick Documentation (F1 / Ctrl-Q) | Popup / tool window: signature + short docs + links | Instant, local PSI |
| External documentation (Shift-F1) | Opens browser to configured URL (JDK, library sites) | Network |
| Downloaded docs | Some SDKs ship local HTML jars | Instant offline |

**What to copy**

- **Two-tier docs:** "quick" (always-on panel / sidebar) vs "full" (symbol page tab).
- Keep documentation **pinned** while navigating code (tool window, not only popup).
- External doc URL templates per library (fallback when IR incomplete).

**What to avoid**

- Making the browser the primary long-form experience (we own the render).
- Coupling docs quality to IDE indexing quality alone — nudox's remote INDEX + local IR should outlive any one editor.

---

### 1.6 Xcode / Swift-DocC archives

**Sources:** [swift-docc-render](https://github.com/swiftlang/swift-docc-render), Apple DocC WWDC sessions, [Distributing documentation](https://developer.apple.com/documentation/xcode/distributing-documentation-to-other-developers).

**Architecture (critical prior art for IR → rendered docs)**

```
Source + Markdown catalogs
        │
        ▼
   Swift-DocC compiler
        │
        ▼
  .doccarchive/                    # machine-readable bundle
    ├── data/documentation/...     # Render JSON per page
    ├── index/                     # navigator / search index
    ├── metadata.json
    └── ...
        │
        ▼
  Swift-DocC-Render (Vue SPA)      # pure presentation over JSON
```

DocC-Render is a Vue SPA that **does not embed content**; it fetches Render JSON from the archive and produces the documentation UI ([README](https://github.com/swiftlang/swift-docc-render)). Themes via `theme-settings.json` (colors, typography, light/dark).

**What to copy**

| Pattern | nudox mapping |
|---------|---------------|
| IR/archive first, renderer second | nudox IR blobs on disk; lindsey is a renderer, not a docs compiler |
| Per-page JSON render nodes | Symbol page model: signature, sections, relationships, code snippets as typed nodes |
| Navigator index separate from page bodies | SQLite/Tantivy index ≠ full SymbolEntry payloads |
| theme-settings as data | Align with gpui-component Theme tokens |
| Tutorials + articles + API refs in one archive | Guides/notes alongside symbols later |

**What to avoid**

- Requiring a web SPA for native desktop (we render in GPUI).
- Apple-only archive packaging — design our own content-addressed IR layout (see storage research).

**Implication.** DocC validates the split: **compile once to structured render data, open many times in any client**. nudox already plans IR blobs; the symbol page should consume a **RenderModel** similar in spirit to DocC Render JSON, not ad-hoc `serde_json::Value` forever (`SymbolEntry.kind: Value` is a known fragility in 03-gui-audit §9.8).

---

### 1.7 rustdoc + docs.rs

**Sources:** [docs.rs](https://docs.rs/), [rustc-dev-guide rustdoc search](https://rustc-dev-guide.rust-lang.org/rustdoc-internals/search.html), [RFC 2963 rustdoc JSON](https://rust-lang.github.io/rfcs/2963-rustdoc-json.html), [docs.rs rustdoc-json](https://docs.rs/about/rustdoc-json), [rustdoc-types crate](https://docs.rs/rustdoc-types/).

**UX patterns**

- Top search bar; static `search-index` loaded client-side (no server search for a single crate's docs).
- Item pages: signature with **clickable types**, sections (fields, methods, trait impls), source links.
- Sidebar: module tree / in-page TOC.
- docs.rs adds version bar + "go to latest" when not on latest ([user discussion](https://users.rust-lang.org/t/standard-library-documentation-on-doc-rust-lang-org-hard-to-navigate/69328)).
- Jump-to-definition in source pages (rustdoc feature; docs.rs metadata supports `rustdoc-args`).

**Search index (simplified mental model from rustc-dev-guide)**

Generated by `search_index.rs`, consumed by `search.js`. Compact parallel arrays / bitmaps for names, types, parent modules, function signatures (type fingerprints), aliases, deprecation flags. Function signatures are encoded for **type-aware ranking** even in the HTML searcher — a weak form of Hoogle-style matching inside rustdoc.

**rustdoc JSON**

Unstable `--output-format=json` produces structured crate docs. docs.rs has hosted rustdoc JSON since **2025-05-23** ([docs.rs/about/rustdoc-json](https://docs.rs/about/rustdoc-json)). `rustdoc-types` crate deserializes with `format_version` negotiation.

**What to copy**

| Pattern | nudox |
|---------|-------|
| Clickable types in signatures | Hyperlink every type path in SymbolView |
| Version bar + "latest" CTA | Package browser + symbol page version picker |
| Module sidebar | Package browser tree |
| Source deep-link with line anchors | Open local file or editor URI |
| Type info in search index | Feed type-directed search from IR, not just names |
| format_version on IR | Avoid dual `@type` / `_type` schema drift |

**What to avoid**

- Shipping a giant per-crate JS search index into the GUI process (use Tantivy/SQLite instead).
- HTML-only docs without JSON/IR for tooling.
- docs.rs's full-page navigation (prefer tabs + back/forward stack).

---

### 1.8 Hoogle — type-directed search UX

**Sources:** [hoogle.haskell.org](https://hoogle.haskell.org/), [ndmitchell/hoogle](https://github.com/ndmitchell/hoogle), [Hoogle 5 search overview](http://neilmitchell.blogspot.com/2020/06/hoogle-searching-overview.html).

**Core idea.** Search APIs by **name or approximate type signature**:

```
map
(a -> b) -> [a] -> [b]
+bytestring concat
```

**Hoogle 5 search algorithm (summary)**

1. Preprocess all Stackage/Hackage signatures into a compact database.
2. Deduplicate types; each type → **18-byte fingerprint**.
3. ~150K distinct signatures ≈ 2.5MB fingerprints → linear scan is fine.
4. Top-K fingerprints refined with precise type matching and ranking.
5. Name search is separate but combinable (`+package` filters).

**What to copy (HIGH differentiator for nudox)**

nudox has **typed IR**. Type-directed search is rare outside Haskell and is a product wedge no Dash/DevDocs clone has.

| UX detail | Implementation note |
|-----------|---------------------|
| Detect type-query vs name-query | Heuristic: contains `->`, `=>`, generics, or structured type tokens from IR |
| Show signature prominently in results | Result rows: name + **normalized type** + package + score |
| Approximate matching | Unify with renamings, argument reordering penalties, subtype freeness |
| Package filters | `+serde` / UI chip = same as Hoogle `+pkg` |
| Explain ranking | Optional "why this match" for power users |

**What to avoid**

- Requiring users to write perfect Haskell-like syntax for every language — provide a **type builder** (pick return type, add args) for lower languages.
- O(n) full scans forever — fine at 150K, plan inverted indexes when multi-language corpus hits millions of signatures.

---

### 1.9 Sourcegraph

**Sources:** [Code Navigation docs](https://docs.sourcegraph.com/user/code_intelligence), [Cross-repo navigation blog](https://sourcegraph.com/blog/cross-repository-code-navigation) (Jan 2026), [Deep Search + code nav](https://sourcegraph.com/changelog/deep-search-code-navigation) (Feb 2026).

**UX patterns**

| Feature | Behavior |
|---------|----------|
| Hover | Signature + short docs without leaving file |
| Go to definition | Jump (precise SCIP/LSIF or search-based fallback) |
| Find references | Bottom/side panel listing all refs; precise + search-based mixed |
| Implementations | For interfaces / traits |
| Cross-repo | References across dependency boundaries |
| Search sidebar preview | Code nav inside search result previews (2026) |

**What to copy**

- **References panel** as a first-class region of the symbol page (not a separate app mode).
- Mixed precision: show "precise" vs "textual" badges when graph data is incomplete.
- Keep search context visible while navigating (Deep Search sidebar stays open).
- Cross-package references are the value of a monorepo/index product — nudox's remote INDEX enables this for deps.

**What to avoid**

- Enterprise search syntax complexity in MVP omni-search.
- Building a full code host; we are a docs + intelligence shell over projects + INDEX.

---

### 1.10 GitHub code view

**Sources:** [GitHub blog — new code search/browse (2022)](https://github.blog/changelog/2022-11-09-introducing-an-all-new-code-search-and-code-browsing-experience/), community discussions on symbols panel.

**UX patterns**

- Left: file tree pane (browse without full page reloads).
- Center: blob view with sticky headers.
- Right: **symbols pane** for current file.
- Fuzzy file finder; symbol search.
- Click symbol → definition / references UI.

**What to copy**

- Three-column mental model: navigate | content | outline/symbols.
- Sticky context headers while scrolling long pages.
- Fuzzy file finder is separate from symbol search (cmd-P vs cmd-T in editors) — nudox should separate **file/path** find from **symbol** find in the command palette.

**What to avoid**

- Symbols panel that only understands the open file (nudox is cross-package by design).
- Web full-page navigations; we use tabs + history stack.

---

### 1.11 Cross-reference matrix

| Capability | DevDocs | Dash | Zeal | JetBrains | DocC | rustdoc | Hoogle | Sourcegraph | GitHub | **nudox target** |
|------------|---------|------|------|-----------|------|---------|--------|-------------|--------|------------------|
| Offline multi-lib | ✓ | ✓✓ | ✓ | partial | archive | per-crate | local | cloud | cloud | ✓✓ progressive |
| Fuzzy name search | ✓✓ | ✓✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓✓ | ✓ | ✓✓ <10ms |
| Type-directed search | — | — | — | weak | — | weak | ✓✓ | — | — | ✓✓ differentiator |
| Version timeline / lineage | — | — | — | VCS blame | — | version bar | — | blame | blame | ✓✓ differentiator |
| Project + trust boundary | — | — | — | project model | — | — | — | multi-repo | repo | ✓✓ core |
| Structured IR render | HTML | HTML | HTML | PSI | JSON | HTML+JSON | text | SCIP | tree-sitter | IR native |
| Keyboard-first | ✓✓ | ✓✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | weak | ✓✓ |
| References panel | — | — | — | ✓ | — | source | — | ✓✓ | ✓ | ✓ |
| Snippets / notes | — | ✓✓ | — | bookmarks | — | — | — | — | — | later |
| Graph / explore | — | — | — | hierarchy | — | — | — | — | — | ✓ prototype |

---

## 2. View catalog for nudox

### 2.1 Priority tiers

| Tier | Views | Goal |
|------|-------|------|
| **MVP (P0)** | Project manager, Omni-search, Symbol page (v1), Settings (core), Jobs/logs (v1) | Usable daily driver for one Rust project + synced deps |
| **P1** | Package browser, Sync dashboard polish, Symbol lineage timeline, References panel | Differentiated product |
| **P2** | Dependency graph (enhanced), Type-directed search UX polish, Diff-between-versions | Power users |
| **P3** | Annotations/notes, Playgrounds, Pinning/collections, Docset import | Stickiness |

---

### 2.2 (a) Project manager / onboarding — P0

**Purpose.** Assign a local project directory; detect workspace / path dependencies; visualize **trust boundary** (local trusted vs remote-delegated deps); show per-dep sync status.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ Nudox                                              [cmd-K Search]  ⚙    │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   Welcome / Projects                                                     │
│   ┌────────────────────────────────────────────────────────────────┐   │
│   │  Open project…          [ ~/Projects/my-app          ] [Browse] │   │
│   │  Recent:  my-app · backend-svc · playground                      │   │
│   └────────────────────────────────────────────────────────────────┘   │
│                                                                          │
│   Project: my-app                                    trust: LOCAL ✓      │
│   root: /Users/you/Projects/my-app                                       │
│   manifest: Cargo.toml (workspace: 3 members)                            │
│                                                                          │
│   Trust boundary                                                         │
│   ┌─ TRUSTED (compile locally) ──────────────────────────────────────┐ │
│   │  ● my-app            path .                                      │ │
│   │  ● my-app-core       path crates/core                            │ │
│   │  ● my-app-api        path crates/api                             │ │
│   └──────────────────────────────────────────────────────────────────┘ │
│   ┌─ DELEGATED (remote INDEX → local sync) ──────────── 247 packages ─┐ │
│   │  status filter: [All ▾] [Syncing] [Ready] [Error]                  │ │
│   │  ● serde        1.0.210   ████████░░  ready     12.4 MB           │ │
│   │  ● tokio        1.40.0    ███░░░░░░░  syncing    3.1 / 8.0 MB     │ │
│   │  ● axum         0.7.5     ░░░░░░░░░░  queued                      │ │
│   │  ● hyper        1.4.1     ████████░░  ready                        │ │
│   │  … virtualized list …                                              │ │
│   └────────────────────────────────────────────────────────────────────┘ │
│                                                                          │
│   [ Open workspace ]     [ Sync all delegated ]     [ Trust settings ]   │
└──────────────────────────────────────────────────────────────────────────┘
```

**Interactions**

- Detect package manager on open (Cargo, npm, go.mod, …) via client library.
- Click dep → package browser for that crate (even if still syncing: show partial).
- Right-click: pin version, force re-sync, mark as trusted (dangerous; confirm).
- Progress rows stream from `JobStore` / `RegistryStore`.
- Empty first-run: single CTA "Open a project" — no fixture axum data (03-gui-audit §5).

**State entities.** `Entity<ProjectStore>`: `{ path, members, trusted: HashSet, deps: Vec<DepState> }`.

---

### 2.3 (b) Omni-search (cmd-K) — P0

**Purpose.** Fuse text (Tantivy) + vector + type-directed results; as-you-type **<10ms local**; filters by language / package / kind.

```
┌─────────────────────────────────────────────────────────────┐
│  🔍  map_err                                      esc close │
│  scope: [Project ▾] [Deps ▾]  kind: [All ▾]  lang: [Rust]  │
│  mode: ● Auto  ○ Name  ○ Type  ○ Semantic                   │
├─────────────────────────────────────────────────────────────┤
│  TYPE-DIRECTED                                         3 ms │
│    Result<T,E> → (E→F) → Result<T,F>                        │
│    ● Result::map_err          core   1.0   fn               │
│    ● map_err (futures)        futures 0.3  fn               │
├─────────────────────────────────────────────────────────────┤
│  NAME / FUZZY                                          2 ms │
│    ● axum::response::Result::map_err    axum 0.7   method   │
│    ● anyhow::Error::map_err?            —          —        │
├─────────────────────────────────────────────────────────────┤
│  SEMANTIC (vector)                                    18 ms │
│    ● convert error types while preserving Ok branch         │
│      … map_err on Result …                                  │
├─────────────────────────────────────────────────────────────┤
│  ↑↓ open  ⏎ open tab  ⌘⏎ split  tab cycle sections  ⌥Enter │
└─────────────────────────────────────────────────────────────┘
```

**Interaction notes**

- Debounce: 16–32ms for local indexes; do **not** wait for vector/remote before showing Tantivy hits.
- Stream results into a **virtualized list** (gpui-component List/Table).
- Auto mode: if query parses as type → boost type section; if natural language → boost semantic; else name.
- Filters as chips (reuse SearchPanel kind chips; extend with package/language).
- Replace title-bar `LibInput` "name version" as primary load path — loading packages is a project/sync concern, not a search-box side channel.

**Latency budget**

| Stage | Budget |
|-------|--------|
| Keystroke → debounce fire | 16–32 ms |
| Tantivy / SQLite name query | <5 ms |
| Type fingerprint top-K | <8 ms |
| Merge + rank + notify | <2 ms |
| First paint of local rows | **<10 ms** from debounce |
| Vector / remote | 20–200 ms; append without clearing local |

---

### 2.4 (c) Symbol page — P0 (+ lineage P1)

**Purpose.** Rendered signature + docs + source snippet (tree-sitter) + references/callers + implementations + **version timeline** from lineage.

```
┌─ tabs: [Router] [State] [map_err] ──────────────── [◀ ▶] ─┐
│ axum › response ›  Result::map_err                         │
│ pub fn map_err<F,O>(self, op: O) → Result<T,F>   [pub]    │
│ package: axum  0.7.5  ▾ versions   language: Rust          │
├────────────────────────────────────────────────────────────┤
│ [Docs] [Source] [Refs 128] [Impls] [Timeline] [Graph]     │
│                                                            │
│  /// Maps a `Result<T, E>` to `Result<T, F>` by …          │
│  ///                                                       │
│  /// # Example                                             │
│  /// ```                                                   │
│  /// let r: Result<i32, &str> = Err("e");                  │
│  /// let s = r.map_err(|e| e.len());                       │
│  /// ```                                                   │
│                                                            │
│  Signature                                                 │
│    self: Result<T, E>                                      │
│    op: O where O: FnOnce(E) → F                            │
│    returns: Result<T, F>                                   │
│                                                            │
│  Source  src/response/result.rs:142                        │
│  ┌──────────────────────────────────────────────────────┐ │
│  │ 142 │ pub fn map_err<F, O>(self, op: O) → …          │ │
│  │ 143 │ where                                          │ │
│  │ 144 │     O: FnOnce(E) → F,                          │ │
│  └──────────────────────────────────────────────────────┘ │
│                                                            │
│  Timeline (lineage) ───────────────────────────────────── │
│   0.6.0 ──●── 0.7.0 ──●── 0.7.5                            │
│            │         └── signature stable                  │
│            └── docs expanded; example added                │
│   [ Compare 0.6.0 → 0.7.5 ]                                │
└────────────────────────────────────────────────────────────┘
```

**Sections (render model inspired by DocC / rustdoc)**

1. Breadcrumb (existing `symbol_view.rs` pattern — keep, make each segment open package browser).
2. Header: fq_name, visibility, kind badge, package+version picker.
3. Tab strip inside page: Docs | Source | Refs | Impls | Timeline | (Graph neighborhood).
4. Docs: `TextView::markdown` with tree-sitter highlight in fenced blocks (gpui-component syntax highlight).
5. Source: read-only editor component or highlighted `TextView` over on-disk source + tree-sitter ranges.
6. Refs/Impls: virtualized table (path, line, kind, precise?).
7. Timeline: lineage events from client (`signature_changed`, `docs_changed`, `moved`, `deprecated`).

**MVP cut.** Docs + header + members + basic source snippet. Timeline and refs can be stubs that show "indexing…" until pipeline ready.

---

### 2.5 (d) Package browser — P1

**Purpose.** Module tree, API surface, version picker, diff-between-versions driven by symbol lineage.

```
┌─ left tree ──────────┬─ API surface / selected module ─────────────────┐
│ axum 0.7.5 ▾         │  Module: axum::routing                          │
│ ▼ axum               │  [Exports] [All items] [Changed since 0.6 ▾]    │
│   ▼ routing          │                                                 │
│       Router         │  ● Router            struct                     │
│       MethodFilter   │  ● get, post, …      fn                         │
│   ▼ extract          │  ● Router::route     method                     │
│       State          │                                                 │
│   ▶ response         │  Version diff mode: 0.6.20 → 0.7.5              │
│   ▶ middleware       │  + 12 symbols   ~ 4 changed   − 2 removed       │
└──────────────────────┴─────────────────────────────────────────────────┘
```

**Widgets.** gpui-component **Tree** + **Table** / virtual List. Version picker = Select.

**Diff-between-versions.** Use lineage IDs, not textual names alone: added / removed / signature-changed / docs-only.

---

### 2.6 (e) Dependency graph view — P1/P2 (prototype exists)

**Current:** `graph_view.rs` — circular layout, canvas edges, click → open symbol. `AppState.current_graph` never populated (03-gui-audit §3.3).

**Target:**

```
┌─ Graph  [force] [hierarchy] [circular] ──── zoom − 100% + ─┐
│     ┌────┐         semantic                 ┌─────┐       │
│     │Router│───────────────────────────────▶│State│       │
│     └──┬─┘                                  └─────┘       │
│        │ member                                            │
│        ▼                                                   │
│     ┌──────┐                                               │
│     │route │                                               │
│     └──────┘                                               │
│  legend: ── member   - - semantic   ··· calls              │
│  [ Expand neighbors ] [ Pin layout ] [ Open as symbol ]    │
└────────────────────────────────────────────────────────────┘
```

**Build cost:** medium-high. No stock force-directed layout in gpui-component; implement with `canvas` + manual layout or embed a small layout crate. Pan/zoom required past ~10 nodes.

**MVP:** wire existing circular view to real graph data; defer force-directed to P2.

---

### 2.7 (f) Sync / jobs dashboard — P0/P1

**Purpose.** What is syncing, what is compiling locally, incremental pipeline status per commit.

```
┌─ Jobs ─────────────────────────────────────────────────────┐
│ Active                                                     │
│  ● compile  my-app @ abc123f   ████████░░  82%   [Cancel]  │
│  ● sync     tokio 1.40.0       ███░░░░░░░  blobs 3/8       │
│  ● embed    serde 1.0.210      ░░░░░░░░░░  queued          │
│                                                            │
│ Recent                                                     │
│  ✓ index    my-app @ abc123f   1.2s   +14 −2 symbols       │
│  ✓ sync     axum 0.7.5         4.1s   ready                │
│  ✗ sync     broken-pkg 0.1     error: 404 from INDEX [Retry]│
│                                                            │
│ Pipeline (project HEAD)                                    │
│  parse → IR → symbols → tantivy → vectors → lineage        │
│   ✓       ✓     ✓         ✓         …         ✓            │
└────────────────────────────────────────────────────────────┘
```

**Relation to LogPanel.** Keep raw logs; Jobs is structured. Per-job log drill-down reuses LogStore filters by `job_id`.

**Replace** LocalIndexPanel subprocess UX with Job-backed local compile (03-gui-audit §7.2).

---

### 2.8 (g) Settings — P0

**Current:** `NudoxSettings` only `dark_mode`, unused `sidebar_width`/`font_size`, macOS-only path (03-gui-audit §4).

**Target sections**

| Section | Fields |
|---------|--------|
| Appearance | theme, font size, sidebar width (actually wired) |
| Backend / INDEX | base URL, auth token, TLS, timeout |
| Storage | local store root, quota, GC policy, clear cache |
| Trust | default trust for path deps; confirm-before-trust-remote |
| Search | debounce ms, enable vector, enable type-search, embedding model |
| Indexer | in-process vs external binary path |
| Keybindings | view / rebind |
| About | version, open logs dir |

Use gpui-component Switch, Select, Input, Modal for confirmations.

---

### 2.9 (h) Views the references suggest we're missing

| View / feature | Source inspiration | Priority |
|----------------|--------------------|----------|
| **Command palette** (actions, not only symbols) | Zed / VS Code / Dash hotkey | P0 |
| **Navigation history** (back/forward) | DevDocs, browser, Dash | P0 |
| **Quick doc peek** (sidebar while browsing) | JetBrains tool window | P1 |
| **Annotations / notes** on symbols | Dash snippets | P3 |
| **Pinning / collections** ("my API cheat sheet") | Dash profiles, bookmarks | P3 |
| **Playground launch** | Dash `DashDocSetPlayURL` | P3 |
| **Compare two symbols** (not only versions) | Sourcegraph multi-file | P2 |
| **Import Dash docset** fallback | Zeal ecosystem | P3 |
| **Notifications / toasts** for job complete | gpui-component Toast | P0 |
| **Context menus** on symbols | Dash / Sourcegraph | P1 |

---

### 2.10 Target workspace chrome (composed)

```
┌─ TitleBar  [sidebar] Nudox  [cmd-K]  [Jobs•2]  [theme] ────┐
├─ left dock ─┬─ center (tabs + history) ──────┬─ right dock ─┤
│ Projects    │ ◀ ▶   [Symbol A][Pkg B][+]    │ Outline /    │
│ Search      │                               │ Refs peek    │
│ Packages    │   active item content         │              │
│             │                               │              │
├─────────────┴───────────────────────────────┴──────────────┤
│ bottom dock: Jobs | Logs | Terminal?                         │
└──────────────────────────────────────────────────────────────┘
```

Replace monolithic `workspace.rs` flex with gpui-component **Dock** layout (resizable, hideable panels).

---

## 3. GPUI + gpui-component capability check (2026-07)

### 3.1 Versions in lindsey today

From `workspace/gui/Cargo.toml`:

```toml
gpui = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe" }
gpui_platform = { git = "…", features = ["font-kit", "x11", "wayland"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", package = "gpui-component" }
gpui-component-assets = { git = "…", package = "gpui-component-assets" }
```

**gpui-component** latest release observed mid-research: **v0.5.1** (2026-02-05) on GitHub; crates.io also lists published versions. License: **Apache-2.0**. Stars ~12k. Gallery: [https://longbridge.github.io/gpui-component/gallery/](https://longbridge.github.io/gpui-component/gallery/).

**GPUI** docs hub: [https://gpui.rs/](https://gpui.rs/). Ownership model: [Zed blog — Ownership and data flow in GPUI](https://zed.dev/blog/gpui-ownership).

---

### 3.2 gpui-component feature inventory (verified README 2026)

| Capability | Status | nudox use |
|------------|--------|-----------|
| 60+ components | Yes | Broad form/chrome coverage |
| Dock layout + resizable + Tiles freeform | Yes | Replace hardcoded flex workspace |
| Virtualized List | Yes | Search results, refs, jobs |
| Virtualized Table / DataTable | Yes | Package lists, API surface tables |
| Tree | Yes (component set; Tree commonly listed in docs/comparisons) | Module browser |
| Tabs / TabBar | Yes — **already used** | Symbol tabs |
| Input / InputState | Yes — **already used** | Search, settings |
| Markdown + simple HTML (`TextView`) | Yes — **already used** | Docs body |
| Syntax highlighting (Tree-sitter) | Yes — for editor **and** markdown | Code fences + source pane |
| Code editor (Rope, up to ~200K lines, LSP hooks) | Yes | Source view; optional light edit |
| Charts | Yes | Sync/storage metrics later |
| Theme / ThemeMode / multi-theme | Yes — **already used** | Appearance |
| Modal, Popover, Drawer | Yes | Project picker, confirms |
| Toast / Notification | Yes | Job completion |
| ContextMenu | Yes | Symbol actions |
| Checkbox, Switch, Select, Radio | Yes | Settings |
| Skeleton, Badge, Indicator | Yes | Loading / status |
| Scrollbar | Yes | Polish |
| WASM web gallery | Yes | Not primary for lindsey |

**Examples to study in the repo:** `cargo run --example dock`, `editor`, `markdown`, `html`; story gallery crate.

---

### 3.3 Currently used vs unused in lindsey

**Used today** (03-gui-audit §6.1): Root, TitleBar, Button, Input, Tab/TabBar, SidebarToggleButton, Theme, TextView markdown, IconName, h_flex/v_flex.

**Highest-ROI unused widgets to adopt**

1. **Dock** — multi-pane workspace  
2. **List / Table (virtualized)** — search + large deps lists  
3. **Tree** — package browser  
4. **Toast** — pipeline feedback  
5. **Modal / Drawer** — project open, trust confirms  
6. **Code editor / tree-sitter highlight** — source + fences  
7. **Switch / Select** — settings  
8. **ContextMenu** — symbol actions  
9. **Skeleton** — SymbolView loading (replace plain text)

---

### 3.4 Zed multi-pane patterns (imitate, don't copy GPL)

Zed's `crates/workspace` (pane, dock, item, …) is **GPL-licensed** as part of Zed ([discussion on splitting components](https://github.com/zed-industries/zed/discussions/13694)). **Do not vend Zed workspace code.**

**Patterns to re-implement cleanly (ideas only):**

| Concept | Meaning | lindsey analog |
|---------|---------|----------------|
| Workspace | Root window state | `Entity<Workspace>` (exists) |
| Pane | Tabbed container of items | TabBar + `open_tabs` (exists, thin) |
| Item | Trait: tab content, title, clone, save? | `trait WorkspaceItem` for Symbol, Package, Graph, Settings |
| Dock | Left/right/bottom panel slots | gpui-component Dock |
| Action + keystroke | Global command dispatch | GPUI `actions!` + keymaps (exists, expand) |
| Navigation history | Stack of locations | New `NavHistory` entity |
| Entity + subscribe | Cross-view events | Existing pattern; move data to Stores |

GPUI entity model (from Zed blog): every model/view is owned by `App`; `cx.new`, `cx.subscribe`, `cx.spawn`, `cx.notify` — lindsey already follows this (`workspace.rs`, stores proposed in 03-gui-audit §8).

---

### 3.5 Async data loading patterns

**Current lindsey**

- `cx.spawn(async move |this, cx| { Compat::new(http_future).await; … }).detach()`
- smol (GPUI) + Tokio (reqwest) via `async-compat`
- LogStore: `async_channel` + long-lived spawn notifying workspace
- Indexer: `std::thread` + mpsc + 200ms poll (should become `spawn_blocking`)

**Recommended patterns**

```text
View event
  → Store method
    → cx.spawn {
         let result = client.op().await;  // or background_executor for CPU
         this.update(cx, |store, cx| {
             store.apply(result);
             cx.emit(StoreEvent::…);
             cx.notify();
         });
       }
  → Views subscribe to StoreEvent
```

- **Debounced search:** store generation counter; ignore stale responses.
- **Streaming:** emit `ResultsPage { gen, items }` multiple times; List appends.
- **Cancellation:** `JobHandle` with oneshot/abort; drop = cancel for sync tasks.
- **CPU work** (Tantivy query, type fingerprint scan): `cx.background_executor().spawn` then marshal back.

---

### 3.6 Text rendering & syntax highlighting

| Need | Solution 2026 |
|------|----------------|
| Markdown docs | `TextView::markdown` (already) |
| Highlighted code fences | gpui-component tree-sitter path for markdown |
| Full source file view | gpui-component **code editor** read-only mode |
| Custom IR → rich text | Build small render layer producing GPUI elements from typed RenderModel |
| Webview escape hatch | **Avoid** for core docs; only if forced for rare HTML docsets |

Zed's editor stack is not a separate crates.io product; **gpui-component's editor** is the pragmatic adopt path for lindsey (Apache-2.0).

---

### 3.7 Theming

- gpui-component: `Theme`, `ThemeMode`, theme schema JSON (`.theme-schema.json` in repo).
- lindsey: light/dark toggle + settings file.
- Extend themes for: trust colors (trusted green / delegated amber / error red), lineage diff colors, kind badges (align with rustdoc item colors).

---

### 3.8 Widget gap list — build vs adopt

| Widget / surface | Adopt | Build custom | Effort | Notes |
|------------------|-------|--------------|--------|-------|
| Dock workspace | gpui-component Dock | — | S | Replace flex layout |
| Virtualized search list | List/Table | — | S | |
| Module tree | Tree | — | S | |
| Markdown docs | TextView | — | S | |
| Syntax-highlighted source | Editor / TS | thin wrapper | M | |
| Forms / settings | Switch, Select, … | — | S | |
| Toasts | Toast | — | S | |
| Command palette UI | Modal + List | glue | M | |
| Omni-search ranking UI | List sections | merge logic | M | |
| **Lineage timeline** | — | **custom** | M | Unique; simple SVG/div timeline first |
| **Version diff view** | — | **custom** | M | Unified/split diff of signatures+docs |
| **Force-directed graph** | canvas | **custom layout** | L | Or hierarchical only in P1 |
| **Type query builder** | form controls | **custom** | M | |
| Trust boundary viz | Table + badges | light custom | S | |
| Job pipeline stepper | — | light custom | S | |
| Webview docset HTML | — | only if P3 import | L | Prefer IR render |

**Cost summary:** ~80% of chrome is adopt; differentiators (timeline, type-search UX, trust viz, graph) are custom but bounded.

---

## 4. Architecture for the refactor

### 4.1 Module structure (refined from 03-gui-audit §8.1)

```
workspace/gui/src/
├── main.rs
├── app/
│   ├── mod.rs
│   ├── actions.rs          # actions! macros, menus
│   ├── keymaps.rs
│   └── settings.rs
├── store/
│   ├── mod.rs
│   ├── project_store.rs
│   ├── symbol_store.rs
│   ├── search_store.rs     # omni-search state, debounce, gens
│   ├── registry_store.rs   # dep sync states
│   ├── job_store.rs
│   └── nav_history.rs      # back/forward stack
├── client/                 # adapters over workspace client lib
│   ├── mod.rs
│   ├── traits.rs
│   └── composite.rs        # local vs remote routing
├── workspace/
│   ├── mod.rs
│   ├── workspace.rs        # layout + docks only
│   ├── pane.rs
│   └── item.rs             # WorkspaceItem trait
├── views/
│   ├── project_panel.rs
│   ├── omni_search.rs      # cmd-K modal
│   ├── search_panel.rs     # docked search (optional dual)
│   ├── symbol_view.rs
│   ├── package_browser.rs
│   ├── graph_view.rs
│   ├── timeline.rs
│   ├── diff_view.rs
│   ├── job_panel.rs
│   ├── log_panel.rs
│   ├── settings_view.rs
│   └── quick_doc.rs
├── render/
│   ├── mod.rs
│   ├── signature.rs        # clickable type links
│   └── markdown_code.rs    # highlight integration
├── types.rs
└── fixtures.rs             # #[cfg(debug_assertions)] only
```

---

### 4.2 Store / model layer

```text
┌─────────────────────────────────────────────┐
│                   Workspace                  │
│  owns Entities: stores + dock layout        │
└───────┬─────────────────────────────────────┘
        │ subscribe / update
   ┌────┴────┬──────────┬──────────┬─────────┐
   ▼         ▼          ▼          ▼         ▼
Project   Search    Symbol    Registry    Job
Store     Store     Store     Store       Store
   │         │          │          │         │
   └─────────┴──── client library ─┴─────────┘
                    (local + remote)
```

**Rules**

- Views never call HTTP directly.
- Workspace does not own search result vectors (SearchStore does).
- One-way: Store events → views; user intents → store methods.
- `CompositeSymbolClient`: trusted project → local Tantivy/SQLite/IR; delegated → local cache if synced else remote INDEX.

---

### 4.3 Command palette & actions (GPUI-style)

Expand `actions!` beyond Quit/Toggle*:

```text
OpenOmniSearch
NavigateBack / NavigateForward
OpenProject
ToggleProjectPanel / ToggleJobPanel / TogglePackageBrowser
CloseTab / NextTab / PrevTab
FocusSearch
RefreshSymbol
CompareWithPreviousVersion
CancelJob
ToggleTypeSearchMode
```

Command palette: fuzzy-filter **actions + recent symbols + packages**. Distinct sections like Zed's palette / VS Code.

---

### 4.4 Navigation history

```rust
struct NavEntry {
    kind: NavKind, // Symbol { uri, version }, Package { id, version }, Graph { root }, Settings
    title: String,
}
struct NavHistory {
    back: Vec<NavEntry>,
    forward: Vec<NavEntry>,
    current: Option<NavEntry>,
}
```

- Opening a symbol from search **pushes** history.
- Breadcrumb click pushes.
- Back/forward restore tab content without losing tab strip (or re-activate existing tab if open).
- Mirror browser UX from DevDocs/Dash — users expect it in a docs app.

---

### 4.5 Keyboard-first design

| Binding (mac / linux) | Action |
|-----------------------|--------|
| cmd/ctrl-K | Omni-search |
| cmd/ctrl-P | Command palette (actions) |
| cmd/ctrl-Shift-O | Symbols only (if split from K) |
| cmd/ctrl-[ / ] | Navigate back / forward |
| cmd/ctrl-B | Toggle left dock |
| cmd/ctrl-J | Toggle bottom jobs/logs |
| cmd/ctrl-W | Close tab |
| cmd/ctrl-1..9 | Switch tabs |
| Escape | Close modal / clear search |
| ? | Shortcut cheatsheet overlay |

All discoverable from `?` and command palette (DevDocs pattern).

---

### 4.6 Keeping search <10ms

```text
on_query_change(q):
  gen += 1
  schedule debounce 16ms with gen

on_debounce(gen, q):
  if gen != current: return
  local_name = tantivy_or_sqlite(q)          // sync or background, <5ms
  local_type = if looks_like_type(q) { type_search(q) } else { [] }
  emit LocalResults { gen, name, type }     // paint immediately

  spawn vector_search(q) → emit SemanticResults { gen, … } if gen matches
  spawn remote_fallback if needed → same
```

- Virtualized list: only ~30 rows mounted.
- Never clear local results when semantic arrives; **merge by section**.
- Fix current bug: do not re-filter `fixture_symbols()` (search_panel.rs ~143).
- Warm indexes on project open (background job), not on first keystroke.

---

### 4.7 Lineage / timeline differentiator (design)

**Data** (from client/IR, not GUI invention):

```text
SymbolId (stable across versions)
  └─ Occurrence @ version V
       signature_hash
       docs_hash
       location
       event: Added | Removed | SignatureChanged | DocsChanged | Moved | Deprecated
```

**UI principles**

- Timeline is a **first-class tab** on the symbol page, not buried in package history only.
- Default comparison: previous release that changed this symbol → current.
- Diff view: signature unified diff + docs prose diff; members added/removed lists.
- Search can filter `changed in last 30 days` later (P2).

No competitor in §1 has this as a core loop. Pair with type-directed search as the two product wedges.

---

### 4.8 Type-directed search differentiator (design)

**Input modes**

1. Free-text type: `Result<T,E> -> (E->F) -> Result<T,F>`
2. Builder UI: Return type [ ]  Args [+]  Async?  Traits required
3. "Find functions that take `Router` and return `MethodRouter`" from context menu on a type

**Ranking signals**

- Exact arity match
- Unifiable types with substitution cost
- Generics freeness
- Package popularity / in-project usage count
- Same-crate bonus

**Result chrome**

Always show the **normalized signature** in the result row (Hoogle does this; name-only UIs don't).

---

## 5. Prioritized view roadmap

### Phase 0 — Stabilize prototype (1–2 weeks)

- Fix fixture search bug; cfg-gate fixtures.
- Wire graph from `/run` response.
- Settings: server URL, XDG paths, wire font/sidebar.
- LogPanel ScrollHandle auto-scroll.
- Toast on backend offline/online.

### Phase 1 — MVP shell (P0)

1. Dock-based workspace layout  
2. Project open + trust boundary list (even if sync is stubbed)  
3. Omni-search modal over local index (Tantivy or SQLite name)  
4. Symbol page v1: docs + signature links + source snippet  
5. JobStore + Job panel (replace bare indexer panel)  
6. Nav history + expanded keymaps + command palette  
7. Client adapter trait; kill hardcoded localhost-only path  

### Phase 2 — Differentiation (P1)

1. Progressive dep sync UI (RegistryStore)  
2. Package browser (Tree + API table)  
3. Symbol timeline + version picker  
4. References panel (precise when available)  
5. Type-directed search mode  

### Phase 3 — Power (P2)

1. Version diff view  
2. Graph force-directed / expand neighbors  
3. Quick doc peek dock  
4. Compare arbitrary versions; semantic search polish  

### Phase 4 — Stickiness (P3)

1. Annotations/notes  
2. Pins/collections / search profiles  
3. Playground links  
4. Optional Dash docset import  

---

## 6. UX principles (distilled)

1. **Search is the homepage.** Browsing trees is secondary (DevDocs/Dash).  
2. **Keyboard completes every loop.** Mouse never required for core paths.  
3. **Local-first paint.** Remote and vector may only refine.  
4. **Trust is visible.** Always show what runs local compiler vs INDEX.  
5. **IR in, pixels out.** Like DocC: structured render model, not HTML soup.  
6. **Versions are first-class.** rustdoc/docs.rs version bar + our lineage timeline.  
7. **Types are queryable.** Hoogle-class UX over multi-language IR.  
8. **Jobs are observable.** No silent subprocesses (fix LocalIndexPanel).  
9. **History like a browser.** Back/forward across symbols and packages.  
10. **Progressive disclosure.** Quick peek → full symbol page → timeline/diff.  
11. **Adopt chrome, build differentiators.** Dock/List/Tree/Editor from gpui-component; timeline/type-search/graph custom.  
12. **Don't ship fixture lies.** Empty states over fake axum data.

---

## 7. Mapping references → concrete lindsey work items

| Work item | Reference | Files / layers |
|-----------|-----------|----------------|
| Omni-search cmd-K | DevDocs, Dash | `views/omni_search.rs`, `store/search_store.rs` |
| SQLite name/type/path | Dash docset schema | client local catalog |
| RenderModel sections | DocC JSON, rustdoc | `render/`, `symbol_view.rs` |
| Clickable signatures | rustdoc | `render/signature.rs` |
| Type search | Hoogle | `search_store` + IR type index |
| Refs panel | Sourcegraph | symbol page tab + client |
| File tree vs symbol search split | GitHub / VS Code | command palette sections |
| Offline progressive packs | DevDocs offline + Dash feeds | `registry_store` |
| Dock IDE layout | Zed patterns + gpui-component Dock | `workspace/` |
| Snippets later | Dash | P3 annotations |

---

## 8. Risks & open questions

1. **GPUI pin drift.** lindsey pins a specific Zed rev; gpui-component tracks evolving GPUI APIs — breakage risk on upgrades.  
2. **smol vs Tokio.** Embedded Tantivy + vectors + HTTP: confirm single-process runtime strategy (03-gui-audit §9.1).  
3. **Editor weight.** Shipping full code-editor component for read-only source may bloat binary; measure before default-on.  
4. **Graph performance.** Custom canvas graphs get expensive >100 nodes; may need level-of-detail or server-side subgraphs.  
5. **Type search multi-language.** Hoogle is mono-language; fingerprint schemes differ for TS/Python/Go — design per-language adapters.  
6. **GPL contamination.** Do not copy Zed workspace sources; only imitate patterns.  
7. **Docset import scope creep.** Resist P0 HTML docset viewer; stay IR-native.  
8. **Auth to INDEX.** Settings must grow credentials before multi-user remote.  
9. **Lineage data availability.** Timeline UI is worthless without client APIs emitting events — coordinate with IR/lineage research tracks.  
10. **Tab entity leaks.** Detached subscriptions on SymbolView (03-gui-audit §9.9) must be fixed during Item trait refactor.

---

## 9. Executive summary

nudox's desktop GUI (lindsey on GPUI + longbridge **gpui-component** v0.5.x) should be redesigned as a **local-first documentation and code-intelligence shell**: projects with explicit **trust boundaries**, progressive **INDEX → disk** sync, and symbol pages backed by structured IR rather than HTML docsets.

**Reference products** converge on a few non-negotiables. **DevDocs** and **Dash** prove that fuzzy, keyboard-first, multi-library search is the primary surface; Dash's SQLite `searchIndex(name, type, path)` docset schema and feed/update model are the right *mental* model for a local catalog (without adopting HTML as IR). **Zeal/Velocity** show the docset *format* outlives any single viewer. **Swift-DocC** is the best prior art for **compile-to-JSON-then-render**: lindsey should consume a versioned RenderModel the way DocC-Render consumes archive JSON. **rustdoc/docs.rs** teach clickable signatures, module trees, and version bars; rustdoc's own search index already encodes type fingerprints. **Hoogle** is the UX template for **type-directed search** — a differentiator nudox can own because it has typed IR. **Sourcegraph** and **GitHub** contribute references panels, outline sidebars, and separation of file vs symbol finders. JetBrains contributes two-tier docs (quick peek vs full page).

**View roadmap:** P0 = Project manager (trust viz), Omni-search (cmd-K, <10ms local), Symbol page v1, Jobs/logs, Settings, command palette, back/forward. P1 = Package browser, sync polish, lineage timeline, references, type-search mode. P2 = version diffs, richer graph, quick doc. P3 = annotations, pins, playgrounds, optional docset import.

**Widget strategy:** Adopt Dock, virtualized List/Table, Tree, Modal, Toast, Editor/tree-sitter, form controls from gpui-component. Build custom: lineage timeline, signature/docs diff, type-query builder, graph layout, trust chrome. Imitate Zed Workspace/Pane/Item/Action patterns without vendoring GPL workspace code.

**Architecture:** Store entities (`Project`, `Search`, `Symbol`, `Registry`, `Job`, `NavHistory`) over a composite client; views subscribe; debounced multi-source search with generation tokens; keyboard maps expanded around omni-search and history.

**Two wedges no competitor combines:** (1) **symbol lineage timeline + version diffs**, (2) **type-directed search over multi-language IR**, both grounded in local indexes inside the GUI process.

---

## 10. Top concrete recommendations

1. **Adopt gpui-component Dock immediately** — stop extending the monolithic flex layout in `workspace.rs`.  
2. **Ship cmd-K omni-search** as the primary entry; demote title-bar lib input.  
3. **Implement Project open + trust boundary UI** before more remote search polish.  
4. **Introduce SearchStore + JobStore** and stop forwarding data through Workspace methods.  
5. **Define a versioned Symbol RenderModel** (DocC-inspired) to replace ad-hoc `kind: Value` rendering.  
6. **Build lineage timeline + type-search as P1**, marketed as core product, not power-user extras.  
7. **Virtualize all long lists** (deps, search, refs) with List/Table.  
8. **Nav history + ? shortcuts overlay** for docs-browser familiarity.  
9. **Gate fixtures and wire real graph data** in Phase 0.  
10. **Coordinate timeline UI with client lineage APIs** so the view is not a mock.

---

## 11. Sources

All primary URLs are cited inline in §§1–4. Key hubs: [devdocs.io](https://devdocs.io/), [kapeli.com/docsets](https://kapeli.com/docsets), [zealdocs.org](https://zealdocs.org/), [swift-docc-render](https://github.com/swiftlang/swift-docc-render), [docs.rs/about/rustdoc-json](https://docs.rs/about/rustdoc-json), [hoogle.haskell.org](https://hoogle.haskell.org/), [Sourcegraph code nav](https://docs.sourcegraph.com/user/code_intelligence), [longbridge/gpui-component](https://github.com/longbridge/gpui-component), [gpui.rs](https://gpui.rs/), [Zed GPUI ownership](https://zed.dev/blog/gpui-ownership). Companion audit: `03-gui-audit.md`. Live code: `workspace/gui/` (crate `lindsey`).

---

*End of report.*
