# Desktop GUI hard-nosed audit

Repo: `/Users/mileswirht/Downloads/backend`, branch `canonical` @ `4b86cbfb5` (2026-09-25).
Method: direct source reading of `apps/desktop/src` (25,400 LOC), `tools/gui-harness`
(9,600 LOC), and the backend contract crates (`crates/library`, `crates/client`). No
cargo/build was run (forbidden for this agent). Two prior static reviews exist —
`docs/reviews/old-backend-parity-v10.md` and `docs/reviews/testing-cutover-research-v10.md`,
both dated 2026-09-22 — but the desktop crate was rewritten from scratch starting
2026-09-21 (`f34a815a6`, `ba40264f3`, "replace shell state with versioned runtime
architecture" / "cut over to one versioned GPUI owner") and has had 8 more commits
since those reviews (latest desktop commit `9f19591a2`, 2026-09-25). Every finding
below was re-verified against the current tree; where a prior review's finding is
now stale (fixed) or still true, that is stated explicitly.

---

## 1. Module map

Legend: KEEP = solid, ship as-is. KEEP-WITH-FIXES = right shape, has a concrete
defect. REWRITE = wrong shape for what it claims to do. DELETE = dead/ceremony.

### `core/` — shared contracts, no GPUI, no I/O

| File | LOC | Verdict | Why |
|---|---|---|---|
| `core/ids.rs` | 639 | KEEP | Stable product identities (`LocalProjectId`, `PackageId`, `VersionedRoot`, `ResourceIdentity`); 4 tests. Cross-platform path admission is handled carefully (`native_wire`, `service_coordinate`). |
| `core/state.rs` | 291 | KEEP | `Resource<T>` sealed availability algebra (NotYet/Loading/Loaded/Unavailable/Fault) with closed `FaultCode`/`UnavailableReason`. Messages are control-char-stripped and bounded to 512 bytes at construction (`ErrorValue::new`, state.rs:45-75) — good discipline, this is exactly the kind of typed-unavailable state `docs/reviews/old-backend-parity-v10.md` asked for. |
| `core/tokens.rs` | 74 | KEEP | Closed token enums (palette channel, surface family, density, semantic mark) per ARCHITECTURE-V3.md. |
| `core/ports.rs` | 33 | KEEP | Two narrow traits (`SnapshotReadModel`, `IntentDispatcher`) — real dependency inversion, not ceremony; used to keep views decoupled from `UiRootEntity`. |
| `core/layout/resolver.rs` | 599 | KEEP | Pure function from window size + text scale + panel prefs → `ResponsiveLayout`. 9 tests. |
| `core/layout/{input,regions,tokens,adapter,transition}.rs` | 157/210/86/116/134 | KEEP | Clean split: inputs, typed region output, geometry tokens, GPUI boundary cache, transition plan. No overlap. |
| `core/layout/test_support.rs` | 128 | KEEP-WITH-FIXES | Docstring says "Capture matrix contract for a **future** native GPUI `test_support` harness" — i.e. an admitted stub for work not yet done. Not harmful, but it is scaffolding without a consumer today; confirm before shipping that nothing silently depends on it. |
| `core/layout.rs`, `core/mod.rs` | 23/22 | KEEP | Thin re-export shells, no logic — appropriately small. |

### `model/` — durable + derived state, no GPUI

| File | LOC | Verdict | Why |
|---|---|---|---|
| `model/snapshot.rs` | 524 | KEEP-WITH-FIXES | `AppSnapshot`/`SnapshotData` is a well-built copy-on-write branch structure (1 test proves branch sharing). **But** `CatalogState` (snapshot.rs:173-178, doc comment: "The live registry catalog admitted by the current producer root") is a *single* `Resource<CatalogState>` slot shared between (a) the home/project shelf grid and (b) whatever narrow `SurfaceCommand` a package page last issued. See §2 for the resulting data-corruption bug. |
| `model/persistence.rs` | 1272 | KEEP | Crash-safe durable JSON (temp-write + rename + directory sync per ARCHITECTURE-V3.md), versioned schema, explicit `PersistenceRecovery::Preserved` path that backs up unadmitted state instead of discarding it (`host/launch.rs:159-164` surfaces this). 8 tests. This directly answers the `old-backend-parity-v10.md` P1 finding about missing fsync — current code calls `sync_all` (per architecture doc) and quarantines bad state rather than dropping it. |
| `model/workspace.rs` | 293 | KEEP | Typed `WorkspaceProject`/`ProjectPhase` (Indexing/Cancelling/Cancelled/Ready/Failed/Missing) lifecycle, shared by onboarding, shelf, settings. This is a real, non-trivial state machine (verified via `onboarding.rs` and `project_shelf.rs` consumers). |
| `model/selectors.rs` | 251 | KEEP | Keyed selector/geometry LRU caches, 3 tests. |
| `model/viewport.rs` | 150 | KEEP | Bounded virtualization state, shared by source/document viewports per architecture doc. |
| `model/local_package/{mod,cargo,manifest,readme}.rs` | 368/330/406/184 | KEEP | Real offline package-fact pipeline: `cargo metadata --no-deps --offline` with typed failure fallback to hand-rolled `Cargo.toml` TOML reading, plus a README→structured-block projector (headings/paragraphs/bullets/code). `tests.rs` (592 LOC, 15 tests) runs against real fixture folders, not mocks. This is the best-built vertical slice in the whole crate. |
| `model/mod.rs` | 28 | KEEP | Re-export shell. |

### `navigation/` — pure reducer + typed intents, no GPUI I/O

| File | LOC | Verdict | Why |
|---|---|---|---|
| `navigation/reducer.rs` | 385 | KEEP | Pure `reduce(snapshot, intent) -> Reduction`. 6 tests including a documented back/forward/zoom-out identity law. `Intent::Navigate` deliberately does **not** re-trigger a root/catalog refresh (reducer.rs:17-20) — correct for a pure reducer, but see §2, this is *why* the catalog-corruption bug persists across navigation. |
| `navigation/workspace_reducer.rs` | 567 | KEEP | Pure reducer for project lifecycle/settings; 5 tests. |
| `navigation/route.rs` | 368 | KEEP | `Coordinate` (control-char rejecting), `RouteDepth` (Orbit→Package→Page→Source), typed route data. 2 tests. |
| `navigation/focus.rs` | 962 | KEEP | Largest navigation file; typed focus scopes/keyboard traversal/route restoration, 14 tests — the most heavily tested single file in the crate. |
| `navigation/action.rs` | 414 | KEEP | Stable `ActionId` enum + accessibility metadata + palette label/shortcut. |
| `navigation/history.rs` | 152 | KEEP | Bounded (64-entry, per architecture doc) persistent route stack, 2 tests. |
| `navigation/modal.rs` | 84 | KEEP | Modal stack with typed focus restoration, 1 test. |
| `navigation/intent.rs` | 290 | KEEP | Typed `Intent`/`Effect`/`EngineCommand` — the one vocabulary every reducer and the runtime share. |
| `navigation/journey_specs.rs` | 497 | KEEP | 7 named onboarding journeys (ColdEmpty, PersistedRestart, PickerCancelled, Indexing, ReadyMultiProject, FailureRetry, McpSetup) with typed step/screenshot-state sequences — real QA scaffolding, consumed by `harness.rs` (see §5). |
| `navigation/mod.rs` | 25 | KEEP | Re-export shell. |

### `runtime/` — off-UI-thread engine actor + GPUI entity

| File | LOC | Verdict | Why |
|---|---|---|---|
| `runtime/actor.rs` | 611 | KEEP | Dedicated OS thread (`nudox-engine-actor`) plus a second local-read thread (`nudox-local-reads`) for filesystem/`cargo metadata` work, so a slow local read can never stall producer root/surface requests (actor.rs:131-136 doc comment, verified by design). Bounded coalescing mailboxes both ways. Cancellation tokens are real (`CancellationToken`, atomic bool). 1 test. |
| `runtime/coordinator.rs` | 377 | KEEP | `DesktopRuntime` is the single state owner; `poll()` rejects any event whose basis doesn't match the current root as `RejectedStale` (coordinator.rs:294-309) — genuine staleness handling, not decorative. |
| `runtime/mapping.rs` | 436 | KEEP-WITH-FIXES | `map_event` for `EngineDto::Root/Object/Index/LocalPackage` is careful (checks request id, authority, producer generation before admitting — 3 tests prove the staleness laws). The `EngineDto::Surface` arm (mapping.rs:131-171) is the one weak spot: it recognizes only `Explored/Package/IndexSearch/PackageVersions/PackageProfile` and folds all of them into the *same* `CatalogState.packages` field used by the home shelf (see §2); every other `SurfaceReply` variant (`Dependencies`, `Dependents`, `Advisory`, `Diff`, `References`, `Read`, `ForgeAdd/Reference`, `Owner`, `SemanticVersions*`, `Subscribe*`, `Releases`, `Project*`, `Tree*`) silently falls into `_ => None` and is discarded after a real network round trip. |
| `runtime/client.rs` | 332 | KEEP | `LocalEngineClient` — the only place that knows the producer is a `backend_client::Session`; reconnect-on-disconnect via `with_reconnect` (grep-confirmed pattern), correctly kept off the UI thread. |
| `runtime/mailbox.rs` | 247 | KEEP | Bounded coalescing mailbox, generic over `Coalescible`, 3 tests. |
| `runtime/animation.rs` | 963 | KEEP | `CaptureFrameClock`/`AnimationTimeline`, retargeting, spring/tween, reduced-motion snap; 13 tests including a deterministic-retargeting proof. |
| `runtime/ui_graph.rs` | 708 | KEEP | `UiRootEntity` — one root GPUI entity, no second state owner (matches ARCHITECTURE-V3.md's claim precisely — verified, not just trusted). `ensure_surface`/`ensure_local_package` (ui_graph.rs:325-383) are the two idempotent boundary calls views use instead of reaching into a client directly — good pattern, undermined only by what mapping.rs does with the reply. |
| `runtime/wiring.rs`, `runtime/mod.rs` | 23/28 | KEEP | Re-export shells. |
| `runtime/tests.rs` | 277 | KEEP | Real threaded race tests: latest-root supersession, stop-cancels-in-flight, object-result-does-not-advance-root, bounded backpressure while UI isn't polling, coalescing retirement, and one full local-package-read-survives-a-root-advance test against real fixture files. This is genuinely good concurrency testing, not shallow. |

### `theme/`, `ui/` — presentation

| File | LOC | Verdict | Why |
|---|---|---|---|
| `theme/{mod,palette,ramp,tokens,kind,language,fonts}.rs` | 398/362/134/499/133/53/100 | KEEP | Closed Facet palette + geometry/typography tokens; theme owns the harness's semantic-action-frame collector (`begin_action_frame_with_modal`, `publish_action_frame`, used throughout `views/mod.rs` and `harness.rs`) — this is the backbone that makes the accessibility/semantic probe in §5 possible. 2+2+4 tests across palette/fonts/tokens. |
| `ui/components/{action_frames,action_tree,semantic,visual}.rs` | 440/268/405/563 | KEEP | Typed adapters around GPUI CE plus the semantic-action registry (id/role/state/bounds/relations) consumed by `harness.rs::capture_rendered_semantics`. `tests.rs` (405 LOC, 13 tests) exercises this directly. |
| `ui/icon.rs` | 380 | KEEP | Closed icon enum, embedded assets; "a view names one, it cannot invent a path" — deliberate closed-set design. |
| `ui/surface.rs`, `ui/text.rs` | 84/215 | KEEP | Small, single-purpose vocabulary (`cut`/`panel`/`sunken`/`ground`, text ladder). 2 tests in text.rs. |
| `ui/search_palette/mod.rs` | 201 | KEEP-WITH-FIXES | Real CE `CommandState` integration (not a fake list box). **But** `targets()`/`command_groups()` (mod.rs:117-159) only ever populate two groups: `Projects` (from `workspace().projects`) and `Actions` (from `ActionId::ALL`). There is no package/document/symbol search row at all, despite `crates/library` fully supporting `Search`/`Names`/`IndexSearch`/`Explore` — confirms `old-backend-parity-v10.md`'s omnibar finding is still current. |
| `ui/mod.rs`, `ui/components.rs` | 14/34 | KEEP | Re-export shells. |

### `views/` — route projections (see §3 for the full real-vs-stub breakdown)

| File | LOC | Verdict | Why |
|---|---|---|---|
| `views/onboarding.rs` | 289 | KEEP | Real per-phase state machine rendering (Indexing/Cancelling/Cancelled/Failed/Missing/Ready), each with its own copy and typed retry/cancel/repair action — directly answers the "empty corpus must never look like success" requirement from the parity review. |
| `views/catalog.rs` | 216 | KEEP-WITH-FIXES | Home/project package grid; real accessible list semantics (`role(List)`, `aria_label`). Reads the same `snapshot.catalog()` slot corrupted by `package.rs` (see §2). |
| `views/project_shelf.rs` | 258 | KEEP | Real per-row lifecycle controls (Reveal/Retry-or-Cancel/Remove), correctly disables actions with no target rather than hiding them. |
| `views/project_admission.rs` | 224 | KEEP | Add-project dialog with real path admission (quote-stripping, control-char rejection), 3 tests. |
| `views/local_package.rs` | 474 | KEEP | The richest view in the crate: renders real README blocks, grouped dependencies (Normal/Build/Development) with per-dependency user counts, features, license/rust-version/workspace facts — all from `LocalPackage`. |
| `views/workspace_settings.rs` | 812 | KEEP | 10 real settings pages (Appearance/Editor/Agents/Connections/Privacy/Diagnostics/Index/Registry/Legend/Help), each wired to a real `Intent` and reading real `snapshot.settings()`/`workspace()` fields; MCP command/config copy is generated per-workspace, not hardcoded. This directly contradicts `old-backend-parity-v10.md`'s "renders a page label and reduced-motion toggle" finding — that finding is now **stale**; settings has been built out substantially since. Only `Editor` is an admitted placeholder ("Local editor integration" note, honestly labeled, no editor integration exists). |
| `views/shell.rs` | 485 | KEEP | Titlebar, orbit rail, responsive sheet triggers, context rail — real chrome, correctly delegates dialog/sheet lifetime to CE `Root`. |
| `views/keys.rs` | 268 | KEEP | Platform-aware chord spelling (`cmd-k` vs `ctrl-k`). |
| `views/primitives.rs` | 138 | KEEP | Shared heading/crumb/fact/loading-card/route-label helpers. |
| `views/package.rs` | 200 | **REWRITE** | See §2/§3. Issues real `SurfaceCommand::{Package,Dependencies,Dependents,PackageVersions,Advisory}` requests via `ensure_surface` (package.rs:177-187), then **ignores the reply entirely** and renders one of five fixed strings selected only by `route.lane` (package.rs:147-168). Two of those five commands (`Package`, `PackageVersions`) also clobber the home/project shelf's catalog data (§2). This is the single most consequential file in the crate: it is the "crates.io-style package dossier" the command registry advertises (`crates/library/command_registry.rs:216-223`: "readme, versions, owners, dependencies, dependents, downloads, and how to install it") and today shows none of it. |
| `views/reader.rs` | 114 | **REWRITE** | `document_page` issues **no command at all** — no `ensure_surface`, no `Command::Document`/`Command::Source` call anywhere in the file — and renders a fixed sentence. `source_page` renders a literal hardcoded string: `"\n   1  // awaiting the live source projection"` (reader.rs:110). Meanwhile `crates/library::Command::Document`/`Command::Source` and the `Session::document`/`Session::source` client methods are fully implemented and return real `signature`/`fragments`/`excerpt`/`location` data (see §4). This is the largest own-goal in the codebase: the backend already answers this exact query and the view never asks. |
| `views/mod.rs` | 262 | KEEP | Legitimate shell composition (theme/layout/overlay sync); not itself broken, just the place that routes into the two REWRITE files above. |

### Harness / bin

| File | LOC | Verdict | Why |
|---|---|---|---|
| `harness.rs` | 1424 | KEEP | See §5 — this is genuinely sophisticated verification infrastructure, one real test (`ime_adapter_uses_focused_ce_state_for_compose_update_commit_cancel_and_blur`). |
| `bin/backend-desktop-gui-harness.rs` | 134 | KEEP | Thin CLI wrapper. |
| `host/{launch,lease,paths}.rs` | 198/341/173 | KEEP | Startup retry loop (12 attempts / 20s deadline, `launch.rs:18-21`), single-owner-wins file-lock lease (`lease.rs`), canonical per-platform data roots (`paths.rs`). 1+2 tests. |
| `lib.rs`, `main.rs`, `host/mod.rs` | 34/5/7 | KEEP | Thin composition roots. |

**Cross-cutting observations for §1:**
- No `todo!()`, `unimplemented!()`, `FIXME`, or `XXX` markers anywhere in `apps/desktop/src` (grep confirmed, zero hits). The crate's incompleteness never crashes; it under-delivers silently instead (static text, discarded replies). That is a different failure mode than "attempted, poorly" usually implies — the plumbing (actor/coordinator/reducer/persistence/harness) is well engineered; the two content-bearing route projections (package dossier, document/source reader) are not.
- No duplicated state authority was found — ARCHITECTURE-V3.md's claim that the old `store/*`/`reducer/*`/`transport/*` modules are physically gone is true (confirmed by directory listing: no such paths exist).
- The one real "duplicated authority" bug is the `CatalogState` slot itself (§2), which is an accidental single point of shared mutable truth for two logically distinct results, not an intentional second store.

---

## 2. Runtime & data flow, traced end to end

Talk path: `views/*.rs` → `UiRootEntity::ensure_surface`/`ensure_local_package`
(`runtime/ui_graph.rs:325-383`) → `DesktopRuntime::dispatch`
(`runtime/coordinator.rs:85-100`) → `EngineActor` on a dedicated background thread
(`runtime/actor.rs:367-481`) → `LocalEngineClient` (`runtime/client.rs`) →
`backend_client::Session` (Unix-socket, authenticated, `crates/client/src/lib.rs`) →
local `backend-locald`. Results return as `EngineEvent` through a bounded
`CoalescingMailbox`, are polled (`DesktopRuntime::poll`, coordinator.rs:284-330),
staleness/authority-checked, mapped (`runtime/mapping.rs::map_event`) into a new
`AppSnapshot`, and only then does GPUI re-render. **Nothing runs synchronously on
the UI thread** — every producer call happens on `nudox-engine-actor` (or
`nudox-local-reads` for filesystem/`cargo metadata` work), confirmed by
`actor.rs:390-398` spawning two named `thread::Builder` threads and the UI side
only ever calling `try_submit_coalesced`/`drain_events` (non-blocking).

### Real end-to-end trace: "open a package → its page shows declarations" (the exact scenario the task asked me to trace) — **it does not work today**

1. User clicks a package card (`views/catalog.rs:150-216` or `views/project_shelf.rs`), which queues `Intent::Navigate(Route::Package(PackageRoute{ lane: Overview, .. }))`.
2. `navigation/reducer.rs:17-20` runs `navigate(&mut next, route)` — a pure route change, **no engine command is emitted**.
3. `views/mod.rs:252` routes to `package::package_page`, which finds the package in `snapshot.catalog().loaded_value()` (already-known coarse facts from the initial `Explore` — name/version/downloads/standing/advisory, not declarations) and renders `package_sections` (`views/package.rs:140-200`).
4. `package_sections` calls `root.ensure_surface(SurfaceCommand::Package{package})` for the `Overview` lane (package.rs:185-187). This **does** reach the real backend — `Session::surface` → `SurfaceReply::Package(records)` with full `RegistryPackageRecord` data (readme is not actually in that DTO — see §4 caveat).
5. `runtime/mapping.rs:150-159` maps that reply into `current.with_catalog(CatalogState{packages}, ...)` — i.e. it **replaces the entire home/project shelf catalog** with a one-element list containing just this package.
6. `package_sections` then ignores the reply completely and renders one of five hardcoded strings (package.rs:147-168) — e.g. for Overview: `"The package overview is pinned to the exact release admitted by the local index. Open docs or source from the same object identity."` No readme, no version list, no dependency graph is ever shown.
7. If the user clicks "Read documentation" (package.rs:71-84), the reducer navigates to `Route::Page(PageRoute{...})`, which renders `reader::document_page` (`views/reader.rs:14-84`). This function has **no** call to `ensure_surface`, `Command::Document`, or any client method — it renders a fixed sentence and a "View source" button.
8. Clicking "View source" navigates to `Route::Source`, rendered by `reader::source_page` (`views/reader.rs:86-114`), which renders the literal string `"awaiting the live source projection"` (reader.rs:110) regardless of route/coordinate/basis.
9. Meanwhile, back on the Home/Project page, the shelf grid (`views/catalog.rs:57-71`) is still reading the *same* `snapshot.catalog()` field that step 5 just overwrote with a single package — so the user's package list is now silently truncated to 1 entry (or, if they had instead opened the "Releases" lane, replaced with that package's version-history rows) until the app either restarts or the user hits "Test connection" in Settings (the only other caller of `Intent::RefreshRoot`, `runtime/ui_graph.rs:201-213`) or indexes a brand-new project (which re-fetches `Explore` inside `EngineDto::Index`, `runtime/client.rs`).

Citations for the catalog-collision claim: `apps/desktop/src/model/snapshot.rs:173-178` (single `Resource<CatalogState>` field, doc comment says it's "the live registry catalog"), `apps/desktop/src/views/catalog.rs:57-71` (Home/Project reader), `apps/desktop/src/views/package.rs:177-187` (writer via `ensure_surface`), `apps/desktop/src/runtime/mapping.rs:149-171` (the fold that conflates them), `apps/desktop/src/runtime/ui_graph.rs:55-74,196-214` (the only two call sites of `Intent::RefreshRoot`, which is the only path that repopulates the full `Explore(limit:64)` catalog — ordinary navigation never does).

This is not a hypothetical: it is a straightforward, deterministic reproduction — visit any package's Overview or Releases tab, then return Home, and the shelf grid is wrong until one of the three narrow recovery paths above fires.

### What *is* real and working end to end
- Onboarding: folder picker → `Intent::AddProject`/`FolderPickerResult` → `Intent::IndexProject` → real `Session::index` call → `EngineDto::Index` → `ProjectPhase` transitions rendered live in `onboarding.rs`/`project_shelf.rs`, including cancel (real `CancellationToken` + a distinct `Cancelling` phase that waits for the producer, not an optimistic UI-only cancel).
- Settings → Connections → "Test connection" issues a real `Intent::RefreshRoot` and reflects `ConnectionStatus::{Testing,Connected,Disconnected}`.
- Local project dossier (`views/local_package.rs`) is fully real, offline, and does not touch the network path at all (correctly — a local project isn't in any registry).
- Persistence: cold start recovers shelf/session/settings from disk with a documented recovery path for corrupt state (`PersistenceRecovery::Preserved`, surfaced to the user via stderr in `host/launch.rs:159-164`).

---

## 3. Views: real vs. stub, per route

| Route | Renders | From what data | Verdict |
|---|---|---|---|
| Orbit / Home | Onboarding copy, first-launch CTA row, workspace-mismatch banner, active-project status card, package grid | `snapshot.workspace()` (real, live), `snapshot.catalog()` (real at cold start, corruptible — §2) | Real, functional |
| Orbit / Project | Project label + package grid (registry + local project cards) | Same as Home, filtered by `snapshot.project()` | Real |
| Package (Overview/Dependencies/Dependents/Releases/Security lanes) | Fixed-tab header + fact rail (downloads/standing/advisory/ecosystem from the *original* catalog row) + one of five **hardcoded** paragraph bodies | Fact rail: real. Body: **static text, command reply discarded** | Stub (data fetched, never shown) |
| Page (declaration/docs) | Crumb + heading (just the coordinate string) + "View source"/"Graph" buttons + one static paragraph | Nothing — no command issued | Pure stub |
| Source | Crumb + heading + 3 lines of literal placeholder text including line number | Nothing — no command issued, `_root`/`_snapshot`/`_cx` are all unused parameters (leading underscore) | Pure stub |
| Settings (10 pages) | Real per-page controls wired to real `Intent`s and `snapshot.settings()`/`workspace()` fields | Real, live | Real, functional (only `Editor` is an honestly-labeled non-feature) |
| Onboarding (empty-shelf / recovery states) | Per-phase card with typed retry/cancel/repair actions | `snapshot.workspace()` | Real, functional |
| Search palette | Two groups: "Projects" (from workspace) and "Actions" (from `ActionId::ALL`) | Real, but **no package/symbol/document search rows at all** | Partial — the omnibar the command registry advertises (`Search`, `Names`, `IndexSearch`) is entirely unwired to the palette |
| Local project dossier (non-registry package) | README blocks, grouped dependencies, features, license/rust-version facts | `LocalPackage` (real, offline, from `cargo metadata`/manifest) | Real, functional, best-built view in the crate |
| Project shelf / rail | Per-project row with status label + Reveal/Retry-or-Cancel/Remove | `snapshot.workspace()` | Real, functional |

---

## 4. Backend data available to a docs reader (this is the part that matters most)

Read directly: `crates/library/command.rs` (1115 LOC), `crates/library/surface.rs`
(1605 LOC), `crates/library/view/model.rs` (795 LOC), `crates/library/command_registry.rs`
(462 LOC, 40 closed commands), `crates/client/src/lib.rs` (1500 LOC, `Session`).

**The backend already returns far more than the desktop shows.** Per command:

- **`show`/`read` (registry name `show`, `CommandId::Document`/`Show`)** — `Session::document`/`document_symbol` return `backend_library::Document` (`view/model.rs:373-393`):
  - `signature: Option<String>` — canonical declaration signature
  - `fragments: Box<[Fragment]>` — ordered doc prose/code/links (`Fragment::{Text,Code,Link{label,target},Break}`, model.rs:356-370)
  - `location: SourceAvailability` — `Captured(SourceLocation)` / `NotCaptured` / `NotHydrated` / `Unconfigured` (model.rs:634-655) — a genuinely typed "why is source missing" state, never collapsed to empty
  - `excerpt: SourceExcerpt` — bounded captured source text with explicit availability/extent (from `backend_compile`)
  - `source: Option<Basis>` — full producer/branch/log/schema attestation, not just a root hash
  - `Document::text()` (model.rs:447-463) even provides a ready-made flattened rendering (signature + fragments) that a view could drop into a text block today with zero new backend work.
  - `outline` (`CommandId::Outline`) — `Outline` (model.rs:476-552): a real tree (`OutlineNode{symbol, children}`) plus `additional_roots` for forest-shaped packages and an explicit `OutlineExtent::{Complete,Truncated}` — no desktop view renders this at all (no "outline"/tree-of-contents UI exists anywhere in `views/`).
- **`source` (`CommandId::Source`)** — same `Document` type via `Session::source`; the excerpt/location fields above **are** the source text + spans the task asked about. The desktop's `source_page` never calls this.
- **`related`/`graph` (`CommandId::Related`/`Graph`)** — `Session::related`/`graph`/`graph_page` return a `ViewSnapshot` whose rows carry a `GraphRelation` sidecar (model.rs:597-613): `{from: RowId, to: RowId, relation: SemanticLinkKind}`. `SemanticLinkKind` (surface.rs:641-664) is a real closed vocabulary: `Calls, MethodCall, TypeReference, Reads, Writes, Imports, Implements, Overrides, Reexports, Inherits, Documents` — i.e. impl/trait/supertrait/caller edges are all present and typed, each with a `SemanticConfidence` (Syntactic/Heuristic/Indexed/Imported/Compiler, surface.rs:667-680). No desktop view renders a graph of any kind; the one "Graph" button in `reader.rs:58-73` just opens the command palette.
- **`references` (`CommandId::References`)** — `SurfaceCommand::References{target}` → `SurfaceReply::References{target, references: Box<[ReferenceRecord]>}` (surface.rs:1089-1094). Each `ReferenceRecord` (surface.rs:816-827) carries `site` (using declaration), `target` (`SemanticLinkTarget::{Local,Stable,Foreign,FragmentEntity}` — resolves across package/fragment boundaries, surface.rs:685-712), `relation`, and `evidence: SemanticLinkEvidence{confidence, source: Option<SemanticSourceSpan{file,start,end}>}` — i.e. an exact byte-range source citation per use-site. Not surfaced anywhere in the desktop.
- **`search` (`CommandId::Search`)** — `Session::search`/`search_page` return a ranked `ViewSnapshot` (rows carry `score: Option<u32>`, model.rs:675-676) with cursor-based pagination (`PageContinuation`) and can be bound to a `ReadManifest` dependency basis. Only used by the (nonexistent) search palette rows.
- **`package`/`package-versions`/`explore`/`dependents`/`dependencies`/`owner`/`index-search`/`package-profile`** — all return `RegistryPackageRecord` (surface.rs:857-885): `coordinate, ecosystem, name, version, bytes, standing: RegistryReleaseStanding{Available,Yanked,Deprecated,Unlisted,Retracted,Removed}, downloads: RegistryDownloadCount{Exact,Approximate,Unavailable(RegistryFactAvailability{NotRecorded,Unsupported,Unavailable,Stale,Unknown})}, native_metadata, forge_sources: Box<[RegistryForgeAssociation]>, advisory: AdvisoryPackageDto`. This is a rich, honestly-typed "unknown is never zero" fact set exactly matching what `docs/reviews/old-backend-parity-v10.md` asked to preserve — and it is fully wired into the `RefreshSurface` plumbing, just discarded at render time (§2/§3).
- **`health`** — `HealthReport` (command.rs:136-234): `revision: RevisionReceipt{root,cursor,source}`, `basis: Basis`, `coverage: Box<[Coverage]>` (per-lane `Complete`/`Partial{lane,completed,total}`/`Unavailable{lane,reason}` with `Reason::{NoIndex,Unconfigured,Offline,Cancelled,Incomplete}` and an explicit `is_outside_declared_scope`/`is_failed_lane` distinction so an unconfigured lane can't hold a healthy summary hostage), `row_count: u64`, `capabilities: CapabilityInventory`, `progress: IngestProgress`. This is genuinely sophisticated indexing-progress plumbing. The desktop's Diagnostics settings page shows only `key().generation()`/`observation()` (workspace_settings.rs:394-398) — none of `coverage`/`capabilities`/`progress` is surfaced anywhere in the UI.
- **`diff`** — `Session::diff` returns `Box<[DiffRecord]>` with `DeclarationChange::{Added,Removed,Changed,Indeterminate}` and per-declaration `SemanticLinkDelta::{Added,Removed,EvidenceChanged}` graph deltas. Not surfaced in the desktop at all (no "diff versions" UI).
- **`semantic-versions`/`select-semantic-version`** — immutable compiler-generation history per package/language profile, with `complete`/`selected` flags. Not surfaced.
- **`tree`/`tree-open`/`tree-close`** — a shared cross-surface session tree (desktop/CLI/MCP nodes with `TreeSubject::{Package,Declaration,Explore,Search,Owner}`). Not surfaced in the desktop UI (the shelf is a separate, desktop-local concept).

**Bottom line for §4:** almost every UI gap identified in §2/§3 is *not* a backend gap. `crates/library`/`crates/client` already expose signature+docs+source+excerpt, typed outline trees, typed graph edges with confidence, byte-span-cited references, and richly typed registry/health facts. The work required to fix `views/package.rs` and `views/reader.rs` is client-side rendering, not new backend surface.

---

## 5. Harness: `tools/gui-harness` + `apps/desktop/src/harness.rs`

**Capability inventory (verified by reading the code, not just the docs):**

- **Offscreen capture**: yes — `gpui_driver.rs` drives a real headless GPUI window (`HeadlessAppContext`), draws, and captures a PNG; a process-global mutex (`gpui_driver.rs:682-692`, per the v10 review) serializes headless GPUI contexts within one process.
- **Virtual/deterministic clock**: yes — `context.advance_clock(Duration)` per frame delta (`gpui_driver.rs:380-384`) plus the product's own `CaptureFrameClock`/`AnimationTimeline` (`runtime/animation.rs`) driven via `UiRootEntity::set_capture_time` so frame timestamps are reproducible.
- **Animation-frame sampling**: yes — `AnimationFrame{label, time_ms}` sequences with named phases (start/first-moving/midpoint/retarget/reversal/near-settled/settled/reduced-motion per `docs/operations/gui-testing.md:61-64`); `journey_frames` in `harness.rs:302-326` generates one frame per onboarding step.
- **Resize sequences**: yes — `InputStep::Resize{width,height}` is dispatched through the real compositor path (`gpui_driver.rs:599+`); `REQUIRED_VIEWPORTS` in `tools/gui-harness/src/lib.rs:60-72` declares **10** sizes: 640×480, 800×600, 900×600, 1024×768, 1280×800, 1440×900, 1440×1000, 1600×1000, 1920×1080, 2560×1440.
  - **Still-live discrepancy** (first flagged in `testing-cutover-research-v10.md`, re-verified today, **not fixed**): `docs/operations/gui-testing.md:111-115` still says "nine viewport sizes" while the code declares 10. This is a live, current mismatch, not a stale finding.
- **Text scale**: yes — `InputStep::TextScale{percent}` (`harness.rs:1238-1268`) drives the *same* `Intent::SetTextScale` the Appearance settings page uses (workspace_settings.rs:151-170), so it is testing the product path, not a harness-only shortcut.
- **Input injection**: yes, broad — Key/Text/FocusNext/FocusPrevious/PointerMove/Down/Up/Click/Drag/Scroll/Pinch/Clipboard/Paste/Modifiers/WindowFocus, all dispatched as real `gpui::PlatformInput` events (`gpui_driver.rs:449-620`).
- **IME**: **fixed since the v10 review.** That review (`testing-cutover-research-v10.md` finding #3, `tools/gui-harness/src/gpui_driver.rs:444-447`) found the generic driver accepted `ImeText/Compose/Commit/Cancel` as a silent no-op. Today, `apply_step` (gpui_driver.rs:417-448) *requires* an `ime_hook` closure to produce a real `ImeObservation` and validates it (`observation.validate_for(step)`) before treating the step as consumed; if the hook doesn't produce one, capture fails with `InputError::Ime`. The desktop supplies a real hook, `dispatch_ime` (`harness.rs:881-929`), which drives actual GPUI CE `EntityInputHandler` methods (`replace_text_in_range`, `replace_and_mark_text_in_range`) and reads back marked-range/selected-range state, with a dedicated `#[gpui::test]` (`harness.rs:1305-1423`) exercising compose→update→commit→cancel→blur-rejection. This finding should be considered resolved, not a current gap.
- **Semantic/accessibility probe**: yes, and fail-closed — `capture_rendered_semantics` (`harness.rs:938-1111`) cross-checks the declared focus owner against GPUI's *native* focus handle and returns `CaptureError::InvalidConfig` on any mismatch (harness.rs:951-956, 1053-1057), and requires every focusable action to have a measured (post-prepaint) bounds/tab-order or fails the capture (harness.rs:1010-1037) — a declared-but-unmeasured control cannot pass.
- **Live onboarding journeys**: yes, real — `JourneyDriver` (`harness.rs:398-786`) creates real temporary project trees (with Unicode/space/apostrophe names to stress path admission, `harness.rs:349-354`), drives the actual `Intent` sequence a user would (`FolderPickerResult`→`IndexProject`→`CancelIndex`/`ActivateProject`/`RemoveProject`/`RetryIndex`/`OpenSettings`/`TestConnection`) against a **real** `backend-locald` via `DesktopHost::start`, and `finish()` (harness.rs:766-785) fails the run if any expected screenshot state (from `navigation/journey_specs.rs`) was never actually observed — this is a genuine fail-closed live-index gate, not a fixture.
- **Design-contract / rendered-evidence gate**: yes — `design_contract.rs` (1143 LOC) extracts a machine-readable contract from the 4 HTML references under `Nudox-Design-System/artifacts`; `conformance.rs` (2197 LOC, the largest file in the harness crate) independently re-derives dimensions/crop/semantic relations/focus/motion from the raw PNG+manifest bytes and fails on stale route/root/probe-hash — I did not fully re-derive this 2197-line file line-by-line given the audit's time budget, but its exported surface (`verify_capture_run`, `ManifestEvidence`, `MatrixEvidence`, `UniformRegion`) matches the documented contract in `docs/operations/gui-architecture.md:137-176`.
- **Artifact/glitch detection**: yes — `diff.rs` provides tolerance-bounded pixel comparison (`DiffPolicy{channel_tolerance, max_changed_fraction, max_mean_error, max_channel_error}`), a bounding box of changed pixels, and an 8×8 perceptual luminance hash distance (`DiffMetrics`, diff.rs:47-72) — this is real glitch detection *given a baseline*; per `docs/operations/gui-architecture.md:153`, "the harness never generates a reference image," so a fresh visual regression with no prior baseline is invisible until one is captured once and pinned.
- **Missing: dedicated stress testing.** I found no journey or harness capability that opens/closes many overlays or tabs in rapid succession, or floods the actor's bounded mailboxes with concurrent producer requests, as a *harness-level* scenario. The closest analogue is `runtime/tests.rs::result_delivery_stays_bounded_while_the_ui_is_not_polling`, which is a unit test of the mailbox's own backpressure (bounded to capacity 1 in that test), not a harness/journey-level stress scenario against a live window. Desktop has no tabs today (single `Route`, no multi-document tab bar is actually wired despite `DocumentTab`/`DocumentState` existing in `model/snapshot.rs:99-128` — that state is defined but I found no view that populates or renders it), so "many tabs" isn't yet a meaningful stress axis; "many overlays" (rapid settings-open/command-palette-open/dialog-close cycling) is a plausible, currently-untested gap.
- **What the harness cannot catch**: none of the semantic/PNG/animation machinery above asserts anything about the *content* of a package or document page — it validates structure (focus order, bounds, route label, PNG provenance), not whether the rendered text matches what the backend actually returned. The catalog-corruption bug (§2) and the two hardcoded reader placeholders (§3) would sail through every existing harness gate, because nothing in `conformance.rs`/`semantics.rs` compares rendered text against a `SurfaceReply`.

---

## 6. Tests: what exists, what it actually asserts

`grep -c '#\[test\]|#\[gpui::test\]'` across `apps/desktop/src` → **138 test functions**,
concentrated as follows (file: count):

```
model/local_package/tests.rs   15   navigation/focus.rs            14
ui/components/tests.rs         13   runtime/animation.rs           13
core/layout/resolver.rs         9   model/persistence.rs            8
runtime/tests.rs                7   navigation/reducer.rs           6
navigation/workspace_reducer.rs 5   theme/tokens.rs                 4
core/ids.rs                     4   views/project_admission.rs      3
runtime/mapping.rs              3   runtime/mailbox.rs               3
model/selectors.rs              3   core/layout/transition.rs        3
(+ 1-2 each in ~14 more files)
```

**These are real assertions**, verified by directly reading three of them:
- `runtime/tests.rs` — genuine threaded races (latest-root supersedes stale in-flight
  work without the UI blocking; `Intent::Stop` cancels without waiting on the actor;
  an object read's delta is recorded without advancing the root; bounded mailbox
  backpressure while unpolled; coalescing correctly retires a replaced request; a
  local-package read on real fixture files survives a concurrent root advance).
- `views/project_admission.rs` — real path-admission edge cases (quoted paths from
  Windows Explorer/shell, blank input, whitespace trimming).
- `model/local_package/tests.rs` — loader/reader/README-projection tests against
  real fixture folders, not synthetic fixtures.

**Where the tests do *not* reach** — zero `#[test]`/`#[gpui::test]` functions in
`views/package.rs`, `views/reader.rs`, `views/catalog.rs`, `views/shell.rs`,
`views/onboarding.rs`, `views/mod.rs` (grep-confirmed). `views/local_package.rs` has
exactly one test, and it asserts data ownership (`owning_project`), not rendered
output. **This is precisely why the two REWRITE-grade defects in §2/§3 exist
undetected**: the reducer/runtime/persistence/animation/layout layers are tested
thoroughly; the route-projection layer that actually decides what a user sees for
a package or a document is not tested at all.

**Workspace-level end-to-end coverage**: `tests/journeys/tests/live_registry.rs`
does exercise `backend_desktop::harness::capture_live` and a helper
`assert_desktop_root` (live_registry.rs:701-780) against a **real** running
`backend-locald`, and does call `Session::document_symbol`/`source`/`graph_symbol`
and assert the reply *shape* (`matches!(reply, CommandReply::Document(_))`) and
basis equality. Two important caveats:
1. This test operates through `Session`/`LocalEngineClient` directly — it never
   touches `apps/desktop/src/views/*`, so it cannot see that `reader.rs` throws
   the document/source reply away. It is client-contract coverage, not
   view-rendering coverage.
2. It asserts shape/basis, not field content (no assertion on `document.excerpt`,
   `.signature`, or `.fragments` text) — consistent with the
   count-based/shape-based testing pattern that misses field loss.

**GUI-harness crate's own tests**: not separately enumerated in this pass given
the time budget, but `apps/desktop/src/harness.rs` contributes one substantive
`#[gpui::test]` for IME (harness.rs:1305-1423, described in §5) that does assert
on real marked-text/selection content, not just success/failure.

---

## Summary of the highest-value findings (ranked)

1. **P0 — `views/package.rs` fetches real data and shows none of it**, while two of
   its five lanes (Overview via `SurfaceCommand::Package`, Releases via
   `PackageVersions`) also **overwrite the home/project shelf's package catalog**
   with narrow, unrelated results (`model/snapshot.rs:173-178`,
   `views/catalog.rs:57-71`, `views/package.rs:177-187`, `runtime/mapping.rs:149-171`).
   Fix is two-part: (a) render the already-fetched `SurfaceReply` instead of
   static strings, (b) give per-page surface results their own snapshot field
   instead of reusing `CatalogState`.
2. **P0 — `views/reader.rs::document_page`/`source_page` are pure hardcoded
   placeholders** that never call the backend at all, despite
   `crates/library::Command::Document`/`Source` and `Session::document`/`source`
   already returning real signature/docs/source-excerpt/location data
   (`crates/library/view/model.rs:373-393`, `crates/client/src/lib.rs:597-647`).
3. **P1 — the command/search palette has no package/symbol/document search**,
   despite `Search`/`Names`/`IndexSearch` being fully implemented backend
   commands (`ui/search_palette/mod.rs:117-159`).
4. **P1 — the view layer (where both P0 bugs live) has zero test coverage**;
   every existing test lives one layer below it (reducer/runtime/persistence)
   or one layer above it (client-contract journeys that bypass the views
   entirely) (`tests/journeys/tests/live_registry.rs:701-780`).
5. **P2 — `docs/operations/gui-testing.md` still claims 9 required viewports
   while the code declares 10** (`tools/gui-harness/src/lib.rs:60-72` vs.
   `docs/operations/gui-testing.md:111-115`) — a live, currently-true
   documentation/code mismatch, first flagged 2026-09-22 and still unfixed.
6. **Fixed since the last review, worth noting so it isn't re-flagged**: the
   generic GUI driver's IME no-op (`testing-cutover-research-v10.md` finding #3)
   is resolved — `tools/gui-harness/src/gpui_driver.rs:417-448` now requires and
   validates a real `ImeObservation`, and `apps/desktop/src/harness.rs:881-929`
   supplies a real dispatch through GPUI CE's `EntityInputHandler`.
7. **Positive**: the actor/coordinator/mailbox/mapping runtime layer, the
   settings surface, onboarding, project-shelf, and local-project-dossier views,
   and the live-capture harness (including its onboarding journeys) are all
   genuinely well-built and should be kept as-is. The rewrite's foundation is
   sound; the gap is concentrated almost entirely in two files
   (`views/package.rs`, `views/reader.rs`) plus one shared-state bug.
