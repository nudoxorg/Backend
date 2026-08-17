# GUI Audit — `workspace/gui` (crate `lindsey`)

> **Audit date:** 2026-07-16  
> **Re-verified:** 2026-07-16 against live tree (`workspace/gui/src/*`, 3510 LOC across 12 `.rs` files + 27-line `Cargo.toml`)  
> **Scope:** all 12 source files + Cargo.toml  
> **Purpose:** exhaustive feed for the librarification architecture plan (trusted-local project vs untrusted-remote INDEX, embedded Tantivy/vectors, `client` library)

**Source inventory (line counts, live):**

| File | Lines | Role |
|------|------:|------|
| `Cargo.toml` | 27 | crate `lindsey` binary package |
| `src/main.rs` | 103 | boot, menus, keybindings, window |
| `src/workspace.rs` | 741 | root entity, layout, orchestration |
| `src/backend.rs` | 309 | reqwest HTTP client + wire types |
| `src/types.rs` | 165 | domain types + `AppState` global |
| `src/search_panel.rs` | 503 | left search / filter / results |
| `src/symbol_view.rs` | 376 | symbol detail tab |
| `src/log_panel.rs` | 325 | bottom log viewer |
| `src/local_index_panel.rs` | 264 | indexer launcher |
| `src/graph_view.rs` | 221 | circular canvas graph |
| `src/log_store.rs` | 207 | ring buffer + tracing layer |
| `src/fixtures.rs` | 192 | fake axum symbols + tests |
| `src/settings.rs` | 104 | `NudoxSettings` + file watcher |

---

## 0. Repository layout

```
workspace/gui/
├── Cargo.toml
└── src/
    ├── main.rs            — entry point, app boot, keybindings, menus
    ├── workspace.rs       — root Workspace entity, layout, orchestration
    ├── backend.rs         — reqwest HTTP client, all server calls
    ├── types.rs           — shared domain types + AppState global
    ├── graph_view.rs      — circular canvas graph view
    ├── symbol_view.rs     — symbol detail / documentation tab
    ├── search_panel.rs    — left-sidebar search / filter / results
    ├── local_index_panel.rs — local indexer launcher panel
    ├── log_panel.rs       — in-app log viewer (bottom bar)
    ├── log_store.rs       — Arc<Mutex<VecDeque>> + tracing layer
    ├── settings.rs        — NudoxSettings global + file-watcher
    └── fixtures.rs        — fake axum symbols + SymbolEntry data
```

### 0.1 Dependencies (Cargo.toml:1–27)

| Crate | Pin / version | Role in GUI |
|-------|---------------|-------------|
| `gpui` | zed `1d217ee39d381ac101b7cf49d3d22451ac1093fe` | UI framework |
| `gpui_platform` | same rev; features `font-kit`, `x11`, `wayland` | window/platform shell |
| `gpui-component` | longbridge git (unpinned rev in Cargo.toml) | widgets (Root, Tab, Input, …) |
| `gpui-component-assets` | longbridge git | icon/font asset bundle |
| `reqwest` 0.12 | features `json` | HTTP to backend |
| `tokio` 1 | features `rt` only | bootstrap runtime for `reqwest::Client` construction |
| `async-compat` 0.2 | — | bridge Tokio futures into GPUI/smol |
| `async-io` 2 | — | timer poll loop for indexer thread |
| `async-channel` 2 | — | LogStore notify ping |
| `tracing` / `tracing-subscriber` | env-filter | structured logging → LogStore |
| `serde` / `serde_json` | — | wire + settings |
| `uuid` v4 | — | search session id |
| `which` 7 | — | `nudox-indexer` PATH lookup |
| `chrono` 0.4 | serde | log timestamps |
| `anyhow` 1 | — | BackendClient construction errors |

**Not present (librarification will need):** `tantivy`, vector/ANN crate, SQLite/`sqlx`, any `client`/`compiler` workspace dep, `dirs`, TLS/auth crates, cancel tokens.

**Edition:** `edition = "2024"` (Cargo.toml:4) — unusual pin; build tooling must support Rust 2024 edition.

---

## 1. App Architecture

### 1.1 Boot sequence (main.rs:24–103)

1. `LogStore::new()` (main.rs:27) — creates the `Arc<Mutex<VecDeque>>` + async-channel pair **before** tracing initialises, so nothing is lost.
2. `tracing_subscriber::registry()` (main.rs:29–41) is composed: `EnvFilter` → `fmt::layer` (compact stderr with target) → `NudoxLogLayer` (captures to LogStore). Default filter is `warn,nudox=debug` (main.rs:32).
3. `gpui_platform::application().with_assets(Assets)` (main.rs:43) — instantiates the GPUI platform with the gpui-component asset bundle.
4. Inside `app.run(|cx|)`:
   - `gpui_component::init(cx)` (main.rs:47) — registers gpui-component globals (theme tokens, icon fonts, etc.).
   - `NudoxSettings::init(cx)` (main.rs:48) — loads `~/Library/Application Support/nudox/settings.json`, sets theme, spawns 2-second file-watcher task.
   - `cx.set_global(store.clone())` (main.rs:51) — `LogStore` is a GPUI global, accessible from any view.
   - Platform-conditional keybindings (cmd/ctrl) (main.rs:53–68): `Quit`, `CloseTab`, `ToggleSidebar`, `ToggleLogPanel`, `ToggleGraphView`.
   - **Note:** `ToggleLocalIndex` is declared in `actions!` (main.rs:22) and handled in Workspace (workspace.rs:522–524) but has **no keybinding** and is not in any menu — only the title-bar "Index" button reaches it.
   - Menus (main.rs:72–78): Nudox → Quit; View → Toggle Search / Toggle Log panels. Graph and Index are **not** in menus.
   - `cx.on_window_closed` (main.rs:80–85) — quit if no windows remain.
5. Single window at 1200×800 (main.rs:87–99): `Root::new(workspace, window, cx)` — `Root` is the gpui-component shell that provides theming context, selection management, and any platform decorations. Title bar options come from `TitleBar::title_bar_options()` (main.rs:90).

### 1.2 Window / workspace layout (workspace.rs:505–741)

The entire layout is one `Workspace` entity rendered as a `v_flex` tree:

```
v_flex (full window)                                           workspace.rs:505
  TitleBar                                                     workspace.rs:527
    SidebarToggleButton  "Nudox"  LibInput(300px)  [Index|Graph|Logs|Theme]
  [optional] error banner (backend_error)                      workspace.rs:635–663
  h_flex (flex-1, min-h-0)                                     workspace.rs:667
    v_flex (left column — width from SearchPanel)              workspace.rs:671–675
      SearchPanel                                              (w=320 or 0)
      LocalIndexPanel (hidden until toggled; no fixed width)
    v_flex (right column, flex-1)                              workspace.rs:677–737
      TabBar (nudox-tabs)
      when !graph_visible → active SymbolView (or empty state)
      when  graph_visible → GraphView with "Exploration Graph" header
  LogPanel (bottom, h=220px, hidden until toggled)             workspace.rs:739
```

There is no dock system. The panels are wired by inline flex layout. `search_panel_collapsed` collapses the left column to `w(px(0.))` via SearchPanel itself (search_panel.rs:408). LocalIndexPanel sits **under** SearchPanel in the left column (workspace.rs:671–675), not as a modal/drawer.

### 1.3 Entity / state management patterns

Every major component is a GPUI `Entity<T>` (`cx.new(|cx| T::new(cx))`). Entities communicate via:

| Pattern | Where | Evidence |
|---------|-------|----------|
| Events + `cx.subscribe` | Search → Workspace open/search; Graph → open; SymbolView → open | workspace.rs:58–69, 122–125, 301–304 |
| Stored subscriptions | `_subscriptions: Vec<Subscription>` for search events | workspace.rs:41, 141 |
| Detached subscriptions | graph, per-tab SymbolView, InputState | workspace.rs:122–125, 301–304; search_panel.rs:119 |
| Direct `entity.update` | push results into SearchPanel; update SymbolView after fetch | workspace.rs:215–217, 322–326 |
| GPUI globals | `AppState`, `LogStore`, `NudoxSettings` | types.rs:165, log_store.rs:104, settings.rs:26 |

Subscriptions stored in `_subscriptions` stay alive with Workspace. Detached ones (notably graph at workspace.rs:122–125 and each tab at workspace.rs:301–304) live as long as the source entity does — closed tabs drop from `open_tabs` (workspace.rs:351) but detached subscription handles may still pin entities until GC of the handle graph is confirmed. **Risk:** tab close does not explicitly drop subscriptions.

### 1.4 Async handling

GPUI on Linux uses smol as its executor; reqwest needs Tokio. The bridge is:

| Concern | Mechanism | Evidence |
|---------|-----------|----------|
| Client construction | throwaway current-thread Tokio runtime | backend.rs:146–158 |
| HTTP in tasks | `Compat::new(future)` + `cx.spawn(…).detach()` | workspace.rs:1, 84, 150, 244, 315 |
| Indexer subprocess | `std::thread` + `mpsc` + `async_io::Timer` 200ms poll | local_index_panel.rs:73–99 |
| LogStore notify | long-running `cx.spawn` on `async_channel` | workspace.rs:109–119 |
| Settings hot-reload | `cx.background_executor().timer(2s)` loop | settings.rs:60–91 |

No request cancellation, no task join handles, no job IDs. Failed pages in `load_library` log and continue; empty page ends pagination (workspace.rs:175–227).

### 1.5 Event flow summary

```
User types in SearchPanel input
  → InputEvent::Change → SearchPanel::run_search()   # filters fixture_symbols() ONLY (BUG)
  → InputEvent::PressEnter → cx.emit(SemanticSearchRequested)
      → Workspace::run_semantic_search()
          → Compat::new(backend.run_search()) (GET /run)
          → search_panel.load_results("semantic", …)
          → resp.graph IGNORED; AppState.current_graph never set

User clicks result row / "+"
  → SearchPanel::open_symbol → SymbolOpenRequested { fq_name, terminus_uri? }
      → Workspace::open_tab()
          → fixture_entries() hit? skip backend : stub_entry + terminus_search
          → cx.new(SymbolView); subscribe SymbolOpenRequested.detach()

User clicks breadcrumb / member in SymbolView
  → SymbolOpenRequested → Workspace::open_tab()

User types in lib_input title bar, Enter
  → parse_lib_input("name version")
  → load_library: POST /api/packages then paginated POST /symbol-search (PAGE_SIZE=50)
  → load_results + AppState.loaded_chunks

User clicks "Graph"
  → toggle_graph_view → GraphView::set_graph(AppState.current_graph)
  → current_graph is always default empty → permanent empty state

User clicks graph node
  → SymbolOpenRequested (terminus_uri: None) → open_tab

User clicks "Index" / ToggleLocalIndex action
  → LocalIndexPanel::toggle → path + Run Indexer → std::thread nudox-indexer
```

### 1.6 Actions inventory

| Action | Declared | Keybinding | Menu | Workspace handler |
|--------|----------|------------|------|-------------------|
| `Quit` | main.rs:22 | cmd/ctrl-q | Nudox menu | main.rs:70 `cx.quit` |
| `CloseTab` | main.rs:22 | cmd/ctrl-w | — | workspace.rs:511–515 |
| `ToggleSidebar` | main.rs:22 | cmd/ctrl-b | View menu | workspace.rs:508–510 |
| `ToggleLogPanel` | main.rs:22 | cmd/ctrl-l | View menu | workspace.rs:516–518 |
| `ToggleGraphView` | main.rs:22 | cmd/ctrl-g | — | workspace.rs:519–521 |
| `ToggleLocalIndex` | main.rs:22 | **none** | **none** | workspace.rs:522–524 (button only) |

---

## 2. backend.rs — Server Communication Layer

### 2.1 Endpoints called

| Method | Path | Purpose | Request | Response | Called from |
|--------|------|---------|---------|----------|-------------|
| GET | `/healthz` | liveness | — | 2xx | workspace.rs:84 (startup only) |
| GET | `/api/packages` | list packages | — | `Vec<PackageInfo>` | **never called** (backend.rs:184) |
| POST | `/api/packages` | register package | `RegisterPackageRequest` | `PackageInfo` | workspace.rs:152 |
| POST | `/symbol-search` | paginated lookup | `SymbolQuery` | `Vec<SymbolMatchResponse>` | workspace.rs:173 |
| GET | `/terminus_search?q=` | full `SymbolEntry` | query | `SymbolEntry` | workspace.rs:317 |
| GET | `/run?q=&session=` | semantic search | query | `RunSearchResponse` | workspace.rs:247 |
| DELETE | `/session?session=` | cleanup session | query | 2xx | **never called** (backend.rs:300) |

Base URL is hardcoded to `http://localhost:3000` in `BackendSettings::default()` (backend.rs:129–131). `AppState.backend_url` (types.rs:144, default types.rs:155) is a **dead field** — never wired into `BackendClient`. Settings has no backend URL field.

Timeout: 10 seconds (backend.rs:131). No retries, no auth headers, no TLS, no request IDs.

### 2.2 Wire types inventory (backend.rs + types.rs)

**backend.rs locals:**

| Type | Lines | Fields / notes |
|------|------:|----------------|
| `PackageInfo` | 9–15 | `id?`, `name`, `version`, `language` |
| `RegisterPackageRequest` | 17–22 | `language`, `name`, `version`; language always `"Rust"` (backend.rs:205) |
| `SymbolQuery` | 26–37 | `name_pattern?`, `kind?`, `lib_name?`, `limit`, `offset?`; comment: `body_query` omitted (501) |
| `RunMatchDocument` | 40–47 | `fq_name?`, `name?`, `documentation?`, `visibility?`, `path?` |
| `RunMatchHit` | 49–60 | `uri`, `fq_name`, `score`, package/version/kind/document |
| `RunMatchHit::to_symbol_match` | 62–88 | builds `SymbolMatchResponse`; `occurrence_id` = `terminusdb:///data/{uri}`; `occurrence_count` always 0 |
| `RunSearchResponse` | 90–96 | `matches`, `session?`, `graph: Value` (default) — **graph never consumed** |
| `BackendError` | 100–106 | `Unavailable \| NotFound \| ServerError \| ParseError` |
| `BackendSettings` | 121–134 | `base_url`, `timeout_secs` |
| `BackendClient` | 138–309 | `Clone` wrapper around `reqwest::Client` |

**types.rs domain types:**

| Type | Lines | Notes |
|------|------:|-------|
| `SymbolKind` | 10–42 | enum + `label()`; **not used by SearchPanel** (uses string kind filter) |
| `SymbolMatchResponse` | 47–57 | mirrors compiler API |
| `SymbolEntry` | 69–82 | `_id` → `id`; `kind: Value`; recursive `members` |
| `SymbolEntry::kind_name` | 85–90 | `@type` or `_type` |
| `SymbolEntry::breadcrumb` | 92–96 | `path` or split `fq_name` |
| `EdgeRelation` | 103–108 | `Member \| Semantic \| Other` |
| `SymbolEdge` | 110–115 | source/target/relation |
| `ExplorationGraph` | 117–121 | nodes + edges; Default empty |
| `LibraryChunk` | 125–130 | package_name, version, symbols |
| `BackendStatus` | 134–138 | Unknown / Online / Offline(String) |
| `AppState` | 143–165 | see §2.4 |

### 2.3 Assumptions the new `client` library must replace

1. **Hardcoded localhost:3000** — `BackendSettings::default()` (backend.rs:129). No runtime discovery, no TLS, no auth.
2. **Rust-only language code** — `RegisterPackageRequest.language` is always `"Rust"` (backend.rs:205). No multi-language capability exposed in GUI.
3. **Synchronous-looking pagination loop** — `load_library` (workspace.rs:154–230) runs a `while` loop over `PAGE_SIZE=50` calls inside one spawned task; no cancel token, no concurrent pages.
4. **No streaming / SSE** — all responses are full JSON blobs. The `/run` graph field is received (backend.rs:95) but never acted on (workspace.rs:249–258 only maps `matches`).
5. **Session as optional string** — UUID at boot (workspace.rs:76–77); updated from `/run` response (workspace.rs:253–255); `delete_session` never called on quit/window close.
6. **terminus_search URI construction** — client hardcodes `terminusdb:///data/Entry/Rust/unknown/{fq_name}` (backend.rs:252) when input is not already a URI. Bakes in TerminusDB scheme + language + "unknown" package slot.
7. **BackendClient is Clone + Send** — reqwest Client cloned into every spawned task (workspace.rs:147, 239, 311). Trait designs must keep `Clone + Send + 'static`.
8. **No offline/registry path** — every call hits the remote server unconditionally. No local cache, trust tier, or fallback.
9. **`get_packages` and `delete_session` are dead API** — implemented (backend.rs:184, 300) but zero call sites in views.
10. **No distinction trusted-local vs untrusted-remote** — single client, single base URL, no routing by package provenance.
11. **Health check is fire-and-forget once** — only at Workspace::new (workspace.rs:80–105); no reconnect poll; banner is dismissible but status is not re-probed.
12. **`PackageInfo.id` discarded** — register response is `let _ = …` (workspace.rs:152).

### 2.4 AppState field usage matrix

| Field | Default | Written | Read | Status |
|-------|---------|---------|------|--------|
| `backend_url` | `"http://localhost:3000"` | never after default | never | **dead** |
| `backend_status` | `Unknown` | health check Online/Offline | never by UI (banner uses `workspace.backend_error`) | **half-dead** |
| `loaded_chunks` | `{}` | load_library first/later pages | never by SearchPanel (panel has own `results`) | **write-only** |
| `available_packages` | `[]` | never | never | **dead** |
| `search_session` | UUID at boot | `/run` response | `run_semantic_search` | **live** |
| `current_graph` | empty | never | `toggle_graph_view` clone into GraphView | **read-only empty** |

Conclusion: `AppState` is a stub of the future model layer. Only `search_session` is fully live; `loaded_chunks` is written but unused by views; graph/backend_url/packages are scaffolding.

---

## 3. View-by-View Analysis

### 3.1 SearchPanel (search_panel.rs)

**State (search_panel.rs:63–73):**

| Field | Type | Notes |
|-------|------|-------|
| `collapsed` | `bool` | `w(px(0.)).overflow_hidden()` (search_panel.rs:408) |
| `input` | `Option<Entity<InputState>>` | lazy-init on first render (search_panel.rs:98–121) |
| `query_text` | `String` | mirrors InputState |
| `results` | `Vec<SymbolMatchResponse>` | starts as `fixture_symbols()` (search_panel.rs:90) |
| `loaded_libs` | `Vec<String>` | chip list; dismiss removes lib rows |
| `preview_mode` | `bool` | compact vs kind+snippet rows |
| `kind_filter` | `KindFilter` | All/Function/Struct/Enum/Trait/Other |
| `focus_handle` | `FocusHandle` | Focusable |

**What it renders (search_panel.rs:188–501):**
- Header: "Search" + "Preview" toggle chip
- Lib chips row when `loaded_libs` non-empty (search_panel.rs:448–476)
- Search input (full-width styled text box)
- Kind filter chip row (6 chips) (search_panel.rs:198–236)
- Scrollable result list: name + score; preview mode adds kind badge + truncated snippet (80 chars); "+" button opens tab
- Empty states: "Load a library to start searching" | `No symbols matching "{query}"`

**Events emitted:**
- `SymbolOpenRequested { fq_name, terminus_uri? }` — row / "+" click (search_panel.rs:167–169)
- `SemanticSearchRequested { query }` — Enter (search_panel.rs:109–114)

**terminus_uri derivation (search_panel.rs:261–263):** only if `occurrence_id.starts_with("terminusdb:///")`. Fixture occurrence_ids are `axum::Router::0` style — no URI. Semantic hits get `terminusdb:///data/{uri}` from `to_symbol_match` (backend.rs:79).

**Fixture bug (critical):**
`run_search()` (search_panel.rs:143–165) always filters `fixture_symbols()`, **not** `self.results`. Sequence:
1. User loads library → `load_results` merges real symbols into `self.results` (search_panel.rs:125–140).
2. User types any character → `InputEvent::Change` → `run_search` → `self.results = filter(fixture_symbols())` → real data discarded.

Kind filter uses `visible_results()` over `self.results` (search_panel.rs:179–185) and is fine; only the local text filter is broken.

**Width:** fixed `px(320.)` (search_panel.rs:404). Ignores `NudoxSettings.sidebar_width` (default 240.0).

### 3.2 SymbolView (symbol_view.rs)

**State:**
- `entry: SymbolEntry` — mutated in place by Workspace after fetch (workspace.rs:322–325)
- `is_loading: bool` — markdown shows `*Loading documentation…*` (symbol_view.rs:239–240)

**What it renders:**
- Breadcrumb row: non-last segments clickable → `SymbolOpenRequested` with parent_fq = join of crumbs (symbol_view.rs:172–218, click at 195–199)
- Header: `fq_name` (xl semibold) + visibility badge pub / pub(crate) / private (symbol_view.rs:220–237, 319–330)
- Scrollable body:
  - Documentation via `TextView::markdown(doc_id, doc_md).selectable(true)` (symbol_view.rs:355–356)
  - Kind section `render_kind_section` (symbol_view.rs:37–166): Function/Method signature, Struct fields+generics, else fallback
  - Members list: clickable → `SymbolOpenRequested` with `terminus_uri: None` (symbol_view.rs:276–278)

**open_tab / fixture interaction (workspace.rs:283–342):**
- Lookup `fixture_entries()` by `fq_name`
- Heuristic `from_fixture = entry.documentation.is_some()` (workspace.rs:288) — fixtures have docs → skip backend; stubs have `None` → fetch
- Stub: `stub_entry` (workspace.rs:435–447) with Terminus URI id and null kind

**Missing:** syntax highlighting, source links, version selector, diff, implementations, "used by", hyperlinked types.

### 3.3 GraphView (graph_view.rs)

**State:**
- `graph: ExplorationGraph` — via `set_graph` (graph_view.rs:62–65)
- `hovered: Option<String>`

**Render:**
- Empty: "No graph yet — run a semantic search to populate" (graph_view.rs:70–79)
- Populated: canvas width/height = `CENTER_X*2` × `CENTER_Y*2` = 680×560 (graph_view.rs:196–197, constants 15–16)
  - Edge layer: `canvas()` + `Path` lines, muted 40% opacity (graph_view.rs:89–113)
  - Nodes: absolute divs, circular layout radius 200 (graph_view.rs:18–35, 116–194)
  - Click → `SymbolOpenRequested { terminus_uri: None }` (graph_view.rs:163–170)
  - Scrollable outer container (graph_view.rs:205–217) but no pan/zoom transform

**Hover bug:** `is_hovered` compares `hovered` to `entry.id` (graph_view.rs:130) but `on_mouse_move` stores `fq` (`entry.fq_name`) (graph_view.rs:158–160). Unless `id == fq_name`, hover highlight never applies.

**Critical data gap:** `AppState.current_graph` is never written. `run_semantic_search` ignores `resp.graph` (workspace.rs:249–258). Toggle only clones empty graph (workspace.rs:371–378). GraphView is UI-complete but permanently empty in normal use.

### 3.4 LocalIndexPanel (local_index_panel.rs)

**State (local_index_panel.rs:11–18):** `visible`, path input/text, `indexer_running`, `last_status`, `log_store`, focus.

**Render when visible (local_index_panel.rs:170–263):** header + close, description, path input, Run Indexer button, status; while running shows "See Logs panel for progress".

**Binary discovery `which_indexer` (local_index_panel.rs:116–134):**
1. `which::which("nudox-indexer")`
2. Relative fallbacks: `../../Backend/target/{debug,release}/nudox-indexer`, `../Backend/target/debug/nudox-indexer`

**Subprocess (local_index_panel.rs:136–168):**
```text
Command::new(binary)
  .arg(project_path)                 # positional argv[1], NOT a --flag
  .env("NUDOX_PROJECT_PATH", project_path)
  .stdout/stderr piped → LogStore as LogLevel::Info, LogCategory::Other, target "nudox::indexer"
```

**Concurrency (local_index_panel.rs:73–112):** dedicated OS thread + `mpsc`; async side polls with `async_io::Timer::after(200ms)`. No cancel. Status on success tells user to "Reload library to see new results" — does **not** auto-refresh SearchPanel.

**Not a project model:** path is ephemeral panel state only; not persisted; not associated with trust tier; not watched for file changes.

### 3.5 LogPanel (log_panel.rs)

**State:** `visible`, `store`, `level_filter`, `category_filter`, `auto_scroll` (log_panel.rs:10–17).

**Render (log_panel.rs:78–324):**
- Fixed height `px(220.)` (log_panel.rs:237)
- Toolbar: category chips All/User/Svc/State/UI/Other; level All/ERR/WRN/INF/DBG/TRC; count; "↓ Auto" toggle; Clear
- Rows: `HH:MM:SS.mmm | LEVEL | CAT | message` (log_panel.rs:164–229)

**auto_scroll gap:** flag toggled (log_panel.rs:296–298) but scroll container is plain `overflow_y_scroll` (log_panel.rs:315–321) with **no** `ScrollHandle` / scroll-to-bottom.

**Notify path:** Workspace drains `notify_rx` and `cx.notify()`s itself (workspace.rs:109–119). LogPanel re-reads via `store.entries()` which **clones entire VecDeque** (log_store.rs:130–131), then filters again. Double full clone possible when counting (`filtered_entries` + `store.entries().len()` at log_panel.rs:84–86).

### 3.6 LogStore (log_store.rs) — shared infrastructure

- Ring buffer `MAX_ENTRIES = 2000` (log_store.rs:93)
- Categories from tracing target prefixes `nudox::{ui,state,services,user}` else Other (log_store.rs:20–31)
- `NudoxLogLayer` visitor appends non-message fields as `key=value` text (log_store.rs:187–206)
- Bounded notify channel 256; `try_send` drops on full (log_store.rs:108, 127–128)

### 3.7 Views not present (inventory of absences)

There is **no** dedicated view for: project picker, dependency tree, trust badges, job queue, settings UI, version timeline, symbol diff, module tree, registry sync status, compiler progress, or package browser beyond title-bar text input.

---

## 4. settings.rs — Configurable State

`NudoxSettings` (settings.rs:9–14) is a GPUI global with three fields:

| Field | Type | Default | Used |
|-------|------|---------|------|
| `dark_mode` | bool | false | theme on load; title-bar toggle mutates global |
| `sidebar_width` | f32 | 240.0 | **never read** — SearchPanel hardcodes 320 |
| `font_size` | f32 | 14.0 | **never read** |

**File path:** `~/Library/Application Support/nudox/settings.json` via `$HOME` (settings.rs:95–99) — **macOS-only path string**; no XDG, no `dirs` crate, no Windows path.

**Load / create:** `init` loads file or default; creates dir + writes default JSON if missing (settings.rs:49–57).

**Hot-reload:** 2s poll on background executor; on change, `set_global` + `Theme::change` + `cx.refresh()` (settings.rs:59–91). Does **not** apply `sidebar_width` / `font_size` even after reload.

**Theme toggle persistence gap:** title-bar button (workspace.rs:614–630) flips `cx.global_mut::<NudoxSettings>().dark_mode` and calls `Theme::change` but **never writes** `settings.json`. Restart reverts to file contents. Hot-reload can overwrite in-memory toggle if file differs.

**Missing settings for librarification:**
- `server_url` / timeout
- indexer binary path
- local registry / cache directory
- project last-opened paths
- embedded Tantivy index roots
- auth tokens / API keys
- trust policy defaults
- log retention / log level override beyond `RUST_LOG`

---

## 5. fixtures.rs — Fake Data

### 5.1 Contents

| API | Lines | Content |
|-----|------:|---------|
| `fixture_symbols()` | 5–61 | 5× axum 0.7.0 SymbolMatchResponse (Router, get, State, Handler, Response); scores 0.98–0.80 |
| `fixture_entries()` | 63–150 | 2× SymbolEntry: `axum::Router` (3 members) + `axum::extract::State`; kind uses `_type` |
| `fixture_library_chunk()` | 153–159 | wraps symbols as LibraryChunk |
| unit tests | 161–192 | 3 tests — **only tests in crate** |

### 5.2 Call sites

| Call site | Behavior |
|-----------|----------|
| `SearchPanel::new` results = `fixture_symbols()` | search_panel.rs:90 — cold start shows axum |
| `run_search` filters `fixture_symbols()` | search_panel.rs:145 — **overwrites real results** |
| `open_tab` finds `fixture_entries()` | workspace.rs:283–286 — full docs without backend |
| `fixture_library_chunk` | tests only |

### 5.3 Production impact

- Release builds still include fixtures (no `cfg(debug_assertions)`).
- Users always see axum sample data until they type (then still only fixtures) or load a library (until they type again).
- Fixture Router/State open without network; other symbols need backend or show empty docs after failed fetch.

---

## 6. gpui-component Usage

### 6.1 Widgets currently used

| Widget/Component | File:line (import/use) | Usage |
|------------------|------------------------|-------|
| `Root` | main.rs:14, 96 | Window root / theming shell |
| `TitleBar` | main.rs:14, 90; workspace.rs:527 | Native title bar + content |
| `Button` / `ButtonVariants` | workspace.rs:4; local_index_panel.rs:3; log_panel.rs:3 | Index/Graph/Logs/theme, Clear, Run Indexer |
| `Input` / `InputState` / `InputEvent` | workspace, search_panel, local_index_panel | search, lib load, indexer path |
| `Tab` / `TabBar` | workspace.rs:7–8, 476–495, 681 | Symbol tabs |
| `SidebarToggleButton` | workspace.rs:6, 538–542 | Collapse search |
| `Theme` / `ThemeMode` | workspace.rs:8; settings.rs:2 | Dark/light |
| `TextView::markdown` | symbol_view.rs:3, 356 | Docs |
| `IconName` Sun/Moon | workspace.rs:602–606 | Theme icon |
| `h_flex` / `v_flex` / `div` | everywhere | Layout |
| FluentBuilder | several | `.when` / `.when_some` |

### 6.2 gpui-component capabilities NOT yet used

Based on longbridge/gpui-component public component surface (tables, docks, trees, etc.):

| Component | Librarification fit |
|-----------|---------------------|
| **Dock / DockPanel / Resizable** | Replace hardcoded 320/220 layout; honor `sidebar_width` |
| **Table / DataTable** | Virtualised symbol hit list |
| **Tree** | Module/crate/project tree, dependency graph outline |
| **Drawer** | LocalIndexPanel / job details overlay |
| **Modal / Popover** | Project open, trust confirmations, settings |
| **Toast / Notification** | Pipeline progress, sync complete (vs danger banner) |
| **ContextMenu** | Symbol actions (open, copy URI, pin version) |
| **Checkbox / Switch / Select** | Settings form |
| **Indicator / Badge / Skeleton** | Backend status, trust tier, loading |
| **Scrollbar** | Consistent scroll UX for logs |

Highest-ROI for librarification: **Dock**, **Table**, **Tree**, **Toast**.

### 6.3 Raw GPUI primitives used without gpui-component

- `canvas` + `Path` for graph edges (graph_view.rs:89–105)
- Manual chip divs instead of Badge/Toggle components
- Manual error banner instead of Toast

---

## 7. Gap Analysis for Target Capabilities

### 7.1 Project assignment & trust management

**Exists:** Nothing first-class. Closest:
- Title-bar lib load for remote crates by `"name version"` (workspace.rs:398–422, parse at 425–432)
- LocalIndexPanel path string for one-shot indexer (local_index_panel.rs:60–66)
- No project entity, no path persistence, no Cargo.toml/workspace parsing, no trusted/untrusted labels

**Must be built:**
- `Entity<Project>` / `ProjectStore`: `{ root, members, path_deps, third_party, trust_map }`
- Project open UX (picker + recent)
- Trust policy UI: local workspace + path deps TRUSTED (embedded compiler); crates.io/registry UNTRUSTED (remote INDEX → local REGISTRY)
- Persist open projects + trust overrides in settings/SQLite
- Wire Search/Symbol queries through trust-aware `CompositeSymbolClient`

### 7.2 Embedded compiler invocation & progress

**Exists:** External `nudox-indexer` subprocess with stdout→logs, no structured progress (local_index_panel.rs:136–168).

**Must be built:**
- In-process compiler library API (workspace `compiler` crate) with event stream
- `JobStore` + progress UI (files, symbols, errors)
- Cancellation (`AbortHandle` / token)
- On complete: inject trusted symbols into local indexes + SearchPanel without "reload library" manual step
- Replace fragile relative binary paths

### 7.3 Dependency delegation (remote → syncing → local)

**Exists:** `register_package` + paginated `symbol_search` only (workspace.rs:145–234). Response `PackageInfo` ignored. No blob store, no SQLite registry, no sync state.

**Must be built:**
- Registry client trait + sync state machine: `Pending → RemoteServing → SyncingBlobs → Local`
- UI chips/rows per dependency with status
- Transparent client routing once local store warm
- Failure UX when remote unavailable and local cold

### 7.4 Search-first UX over embedded Tantivy + vectors

**Exists:** Fixture local filter (broken vs real data) + remote `/run` semantic search + remote `/symbol-search` for library load.

**Must be built:**
- Embed Tantivy on local disk (per-project + registry cache)
- Embed/local vector index for semantic
- Debounced multi-backend search with merge ranking
- Fix `run_search` bug (search_panel.rs:143–145)
- Query mode detection (identifier vs natural language)
- Cancel in-flight searches when query changes

### 7.5 Symbol browsing with rendered docs

**Exists:** Strongest area — markdown docs, breadcrumb, kind signatures, members (symbol_view.rs full file).

**Must be built:**
- Syntax highlighting in docs/code
- Source deep links
- Hyperlinked types in signatures
- Impls / "used by"
- Version pin + multi-version compare entry points

### 7.6 Graph / lineage views

**Exists:** Circular GraphView UI complete; data path dead.

**Must be built:**
1. Immediate: deserialize `RunSearchResponse.graph` → `ExplorationGraph` → `AppState.current_graph` in `run_semantic_search` (workspace.rs:249–258)
2. Fix hover id vs fq_name bug (graph_view.rs:130 vs 158–160)
3. Layout beyond circle; pan/zoom; edge labels by `EdgeRelation`
4. Expand-neighbourhood via client
5. Dedicated lineage view (version ancestry / call graph)

### 7.7 Version diff across generations

**Exists:** None. `lib_version` on hits is display-only (not used for multi-version open).

**Must be built:** version list API, pinned fetch, side-by-side/unified diff UI, timeline control on SymbolView.

### 7.8 Job / log observability

**Exists:** Solid tracing → LogStore → LogPanel filter/clear. Indexer lines multiplexed into same store.

**Gaps:**
- auto_scroll not implemented
- full buffer clone each notify
- no per-job isolation
- no export/copy
- no structured field table

**Must be built:** `JobStore` + job panel; ScrollHandle; append-only read API (`since(seq)`); optional job-scoped filters.

### 7.9 Capability readiness matrix (summary)

| Capability | Exists | Quality | Effort to target |
|------------|--------|---------|------------------|
| Project assignment | No | — | Large |
| Trust management UI | No | — | Large |
| Trusted local compile | Subprocess only | Poor | Large |
| Untrusted remote INDEX | HTTP all-or-nothing | Prototype | Medium (client trait) |
| Registry sync UI | No | — | Large |
| Embedded Tantivy | No | — | Large |
| Embedded vectors | No | — | Large |
| Symbol docs browse | Yes | Good prototype | Medium polish |
| Graph explore | UI only | Broken data | Small fix + Medium layout |
| Version diff | No | — | Large |
| Logs | Yes | Good prototype | Small–Medium |
| Jobs / progress | No | — | Medium |
| Settings | Minimal | Incomplete | Medium |
| Auth / multi-user | No | — | Large |

---

## 8. Refactor readiness: trusted-local vs untrusted-remote

### 8.1 Current topology (single path)

```
                    ┌─────────────────────┐
  All GUI views ──► │ Workspace.backend   │ ──HTTP──► localhost:3000
                    │ BackendClient only  │
                    └─────────────────────┘
  LocalIndexPanel ──std::process──► nudox-indexer (fire-and-forget)
```

There is **no** branch on package trust. Local indexer output does not re-enter the GUI data path automatically.

### 8.2 Target topology (librarification)

```
  Views ──subscribe──► Stores ──► dyn SymbolClient / SearchClient / GraphClient
                                      │
                    ┌─────────────────┴─────────────────┐
                    ▼                                   ▼
           LocalTrustedClient                  RemoteIndexClient
           (embedded compiler,                 (HTTP INDEX)
            Tantivy, local registry)                    │
                    ▲                                   ▼
                    └──── RegistryStore (sqlite+blobs) ─┘
                         takes over after sync
```

### 8.3 Seams that already help

| Existing idiom | Why reusable |
|----------------|--------------|
| `Entity` + events (`SymbolOpenRequested`, …) | Views already event-driven; can subscribe to stores instead of Workspace |
| `cx.spawn` + `Compat` | Pattern for any async client impl |
| `AppState` global | Sketch of shared model (needs store entities) |
| `LogStore` notify channel | Template for JobEvent channels |
| `BackendClient: Clone` | Matches trait object + Arc pattern |
| Wire types in `types.rs` | Can move to shared `client` / `ir` crates |

### 8.4 Seams that block dual-path

| Blocker | Evidence | Fix direction |
|---------|----------|---------------|
| Concrete `BackendClient` field on Workspace | workspace.rs:37, 71–72 | Inject `Arc<dyn SymbolClient>` or store entity |
| Hardcoded URL + Rust language | backend.rs:129, 205 | Settings + multi-lang register |
| Fixtures baked into SearchPanel | search_panel.rs:90, 145 | cfg-gated or empty default + real cache |
| Graph response discarded | workspace.rs:249–258 | Parse into ExplorationGraph |
| No project/trust types | types.rs entire file | New domain types in client crate |
| Indexer outside client abstraction | local_index_panel.rs | Compiler/job service trait |
| Session delete never on shutdown | backend.rs:300 unused | Lifecycle hooks in client |
| `loaded_chunks` vs panel.results split brain | types.rs vs search_panel | Single SymbolStore |
| Theme/settings incomplete | settings.rs | Expand schema before dual endpoints |

### 8.5 Recommended migration order (GUI only)

1. **Bugfix ship:** `run_search` filter; graph wire-up; hover id; optional ScrollHandle.
2. **Introduce traits in-repo** (`SymbolClient`, `SearchClient`) with `HttpSymbolClient` = current BackendClient.
3. **Extract stores:** SymbolStore, JobStore from Workspace methods without changing layout.
4. **ProjectStore + trust labels** in UI (even if both tiers still call HTTP initially).
5. **LocalTrustedClient** when embedded compiler/Tantivy land.
6. **Composite router** by trust; remove fixtures from non-debug builds.
7. **Dock layout + settings URL** for multi-environment (local INDEX vs cloud).

---

## 9. Structural Recommendation

### 9.1 Proposed module tree

```
workspace/gui/src/
├── main.rs                  # unchanged boot shape
├── app/
│   ├── app_state.rs         # slim session/UI flags (not business data)
│   ├── settings.rs          # expanded NudoxSettings + XDG paths
│   └── keybindings.rs       # extracted from main.rs
├── store/                   # model layer over client library
│   ├── project_store.rs     # projects, trust map, roots
│   ├── symbol_store.rs      # results, open entries, sessions
│   ├── job_store.rs         # compile/index/sync jobs
│   └── registry_store.rs    # package sync + local cache status
├── client/                  # thin shim → workspace client crate
│   ├── traits.rs            # SymbolClient, SearchClient, GraphClient, CompilerClient
│   ├── http.rs              # today's BackendClient
│   ├── local.rs             # trusted path (future)
│   └── composite.rs         # trust router
├── workspace/
│   └── workspace.rs         # layout shell only
├── views/
│   ├── title_bar.rs
│   ├── project_panel.rs     # NEW
│   ├── search_panel.rs
│   ├── symbol_view.rs
│   ├── graph_view.rs
│   ├── job_panel.rs         # NEW
│   ├── log_panel.rs
│   ├── local_index_panel.rs # becomes compile panel or merges into jobs
│   └── settings_view.rs     # NEW
├── types.rs                 # re-export shared wire types
└── fixtures.rs              # #[cfg(debug_assertions)] only
```

### 9.2 Store / model layer pattern

Each store is a GPUI `Entity<T>` owned by Workspace; views get `Entity` clones and subscribe:

```rust
// Conceptual — not current code
pub struct SymbolStore {
    client: Arc<dyn SymbolClient>,
    results: Vec<SymbolMatchResponse>,
    open_entries: HashMap<String, SymbolEntry>,
    search_session: Option<String>,
    current_graph: ExplorationGraph,
}

impl SymbolStore {
    pub fn semantic_search(&mut self, query: String, cx: &mut Context<Self>) {
        // spawn Compat; update results + graph; cx.emit(ResultsUpdated); cx.notify()
    }
}
```

This retires Workspace as a god-object for `load_library` / `run_semantic_search` / `open_tab` backend logic (workspace.rs:145–343).

### 9.3 Background task orchestration

- Keep `cx.spawn(Compat::new(…)).detach()` for HTTP until client embeds its own runtime strategy.
- Prefer `cx.background_executor().spawn_blocking` for indexer/compiler CPU over `std::thread` + 200ms poll (local_index_panel.rs:73–99).
- Job handles: store abort sender in `JobStore`; UI Cancel button.
- Progress: reuse LogStore pattern — `async_channel::Sender<JobEvent>` drained by a single Workspace/JobPanel task.

### 9.4 Client library trait sketch

```rust
pub trait SymbolClient: Send + Sync + Clone + 'static {
    async fn health(&self) -> Result<(), ClientError>;
    async fn search_symbols(&self, q: SymbolQuery) -> Result<Vec<SymbolMatchResponse>, ClientError>;
    async fn get_symbol(&self, uri: &str) -> Result<SymbolEntry, ClientError>;
    async fn semantic_search(&self, q: &str, session: Option<&str>) -> Result<RunSearchResponse, ClientError>;
    async fn register_package(&self, name: &str, version: &str, lang: &str) -> Result<PackageInfo, ClientError>;
}
```

Later: streaming semantic search; `CompilerClient::index_project(path) -> impl Stream<Item = Progress>`; `RegistryClient::sync(coord) -> SyncStatus`.

`CompositeSymbolClient` routes:
- trusted coordinates → Local (Tantivy + embedded IR)
- untrusted → HTTP until RegistryStore reports Local

### 9.5 Immediate fixes (low-effort, high-impact)

1. **Fix `run_search()`** (search_panel.rs:143–145): filter `self.results.clone()` (or retain+filter in place), not `fixture_symbols()`.
2. **Wire graph:** in `run_semantic_search` (workspace.rs:249–258), `serde_json::from_value(resp.graph)` → `AppState.current_graph`; if graph view visible, `set_graph`.
3. **Fix GraphView hover** (graph_view.rs:130 vs 158): store and compare the same key (`entry.id`).
4. **LogPanel ScrollHandle** for `auto_scroll`.
5. **`server_url` in NudoxSettings** → `BackendSettings` at Workspace construction.
6. **`#[cfg(debug_assertions)]` fixtures**; release starts with empty results.
7. **Indexer:** `spawn_blocking` + configurable binary path; on success call SymbolStore refresh.
8. **Persist theme toggle** to settings.json (or always hot-reload as source of truth and write on toggle).
9. **Bind ToggleLocalIndex** or add View menu entry.
10. **Call `delete_session` on window close** when session exists.

---

## 10. Cross-cutting bugs & debt register

| ID | Severity | Issue | Evidence |
|----|----------|-------|----------|
| B1 | High | Local search wipes real results | search_panel.rs:143–145 |
| B2 | High | Graph never populated | workspace.rs:249–258; types.rs:149 |
| B3 | Medium | Hover highlight broken (fq vs id) | graph_view.rs:130, 158–160 |
| B4 | Medium | auto_scroll no-op | log_panel.rs:15, 315–321 |
| B5 | Medium | Theme toggle not persisted | workspace.rs:614–630 vs settings write only at create |
| B6 | Medium | Fixtures in production UX | search_panel.rs:90; fixtures.rs |
| B7 | Low | sidebar_width / font_size ignored | settings.rs:12–13; search_panel.rs:404 |
| B8 | Low | macOS-only settings path | settings.rs:95–99 |
| B9 | Low | ToggleLocalIndex unbound / unmenu'd | main.rs:22, 53–78 |
| B10 | Low | Session never deleted | backend.rs:300 unused |
| B11 | Low | LogStore full clone each frame | log_store.rs:130–131 |
| B12 | Info | dead AppState fields | types.rs:144, 147 |
| B13 | Info | dead BackendClient methods | backend.rs:184, 300 |
| B14 | Info | from_fixture heuristic = docs present | workspace.rs:288 — false positive if live entry has docs stub |
| B15 | Info | Indexer relative paths fragile | local_index_panel.rs:122–126 |
| B16 | Info | Graph layout collapses >~10 nodes | circular_layout graph_view.rs:18–35 |

---

## 11. Open Questions / Risks

1. **smol vs Tokio under load:** `async-compat` + reqwest internal runtime + future embedded Tantivy/compiler I/O. Need one runtime strategy before embedding heavy crates.
2. **Graph JSON contract:** `graph: Value` untyped (backend.rs:95). Must freeze schema before force-directed layout investment.
3. **LogStore clone cost** at 2000 entries during compile spam → frame drops. Need seq-based reads.
4. **Fixture leakage** to end users if debug guard forgotten.
5. **`nudox-indexer` discovery** breaks when monorepo layout moves; need app resource or settings path.
6. **Linux/Windows settings paths** currently wrong for non-macOS `$HOME/Library/...`.
7. **No authentication** for planned remote INDEX / multi-user.
8. **`SymbolEntry.kind: Value` dual `@type`/`_type`** (types.rs:85–90) fragile vs IR evolution — negotiate versioned schema in client crate.
9. **Detached tab subscriptions** (workspace.rs:301–304) may retain closed SymbolView entities.
10. **No session persistence** — open tabs, projects, search history die on restart.
11. **gpui-component git dep unpinned** in Cargo.toml (unlike gpui rev pin) — reproducibility risk.
12. **Edition 2024** — toolchain floor for all contributors.
13. **Health check one-shot** — offline → online transitions invisible after dismiss.
14. **Trust UX ethics:** users must never confuse remote INDEX docs with local unverified edits; UI must always show provenance (local compile vs remote blob).

---

## 12. Executive Summary

The `workspace/gui` crate (`lindsey`, ~3.5k LOC, 12 modules) is a **structurally coherent GPUI prototype**, not a product-ready shell for librarification. Boot is correct: LogStore precedes tracing, gpui-component `Root`/`TitleBar` wrap a single `Workspace` entity, and async work uses the established `cx.spawn` + `async_compat::Compat` bridge around reqwest (itself constructed on a throwaway Tokio runtime). Cross-view communication via GPUI events (`SymbolOpenRequested`, `SemanticSearchRequested`) and globals (`AppState`, `LogStore`, `NudoxSettings`) is consistent and should be preserved.

What the app can do today: load a third-party crate by name/version against `http://localhost:3000`, paginate symbol hits into a hand-rolled search list, open symbol tabs with markdown documentation and kind/member navigation, toggle a graph canvas and log panel, and launch an external `nudox-indexer` subprocess. SymbolView is the most complete surface. LogPanel + LogStore form a solid observability skeleton.

What it cannot do relative to the planned architecture: there is **no project model**, **no trust tier**, **no local registry**, **no embedded Tantivy/vector search**, **no job system**, **no version diff**, and **no dual-path client**. Every data fetch is unauthenticated HTTP to a hardcoded host. `AppState` fields for `backend_url`, `available_packages`, and `current_graph` are scaffolding; `loaded_chunks` is written but unused by the panel. Critical functional bugs block even the prototype path: local search refilters **fixtures only** (discarding loaded libraries), `/run`'s `graph` field is never parsed so GraphView stays empty, graph hover compares the wrong identifier, and `auto_scroll` is a no-op.

Refactor readiness is **moderate on idioms, low on domain model**. The clean migration is: (1) fix the high-severity data bugs, (2) extract `SymbolClient` trait + store entities without changing layout, (3) introduce Project/Job/Registry stores and trust routing, (4) swap LocalTrusted vs RemoteIndex implementations under the same traits. Preserve entity/subscription/async-compat patterns; replace the Workspace god-object's HTTP methods and the fixture-coupled SearchPanel as the first real seams for the `client` library.

---

## 13. Top Concrete Recommendations

1. Fix `SearchPanel::run_search` to filter `self.results`, not `fixture_symbols()` (search_panel.rs:143–145).
2. Parse `RunSearchResponse.graph` into `ExplorationGraph` and store in `AppState.current_graph` during `run_semantic_search` (workspace.rs:249–258).
3. Introduce `SymbolClient` trait now; keep `BackendClient` as `HttpSymbolClient` only.
4. Extract `SymbolStore` / `JobStore` / `ProjectStore` entities; demote Workspace to layout shell.
5. Replace indexer `std::thread` + 200ms poll with `background_executor().spawn_blocking` and auto-refresh search on success.
6. Add `server_url` (and later registry paths) to `NudoxSettings`; wire into `BackendSettings`.
7. Guard fixtures with `#[cfg(debug_assertions)]`; empty results in release.
8. Adopt gpui-component Dock/Resizable for sidebar/log heights; honor `sidebar_width`.
9. Implement LogPanel `ScrollHandle` auto-scroll; add seq-based LogStore reads.
10. Design Composite client trust router UI early (badges on every symbol hit) so provenance is never ambiguous.

---

*End of audit. All file:line citations re-verified against `/Users/philocalyst/Projects/Backend/workspace/gui` on 2026-07-16.*
