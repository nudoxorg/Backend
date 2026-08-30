# Nudox native GUI revamp plan

Status: proposed implementation plan  
Research and repository audit: 2026-08-29  
Scope: `workspace/gui`, the GUI-facing engine/MCP/indexing APIs, and desktop packaging

## 1. Executive decision

Do not patch the current screens one at a time. Replace the GUI through a staged “strangler” migration while keeping the working engine behind typed adapters. The first gate is to put GPUI and GPUI Component on one coherent upstream revision; the present mix of an older GPUI-CE commit, an older vendored GPUI Component, and compatibility patches makes every component fix harder to reason about.

The redesigned app should have five visible product areas:

1. **Home** — setup status, recent projects, and the single best next action.
2. **Libraries** — what is indexed, why it was selected, state/size/last scan, rescan, and removal.
3. **Search** — lexical and semantic search with honest capability status.
4. **Connections** — Claude and other MCP clients, connection health, setup, and diagnostics.
5. **Settings** — appearance, indexing, model storage, privacy, logs, updates, and advanced overrides.

The shell must be stable from its first frame. No screen should flash, shimmer, pulse, dim while refreshing, arrive as a late overlay, or animate indefinitely. Motion is opt-in and functional: at most a 75–100 ms state interpolation for hover, selection, and a user-opened surface. Reduced motion is respected; a “No motion” setting removes even those transitions. An idle Nudox window must request no animation frames and run no UI polling loop.

The redesign is complete only when all twelve reported problems have automated regression coverage on macOS, Windows, X11, and Wayland—not when the new theme looks finished.

## 2. What the audit found

| Reported problem | Current cause in the workspace | Required outcome |
|---|---|---|
| 1. Arrowing through Command Palette does not scroll | `CommandOverlay` owns only `selected: usize`; its ordinary scrolling `div` has no list state or scroll handle. `move_selection` changes the integer and notifies. | A virtualized picker owns selection and scrolling together. Up/down skips disabled/group rows and calls “reveal nearest” after every selection change. Add Home/End/Page Up/Page Down and a visible scrollbar. |
| 2. MCP setup is hidden behind Ctrl/Cmd+, | Settings is described as a `cmd-,` surface and currently contains only Connection. There is no first-run discovery path. | Connections is first-class navigation, a Home setup card, a status-bar target, and a searchable command. Shortcuts are accelerators, never the only route. |
| 3. Linux needs a hand-made executable script; Windows triggers Defender | Linux packaging currently relies on a wrapper for runtime libraries. The WiX template explicitly has no production signing path. | Ship installed launchers and packages. Linux users receive Flatpak plus AppImage/tar fallback with executable bits and desktop metadata. Windows artifacts and installer are signed with one stable publisher identity; Store distribution is evaluated as the strongest SmartScreen route. |
| 4. Windows terminal stays open with MCP logs | The GUI binary uses the default Windows console subsystem and logging writes to stderr. | Mark the release GUI as the Windows GUI subsystem. Write bounded rotating logs to the application data directory; show them in Diagnostics. Keep a separate console-enabled developer binary or flag. |
| 5. Sign-in does not appear promptly | Account and MCP services are installed after the window opens; the shell sees `Absent` and notices later through a 60-second timer. | Create one observable bootstrap entity before opening the window. Render a reserved “checking account” frame immediately, then atomically choose signed-in or sign-in content from service events. No polling and no late overlay. |
| 6. Zoom control seems useless | There is no coherent UI-scale model. The graph “fit” action only increments a revision counter; it does not alter a camera. | Rename global zoom to **Interface size**, show its percentage, and scale text and geometry together. Implement graph camera zoom/fit with a visible percentage and disabled bounds—or remove those controls until real. |
| 7. Command Palette is too small and dim | The palette is constrained to 480 px; default UI text is 13 px and shortcut/caption text is 11 px, with muted foreground use. | Default command label 15–16 px, group/header 13–14 px, shortcut 12–13 px; 4.5:1 text contrast; 2 px/3:1 keyboard focus indicator; width responsive from 560 to 760 px. Interface size applies here too. |
| 8. Claude cannot use Nudox without prior explanation | The GUI exposes connection configuration but not a guided workflow. Server-level instructions do not carry the entire “connect → inspect → index → search” path. | Put workflow hints in MCP initialization instructions and tool descriptions/results. Add one-click Claude configuration and an optional, previewed, idempotent managed block for `CLAUDE.md`. Explain that a repository must be indexed before repository search. |
| 9. Claude indexed `dist/` instead of TypeScript | TypeScript discovery follows `package.json` exports/main and its fallback walks `.js`/`.d.ts`; generated output directories are not excluded in workspace-source mode. | Separate **workspace source** from **published package** indexing. Workspace mode follows `tsconfig` files/include/references/import graph and excludes compiler outputs. Show an Index Preview with every root and reason before commit. |
| 10. Indexed libraries cannot be deleted | There is no complete GUI/library lifecycle or engine unload operation. | Library Manager supports cancel, rescan, remove from index, and separately clear cached downloads/models. Removal cancels work and deletes every corpus/search/vector/manifest record transactionally. Never touch the user’s source tree. |
| 11. `NUDOX_EMBED_MODEL_DIR` is required | Main calls `embed::load_from_env`; MCP explains missing semantics by telling the model/user to set an environment variable. | App-managed Model Manager downloads, verifies, locates, updates, and removes the model. Environment variables remain an advanced override. Lexical search works without a model and reports one concise setup action, not repeated complaints. |
| 12. Moving `dist/` makes TypeScript scanning fail | The no-exports fallback admits `.js` and `.d.ts`, but not `.ts` or `.tsx`; source-repository and npm-package semantics are conflated. | Workspace mode discovers `.ts`, `.tsx`, `.mts`, `.cts`, and declarations from the TypeScript project graph. Published-package mode retains exports/types behavior. Fixtures cover both. |

### Repository seams to change

| Area | Current seam | Migration ownership |
|---|---|---|
| Process/bootstrap | `src/main.rs` | Construct observable services before the first window; split GUI and stdio launcher behavior; initialize bounded file logging. |
| Shell/account gate | `src/workspace/shell.rs` | Replace `Absent` plus 60-second recheck with service events and stable bootstrap geometry. |
| Palette | `src/views/command_overlay.rs` | Replace the hand-built scrolling `div` and integer selection with `ui_kit::CommandPalette`. |
| Settings/discovery | `src/views/settings.rs`, `src/app/keymaps.rs` | Create visible Connections and full Settings routes; keep shortcuts as accelerators. |
| Motion/loading | `src/motion/**`, `src/ui/slot_view.rs`, `src/ui/provenance_dot.rs` | Retire loop permits, shimmer, breathing, row entrances, nav flash, and stale-content dimming. |
| Theme/scale | `src/theme/tokens.rs`, `src/theme/ext.rs` | Replace undersized type tokens with scalable product tokens and audited semantic contrast. |
| Graph controls | `src/views/graph.rs` | Add a real camera transform/fit calculation or remove the controls. |
| Performance | `src/perf/mod.rs` | Expand named render self-time into end-to-end frame, resource, task, startup, and memory instrumentation. |
| TypeScript selection | `workspace/compiler/languages/src/typescript/entry.rs` and producer | Add source/artifact policy, TSConfig project graph, output detection, ignore reasons, and fixtures. |
| Model setup/errors | `workspace/nudox-engine/src/embed/**`, `semantic.rs`, `mcp/semantic_format.rs` | Introduce managed model configuration and structured lexical fallback. |
| Library lifecycle | engine corpus/search/semantic persistence plus new GUI Library Store | Add a journaled, idempotent unload operation before exposing Remove in the GUI. |
| Distribution | `packaging/linux/**`, `packaging/windows/**`, `package.nix` | Produce launchable, signed/verified platform artifacts in CI instead of user workarounds. |

## 3. Upstream research: take the patterns, not the products

The research baseline is pinned so future readers can reproduce it:

| Project | Audited revision | What Nudox should reuse | What Nudox should not copy |
|---|---:|---|---|
| GPUI-CE | `455f2b1225e9be8552fce0f8bbcb742b297a0c33` main | Cross-platform platform layer, current transitions, accessibility integration, test support, and the documented patch route for Zed-GPUI consumers. | A floating branch dependency in release builds. GPUI-CE has no GitHub release objects; the crates.io package observed during the audit was `gpui-ce 0.2.2`, while its README already showed a `0.3` example. Use a tested lock, not a version assumption. |
| GPUI Component | `b4393e22d3a04a880bc187dfdabca9b1eeee270a` main | Current `Command`, `CommandState`, `ListState`, `SearchableListState`, Settings, dialogs, notifications, tooltips, inputs, scrollbars, and unstyled `gpui-base` behavior. Its command state already couples selection to `ScrollStrategy::Nearest`. | Importing the whole gallery/editor/chart/dock stack. Every enabled feature and transitive crate must justify binary and memory cost. |
| Zed mainline | `1662f5f3f6497c5f80830ccdca1edfd1fc0c6c6a` | Behavioral reference: `PickerDelegate`, virtual list or uniform-list scroll handles, selection/scroll coupling, async fuzzy matching, focus restoration, task ownership, and component tests. GPUI itself is Apache-2.0. | Zed `picker` and `ui` source are GPL-3.0-or-later. Reimplement behavior or use Apache GPUI Component; do not copy those crates into Nudox unless the product intentionally adopts GPL. |
| Hummingbird | `0236a8a297d01e4f71388985f49722bd51378590` | Bounded fuzzy results; only materialize palette state while visible; `ListState::scroll_to_reveal_item`; bounded notification channels; virtual tables; cache eviction that explicitly drops GPU images/resources; rotating 1 MiB × 4 file logs. Apache-2.0 permits adaptation with notices. | Its finder’s detached 10 ms polling loop. Nudox’s UI must wake on events and coalesce notifications, not poll when idle. |
| Frame | `eefde7a4b5424f11ef393a53fa72e7dfae3e693f` | Behavioral reference: native-GPUI product shell, persisted interface scale, stable focus handles keyed to controls, modal focus loops, 75/100 ms keyed transitions, premultiplied-alpha color interpolation, native dialogs, signed update manifests, AppImage/Flatpak/WinGet release coverage. | Frame is GPL-3.0-or-later. Do a clean-room implementation from requirements. Also do not copy its unsigned-app posture; Nudox must sign Windows releases. |

### Dependency decision

There are two different meanings of “latest” on the audit date, and the implementation must not blur them:

- **Published line:** crates.io reported `gpui 0.2.2`, `gpui-ce 0.2.2`, and `gpui-component 0.5.1`. This is the reproducible released line. GPUI Component 0.5.1 has Settings/List primitives, but not the newer `command` or `searchable_list` modules.
- **New split/mainline line:** current GPUI-CE/Zed use a family including `gpui`, `gpui_platform`, platform crates, macros, collections, scheduler, and other support crates; GPUI Component main calls itself 0.5.2 and contains the new Command/Searchable List implementation, but that component version was not published during the audit.

Because this revamp explicitly targets the new GPUI crate release, the intended destination is the coherent **new split crate family**, once its exact release artifacts and matching GPUI Component release can be locked. The 0.2.2 published line is the rollback/reference target, not permission to downgrade silently. If release packaging is still incomplete when Wave 1 starts, pin every member of the new family plus GPUI Component to reviewed commits for the spike, but do not ship that as a stable channel until the release/pinning policy is approved.

Create a short-lived upgrade spike and prove one coherent graph:

```text
Nudox UI wrappers
        │
        ├── selected GPUI Component revision
        │       └── its behavioral base/list/command primitives
        │
        └── exactly one GPUI + GPUI macros + platform family
                └── one pinned GPUI-CE revision or one published GPUI family
```

Preferred spike candidate: current GPUI Component with all Zed GPUI dependencies patched to the same pinned current GPUI-CE revision, following GPUI-CE’s supported patch strategy. Preferred release candidate: the matching published versions of that same family. Do not mix a crates.io `gpui`, a git `gpui_platform`, and another macros revision without a lockfile identity check.

The spike passes only when:

- `cargo tree` proves one `gpui`, one macro type universe, and one platform family;
- macOS, Windows, X11, and Wayland compile and open a test window;
- text input, IME, clipboard, window decorations, menus, AccessKit, and GPU recovery smoke tests pass;
- current palette, settings, dialog, notification, virtual list, and scrollbar stories render;
- a 30-minute idle soak has no growing task/entity/image count;
- the compatibility fork is either deleted or reduced to a documented, tested patch with an owner and upstream issue.

Do not begin the visual rewrite before this gate. Land the lockfile and an `UPSTREAM_PINS.md` containing commit IDs, licenses, patch reasons, and the update procedure.

## 4. Product and information architecture

### First run

The first run is a durable checklist, not a modal tour:

1. **Account** — checking, signed out, or signed in. The sign-in button occupies a stable reserved area from frame one.
2. **Search model** — “Lexical search ready” and an optional “Install semantic search (download size)” action.
3. **Connect an AI client** — detected clients, Claude first when present, with Configure, Copy config, Test connection, and Troubleshoot.
4. **Add a library** — choose a local folder or package, preview what will be indexed, then start.
5. **Try a query** — a real search that proves the whole path.

The checklist remains reachable from Help → Setup and automatically collapses on Home when complete. Dismissal is remembered but health regressions resurface as a quiet status card, never a blocking overlay.

### Persistent shell

- Left rail: Home, Libraries, Search, Connections, Settings.
- Main header: current area, project/library scope, contextual primary action.
- Bottom status strip: indexing jobs, MCP health, search mode, app version/update. Every status item is clickable and names its destination.
- Global command palette: Cmd/Ctrl+K (or current binding), plus menu and visible Home entry.
- Settings shortcut: Cmd/Ctrl+, remains supported but is displayed in the Settings menu item and palette.

Avoid a dockable IDE shell for this product. Dock frameworks increase state, interaction surface, persistence complexity, and memory. Use one resizable navigation rail and one content pane; introduce split panes only for evidence-backed workflows such as Index Preview or symbol/detail inspection.

## 5. State architecture: stable first frame, event-driven thereafter

### App bootstrap

Replace late global installation and 60-second polling with one entity installed before `open_window`:

```rust
BootstrapState {
    account: Capability<AccountSession>,
    mcp: Capability<McpRuntime>,
    engine: Capability<EngineHandle>,
    model: Capability<ModelStatus>,
    updates: Capability<UpdateStatus>,
}

Capability<T> = Starting | Ready(T) | Degraded(ActionableError) | Stopped
```

`AppServices` owns the tasks and emits typed events. Views subscribe to only the stores they read. The shell renders the same geometry for `Starting` and the resolved state, preventing a layout flash. Service completion schedules one coalesced notification; no timer checks whether a global appeared.

### Store rules

- One entity per domain: `AccountStore`, `ConnectionStore`, `LibraryStore`, `SearchStore`, `ModelStore`, `PreferencesStore`, `TaskStore`.
- Stores publish small immutable snapshots backed by `Arc`; do not clone result bodies or whole corpora into view state.
- Every async request has a generation ID and cancellation token. Late results are ignored before they allocate view models.
- Events are coalesced to at most one UI notification per store per frame.
- Never hold a store borrow while rendering a child entity or awaiting work.
- Stable identifiers, not row indices, key row state, focus, selection, animations, and async results.
- Render methods format no large strings, perform no disk/network work, and do not rebuild fuzzy-search indexes.
- Background channels are bounded. Progress updates overwrite/coalesce; completion and error events are lossless.

### Loading without flashing

Use one shared `ResourceState<T>` contract:

```text
Idle
Loading { started_at }
Ready { value, refreshing }
Empty
Failed { error, retained_value? }
```

Rules:

- Loading under 250 ms shows no indicator; layout space is already reserved.
- After 250 ms, show one static progress indicator and plain text. No shimmer.
- Refresh preserves old content at full opacity and adds a small static “Refreshing…” label in the header. Do not dim or animate the content.
- Error preserves last good content when safe and puts a single actionable error above it.
- State changes are atomic at a frame boundary; never render an empty intermediate collection.
- No provenance pulse, breathing status dot, skeleton shimmer, nav flash, row cascade, or blinking caret.

Delete or retire the existing infinite-loop permit system after all call sites move to this contract.

## 6. Visual and interaction system

### Typography and density

Use a product-owned type scale expressed in `rem` so Interface size controls text and geometry together:

| Token | 100% size / line height | Use |
|---|---:|---|
| Display | 22 / 30 px | onboarding and empty-state title only |
| Title | 18 / 26 px | page and dialog titles |
| Body | 15 / 22 px | descriptions, settings, command rows |
| UI | 14 / 20 px | buttons, tabs, table cells |
| Small | 13 / 18 px | secondary metadata |
| Code | 13 / 19 px | paths, config, logs |

Do not render essential information at 11 px. Command Palette labels use Body; shortcuts use Small with normal, not “muted until invisible,” contrast.

Interface-size presets: 80%, 90%, 100%, 110%, 125%, 150%, 175%. The control shows the active percentage, supports keyboard commands, persists, and disables decrement/increment at bounds. “Reset to 100%” is explicit. Window content reflows; it is not bitmap zoom.

### Color and focus

- Body text and essential metadata: at least 4.5:1 against its actual background.
- Large text: at least 3:1.
- Controls, selection boundaries, and focus indicator: at least 3:1.
- Keyboard focus: a 2 px minimum perimeter (3 px preferred) that is visible in light, dark, and high-contrast palettes.
- Never encode connected/error/indexing state by color alone; pair icon, label, and optional detail.
- Selected-row background must not reduce label or shortcut contrast.
- Use premultiplied-alpha interpolation if a color transition remains, preventing the “fade through black” artifact.

### Motion policy

| Class | Default | Reduced/no motion |
|---|---|---|
| Hover/pressed/selection color | keyed transition, 75 ms | immediate |
| User-opened popover/sheet | opacity + ≤4 px translation, 100 ms | immediate |
| Route/page change | immediate | immediate |
| Loading/progress | static or determinate bar | same |
| Success/error/status | static icon and text | same |
| Background refresh | no visual motion | same |

Never animate height from unknown content, whole-page opacity, large blur, layout position during async updates, or list row entrances. A transition is keyed by semantic control ID and retargetable; remounting an item must not restart it.

### Command Palette specification

- Responsive width: `min(760 px, viewport - 32 px)`, minimum 560 px on normal desktop windows.
- Stable maximum height with a definite parent height; do not rely on `max_h` alone for list layout.
- Search input 16 px, command label 15–16 px, key hint 12–13 px.
- Virtual list renders only visible rows plus a small overscan.
- `selected_id` is authoritative; projected display index is derived after filtering.
- Up/down calls `scroll_to_reveal_item` or `scroll_to_item(...Nearest)` after selection.
- Home/End, Page Up/Page Down, Enter, Escape, mouse hover, mouse wheel, and scrollbar all update the same state.
- Category headers and separators are not selectable. Disabled commands are announced and skipped.
- Search is accent/case tolerant with stable ranking: exact prefix, word boundary, fuzzy score, recent successful use, then registry order.
- Limit initial results to 100; show category counts and “Show all” rather than allocating every row.
- Preserve query only while the palette is open; persist command-use frequency in a tiny bounded history, not full query text unless privacy policy explicitly allows it.
- Opening warm palette to first paint ≤16.7 ms; key-to-paint p95 ≤16.7 ms with 10,000 registered commands.

## 7. Reusable GPUI patterns and components

Create `src/ui_kit/` as the only normal import surface for GPUI Component. Product views should not directly depend on dozens of upstream component APIs. Thin wrappers give Nudox stable defaults, accessibility, metrics, and an escape hatch during future GPUI updates.

### Adopt or wrap from current GPUI Component

| Nudox component | Upstream foundation | Nudox contract |
|---|---|---|
| `CommandPalette` | new-line `Command`/`CommandState`; `ListState` fallback on the 0.5.1 line | Product ranking, larger type, command registry, usage history, navigation tests. Prefer the released new command once available. Until then, put the same delegate contract over `ListState` rather than binding product code to an unpublished API. |
| `VirtualList` | `ListState`/list | Stable IDs, overscan, reveal selection, custom scrollbar, empty/error rows. |
| `SearchableSelect` | `SearchableListState` | Unified focus, query, selection, confirm/cancel; used for model, project, language, and client selectors. |
| `SettingsPage` / `SettingRow` | Settings groups/pages/fields | Searchable settings, label/help/control alignment, reset-to-default, validation and deep links. |
| `Dialog` / `ConfirmDialog` | Dialog | Focus trap, destructive-action wording, async action state, focus restoration. |
| `ToastCenter` | Notification | Deduplicate by semantic error ID; success auto-dismiss, errors persist; no animated stacks. |
| `ScrollArea` | Scrollable/Scrollbar | Explicit handle, consistent scrollbar policy, keyboard page scrolling. |
| `Button`, `IconButton`, `Input`, `Checkbox`, `Switch`, `Select`, `Tooltip`, `Tabs` | matching components | Nudox sizes, contrast, labels, focus rings, disabled semantics, telemetry-free IDs. |
| `DataTable` | Table only where column behavior is needed | Virtual rows, fixed/resizable columns, sort state; Libraries and Index Preview. Do not use for simple lists. |

Keep feature flags narrow. Do not compile the code editor, charts, web view, markdown/HTML stack, dock, tiles, or JavaScript shell unless a shipped Nudox workflow uses them.

### Build as product components

| Pattern | Purpose and behavior |
|---|---|
| `AppFrame` | Stable title/navigation/content/status layout; owns no domain data. |
| `PageHeader` | Title, optional scope, primary action, compact status. Prevents every page inventing spacing. |
| `CapabilityCard` | `Starting/Ready/Degraded` display with one best action; account, MCP, and semantic model share it. |
| `AsyncActionButton` | Idle/running/success/failure with cancellation and duplicate-submit prevention; width does not change. |
| `ResourcePanel<T>` | Implements the non-flashing resource rules; retains prior content on refresh. |
| `TaskProgressRow` | Determinate progress, current item, elapsed time, cancel/retry; static when indeterminate. |
| `ConnectionCard` | Client detected/configured/connected/last seen; configure, copy, test, logs. |
| `LibraryRow` | Name/path/source mode/languages/size/state/last indexed/actions. |
| `IndexPreview` | Included roots, excludes, reasons, warnings, estimated files/bytes; tree and table views share one model. |
| `ModelManagerPanel` | Search modes, model version/hash/size/path, download/update/remove, advanced override. |
| `DiagnosticsPanel` | Version matrix, GPU/backend, account/MCP status, model/index status, bounded logs, copy support bundle. |
| `StatusItem` | Icon + label + state + destination, fully keyboard accessible. |
| `EmptyState` | One explanation and one primary action; no illustration dependency required. |
| `InlineProblem` | Severity, concise cause, single primary repair and details disclosure. |
| `DangerAction` | Explicit object name and consequence; separates index removal from deleting caches or source. |
| `FocusScope` | Stable keyed focus handles, modal wrapping, focus restoration, stale-handle cleanup. |
| `InterfaceScale` | Persisted `rem` multiplier and presets; affects typography, hit targets, spacing, and icons coherently. |
| `ByteBudgetLru<K,V>` | Cache bounded by decoded bytes, not item count; eviction explicitly releases GPUI image/assets. |

### Cross-cutting implementation patterns

1. **Delegate-based picker** — data/ranking/render/confirm live in a delegate; the picker owns input, focus, list virtualization, selection, scroll, and dismissal.
2. **Visible-only entities** — keep durable data as `Arc` values and create GPUI row entities only for visible complex rows. Clear entity caches when a surface/mode closes.
3. **Byte-bounded resources** — track decoded image and text sizes; evict least recently used resources and call GPUI’s explicit release APIs.
4. **Task ownership** — the store/entity that starts work owns the task handle and cancels on replacement or release. Detached loops are forbidden unless process-lifetime and documented.
5. **Frame coalescing** — high-rate scanner events update a background accumulator; UI receives at most one snapshot per frame.
6. **Stable focus registry** — controls request handles by semantic ID each frame; stale entries are removed, and removed focused controls restore to a known fallback.
7. **Draft/commit fields** — validate and normalize paths/URLs/numbers on submit or focus loss, not on each partial keystroke.
8. **Actionable errors** — typed error code + user sentence + repair action + details/log correlation ID. No raw environment-variable sermon in ordinary MCP results.
9. **Platform seam** — shell calls `PlatformIntegration` for open path/URL, reveal file, autostart, notifications, key labels, app menu, and client detection. Platform `cfg` code stays out of views.
10. **Golden state fixtures** — each page renders deterministic starting/ready/empty/error/large-data/reduced-motion/high-contrast fixtures on all platforms.

## 8. Indexing redesign

### Make source intent explicit

Persist an `IndexSpec` rather than inferring everything again on every scan:

```rust
IndexSpec {
    id,
    root,
    mode: WorkspaceSource | PublishedArtifact,
    languages,
    include,
    exclude,
    detected_manifests,
    entry_points,
    output_directories,
    ignore_sources,
    created_at,
}
```

**Workspace Source** is the default for folders users select from a checkout:

- Read `tsconfig.json` and extended configs.
- Respect `files`, `include`, project `references`, `rootDir`, `allowJs`, and the module/import graph.
- Treat `outDir` and `declarationDir` as generated outputs.
- Default candidates: `.ts`, `.tsx`, `.mts`, `.cts`, and `.d.*`; include `.js/.jsx` only when `allowJs` or explicit user choice permits it.
- Exclude `.git`, `.hg`, `.svn`, `node_modules`, `dist`, `build`, `out`, `lib` when generated, `coverage`, `.next`, `.nuxt`, `.svelte-kit`, `.turbo`, caches, minified bundles, and source-map outputs.
- Apply user rules (`.nudoxignore` and settings) first, then project/VCS ignores, then ecosystem defaults. Display which rule excluded each path.
- If `package.json` exports point into an excluded output directory, warn “Published output points to dist; source mode is indexing the TypeScript project instead.”

**Published Artifact** is explicit for npm packages or extracted distributions:

- Treat `exports`, `types`, `typings`, `module`, and `main` as public-surface signals.
- Prefer declaration files for the API surface, follow conditional exports, and allow classic deep imports only in this mode.
- Do not pretend artifact mode is a source-code index; label it in Libraries and MCP metadata.

### Index Preview

Before the first scan—and on rule changes—show:

- detected mode and why;
- manifests/configs used;
- entry points with reason (`tsconfig files`, import graph, package export, manual include);
- generated output directories;
- top included/excluded folders with file/byte counts;
- languages and unsupported-file warnings;
- “Compiled output only” and “No TypeScript source found” errors with mode/rule repair actions.

The preview model is also exposed through MCP so Claude can ask what will be indexed before starting. Scan results return a concise summary and `next_action`, such as “Index complete; use `search_symbols`” or “No source selected; open Index Preview.”

### Library removal lifecycle

Add engine operations, not just a GUI delete button:

```text
Active → Removing → Removed
          └──────→ Failed (retryable, with exact retained parts)
```

`remove_library(id)` must:

1. cancel discovery, compile, embed, and persistence work for that library generation;
2. prevent new searches from observing the generation;
3. delete lexical records, semantic vectors, corpus/package registry entries, stored IR/manifests, and library-scoped caches in one recoverable transaction or journaled sequence;
4. publish one atomic catalog snapshot;
5. leave the source folder untouched;
6. be idempotent after restart.

Offer **Remove from Nudox** as the normal action. Put **Delete downloaded package cache** and **Delete semantic model** in separate storage controls. The confirmation names the library and states “Your source files will not be deleted.”

## 9. Semantic model redesign

`NUDOX_EMBED_MODEL_DIR` becomes an advanced override, not setup.

`ModelStore` owns:

- model catalog version and compatibility with the engine;
- download URL from a signed manifest, expected compressed/unpacked bytes, SHA-256, and license notice;
- resumable download, bounded progress events, cancellation, verification, atomic install, rollback, update, and removal;
- app-managed platform path;
- effective source: managed model or environment override;
- lazy runtime/session creation and explicit release;
- health/self-test and last error.

Default UX:

- App starts in lexical-only mode with no error.
- Search clearly says “Lexical” or “Lexical + semantic.”
- Model install is optional and shows size before download.
- Semantic queries when unavailable degrade to lexical when that preserves intent, returning one structured capability note and a setup deep link.
- Do not repeat `NUDOX_EMBED_MODEL_DIR` in every Claude result. Advanced diagnostics can show it once.

Memory rules:

- Never instantiate the ONNX session during app bootstrap.
- Load on first semantic use or immediately after the user enables “keep semantic search ready.”
- Use one shared session; never one per window/client/query.
- Bound tokenizer/input/output buffers and reuse them.
- Keep model file data memory-mapped where the runtime supports it; measure actual resident pages.
- Allow “Release semantic model when idle” with a conservative timeout, but do not poll; schedule one cancellable deadline after last use.

## 10. Claude and MCP integration

### Three layers of guidance

1. **Protocol-native** — MCP `InitializeResult.instructions` explains the shortest workflow and capability relationships. Tool descriptions state prerequisites. Results contain typed `status`, `next_action`, and deep links. This is the source of truth for every client.
2. **Client configuration** — Connections detects supported Claude installations/config scopes, generates the correct config, previews the exact edit, makes an atomic backup, applies idempotently, and tests initialize/tools/list plus one harmless status call.
3. **Project guidance** — optional “Add Claude project instructions” previews an idempotent managed block in `CLAUDE.md`. It says to check Nudox status, preview/index the repository when absent, avoid generated output in workspace mode, and use search before broad reads. Never overwrite user prose or silently edit the file.

The managed block has explicit markers and a version so update/remove is safe:

```text
<!-- nudox:start version=1 -->
…generated guidance…
<!-- nudox:end -->
```

### Connections page

Each client card shows:

- Detected / Not detected
- Not configured / Configured / Connected / Error
- Configuration scope and file location
- Last handshake and protocol version
- Configure, Test, View instructions, Open config folder, Disconnect
- Concise error plus “View diagnostics”

The Home card and status strip open this page. Setup never depends on knowing Ctrl/Cmd+, and the command palette includes “Connect Claude to Nudox.”

## 11. Platform distribution and diagnostics

### Windows

- Set `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]` on the release GUI binary. Keep diagnostics in files and the app, not a permanent terminal.
- Keep MCP stdio behavior in a distinct server/launcher binary if Claude must spawn it. That process may write protocol to stdout and logs only to stderr; it must not create a visible console window when launched normally.
- Sign executable, helper, installer, and updater payloads before packaging; timestamp signatures and verify them in CI after download.
- Use one stable publisher identity. Microsoft currently recommends Artifact Signing for non-Store distribution; also evaluate Store delivery because it is the most reliable way to eliminate SmartScreen download warnings.
- Replace the template-only unsigned WiX flow with signed MSI/MSIX and a WinGet manifest. Test clean Windows 10/11 VMs, standard-user install, upgrade, uninstall, SmartScreen presentation, paths with spaces/non-ASCII, and Defender scan.
- Rotating logs: default 1 MiB × 4 files, redaction applied before write. Diagnostics can reveal the folder and create a user-reviewed support bundle.

### Linux

- Primary: Flatpak/Flathub with desktop file, icons, metainfo, portals, Wayland/X11 permissions, and an executable `/app/bin` command.
- Secondary: AppImage for direct downloads; optional signed tarball for advanced users. Preserve executable mode in every archive.
- The development GPU-library wrapper stays development-only. Production packages either use the Flatpak runtime or bundle/resolve required libraries without user-authored scripts.
- Test Ubuntu/Fedora current LTS/stable, X11 and Wayland, NVIDIA/AMD/Intel, launch from app menu with no terminal, MIME/URL opening if supported, and uninstall cleanup boundaries.

### macOS

- Preserve native `.app`/DMG packaging, code signing, hardened runtime, notarization, and stapling.
- Logs/config/model/cache live in their correct application-support/cache locations.
- Test first launch, update, keychain account bootstrap, Cmd key labels, reduced motion, and Apple Silicon/Intel artifact behavior if both are supported.

### Diagnostics

Diagnostics shows, in plain language:

- app version/build/commit;
- GPUI/GPUI Component pin and renderer/backend;
- account, engine, MCP endpoint/clients, model, and index health;
- recent jobs and last failures;
- bounded redacted logs with copy/open-folder/export actions;
- “Run self-check,” producing pass/fail items without changing user data.

## 12. Performance and memory contract

Wave 0 records baselines on representative macOS, Windows, X11, and Wayland machines. The values below are product gates, adjusted only through an explicit design review with measured evidence.

| Metric | Gate |
|---|---:|
| First stable window, warm configuration | p50 ≤250 ms, p95 ≤500 ms |
| Command Palette open, warm | p95 ≤16.7 ms |
| Keyboard selection to presented frame | p95 ≤16.7 ms, p99 ≤33.3 ms |
| Normal frame build + layout + paint | p95 ≤8.3 ms at 120 Hz target; no interaction frame >50 ms |
| Idle UI CPU after settling | ≤0.3% of one core, zero continuous animation frames |
| Idle wakeups | no GUI polling loops; only OS/service events and explicit deadlines |
| UI-only working-set delta over engine baseline | target ≤45 MiB after warm navigation |
| Total app RSS, no semantic model, empty catalog | target ≤150 MiB after 60 s idle |
| 30-minute idle soak growth | ≤2 MiB after allocator settling; zero leaked GPUI entities/tasks/images |
| 100,000-row catalog | memory O(model + visible rows); ≤2× viewport row entities |
| Progress event rate into UI | ≤1 notification/store/frame |
| On-disk release GUI | current observed macOS release binary is ~97 MiB; initial target ≤75 MiB before symbols, then ratchet |

The 45/150/75 MiB targets are initial budgets, not claims about current performance. Wave 0 records allocator and platform variance and assigns sub-budgets.

### Instrumentation required

- Frame spans: event dispatch, entity update, root render, layout, paint preparation, GPU submit/present.
- Long-frame recorder with the last 120 actions/store notifications, redacted.
- Counters: live entities, subscriptions, tasks, timers, animation-frame requests, row views, images, decoded image bytes, search result bytes, channels/backlog.
- RSS/private bytes and allocator allocated/resident samples at stable checkpoints.
- Startup markers from process entry through services scheduled, window opened, first frame, account resolved, and interactive.
- A debug performance HUD and machine-readable JSON benchmark output; neither ships enabled by default.

### Optimization order

1. Remove idle work and infinite animation.
2. Virtualize lists/tables and bound result sets.
3. Stop cloning strings/snapshots in render paths.
4. Coalesce progress and store notifications.
5. Bound and explicitly release caches/resources.
6. Lazy-load semantic/runtime-heavy capabilities.
7. Prune unused component features/dependencies and tune release LTO/codegen/strip only after behavior is measured.

Do not use memoization as a blanket fix. Profile which entity rerenders, reduce its observed state, and give it stable inputs first.

## 13. Delivery sequence

### Wave 0 — freeze regressions and measure

- Write executable tests for all twelve reports before changing behavior.
- Add deterministic visual fixtures for every major state and capture platform baselines.
- Record startup, idle, palette, large-library, search-stream, and model-load performance/memory.
- Inventory every animation, timer, detached task, unbounded channel/cache/list, and direct upstream component import.
- Add feature flag `gui_revamp` and keep engine APIs behind adapters.

Exit: reproducible baselines, regression tests failing for the known defects, and no unexplained GUI task.

### Wave 1 — dependency unification and UI kit

- Complete the GPUI/GPUI Component upgrade spike and land one pin set.
- Add `ui_kit` wrappers, tokens, Interface size, focus registry, ResourcePanel, virtual list, dialogs, notifications, and test harness.
- Replace Command Palette first; it validates focus, input, virtualization, scrolling, font, and theme fundamentals.
- Remove unused upstream features and compare binary/RSS to Wave 0.

Exit: palette requirements pass on four backends; no duplicate GPUI family; no dependency-related memory regression.

### Wave 2 — stable shell and discovery

- Introduce `AppServices` and domain stores before the window opens.
- Replace shell navigation, Home, status strip, Connections, Settings, and first-run checklist.
- Remove the account 60-second polling path and late globals.
- Implement file logs, Diagnostics, and Windows GUI subsystem split.
- Retire shimmer/pulse/nav-flash call sites in migrated surfaces.

Exit: sign-in or stable checking state appears on first frame; MCP setup is discoverable without a shortcut; idle has zero animation frames/poll loops.

### Wave 3 — indexing, libraries, model

- Add `IndexSpec`, source/artifact modes, TypeScript project discovery, output exclusion, and Index Preview.
- Implement transactional library removal and Library Manager.
- Implement ModelStore/Model Manager and lexical fallback; remove environment setup from normal UX.
- Update MCP instructions/tools/results and optional Claude managed block.

Exit: TypeScript source/dist fixtures, delete/restart/idempotency, model install/failure/remove, and Claude first-use journeys pass.

### Wave 4 — remaining views and graph semantics

- Move Search, symbol/detail, graph, tabs/history, errors, and empty states onto shared patterns.
- Implement real graph camera fit/zoom or remove the controls.
- Delete legacy motion/slot/component code after its last caller moves.
- Run keyboard-only, screen-reader, high-contrast, 80–175% scale, localization expansion, and reduced-motion passes.

Exit: no direct legacy UI paths, no fake/no-op action, every control has role/name/state/focus behavior.

### Wave 5 — production distribution and hard gates

- Build signed/notarized macOS, signed Windows, and Flatpak/AppImage Linux release candidates.
- Clean-VM install/connect/index/search/update/uninstall journeys.
- 30-minute idle and 2-hour indexing/search stress runs with memory and long-frame reports.
- Remove the feature flag only when all gates pass; keep rollback possible for one release.

Exit: release artifacts satisfy packaging/signing tests and the performance table; every reported issue has a green end-to-end test.

## 14. Acceptance matrix for the twelve reports

1. Open palette with 200 commands, press Down beyond viewport: selection remains visible; mouse and keys share state.
2. Fresh install: a visible “Connect Claude” route is present on Home and Connections; no shortcut knowledge needed.
3. Fresh Linux VM: install and launch from app menu with no created wrapper. Fresh Windows VM: signed publisher is shown and CI records SmartScreen/Defender behavior.
4. Launch Windows GUI: no console window; Diagnostics receives MCP logs; stdio server protocol remains valid.
5. Add 0/100/2,000 ms account probe delays: first frame has stable geometry and resolves immediately on event, never at 60 seconds.
6. Interface size changes measured text and control bounds and shows percentage. Graph fit changes camera bounds; if absent, no fit/zoom control appears.
7. Palette automated contrast checks pass; 100/125/175% screenshots and a low-vision review require no leaning/zoom workaround.
8. Clean Claude project: configure, test, ask a natural Nudox question. Claude gets status/index guidance from MCP; optional `CLAUDE.md` block is previewed, repeatable, updateable, removable.
9. Repo with `src/index.ts` and `dist/index.js`: workspace preview/index includes source and excludes dist with reasons.
10. Remove a library during embedding, restart, and search: no results or orphan vectors remain; source files remain byte-identical.
11. Launch with no environment variable: no startup error, lexical search works, one UI action installs model, semantic self-test passes.
12. Remove `dist` from the TypeScript fixture: workspace indexing still selects `.ts/.tsx`; published-artifact mode reports missing artifact clearly.

## 15. Explicit non-goals and guardrails

- No “design system rewrite” that postpones fixing first-run, indexing, removal, or packaging.
- No dockable IDE framework without a proven user workflow.
- No continuous decorative animation, animated skeleton, or polling-based UI freshness.
- No raw engine error/environment variable as primary user guidance.
- No silent edit of `CLAUDE.md` or client configuration; preview, backup, idempotency, and removal are mandatory.
- No deletion of user source files from Library Manager.
- No floating production dependency on a main branch.
- No copying GPL Frame/Zed UI source into a non-GPL product. GPUI’s Apache crate and Apache GPUI Component/Hummingbird code have different boundaries; preserve notices and run license checks.
- No performance claim based on FPS alone. Frame latency, idle work, memory, entity/task/resource counts, startup, and package size are separate gates.

## 16. Primary sources

- [GPUI-CE repository and supported dependency patch strategy](https://github.com/gpui-ce/gpui-ce)
- [GPUI-CE current workspace crate set](https://github.com/gpui-ce/gpui-ce/blob/main/Cargo.toml)
- [Published `gpui-ce` crate](https://crates.io/crates/gpui-ce)
- [Published Zed `gpui` crate](https://crates.io/crates/gpui)
- [GPUI Component repository and component architecture](https://github.com/longbridge/gpui-component)
- [GPUI Component releases](https://github.com/longbridge/gpui-component/releases)
- [Published GPUI Component crate](https://crates.io/crates/gpui-component)
- [GPUI Component current command implementation](https://github.com/longbridge/gpui-component/tree/main/crates/ui/src/command)
- [Zed picker implementation](https://github.com/zed-industries/zed/blob/main/crates/picker/src/picker.rs)
- [Zed GPUI examples](https://github.com/zed-industries/zed/blob/main/crates/gpui/examples/README.md)
- [Hummingbird](https://github.com/hummingbird-player/hummingbird)
- [Frame source and release packaging](https://github.com/66HEX/frame)
- [Frame releases](https://github.com/66HEX/frame/releases)
- [MCP initialization schema, including server instructions](https://modelcontextprotocol.io/specification/2025-11-25/schema)
- [MCP lifecycle and capability negotiation](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)
- [Anthropic MCP documentation](https://docs.anthropic.com/en/docs/mcp)
- [TypeScript TSConfig reference](https://www.typescriptlang.org/tsconfig/)
- [Rust `windows_subsystem` reference](https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute)
- [Microsoft SmartScreen reputation guidance](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)
- [Flatpak build documentation](https://docs.flatpak.org/en/latest/building.html)
- [WCAG 2.2 focus appearance guidance](https://www.w3.org/WAI/WCAG22/Understanding/focus-appearance.html)
