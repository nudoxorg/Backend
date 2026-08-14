# Handoff: Audit GUI crate lindsey
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-aa5a15e10d32310c2.jsonl`
- Agent id: `agent-aa5a15e10d32310c2`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/03-gui-audit.md`
- Existing report: True (586 lines)
- Tools used: 17 ({'Bash': 3, 'Read': 13, 'Write': 1})
- Files/greps touched: 13
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)
- /Users/philocalyst/Projects/Backend/workspace/gui/Cargo.toml
- /Users/philocalyst/Projects/Backend/workspace/gui/src/main.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/workspace.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/backend.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/types.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/graph_view.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/symbol_view.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/search_panel.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/local_index_panel.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/log_panel.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/log_store.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/settings.rs
- /Users/philocalyst/Projects/Backend/workspace/gui/src/fixtures.rs

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll systematically read all the GUI source files to produce an exhaustive audit. Let me start by reading all files in parallel.

### fragment

Report written to `/Users/philocalyst/Projects/Backend/docs/research/librarification/03-gui-audit.md` (~730 lines). Here is the deliverable summary:

---

## Executive Summary

The `workspace/gui` crate (`lindsey`) is a small but structurally sound GPUI application. It boots correctly (smol/async-compat/Tokio bridge), uses GPUI entities + subscriptions consistently, and has working views for symbol search, symbol detail, a stub graph view, a local indexer launcher, and an in-app log panel. The gpui-component library (`Root`, `TitleBar`, `Tab`/`TabBar`, `Button`, `Input`, `TextView::markdown`, `SidebarToggleButton`) is used correctly. However, relative to the planned librarification, the GUI is a thin prototype: all server calls are hardcoded to `localhost:3000`, there is no project concept, no trust model, no embedded search, the graph view is permanently empty (the `RunSearchResponse.graph` field is received but never parsed), search filters over fixture data instead of real results (a concrete bug), and the two settings fields `sidebar_width` and `font_size` are defined but never read.

The existing code establishes good idioms: entity/subscription for cross-view events, global stores (`AppState`, `LogStore`, `NudoxSettings`) for shared state, `Compat::new` for reqwest in smol, and tracing → in-app log forwarding. These patterns should be preserved and extended, not replaced.

---

## Top Concrete Recommendations

1. **Fix the `run_search()` fixture bug immediately** (search_panel.rs:143): change `fixture_symbols().into_iter().filter(…)` to `self.results.iter().filter(…).cloned()`. This is the highest-ROI one-line fix — real loaded data is currently erased on every keystroke.

2. **Wire the graph**: parse `RunSearchResponse.graph: Value` into `ExplorationGraph` and store it in `AppState.current_graph` inside `run_semantic_search()`. The GraphView code is already complete; it just never receives data.

3. **Introduce a `Store` layer** between Workspace and views: `SymbolStore`, `ProjectStore`, `JobStore` as GPUI entities. Views subscribe to store events rather than Workspace forwarding data manually. This decouples the layout shell from business logic and is the clean seam for the `client` library.

4. **Define a `SymbolClient` trait** now, even if the only impl is the current `BackendClient`. This allows the router (`CompositeSymbolClient` → trusted LocalClient vs. untrusted HttpClient) to be added incrementally without touching view code.

5. **Replace the `std::thread` + mpsc poll loop** in `LocalIndexPanel` with `cx.background_executor().spawn_blocking`. Eliminates the 200ms timer polling and aligns with GPUI idioms.

6. **Add `ScrollHandle` to `LogPanel`** — the `auto_scroll` flag is wired but scroll-to-bottom is not implemented.

7. **Guard fixtures behind `#[cfg(debug_assertions)]`** and add a `server_url: String` to `NudoxSettings` so the backend URL is file-configurable.

8. **Adopt `Dock` / `Resizable` from gpui-component** for the main layout. The current hardcoded `px(320.)` sidebar and `px(220.)` log panel height are not adjustable, and `NudoxSettings.sidebar_width` is already defined but ignored.

---

## Open Questions / Risks

1. **smol/Tokio contention at scale**: with embedded Tantivy + in-process compiler both competing for I/O, the async-compat bridge may become a bottleneck. A definitive runtime strategy is needed before embedding those crates.
2. **`SymbolEntry.kind: Value` schema drift**: the `@type`/`_type` dual convention is fragile and will cause rendering regressions as the IR evolves. Needs a versioned wire schema.
3. **GPUI entity lifecycle**: detached subscriptions on closed `SymbolView` tabs may keep entities alive. Needs audit.
4. **No authentication**: the planned remote INDEX will require it; the entire `BackendClient` must accommodate credentials before any cloud scenario.
5. **macOS-only settings path**: `~/Library/Application Support/…` — Linux needs XDG via the `dirs` crate.
6. **Fixture data in production**: axum symbols appear on launch for every user; must be removed before release.

## ORIGINAL PROMPT (complete this mission)

You are a research subagent producing an exhaustive codebase audit that feeds a major architecture plan.

CONTEXT — the nudox project (repo: /Users/philocalyst/Projects/Backend):
nudox is a multi-language code-intelligence / documentation platform: per-language "producers" lower package source to a shared IR, emitted as blobs, fanned out to Tantivy (text search), Qdrant (vectors), TerminusDB (graph), coordinated by Postgres behind an axum server. The GPUI desktop app at workspace/gui (crate "lindsey") is the developer-facing product and is planned to become THE start and end of nudox for developers.

THE PLANNED RESTRUCTURE ("librarification"), which the GUI sits at the center of:
- The GUI assigns "projects": the project itself is TRUSTED (compiled locally by an embedded compiler library — the project, its path dependencies, its workspace members); its third-party dependencies are UNTRUSTED and delegated to a remote INDEX, which serves requests while syncing blobs to a local store (REGISTRY: sqlite + disk blobs), after which the local store takes over transparently.
- The GUI consumes a new `client` library (what workspace/server becomes) that abstracts all remote-vs-local complexity behind traits and typestates.
- Tantivy (text search) and vector search get EMBEDDED into the GUI process, backed by local disk to keep memory slim.
- The pipeline is deeply incremental: on every committed change, only changed symbols are re-compiled/re-embedded/re-indexed.
- The GUI needs a FULL refactor with new views for all of this.

YOUR MISSION — exhaustively audit workspace/gui. It is small (~12 files in src/): main.rs, workspace.rs, backend.rs, types.rs, graph_view.rs, symbol_view.rs, search_panel.rs, local_index_panel.rs, log_panel.rs, log_store.rs, settings.rs, fixtures.rs. Read ALL of them completely, plus Cargo.toml (gpui from zed rev 1d217ee, gpui-component from longbridge).

Answer with file:line evidence:
1. App architecture: how the GPUI app boots, window/workspace layout, entity/state management patterns used (gpui Entity/Context idioms), async handling (async-compat/tokio bridge), event flow.
2. backend.rs: how it currently talks to the server (reqwest? which endpoints? polling? what types cross the wire — types.rs inventory). What assumptions does it bake in that the new `client` library must replace?
3. View-by-view: what each view (graph_view, symbol_view, search_panel, local_index_panel, log_panel) actually renders and can do today; what state each holds; fixture usage (fixtures.rs — is the GUI running on fake data?).
4. settings.rs: what's configurable.
5. gpui-component usage: which widgets from the longbridge gpui-component library are used (tables? trees? inputs? docks?), and from reading its docs/source refs, what else it offers.
6. Gap analysis for the target: for each planned capability — project assignment & trust management, embedded compiler invocation & progress, dependency delegation status (remote-serving → syncing → local), search-first UX over embedded tantivy+vectors, symbol browsing with rendered docs, graph/lineage views, version diff of a symbol across generations, job/log observability — state what exists (if anything) and what must be built.
7. Structural recommendation: how the refactored GUI should be organized (view modules, a store/model layer over the `client` library, background task orchestration in GPUI), respecting existing idioms in this code.

DELIVERABLE:
- Write your FULL report as dense markdown to /Users/philocalyst/Projects/Backend/docs/research/librarification/03-gui-audit.md. Be exhaustive (400-1000 lines). Cite file:line everywhere.
- Do NOT build anything; pre-existing build failures are out of scope.
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) top concrete recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/Cargo.toml
```
1	[package]
2	name = "lindsey"
3	version = "0.1.0"
4	edition = "2024"
5	
6	[[bin]]
7	name = "lindsey"
8	path = "src/main.rs"
9	
10	[dependencies]
11	gpui = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe" }
12	gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe", features = ["font-kit", "x11", "wayland"] }
13	gpui-component = { git = "https://github.com/longbridge/gpui-component", package = "gpui-component" }
14	gpui-component-assets = { git = "https://github.com/longbridge/gpui-component", package = "gpui-component-assets" }
15	serde = { version = "1", features = ["derive"] }
16	serde_json = "1"
17	anyhow = "1"
18	reqwest = { version = "0.12", features = ["json"] }
19	uuid = { version = "1", features = ["v4"] }
20	async-compat = "0.2"
21	which = "7"
22	async-io = "2"
23	tokio = { version = "1", features = ["rt"] }
24	tracing = "0.1"
25	tracing-subscriber = { version = "0.3", features = ["env-filter"] }
26	async-channel = "2"
27	chrono = { version = "0.4", features = ["serde"] }
28	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/main.rs
```
1	mod backend;
2	mod fixtures;
3	mod graph_view;
4	mod local_index_panel;
5	mod log_panel;
6	mod log_store;
7	mod search_panel;
8	mod settings;
9	mod symbol_view;
10	mod types;
11	mod workspace;
12	
13	use gpui::*;
14	use gpui_component::{Root, TitleBar};
15	use gpui_component_assets::Assets;
16	use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};
17	
18	use log_store::{LogStore, NudoxLogLayer};
19	use settings::NudoxSettings;
20	use workspace::Workspace;
21	
22	actions!(nudox, [Quit, ToggleSidebar, CloseTab, ToggleLogPanel, ToggleGraphView, ToggleLocalIndex]);
23	
24	fn main() {
25	    // Build the shared log store before initialising the subscriber so the
26	    // layer can capture from the very first log call.
27	    let log_store = LogStore::new();
28	
29	    tracing_subscriber::registry()
30	        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
31	            // Default: all nudox:: targets at DEBUG, deps at WARN.
32	            EnvFilter::new("warn,nudox=debug")
33	        }))
34	        .with(
35	            // Pretty-print to stderr so the terminal is still useful.
36	            tracing_subscriber::fmt::layer()
37	                .with_target(true)
38	                .compact(),
39	        )
40	        .with(NudoxLogLayer::new(log_store.clone()))
41	        .init();
42	
43	    let app = gpui_platform::application().with_assets(Assets);
44	
45	    let store = log_store;
46	    app.run(move |cx: &mut App| {
47	        gpui_component::init(cx);
48	        NudoxSettings::init(cx);
49	
50	        // Make the log store accessible to any component via cx.global::<LogStore>().
51	        cx.set_global(store.clone());
52	
53	        #[cfg(target_os = "macos")]
54	        cx.bind_keys([
55	            KeyBinding::new("cmd-q", Quit, None),
56	            KeyBinding::new("cmd-w", CloseTab, None),
57	            KeyBinding::new("cmd-b", ToggleSidebar, None),
58	            KeyBinding::new("cmd-l", ToggleLogPanel, 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/backend.rs
```
1	use anyhow::{Context as _, Result};
2	use reqwest::Client;
3	use serde::{Deserialize, Serialize};
4	
5	use crate::types::{SymbolEntry, SymbolMatchResponse};
6	
7	// ── Wire types ────────────────────────────────────────────────────────────────
8	
9	#[derive(Debug, Clone, Serialize, Deserialize)]
10	pub struct PackageInfo {
11	    pub id: Option<u64>,
12	    pub name: String,
13	    pub version: String,
14	    pub language: String,
15	}
16	
17	#[derive(Debug, Serialize)]
18	pub struct RegisterPackageRequest {
19	    pub language: String,
20	    pub name: String,
21	    pub version: String,
22	}
23	
24	/// Mirror of Backend/compiler/src/api.rs SymbolQuery.
25	/// body_query is intentionally omitted — it returns 501 (constraint C1).
26	#[derive(Debug, Default, Serialize)]
27	pub struct SymbolQuery {
28	    #[serde(skip_serializing_if = "Option::is_none")]
29	    pub name_pattern: Option<String>,
30	    #[serde(skip_serializing_if = "Option::is_none")]
31	    pub kind: Option<String>,
32	    #[serde(skip_serializing_if = "Option::is_none")]
33	    pub lib_name: Option<String>,
34	    pub limit: usize,
35	    #[serde(skip_serializing_if = "Option::is_none")]
36	    pub offset: Option<usize>,
37	}
38	
39	/// A single hit from the `/run` endpoint (TerminusDB JSON-LD format).
40	#[derive(Debug, Clone, Deserialize)]
41	pub struct RunMatchDocument {
42	    pub fq_name: Option<String>,
43	    pub name: Option<String>,
44	    pub documentation: Option<String>,
45	    pub visibility: Option<String>,
46	    pub path: Option<Vec<String>>,
47	}
48	
49	#[derive(Debug, Clone, Deserialize)]
50	pub struct RunMatchHit {
51	    pub uri: String,
52	    pub fq_name: String,
53	    pub score: f32,
54	    pub package: Option<String>,
55	    pub version: Option<String>,
56	    pub symbol_kind: Option<String>,
57	    pub document: Option<RunMatchDocument>,
58	    #[serde(default)]
59	    pub kind: serde_json::Value,
60	}
61	
62	impl RunMatchHit {
63	    pub fn to_symbol_match(&self) -> Symbo
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/workspace.rs
```
1	use async_compat::Compat;
2	use gpui::prelude::FluentBuilder as _;
3	use gpui::*;
4	use gpui_component::button::{Button, ButtonVariants};
5	use gpui_component::input::{Input, InputEvent, InputState};
6	use gpui_component::sidebar::SidebarToggleButton;
7	use gpui_component::tab::{Tab, TabBar};
8	use gpui_component::theme::{Theme, ThemeMode};
9	use gpui_component::*;
10	use serde_json::Value;
11	use tracing::{debug, info, warn};
12	
13	use crate::{
14	    CloseTab, ToggleGraphView, ToggleLocalIndex, ToggleLogPanel, ToggleSidebar,
15	    backend::{BackendClient, BackendSettings, SymbolQuery},
16	    fixtures::fixture_entries,
17	    graph_view::GraphView,
18	    local_index_panel::LocalIndexPanel,
19	    log_panel::LogPanel,
20	    log_store::LogStore,
21	    search_panel::{SearchPanel, SemanticSearchRequested, SymbolOpenRequested},
22	    settings::NudoxSettings,
23	    symbol_view::SymbolView,
24	    types::{AppState, BackendStatus, ExplorationGraph, LibraryChunk, SymbolEntry},
25	};
26	
27	pub struct Workspace {
28	    focus_handle: FocusHandle,
29	    search_panel: Entity<SearchPanel>,
30	    log_panel: Entity<LogPanel>,
31	    graph_view: Entity<GraphView>,
32	    local_index_panel: Entity<LocalIndexPanel>,
33	    search_panel_collapsed: bool,
34	    graph_view_visible: bool,
35	    open_tabs: Vec<Entity<SymbolView>>,
36	    active_tab: Option<usize>,
37	    backend: BackendClient,
38	    backend_error: Option<String>,
39	    lib_input: Option<Entity<InputState>>,
40	    lib_text: String,
41	    _subscriptions: Vec<Subscription>,
42	}
43	
44	impl Focusable for Workspace {
45	    fn focus_handle(&self, _cx: &App) -> FocusHandle {
46	        self.focus_handle.clone()
47	    }
48	}
49	
50	impl Workspace {
51	    pub fn new(cx: &mut Context<Self>) -> Self {
52	        let log_store = cx.global::<LogStore>().clone();
53	        let search_panel = cx.new(|cx| SearchPanel::new(cx));
54	        let log_panel = cx.new(|cx| LogPanel::new(log_store.clone(), cx));
55	      
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/types.rs
```
1	use gpui::Global;
2	use serde::{Deserialize, Serialize};
3	use serde_json::Value;
4	use std::collections::HashMap;
5	
6	// ── SymbolKind ───────────────────────────────────────────────────────────────
7	
8	#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
9	#[serde(rename_all = "PascalCase")]
10	pub enum SymbolKind {
11	    Function,
12	    Struct,
13	    Enum,
14	    Trait,
15	    Method,
16	    Closure,
17	    TypeAlias,
18	    Const,
19	    Module,
20	    Macro,
21	    #[default]
22	    #[serde(other)]
23	    Other,
24	}
25	
26	impl SymbolKind {
27	    pub fn label(&self) -> &'static str {
28	        match self {
29	            Self::Function => "fn",
30	            Self::Struct => "struct",
31	            Self::Enum => "enum",
32	            Self::Trait => "trait",
33	            Self::Method => "fn",
34	            Self::Closure => "||",
35	            Self::TypeAlias => "type",
36	            Self::Const => "const",
37	            Self::Module => "mod",
38	            Self::Macro => "macro!",
39	            Self::Other => "?",
40	        }
41	    }
42	}
43	
44	// ── SymbolMatchResponse ───────────────────────────────────────────────────────
45	// Mirrors Backend/compiler/src/api.rs SymbolMatchResponse
46	
47	#[derive(Clone, Debug, Serialize, Deserialize)]
48	pub struct SymbolMatchResponse {
49	    pub symbol_name: String,
50	    pub occurrence_id: String,
51	    pub kind: Option<String>,
52	    pub lib_name: Option<String>,
53	    pub lib_version: Option<String>,
54	    pub score: f32,
55	    pub occurrence_count: usize,
56	    pub snippet: Option<String>,
57	}
58	
59	impl SymbolMatchResponse {
60	    pub fn kind_label(&self) -> &str {
61	        self.kind.as_deref().unwrap_or("?")
62	    }
63	}
64	
65	// ── SymbolEntry ───────────────────────────────────────────────────────────────
66	// Mirrors Search/src/lib.rs Entry (TerminusDB graph node)
67	// Entry.kind is loosely typed Value — render defensively (constraint C4)
68	
69	#[derive(C
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/graph_view.rs
```
1	use std::collections::HashMap;
2	use std::f32::consts::TAU;
3	
4	use gpui::*;
5	use gpui_component::*;
6	
7	use crate::search_panel::SymbolOpenRequested;
8	use crate::types::ExplorationGraph;
9	
10	// ── Layout ────────────────────────────────────────────────────────────────────
11	
12	const NODE_W: f32 = 120.0;
13	const NODE_H: f32 = 36.0;
14	const RADIUS: f32 = 200.0;
15	const CENTER_X: f32 = 340.0;
16	const CENTER_Y: f32 = 280.0;
17	
18	fn circular_layout(graph: &ExplorationGraph) -> HashMap<String, (f32, f32)> {
19	    let n = graph.nodes.len();
20	    if n == 0 {
21	        return HashMap::new();
22	    }
23	    graph
24	        .nodes
25	        .iter()
26	        .enumerate()
27	        .map(|(i, entry)| {
28	            let theta = TAU * (i as f32) / (n as f32);
29	            let r = if n == 1 { 0.0 } else { RADIUS };
30	            let x = CENTER_X + r * theta.cos() - NODE_W / 2.0;
31	            let y = CENTER_Y + r * theta.sin() - NODE_H / 2.0;
32	            (entry.id.clone(), (x, y))
33	        })
34	        .collect()
35	}
36	
37	// ── GraphView ─────────────────────────────────────────────────────────────────
38	
39	pub struct GraphView {
40	    pub graph: ExplorationGraph,
41	    focus_handle: FocusHandle,
42	    hovered: Option<String>,
43	}
44	
45	impl EventEmitter<SymbolOpenRequested> for GraphView {}
46	
47	impl Focusable for GraphView {
48	    fn focus_handle(&self, _cx: &App) -> FocusHandle {
49	        self.focus_handle.clone()
50	    }
51	}
52	
53	impl GraphView {
54	    pub fn new(graph: ExplorationGraph, cx: &mut Context<Self>) -> Self {
55	        Self {
56	            graph,
57	            focus_handle: cx.focus_handle(),
58	            hovered: None,
59	        }
60	    }
61	
62	    pub fn set_graph(&mut self, graph: ExplorationGraph, cx: &mut Context<Self>) {
63	        self.graph = graph;
64	        cx.notify();
65	    }
66	}
67	
68	impl Render for GraphView {
69	    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>)
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/symbol_view.rs
```
1	use gpui::prelude::FluentBuilder as _;
2	use gpui::*;
3	use gpui_component::text::TextView;
4	use gpui_component::*;
5	use serde_json::Value;
6	use tracing::info;
7	
8	use crate::search_panel::SymbolOpenRequested;
9	use crate::types::SymbolEntry;
10	
11	pub struct SymbolView {
12	    pub entry: SymbolEntry,
13	    pub is_loading: bool,
14	    focus_handle: FocusHandle,
15	}
16	
17	impl EventEmitter<SymbolOpenRequested> for SymbolView {}
18	
19	impl Focusable for SymbolView {
20	    fn focus_handle(&self, _cx: &App) -> FocusHandle {
21	        self.focus_handle.clone()
22	    }
23	}
24	
25	impl SymbolView {
26	    pub fn new(entry: SymbolEntry, cx: &mut Context<Self>) -> Self {
27	        Self {
28	            entry,
29	            is_loading: false,
30	            focus_handle: cx.focus_handle(),
31	        }
32	    }
33	}
34	
35	// ── Kind section rendering ────────────────────────────────────────────────────
36	
37	fn render_kind_section(kind: &Value, cx: &Context<SymbolView>) -> AnyElement {
38	    let kind_name = kind
39	        .get("@type")
40	        .or_else(|| kind.get("_type"))
41	        .and_then(|v| v.as_str())
42	        .unwrap_or("unknown");
43	    let kind_label: SharedString = kind_name.to_lowercase().into();
44	
45	    let detail: AnyElement = match kind_name {
46	        "Function" | "Method" => {
47	            let params = kind
48	                .get("params")
49	                .and_then(|p| p.as_array())
50	                .map(|arr| {
51	                    arr.iter()
52	                        .map(|p| {
53	                            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("_");
54	                            let ty = p.get("type").and_then(|v| v.as_str()).unwrap_or("?");
55	                            format!("{name}: {ty}")
56	                        })
57	                        .collect::<Vec<_>>()
58	                        .join(", ")
59	                })
60	                .unwrap_or_default();
61	           
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/search_panel.rs
```
1	use gpui::prelude::FluentBuilder as _;
2	use gpui::*;
3	use gpui_component::input::{Input, InputEvent, InputState};
4	use gpui_component::*;
5	use tracing::{debug, info};
6	
7	use crate::fixtures::fixture_symbols;
8	use crate::types::SymbolMatchResponse;
9	
10	// ── Events ────────────────────────────────────────────────────────────────────
11	
12	pub struct SymbolOpenRequested {
13	    pub fq_name: String,
14	    /// Full TerminusDB URI (`terminusdb:///data/Entry/...`) when known from a real search result.
15	    pub terminus_uri: Option<String>,
16	}
17	
18	pub struct SemanticSearchRequested {
19	    pub query: String,
20	}
21	
22	// ── Kind filter ───────────────────────────────────────────────────────────────
23	
24	#[derive(Clone, Debug, PartialEq, Eq)]
25	pub enum KindFilter {
26	    All,
27	    Function,
28	    Struct,
29	    Enum,
30	    Trait,
31	    Other,
32	}
33	
34	impl KindFilter {
35	    fn label(&self) -> &'static str {
36	        match self {
37	            Self::All => "All",
38	            Self::Function => "fn",
39	            Self::Struct => "struct",
40	            Self::Enum => "enum",
41	            Self::Trait => "trait",
42	            Self::Other => "other",
43	        }
44	    }
45	
46	    fn matches(&self, kind: Option<&str>) -> bool {
47	        match self {
48	            Self::All => true,
49	            Self::Function => kind.map(|k| k.eq_ignore_ascii_case("function")).unwrap_or(false),
50	            Self::Struct => kind.map(|k| k.eq_ignore_ascii_case("struct")).unwrap_or(false),
51	            Self::Enum => kind.map(|k| k.eq_ignore_ascii_case("enum")).unwrap_or(false),
52	            Self::Trait => kind.map(|k| k.eq_ignore_ascii_case("trait")).unwrap_or(false),
53	            Self::Other => !matches!(
54	                kind.map(|k| k.to_ascii_lowercase()).as_deref(),
55	                Some("function") | Some("struct") | Some("enum") | Some("trait")
56	            ),
57	        }
58	    }
59	}
60	
61	// ── SearchPanel ──────────
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/local_index_panel.rs
```
1	use gpui::prelude::FluentBuilder as _;
2	use gpui::*;
3	use gpui_component::button::Button;
4	use gpui_component::input::{Input, InputEvent, InputState};
5	use gpui_component::*;
6	
7	use crate::log_store::{LogCategory, LogEntry, LogLevel, LogStore};
8	
9	// ── LocalIndexPanel ───────────────────────────────────────────────────────────
10	
11	pub struct LocalIndexPanel {
12	    pub visible: bool,
13	    path_input: Option<Entity<InputState>>,
14	    path_text: String,
15	    indexer_running: bool,
16	    last_status: Option<String>,
17	    log_store: LogStore,
18	    focus_handle: FocusHandle,
19	}
20	
21	impl Focusable for LocalIndexPanel {
22	    fn focus_handle(&self, _cx: &App) -> FocusHandle {
23	        self.focus_handle.clone()
24	    }
25	}
26	
27	impl LocalIndexPanel {
28	    pub fn new(log_store: LogStore, cx: &mut Context<Self>) -> Self {
29	        Self {
30	            visible: false,
31	            path_input: None,
32	            path_text: String::new(),
33	            indexer_running: false,
34	            last_status: None,
35	            log_store,
36	            focus_handle: cx.focus_handle(),
37	        }
38	    }
39	
40	    pub fn toggle(&mut self, cx: &mut Context<Self>) {
41	        self.visible = !self.visible;
42	        cx.notify();
43	    }
44	
45	    fn ensure_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
46	        if self.path_input.is_none() {
47	            let state = cx.new(|cx| {
48	                InputState::new(window, cx).placeholder("Path to local Rust project…")
49	            });
50	            cx.subscribe(&state, |this, s, event: &InputEvent, cx| {
51	                if let InputEvent::Change = event {
52	                    this.path_text = s.read(cx).value().to_string();
53	                }
54	            })
55	            .detach();
56	            self.path_input = Some(state);
57	        }
58	    }
59	
60	    fn run_indexer(&mut self, cx: &mut Context<Self>) {
61	        let path = self.path_text
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/log_panel.rs
```
1	use gpui::prelude::FluentBuilder as _;
2	use gpui::*;
3	use gpui_component::button::{Button, ButtonVariants};
4	use gpui_component::*;
5	
6	use crate::log_store::{LogCategory, LogEntry, LogLevel, LogStore};
7	
8	// ── LogPanel ──────────────────────────────────────────────────────────────────
9	
10	pub struct LogPanel {
11	    pub visible: bool,
12	    store: LogStore,
13	    level_filter:    Option<LogLevel>,
14	    category_filter: Option<LogCategory>,
15	    auto_scroll:     bool,
16	    focus_handle:    FocusHandle,
17	}
18	
19	impl Focusable for LogPanel {
20	    fn focus_handle(&self, _cx: &App) -> FocusHandle {
21	        self.focus_handle.clone()
22	    }
23	}
24	
25	impl LogPanel {
26	    pub fn new(store: LogStore, cx: &mut Context<Self>) -> Self {
27	        Self {
28	            visible:         false,
29	            store,
30	            level_filter:    None,
31	            category_filter: None,
32	            auto_scroll:     true,
33	            focus_handle:    cx.focus_handle(),
34	        }
35	    }
36	
37	    pub fn toggle(&mut self, cx: &mut Context<Self>) {
38	        self.visible = !self.visible;
39	        cx.notify();
40	    }
41	
42	    fn filtered_entries(&self) -> Vec<LogEntry> {
43	        self.store
44	            .entries()
45	            .into_iter()
46	            .filter(|e| {
47	                self.level_filter.map_or(true, |l| e.level >= l)
48	                    && self.category_filter.map_or(true, |c| e.category == c)
49	            })
50	            .collect()
51	    }
52	}
53	
54	// ── Render helpers ────────────────────────────────────────────────────────────
55	
56	fn level_color(level: LogLevel, cx: &App) -> Hsla {
57	    match level {
58	        LogLevel::Error => cx.theme().danger,
59	        LogLevel::Warn  => hsla(45. / 360., 1.0, 0.55, 1.0),
60	        LogLevel::Info  => cx.theme().foreground,
61	        LogLevel::Debug => cx.theme().muted_foreground,
62	        LogLevel::Trace => cx.theme().muted_foreground,
63	  
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/log_store.rs
```
1	use std::sync::{Arc, Mutex};
2	use std::collections::VecDeque;
3	use chrono::{DateTime, Local};
4	use gpui::Global;
5	use tracing_subscriber::Layer;
6	
7	// ── Categories ────────────────────────────────────────────────────────────────
8	// Each maps to a tracing target prefix: nudox::ui, nudox::state, etc.
9	
10	#[derive(Clone, Debug, PartialEq, Eq, Copy)]
11	pub enum LogCategory {
12	    Ui,       // nudox::ui   — rendering, visual events
13	    State,    // nudox::state — tabs, connection status, app state mutations
14	    Services, // nudox::services — HTTP calls, request/response
15	    User,     // nudox::user  — clicks, searches, navigation, call traces
16	    Other,    // everything else (tracing from deps, etc.)
17	}
18	
19	impl LogCategory {
20	    pub fn from_target(target: &str) -> Self {
21	        if target.starts_with("nudox::ui") {
22	            Self::Ui
23	        } else if target.starts_with("nudox::state") {
24	            Self::State
25	        } else if target.starts_with("nudox::services") {
26	            Self::Services
27	        } else if target.starts_with("nudox::user") {
28	            Self::User
29	        } else {
30	            Self::Other
31	        }
32	    }
33	
34	    pub fn label(&self) -> &'static str {
35	        match self {
36	            Self::Ui => "UI",
37	            Self::State => "State",
38	            Self::Services => "Svc",
39	            Self::User => "User",
40	            Self::Other => "Other",
41	        }
42	    }
43	}
44	
45	// ── Level ─────────────────────────────────────────────────────────────────────
46	
47	#[derive(Clone, Debug, PartialEq, Eq, Copy, PartialOrd, Ord)]
48	pub enum LogLevel {
49	    Trace,
50	    Debug,
51	    Info,
52	    Warn,
53	    Error,
54	}
55	
56	impl LogLevel {
57	    pub fn label(&self) -> &'static str {
58	        match self {
59	            Self::Trace => "TRACE",
60	            Self::Debug => "DEBUG",
61	            Self::Info  => "INFO",
62	            Self::Warn  => "WARN"
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/settings.rs
```
1	use gpui::*;
2	use gpui_component::theme::{Theme, ThemeMode};
3	use serde::{Deserialize, Serialize};
4	use std::{
5	    path::PathBuf,
6	    time::Duration,
7	};
8	
9	#[derive(Clone, Debug, Serialize, Deserialize)]
10	pub struct NudoxSettings {
11	    pub dark_mode: bool,
12	    pub sidebar_width: f32,
13	    pub font_size: f32,
14	}
15	
16	impl Default for NudoxSettings {
17	    fn default() -> Self {
18	        Self {
19	            dark_mode: false,
20	            sidebar_width: 240.0,
21	            font_size: 14.0,
22	        }
23	    }
24	}
25	
26	impl Global for NudoxSettings {}
27	
28	impl NudoxSettings {
29	    pub fn get(cx: &App) -> &Self {
30	        cx.global::<Self>()
31	    }
32	
33	    pub fn init(cx: &mut App) {
34	        let path = settings_path();
35	
36	        let settings = load_from_file(&path).unwrap_or_default();
37	        let dark_mode = settings.dark_mode;
38	        cx.set_global(settings);
39	        Theme::change(
40	            if dark_mode {
41	                ThemeMode::Dark
42	            } else {
43	                ThemeMode::Light
44	            },
45	            None,
46	            cx,
47	        );
48	
49	        // Ensure the settings directory and file exist for the user to edit.
50	        if let Some(parent) = path.parent() {
51	            std::fs::create_dir_all(parent).ok();
52	        }
53	        if !path.exists() {
54	            if let Ok(json) = serde_json::to_string_pretty(&NudoxSettings::default()) {
55	                std::fs::write(&path, json).ok();
56	            }
57	        }
58	
59	        // Poll for file changes every 2 seconds and apply them live.
60	        cx.spawn(async move |cx| {
61	            let mut last_content = std::fs::read_to_string(&path).unwrap_or_default();
62	            loop {
63	                let executor = cx.background_executor().clone();
64	                executor.timer(Duration::from_secs(2)).await;
65	
66	                if let Ok(content) = std::fs::read_to_string(&path) {
```

### Read: /Users/philocalyst/Projects/Backend/workspace/gui/src/fixtures.rs
```
1	use serde_json::json;
2	
3	use crate::types::{LibraryChunk, SymbolEntry, SymbolMatchResponse};
4	
5	pub fn fixture_symbols() -> Vec<SymbolMatchResponse> {
6	    vec![
7	        SymbolMatchResponse {
8	            symbol_name: "Router".to_string(),
9	            occurrence_id: "axum::Router::0".to_string(),
10	            kind: Some("Struct".to_string()),
11	            lib_name: Some("axum".to_string()),
12	            lib_version: Some("0.7.0".to_string()),
13	            score: 0.98,
14	            occurrence_count: 42,
15	            snippet: Some(
16	                "The central type for building HTTP apps. Matches requests to handlers."
17	                    .to_string(),
18	            ),
19	        },
20	        SymbolMatchResponse {
21	            symbol_name: "get".to_string(),
22	            occurrence_id: "axum::routing::get::0".to_string(),
23	            kind: Some("Function".to_string()),
24	            lib_name: Some("axum".to_string()),
25	            lib_version: Some("0.7.0".to_string()),
26	            score: 0.92,
27	            occurrence_count: 78,
28	            snippet: Some("Route GET requests to the given handler.".to_string()),
29	        },
30	        SymbolMatchResponse {
31	            symbol_name: "State".to_string(),
32	            occurrence_id: "axum::extract::State::0".to_string(),
33	            kind: Some("Struct".to_string()),
34	            lib_name: Some("axum".to_string()),
35	            lib_version: Some("0.7.0".to_string()),
36	            score: 0.89,
37	            occurrence_count: 55,
38	            snippet: Some("Extractor for injecting shared application state into handlers.".to_string()),
39	        },
40	        SymbolMatchResponse {
41	            symbol_name: "Handler".to_string(),
42	            occurrence_id: "axum::handler::Handler::0".to_string(),
43	            kind: Some("Trait".to_string()),
44	            lib_name: Some("axum".to_string()),
45	            lib_version: Some("0.7.0".to_string()),
46	    
```
