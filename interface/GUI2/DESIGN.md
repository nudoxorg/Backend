# Nudox GUI2 — design and build contract

This document is the source of truth for the `GUI2/` rewrite, for the engine work beneath it, and
for the CLI/MCP parity work beside it. Every builder reads it first and every checkpoint is judged
against it. It is deliberately opinionated: where it names a rule, the rule is the deliverable.

```
GUI2/            the desktop surface (package `nudox-gui2`, binary `nudox-gui`, bundle `interface-gui.app`)
interface/…      the shared contract every surface projects (identity · documents · search · library)
server/index/…   the index server: catalog, acquisition, registry adapters, retrieval lanes
```

## 0. What Nudox is

Two modalities, one library, three surfaces.

1. **Explore** — a native alternative to crates.io / npmjs / pypi.org across seven ecosystems
   (`cargo npm pypi go maven nuget cpp`). Cards of packages, a detail page with readme, versions,
   dependencies, dependents, downloads, owners; a one-click add that compiles the package into the
   local library; a subscribe button that follows releases.
2. **Read** — a documentation browser over the compiled semantic image of a package: the crate root
   with its prose and item tables, item pages with hyperlinked signatures, members, relations, and
   source, a hover card of related symbols, and a tree of everything you have opened.

The GUI, the CLI (`nudox`), and the MCP server (`nudox-mcp`) are three renderings of one closed
command registry (`interface/library/command.rs`). They share one durable library under the data
root and observe each other through the epoch file. A page an agent opens over MCP appears in the
GUI's tree, marked as the agent's.

## 1. Information architecture

```
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ ⌘K  ⌕ search packages, docs, commands…            [Home] [Explore] [Read]  ⚙  ◐       │  titlebar / omnibar
├──────────┬───────────────────────────────────────────────────────────────────────────┤
│ TREE     │ MAIN                                                                        │
│ (tabs)   │                                                                             │
│ ▾ tokio  │  Home:     project folders (iOS-style tiles) · releases feed · shelf health  │
│   ▾ sync │  Explore:  ecosystem chips · sort · gallery of squircle cards · detail page   │
│     Mutex│  Read:     crate root (README + item tables) · item page · source modal       │
│   task   │                                                                             │
│ serde    │                                                                             │
│ …        │                                                                             │
├──────────┴───────────────────────────────────────────────────────────────────────────┤
│ status: compiler ✓ · lexical ✓ · graph ✓ · vector ✗ unconfigured · registry ✓ · epoch 41 │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

* **Home** (the "final view", the default on launch once anything is subscribed): your
  subscriptions grouped into **projects** — folders in the iOS sense, a tile per project showing up
  to nine package marks, a badge with unseen releases. A project may be **bound to a lockfile**
  (`Cargo.lock`, `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `uv.lock`, `poetry.lock`,
  `requirements.txt`, `go.mod`/`go.sum`, `pom.xml`, `packages.lock.json`, `conan.lock`,
  `vcpkg.json`); **sync** reconciles the folder with the lockfile so "the docs you read are the
  versions you build". Below the folders: the **releases** feed (new versions since last seen) and
  the shelf (every compiled package with its compile state).
* **Explore**: ecosystem chips (all / one), sort (downloads · recently updated · relevance · name),
  a query field, and the gallery. A card shows mark, name, latest stable, one-line description,
  downloads, updated-ago, license, and an "on shelf" tick. Clicking a card opens the **detail page**:
  a big **Add** button in the top bar that follows the selected version (default: latest stable),
  a **Subscribe** button, a version picker, the readme, tabs for Dependencies / Dependents /
  Versions / Owners, an install snippet with one-click copy for the reader's ecosystem. Owners are
  links to an owner page listing every package they own across ecosystems. Dependencies and
  dependents are cards you can click through.
* **Read**: opening a package (from Home, Explore's Add, the tree, or the palette) lands on the
  **root page**: the crate's own prose, then sections in rustdoc order — Re-exports, Modules,
  Structs, Enums, Traits, Functions, Macros, Type aliases, Constants, Statics — each a two-column
  list of name + first sentence. Clicking an item opens its **item page**: kind + name + "source"
  button, the hyperlinked signature, prose, then Fields / Variants / Methods / Implementations /
  Trait implementations / relations groups, then source location. Every type token, member, prose
  link and relation row is a hyperlink. Hovering any symbol shows a **related card** (graph
  neighbours, siblings, semantic neighbours when the vector lane is configured); no card when
  nothing is related. The **source** button opens a modal code viewer with the declaration's span
  highlighted and syntax coloured.
* **Tree** (left): the tab system, modelled on browser.horse — every opened subject nests under
  the subject it was opened from. Collapsible branches, close node / close branch, keyboard
  navigation, drag to re-parent. Nodes opened by the CLI or MCP are shown with the opener's mark
  and a highlight ring until the reader visits them.
* **Palette** (⌘K): one field, three row families ranked together: commands (the registry),
  packages (shelf + registry search-as-you-type), and documentation symbols (the four-lane search).
  Prefixes narrow: `>` commands, `#` packages, `@pkg` scope docs to a package. Enter runs or opens;
  ⌘Enter opens in a new tree branch under the current node.
* **Settings**: appearance (Espresso dark / Cream light / system), interface size, motion, fonts,
  registry endpoints and cache TTL, vector lane (Qdrant URL + model), data root, MCP client
  snippet, keyboard map, "reset tree".
* **Add flow** (kept from the current GUI because the owner loves it): a collapsed `+ add package`
  button that expands into a compact box of ecosystem chips and one coordinate field
  (`cargo:name@version`); typing shows **autocomplete from the registry** (names, then versions
  once `@` is typed); Enter compiles; progress is the eight-dot phase journey drawn in the row
  itself.

## 2. Visual language

**Palette**: hues of brown with accents of teal, forest, green, and red. Dark is the default.

```
                      Espresso (dark)      Cream (light)
ground/app            #17110e              #f7f0e6
ground/panel          #201813              #efe6d8
ground/element        #2a2019              #e6dac9
ground/hover          #34291f              #dccdb8
ground/active         #3f3227              #d1bfa7
border (2px)          #4a3a2e              #c7b39c
border/strong (3px)   #6a553f              #9c8266
text                  #f1e6d6              #2a1f18
text/low              #b8a48e              #6c5b4c
text/inert            #7d6c5b              #a08d79
caramel (identity)    #d9a066              #a8642a     the one accent for identities you copy
teal (struct/type)    #4fc1b1              #1f8f80
forest (trait/impl)   #3e8a63              #2d6b4b
green (enum/ok)       #9ccc65              #5f9a2a
red (macro/danger)    #e26d5a              #c2412e
olive (const/static)  #b9b45a              #7f7b1e
sand (module)         #e0c9a6              #7c6a52
```

Kind hue table (fixed, used by glyphs, tree marks, section headers, signature tokens):
`mod → sand · struct/record → teal · enum → green · trait → forest · impl → forest (dim) ·
fn → caramel · macro → red · const/static → olive · type alias → teal (light) · field → text/low ·
variant → green (dim) · param → inert · namespace → sand`.

Ecosystem hue table (marks on cards, chips, tree): `cargo → caramel · npm → red · pypi → teal ·
go → teal (light) · maven → forest · nuget → olive · cpp → sand`.

**Shapes**: squircles everywhere. A squircle is a superellipse (n ≈ 4) drawn as a path, not a
CSS radius; the `ui::squircle` primitive paints the fill, the 2 px (3 px when focused/selected)
border, and an optional gradient. Radii: card 20 px, control 12 px, chip pill, modal 24 px, tree
row 10 px. Thick borders are the identity of the interface: cards and controls are outlined, never
floating on shadow alone. Elevation is expressed with a border-strong outline plus a soft warm
shadow.

**Gradients and patterns**: the app ground carries a very subtle radial warm gradient (espresso →
slightly lighter top-left). The gallery ground carries a faint dot grid; the home folders carry a
faint diagonal hatch inside the tile. Accents are used as 8–12% tints behind section headers and as
1 px gradient hairlines under the omnibar. Never more than one chromatic accent per component.

**Type**: bundled OFL fonts under `GUI2/assets/fonts/` — *Instrument Serif* for display headings
(crate names, page titles), *Instrument Sans* for UI and body, *JetBrains Mono* for signatures,
code, addresses. Type roles: `Display 32/36 serif · Title 22/28 sans 600 · Section 13/16 sans 700
uppercase tracking 0.08em · Body 15/23 sans · Ui 14/20 sans 500 · Dense 12/16 sans · Mono 13/20`.
The interface scales as one piece through `Window::set_rem_size` (Compact 14 / Regular 16 /
Large 18 / Larger 20).

**Bold use of space**: the reader measure is 76ch; cards are 300–340 px wide in a responsive grid;
the tree is 280 px by default and resizable; the omnibar is 56 px tall. Nothing is smaller than
12 px text. Empty states teach in one dense line, never a paragraph.

**Motion**: springs for panel widths and reveals (reuse the current `motion/` integrator: an idle
window requests no frames); 120 ms ease for hover; no fades longer than 200 ms; the source modal
scales from 0.98 → 1.0.

**Faults are content**: every typed failure is drawn in place — beside the row, in the card, under
the field — as the `Fault { slug, operand, detail, affordance }` the library returns. No toast, no
modal alert, no "something went wrong".

## 3. Crate layout (flat, no `src/`, no `mod.rs` — repository lint)

```
GUI2/
  Cargo.toml          package nudox-gui2 · lib nudox_gui2 · bin nudox-gui · bundle name "interface-gui",
                      identifier "nudox-gui.interface-gui" (keeps the granted automation identity)
  DESIGN.md           this document
  main.rs             process entry: run() → ExitCode, nothing else
  lib.rs              crate root and module map
  assets.rs           AssetSource: bundled fonts + icons (include_dir-style, no runtime paths)
  assets/fonts/…      Instrument Serif · Instrument Sans · JetBrains Mono (OFL, licences beside)
  assets/icons/…      SVG icons (kind glyphs, ecosystem marks, chrome)
  theme.rs            Nudox theme: palette, type roles, radii, kind/ecosystem hues → gpui_component Theme
  theme/palette.rs    the two appearances as typed colour ramps
  theme/tokens.rs     Role · Space · Radius · Weight · Status newtypes (rems, never raw px in views)
  theme/kinds.rs      EntityKind → hue/glyph/word; PackageEcosystem → hue/glyph/label
  theme/fonts.rs      font family names + registration
  ui.rs               element library over gpui_component: everything views compose
  ui/squircle.rs      the superellipse primitive (path-based; fill, border, gradient, shadow)
  ui/card.rs          Card, CardGrid
  ui/chip.rs          Chip, ChipRow (ecosystem chips, kind chips, lane chips)
  ui/glyph.rs         KindGlyph, EcosystemMark, OpenerMark
  ui/section.rs       SectionHeader (uppercase tracked, tinted), ItemTable (name + summary rows)
  ui/signature.rs     hyperlinked signature renderer (InteractiveText ranges → Target)
  ui/prose.rs         documentation prose renderer (paragraphs, inline code, links, fenced code)
  ui/code.rs          syntax-highlighted code block (tree-sitter via gpui_component highlighter)
  ui/fault.rs         Fault block with affordance button
  ui/skeleton.rs      reserved geometry while loading (never a spinner in the reader)
  ui/kbd.rs           key caps
  ui/field.rs         the one text field (omnibar, add flow, settings) with autocomplete popover
  ui/toolbar.rs       top bar builder
  engine.rs           resident engine bridge: request/reply channels, epoch watcher, errands
  engine/errand.rs    Errand (which store owes which reply) — mirrors Reply variants exactly
  engine/pump.rs      the two threads and the fold into stores
  store.rs            the state DAG; every store is headless and tested without a window
  store/shelf.rs      shelf rows, active compile, add draft + autocomplete state
  store/registry.rs   explore pages, details, owners, dependents (keyed by request)
  store/docs.rs       pages, outlines, hover cards, root pages (keyed by address)
  store/tree.rs       the session tree mirror + local expand/collapse + selection
  store/search.rs     palette query, mode, ranked rows, debounce generation
  store/home.rs       subscriptions, releases, projects
  store/settings.rs   preferences in force + faults
  prefs.rs            the typed line codec (`gui.prefs`) — same rules as the current crate
  shell.rs            the window: root layout, panes, keymap, actions, focus
  shell/actions.rs    actions! + KeyBindings (one place)
  shell/layout.rs     titlebar · tree · main · status bar composition
  shell/status.rs     status bar
  views.rs            the views (pure projections of stores)
  views/home.rs       folders, releases, shelf
  views/explore.rs    chips, sort, gallery
  views/detail.rs     package detail page (readme, versions, deps, dependents, owners, install)
  views/owner.rs      owner page
  views/reader.rs     root page + item page
  views/hover.rs      related card
  views/source.rs     source modal
  views/tree.rs       tree panel
  views/palette.rs    command palette
  views/settings.rs   settings page
  views/add.rs        the add flow
  preview.rs          typed fixtures (feature `preview`) rich enough to screenshot every view offline
  tests/store.rs      headless store laws
  tests/gpui.rs       #[gpui::test] laws (feature preview)
```

Dependencies: `gpui = { package = "gpui-ce", version = "=0.2.2" }`, `gpui_platform = { package =
"gpui_ce_platform", version = "=0.1.0" }`, `gpui_component = { package = "gpui_ce_components", git
= "https://github.com/gpui-ce/gpui-component", rev = <pinned> , features = [tree-sitter-rust,
-python, -typescript, -tsx, -go, -java, -csharp, -c, -cpp, -toml, -json, -markdown, -yaml] }`,
`gpui_component_assets`, the `interface-*` crates, `async-channel`. The crate depends on
`interface-library` and nothing below it (dependency direction is law).

## 4. State model

One `Workspace` entity owns the stores; render is a pure projection of them. Every keystroke,
click, and hover becomes exactly one `Command` dispatched through the engine bridge; every
`Reply` is folded into exactly one store by its `Errand`. Stores never touch the engine, never
touch GPUI, and are tested headless. Slots are typed: `Idle | Loading { previous } | Ready |
Failed { fault }` — a loading slot keeps the previous value on screen so nothing flashes.

Cross-process: the epoch watcher re-reads the shelf, the tree, subscriptions and projects when any
other process bumps the epoch. The GUI never polls the registry on its own; registry replies carry
`Provenance { origin: Live | Cached, fetched_at, stale }` and the view says which.

## 5. The contract (registry rows)

Twelve rows exist. Nineteen rows are added by this rewrite. Names are the CLI subcommand, the MCP
tool, and the palette label; descriptions are shared verbatim.

| name | purpose | reply |
|---|---|---|
| explore | browse or search registry packages across ecosystems, paged, sorted | ExplorePage |
| package | one registry package's full profile: readme, versions, owners, deps, dependents, downloads, install | PackageDetail |
| dependents | a page of packages depending on one package | DependentsPage |
| owner | every package one owner publishes, across ecosystems | OwnerPage |
| subscribe | follow one package for new releases, optionally into a project | FollowOutcome |
| unsubscribe | stop following | FollowOutcome |
| subscriptions | every followed package with unseen release counts | Subscriptions |
| releases | new versions since last seen, optionally marking them seen | Releases |
| projects | every project folder and its members | Projects |
| project-create | create a folder, optionally bound to a lockfile | Project |
| project-delete | delete a folder (subscriptions survive) | ProjectId |
| project-add | put one pinned package into a folder | Project |
| project-remove | take one package out of a folder | Project |
| project-sync | reconcile a bound folder with its lockfile, optionally compiling new members | SyncReport |
| tree | the shared session tree of opened subjects across surfaces | SessionTree |
| tree-open | open a subject under a parent, recording who opened it | TreeOutcome |
| tree-close | close one node or its whole branch | TreeOutcome |
| source | the source text of one declaration with context lines | SourceText |
| related | symbols related to one declaration: graph, siblings, semantic | Related |

Types live in `interface/library/{registry,follow,project,session,source}.rs`. They are the
contract; implementations live in `*_service.rs` modules owned by the engine builders.

## 6. Storage beneath the data root

```
<root>/library/
  epoch            unchanged: decimal counter, renamed into place after every mutation
  shelf.db         SQLite (turso): rows, status, card facts, source root per package
  registry.db      SQLite (turso): registry cache with fetched_at per record family
  session.tree     line codec: `node <id> <parent|-> <opener> <opened_at> <focused_at> <collapsed> <subject> <title>`
  subscriptions    line codec: `follow <ecosystem> <name> <seen|-> <followed_at> <project|->`
  projects         line codec: `project <id> <name> <hue> <created_at> <synced_at|-> <lockfile-kind|-> <path|->` + `member <id> <coordinate>`
  sources/<ecosystem>/<name>@<version>/   materialised package sources the compiler read (what `source` serves)
  tantivy/         unchanged
  catalog.db       unchanged
```

## 7. Workstreams and ownership

| stream | owner | files |
|---|---|---|
| contract | lead (this doc's author) | `interface/library/{command,registry,follow,project,session,source}.rs`, `COMMANDS`, health rows |
| engine | Opus "engine" | `interface/library/{library,shelf,add,admission,image,page,session_service,source_service,search_service}.rs`, lexical/graph/semantic lanes |
| registry | Opus "registry" | `server/index/registry/*` (new crate), `interface/library/{registry_service,follow_service,project_service}.rs` |
| gui | Opus "gui" (foundation → checkpoint → explore/home/palette/settings) then a second Opus "reader" (reader/hover/source/tree) | `GUI2/*` |
| surfaces | Opus "surfaces" | `interface/cli/*`, `interface/mcp/*`, `interface/library/render/*` |

Build lock: at most three cargo users at once. Each builder checks only its own crates
(`cargo check -p <crate>`), never `--workspace`, and never runs `cargo clean`.

## 8. Checkpoints (each returns to the lead before continuing)

1. **Plan** — a written plan naming files, types, and the tests that will prove each rule.
2. **Foundation** — compiles, clippy clean at `-D warnings`, headless tests green, and for the GUI:
   the window launches from the bundle and screenshots of every view exist under
   `.local/screenshots/`.
3. **Complete** — every rule in this document is either implemented or listed as not done with
   the reason. Content-asserting tests (never count-only). A fault path exercised per view.

## 9. Commands

```
nix develop path:.config#development -c cargo check -p nudox-gui2
nix develop path:.config#development -c cargo clippy --locked --all-targets -p nudox-gui2 -- -D warnings
nix develop path:.config#development -c cargo nextest run -p nudox-gui2 --features preview
nix develop path:.config#development -c cargo bundle -p nudox-gui2 --bin nudox-gui --format osx
open .local/target/debug/bundle/osx/interface-gui.app
```

The compiler host discovers toolchains from `NUDOX_RUSTC`/`NUDOX_RUST_SYSROOT` (and the platform
table); the launch scripts under `GUI2/scripts/` export them from the nix shell before opening the
bundle so an add can compile.
