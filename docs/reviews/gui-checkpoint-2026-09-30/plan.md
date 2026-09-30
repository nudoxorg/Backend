# Inventory: plan vs built (Nudox desktop GUI)

Method: read the seven source families in the order given, then grepped apps/desktop/src and apps/facet/src per item (unbounded greps; a count of 0 is only reported as "none found" after an unbounded grep). Paths are relative to /Users/mileswirht/Downloads/backend/apps unless they start with docs/, Nudox-Design-System/ or .local/. Classes: BUILT-REAL, BUILT-FIXTURE, GALLERY-ONLY, PARTIAL, NOT-BUILT, SUPERSEDED, OPEN. Tree state: HEAD 525355f68 plus the uncommitted working-tree changes listed in gitStatus.

Cross-cutting facts used by many records (verified):
- The desktop mounts none of these facet::data marks: comb, compass, facts, lens_bar, strands, caps, spell, mosaic, rose, territory (grep of desktop/src for each returns 0 production uses). What it does use from facet::data: progress (gem_progress and seam, desktop/src/shell/onboard/library.rs:22) and release::view::section (desktop/src/shell/bodies/symbol.rs:185); the sidebar uses facet::controls::Release / version_comb (desktop/src/shell/side/listing.rs:41,531). The package page uses facet::folio::* (shingles, ticker, crest, berg, heads, features) instead; the symbol page uses facet::anatomy::symbol::*.
- Fixture data still read by the product: desktop/src/runtime/fixture_world.rs (graph world.json, labelled fixture), desktop/src/runtime/fixture_releases.rs (facet embedded release fixture: toml + smallvec only). desktop/src/runtime/releases.rs is new and untracked (see the releases record).
- The shelf splitter (200-420 px) is not wired: Shell.shelf_width is assigned only at construction (desktop/src/shell/root.rs:92,206) and never again.

Caveats on freshness (read before trusting a "missing"):
- The working tree is mid-change (git status: about 40 modified files plus untracked apps/desktop/src/runtime/releases.rs). In particular crates/local-service/src/builtin/view_build/{structural,semantic,query}.rs, crates/local-service/src/query/local.rs and apps/desktop/src/runtime/{page_mapping,store}.rs are being edited: the module-path/semantic-row problem (production-readiness.md:78-83 §1.4), search ranking and earlier-release trees may already be fixed there; records that lean on production-readiness.md for owner-side facts say so.
- production-readiness.md was written at 6ef7ebb6a; HEAD has since added F-Data 1 (dependencies indexed, Finder toolchain discovery), F-Shell 1 (green suites, sidebar on real data, Esc closes Settings, J10) and F-Data 2 (pages read while compiling, refusals survive relaunch, manifest names). Where I could see the fix in code I recorded the fix; where I could not I kept the claim with its source.
- Lane reports under .local/lanes are git-ignored; I read GAPS.md, transitions/CP2.md, world/BRIEF.md, wave4/surfaces/BRIEF.md and the folio/journey directories, but not every FOLIO/SYM6/FEEL checkpoint.

======== SHELL / FRAME / TITLEBAR / FOOT ========

### Shelf drag splitter (200-420 px)
surface: shell/frame
class: PARTIAL
intent: gui-plan.md:155-158 §2.6: shelf 264 px, resizable 200-420, collapses to a 42 px kspine.
code: facet/src/tokens.rs geo::SHELF_MIN/SHELF_MAX; desktop/src/shell/frame.rs:84 clamps a preferred width; facet/src/controls/splitter.rs exists (587 lines) but desktop/src/shell/root.rs:92,206 sets shelf_width once at construction and nothing assigns it afterwards; grep of desktop/src for "splitter" hits only frame.rs comments/tests.
missing/unknown: the drag handle is never mounted, so the shelf cannot be resized by a person; the clamp is exercised only by unit tests. No persistence of a dragged width either.

### Dock ladder: shelf / spine / drawer with hysteresis
surface: fluid widths
class: BUILT-REAL
intent: gui-plan.md:161-167 §2.6 responsive rules (<900 spine, <640 overlay); W-Fluid.md Modes with hysteresis and a transition.
code: facet/src/tokens.rs:747-751 DOCK ladder (Drawer 0, Spine 640, Shelf 900); desktop/src/shell/frame.rs:84-111; root.rs:1325 animates "shelf-w" on spec::SETTLE.
missing/unknown: real-data behaviour not judged in a release build; production-readiness.md:187 records 16 package-page and 5 symbol-page thresholds that jump under a mode change. No keyboard for the drawer instance of the sidebar (production-readiness.md:171).

### Pins column (third column of pinned peeks at 1900 effective px)
surface: overlays/popovers/hover
class: PARTIAL
intent: gui-plan.md:298-300, 6.1 "peeks chain three deep... pin into a column (Vast) or a ⌘P stack".
code: desktop/src/shell/pins.rs:1-52 mounts facet::overlay::float::pinned_column; facet/src/tokens.rs PINS ladder at 1900.
missing/unknown: the ⌘P pin-stack for narrower windows: none found (grepped "secondary-p", "PinStack", "pin_stack", "⌘P" in desktop/src and facet/src; key table has no pin chord, keys.rs:200-241). Pin is reached only by Space-again in a peek. The column has never been judged with real peeks on real data at 1900+ (no journey mentions it).

### Titlebar 50 px, traffic-light inset, shelf toggle
surface: titlebar/jump bar
class: BUILT-REAL
intent: gui-plan.md:155 §2.6 titlebar 50 px with traffic lights, shelf toggle.
code: facet/src/tokens.rs geo::TITLEBAR px(50); desktop/src/shell/titlebar.rs:1-18 (shelf toggle never goes, compacts in three modes via facet::tokens::fluid::BAR).
missing/unknown: Windows/Linux titlebar and traffic-light inset never exercised (production-readiness.md:309).

### Bead thread + here capsule
surface: titlebar/jump bar
class: SUPERSEDED
intent: gui-plan.md:155-156 §2.6 and §2.5 "titlebar thread of beads + here capsule".
code: replaced by the D-Hand jump bar: desktop/src/shell/jump.rs:1-6 ("History never shows at rest"); desktop/src/shell/jump_tests.rs:44 asserts "no beads"; the bead/capsule primitives survive in facet/src/chrome/titlebar.rs (834 lines, gallery scenes at facet/src/chrome/gallery.rs:43-63) but desktop/src/shell/mod.rs:4 still documents beads in its diagram.
missing/unknown: replacement (jump bar path + siblings menus) is built; the stale shell/mod.rs diagram and the unused facet::chrome::titlebar beads are dead weight nobody decided to delete. jump::Here (capsule) is still used for Settings/Inbox/graph focus.

### Trail and inbox buttons in the titlebar
surface: titlebar/jump bar
class: PARTIAL
intent: gui-plan.md:155 "trail/inbox buttons"; §2.7 Inbox row "releases, subscriptions: followed releases, calm".
code: inbox button exists (titlebar.rs:1-3 doc); shell/bodies/inbox.rs:1-23 is a stub room saying "Nothing followed yet. Releases you follow arrive here once the local service publishes a release feed." No trail button: grepped "trail" in desktop/src; hits are identity trails and the sidebar's Trail footer (shell/side/listing.rs:95,109), none in titlebar.rs.
missing/unknown: no release feed, no subscriptions, no "follow a release" action anywhere (grepped "follow", "subscribe", "Subscription" in desktop/src: only runtime store subscriptions, none product-facing). The Inbox is a permanent empty room.

### Foot / status bar: address, Copy link / Open in editor / Reveal menu
surface: foot/status
class: SUPERSEDED
intent: .local/lanes/wave6/PLAN.md:71-74 "The foot: the address (nudox://...) on the left. Clicking it offers Copy link, Open in editor (at the line), and Reveal."
code: desktop/src/shell/status.rs:1-5 "The address left the foot"; the address is hovered on the jump bar plate and ⌘⇧C copies it (keys.rs:239). No click menu: grepped "Copy link", "Reveal" in shell: Reveal is only onboard/commands.rs:77 (a project command).
missing/unknown: the foot menu (Copy link / Open in editor at the line / Reveal) was dropped without a recorded owner decision; ⌘⇧C gives no feedback (production-readiness.md:180, FEEL D10); "Open in editor" exists only as symbol-page uses rows and the rail source link, never from a package/file address.

### Foot: hand marks at rest
surface: foot/status
class: BUILT-FIXTURE
intent: PLAN.md:73 "The hand's marks."; hand rung Mark.
code: desktop/src/shell/status.rs:170-200 draws marks via shell/hand.rs; the hand view is computed on the fixture world: desktop/src/runtime/fixture_world.rs hand_view (called at status.rs:207, root.rs:1079,1115,1452, bodies/orbit.rs:205).
missing/unknown: hand arrangement/roads ("Continue A → B") are computed on the prototype world, not the index; production-readiness.md:91 says so.

### Foot: live index rule (fills while indexing, absent at rest)
surface: foot/status
class: NOT-BUILT
intent: PLAN.md:74 "A live index rule, which fills while indexing and is absent at rest."
code: none found: grepped "index rule", "IndexRule", "index_rule", "rule" + status.rs; status.rs draws only whisper, marks, graph line, retry button. Indexing progress lives in the Library body (shell/onboard/library.rs `indexing`, referenced at bodies/orbit.rs:98), not in the foot.
missing/unknown: no foot rule; progress is a Library section and depends on the owner reporting stages (production-readiness.md:69-77 §1.3: owner blocks reads and reports no progress).

### Foot: "hold ⌘ for keys" hint and ⌥ x-ray note
surface: foot/status
class: NOT-BUILT
intent: gui-plan.md:158 status bar 26 px "(mono address, 'hold ⌘ for keys')"; shell/mod.rs:6 diagram still claims "⌥ x-ray · hold ⌘ for keys".
code: none found: grepped "hold ⌘", "hold cmd", "for keys" in desktop/src (only the stale diagram at shell/mod.rs:6 and a facet control gallery script).
missing/unknown: nothing in the foot teaches the reveal chords; discoverability of ⌘ caps and ⌥ x-ray is zero at rest (gui-plan 6.2 rule 6 wants keys "when asked", but the ask is undiscoverable).

### Keys: J/K, Space, S, F, ⌘K, ⌘[ ⌘], Tab, Esc, ⌘\, ⌘,
surface: keys/focus
class: BUILT-REAL
intent: gui-plan.md:167-169 §2.6 key list.
code: desktop/src/shell/keys.rs:200-241 TABLE (FocusNext/Prev, Activate, Peek, PeelSource, HintMode, Ask, Back, Forward, NextZone, Escape, ToggleShelf, OpenSettings); J11 generates a journey from this table: desktop/src/harness/journey/keys.rs.
missing/unknown: J11 has not been reported passing on the real owner; production-readiness.md:290 lists it as required and unrun.

### Keys: ⌘- surface one depth, ⌘. zen
surface: keys/focus
class: SUPERSEDED
intent: gui-plan.md:168-169 "⌘- surface one depth, ⌘. zen".
code: keys.rs:215 Surface is now ⌘↑; keys.rs:217 Zen is ⌘⇧. ; keys.rs:210 ⌘. is code<->page; ⌘- is ZoomOut (keys.rs:224), depth is ⌃1-⌃4 (keys.rs:227-230).
missing/unknown: gui-plan.md was never corrected to the new chords; navigation/action.rs:204-225 still lists cmd-shift-p, cmd-b, cmd-n, cmd-shift-a, cmd-shift-y, cmd-shift-/, cmd-shift-m, cmd-left/right, cmd-0 as documented shortcuts that nothing binds (production-readiness.md:176 L2).

### Keys: sidebar chords (G C / G V / G R / G U), ⇧⌘J reveal, H hold, G graph in sidebar
surface: keys/focus
class: PARTIAL
intent: COHESION.md:132-153 (sidebar primitives 2 and 10) primitive 2 chords G C/G V/G R/G U; primitive 10 "⇧⌘J reveals on demand"; primitive 5 "H holds, G shows the graph".
code: desktop/src/shell/side/input.rs:49 KeyDoc "G C  G V  G R  G U"; Settings > Keys lists them via side::KEYS (settings.rs keys()).
missing/unknown: ⇧⌘J reveal-on-demand has no key-table row (production-readiness.md:169); typing/G-chords are not in the shell key table so J11 (generated from keys.rs TABLE) cannot cover them; chips reach only ⌘5 not ⌘9 (see hold record).

### Reveal: ⌘ held shows key caps, ⌥ held x-rays one rung
surface: keys/focus
class: PARTIAL
intent: gui-plan.md:298-300, 6.1 Reveal { keys, xray }; 6.2 rule 6 "Keys appear when asked".
code: desktop/src/shell/reveal.rs:1-106 (260 ms hold, state machine); root.rs:452-465 applies it; shell/tests.rs:757-769 asserts the reader rises a rung.
missing/unknown: which desktop bodies react to xray: the symbol page anatomy, the release view, the hero dependency line (facet/src/marks/deps.rs), peek cards (overlay/peek.rs) and the graph (graph/view.rs) yes; the package page (facet::folio: berg, ticker, crest, shingles) does not read xray at all (grepped xray in facet/src/folio: no file listed); the Library, Find (facet::browse) and sidebar rows also ignore it. Under ⌥ the sidebar has nothing "one rung up".

### Text scale 85-200 % via rem size, ⌘+ ⌘- ⌘0
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:166 text scale 85-200 %.
code: keys.rs:222-225 ZoomIn/ZoomOut/ZoomReset; settings.rs appearance() comment "Text size is not a setting: the system sets it".
missing/unknown: no in-app text-size control in Settings (deliberate); J10 persists "text scale" per W-Journey but round-trip through restart only unit-tested (production-readiness.md:206).

### Themes Abyss / Glacier, contrast High, density Compact / Dense, motion System/Full/Reduced
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:210 matrix (Abyss/Glacier); 6.1 Theme roles Contrast::{Normal, High}, Density.
code: desktop/src/shell/bodies/settings.rs:44-127 (theme swatch, contrast seg, density toggle, motion seg); shell/facet_sync.rs:30-45; model/persistence.rs:738-956 round-trips density/contrast.
missing/unknown: Density is a Settings control only; "dense folds summaries away" (gui-plan 6 Density row) has not been verified page by page on real data (no density sweep in any journey); a keyboard/quick toggle for density does not exist (grepped Intent::SetDensity: only settings.rs:88-92).

======== LIBRARY / HOME / FIRST RUN / ONBOARDING ========

### Home is the map (projects at the centre, dependencies in rings, Map / List)
surface: Library/home
class: NOT-BUILT
intent: PLAN.md:68-70 "Home is the map: projects at the centre with their dependencies in rings (Orbit4), plus one resume line, one new-release line, and Map / List. A package's plate on the map is the node that opens into its page."; gui-plan.md:175 Orbit "rings map + list; language comb".
code: desktop/src/shell/bodies/orbit.rs:1-3 "The rings map is wave 3; this is its list form." The body is project tiles in the middle plus a wrapped Flow of package name chips (orbit.rs:127-151). No Map/List toggle: grepped "Map / List", "MapList", "orbit4", "rings" in desktop/src and facet/src; only ring-flow variables and the twin ring layer match.
missing/unknown: the rings map, package plate as the opening node, the Map/List switch and the "language comb" are all unbuilt; nobody has recorded that the list form is the final decision. The resume line exists (orbit.rs:159 `resume`); the "new-release line" does not (no release feed, see Inbox).

### Library as strata (your crates on top, what they rest on beneath, true strata, loupe)
surface: Library/home
class: NOT-BUILT
intent: MOMENTS.md:33-38 (Browse: strata, hover, loupe, type-to-light) "Browse (the Library as strata)": layers by distance from your code, barycentre ordering, hover legit card, a bright core drawn up to the crate of yours, the loupe magnifier, type-to-light every matching name, V for the versions lens.
code: none found: grepped "strata", "stratum", "loupe", "barycent", "core up" in desktop/src and facet/src; the only "strat" hits are unrelated words. COHESION.md:155-163 (Versions, dependents, dependencies: Rests on strata) has no code either.
missing/unknown: entirely unbuilt; the Library page is a name ring (orbit.rs) and the roles Tree page (facet::browse::library) is unreachable in the product (next record). Whether strata scale to 1,350 packages was only ever tried in the HTML board.

### Library tree page (dependencies by role, unmaintained alert, duplicates)
surface: Library/home
class: PARTIAL
intent: package-browsing.md §2 slice S1 "Your tree": roles, alert, "60 crates are here twice"; COHESION.md:173-188 (Scopes and lenses: Library card per package) Library = a card per package.
code: built end to end: facet/src/browse/library.rs (512 lines), desktop/src/shell/bodies/browse.rs:30-34, runtime/browse_reads.rs, model/browse/mod.rs. Reachability: grepped BrowseRoute::Tree in desktop/src: only desktop/src/harness.rs:903,1193,1308 (harness) and tests; no click, key or link in shell/** navigates to it.
missing/unknown: a person cannot reach the built Library tree page; it has never been shown in a journey; the roles vocabulary is Cargo-only (crates/library/browse/cargo.rs; package-browsing.md:§6 says other ecosystems are "unverified and unplanned").

### Library: a card per package with mint (uses) and amber (newer release changes what you use)
surface: Library/home
class: NOT-BUILT
intent: COHESION.md:173-188 (Scopes and lenses, Library scope) "At Library scope, the reader is the one place shingles live as the primary view: a card per package with its overview. Mint shows what you use; amber shows what a newer release changes. Opening a card plays the Browse -> package film."
code: none found: orbit.rs draws names (kind_mark + mono name + apart, orbit.rs:170-230), not cards with overviews; grepped "Browse -> package", "browse_to_package", "film-browse" in desktop/src and facet/src: no hits.
missing/unknown: no per-package overview card, no mint/amber state on Library items, no browse->package shingle-fly transition.

### Library hover popovers on package names
surface: overlays/popovers/hover
class: OPEN
intent: MOMENTS.md:16 (progressive disclosure is a primitive) "Every package name anywhere is a preview link... Hovering opens its card"; FEEL D9.
code: desktop/src/shell/bodies/orbit.rs:222-226 warms the package page on hover (`hover_link`) but opens no card; production-readiness.md:189 "The Library shows no hover popover on package words or shelf rows. That is a design call (FEEL D9)."
missing/unknown: owner decision pending (production-readiness.md:364 item 4).

### Resume line ("Continue X -> Y . you left at Z . 4 min ago")
surface: Library/home
class: BUILT-FIXTURE
intent: PLAN.md:69 "one resume line".
code: desktop/src/shell/bodies/orbit.rs:159-205; with a non-empty hand it reads roads from `fixture_world::hand_view` (orbit.rs:205); with an empty hand it uses the last symbol route in back history.
missing/unknown: the hand's road sentence is computed on the prototype world, so it exists only for packages that appear in world.json.

### First-run screen (what the app is for, Add a folder, ⌘O, toolchain line)
surface: onboarding/first run
class: BUILT-REAL
intent: gui-plan.md:185 Onboarding row; W-Install.md §1.
code: desktop/src/shell/onboard/library.rs:143-205 `empty` (lede "Read the code you depend on.", "Add a folder" primary, "or press ⌘O", privacy line, compiler line from host::toolchain::report()); ⌘O bound at shell/keys.rs:241.
missing/unknown: Finder-launch toolchain discovery is now built (desktop/src/host/toolchain.rs: find_rust over PATH, rustup ~/.cargo/bin, Homebrew, Nix profiles; a Go module cache from GOMODCACHE/GOPATH/~/go/pkg/mod; `report()` says what was found or where it looked) with unit tests (toolchain.rs:407-426), but no journey has launched the app under `env -i HOME PATH=/usr/bin:/bin` (production-readiness.md:58) and the screen has never been seen by a stranger without a toolchain; nothing discovers Node, Python, JDK, .NET or Clang.

### Add-a-folder dialog (typed path, completion, inline refusal)
surface: onboarding/first run
class: PARTIAL
intent: W-Install.md §1 "keyboard-first: type or paste a path, get completion, see a validation message inline"; gui-plan §2.7 Add flow.
code: desktop/src/shell/onboard/add.rs, path.rs (admit/complete off the UI thread), mounted on the float layer's dialog per onboard/mod.rs:1-9.
missing/unknown: focus ring jumps on close (GAPS D6); page under dialog reads as 13 false contrast lints (GAPS D4; check whether closed by F-Shell 1); the OS folder picker (`OpenFolderPicker`/`FolderPickerResult`) is the only non-keyboard route and was never exercised on Windows/Linux. No drag-a-folder-onto-the-window path was designed.

### Dependencies arrive on the rings / seam of stages while a project indexes
surface: onboarding/first run
class: PARTIAL
intent: gui-plan.md:185 "dependencies arrive on the rings; seam of stages"; W-Install.md §2 "stages as the owner reports them (discovery, resolution, per-package compile, sealing)".
code: desktop/src/shell/onboard/library.rs:39-56 draws three stages the window itself can attest (admitted / indexing / ready) as facet::data::{gem_progress, seam}; library.rs:279 `indexing` then draws per-package acquisition from runtime/acquire.rs once the project answers.
missing/unknown: no ring arrival (there are no rings); the owner still reports no discovery/resolution/compile/sealing events (production-readiness.md:69-77 §1.3), so the middle step is a fraction-less "indexing" and the tooltip says so (library.rs:58-68). No cancel control is drawn during indexing (Intent::CancelIndex exists, runtime/ui_graph.rs; grepped shell/onboard for CancelIndex: only commands.rs vocabulary).

### Clean install indexes the project's dependencies
surface: onboarding/first run
class: PARTIAL
intent: first-run copy at onboard/library.rs:160 "Nudox compiles it and every package it uses"; production-readiness.md:61-67 §1.2 (G15).
code: desktop/src/runtime/acquire.rs:1-40 (registry releases and "every registry package a project builds with, once the project itself is indexed"), acquire/work.rs `dependencies`; host/registry.rs `RegistrySource` (offline from ~/.cargo/registry).
missing/unknown: offline-only by design; no network adapter is allowed to run, so a dependency not in ~/.cargo/registry is named as failed ("Partial"/"Failed" stages, acquire.rs:38-52); network policy is undecided (production-readiness.md:119, owner decision 1). Only Cargo.lock is resolved (acquire::NOT_CARGO refusal for other ecosystems, runtime/acquire.rs:26): npm, PyPI, Go, Maven, NuGet, Conan projects get no dependencies at all.

### Failure card (what stopped, Try again / Reveal / Remove)
surface: states (loading/empty/failure/stale)
class: BUILT-REAL
intent: W-Install.md §3; gui-plan §2.7 States "fault anatomy".
code: desktop/src/shell/onboard/failure.rs (Cause::of reads file/language from the owner's text, disclosure shows owner words), commands.rs ProjectCommand::{Retry,Reveal,Remove}.
missing/unknown: the cause parser is string-matching on owner prose (failure.rs:1-12 quotes the format), typed reasons do not exist upstream (production-readiness.md:145,322); owner-failed notice outside page bodies has only a text line in the Library and a status-bar Try again (status.rs:238-262); J13 with a real failing fixture has not been written or run.

### Multi-project (two admitted folders, one active, instant switch)
surface: Library/home
class: BUILT-REAL
intent: W-Install.md §5; journey_specs.rs ReadyMultiProject.
code: desktop/src/navigation/workspace_reducer.rs:196-213 (ActivateProject sets active without re-index; GAPS G8 is closed in code); orbit.rs:63-125 tiles.
missing/unknown: no multi-project journey (J0b never written; apps/desktop/journeys has J0,J1,J9,J10,J12 only); "each project's Library is correct" is unasserted since every project shares the owner's one index.

### State set aside ("Your library was built by an earlier version and is being rebuilt")
surface: states (loading/empty/failure/stale)
class: PARTIAL
intent: W-Install.md §4 "told to the person once, calmly".
code: desktop/src/host/aside.rs (35 lines) defines the set-aside; onboard/library.rs `notes` (line 91) renders host notes.
missing/unknown: whether the sentence ships verbatim and is shown once is unproved (no journey); `unsupported view DTO version` from an older wire is still refused whole as prose (production-readiness.md:136).

### Window size persistence
surface: onboarding/first run
class: PARTIAL
intent: W-Install.md §4 lifecycle; GAPS G5.
code: desktop/src/host/window_size.rs (86 lines), host/launch.rs.
missing/unknown: unit-tested only; never proved by a real resize + relaunch (production-readiness.md:206).

### Settings restart persistence (theme, contrast, density, motion)
surface: settings
class: PARTIAL
intent: W-Journey J10.
code: desktop/src/model/persistence.rs:738-956 round-trips density/contrast/appearance/motion; apps/desktop/journeys/J10.journey exists.
missing/unknown: text scale is a system/⌘± value, not a setting, so "text scale persists" is untested; production-readiness.md:175 (D5) Esc does not close Settings so a relaunch reopens Settings (state shows route "Settings"); F-Shell claimed a fix (commit 21f19a6f1 "Esc closes Settings") but no post-fix journey report is in the tree.

======== FIND / ASK / SEARCH / THE ONE QUERY ========

### One query in the jump bar (path at rest, query when you type; typing anywhere starts it)
surface: Ask/search
class: PARTIAL
intent: PLAN.md:58-66 "The jump bar is the path at rest and the query when you type. Typing anywhere (outside a text field) starts it, and ⌘K focuses it."
code: desktop/src/shell/ask.rs:1-20 (the jump bar draws the field; the plate is over the shelf's column); titlebar.rs:539-620 ask_typing/ask_field; ⌘K in keys.rs:212.
missing/unknown: "typing anywhere starts it" is not built: plain letters are bound to J/K/S/G/F/H/T and, in the sidebar, to narrowing (side/input.rs:1-30), so no printable key opens the query (grepped keys.rs TABLE: no catch-all). Only ⌘K opens it.

### The query lights the world (shelf narrows, page names take the underline, graph matches light)
surface: Ask/search
class: NOT-BUILT
intent: PLAN.md:58-67 "results unfurl from the bar's lower edge; the shelf narrows to matching rows (replaces the Filter field); matching names on the page take the hover underline; in the graph, matches light and the rest step down to ink4."
code: none found: grepped "query_lit", "lights the world", "lit_by_query", and read ask.rs/side/narrow.rs: the Ask plate previews places (Intent::Preview) but publishes no query set to the shelf, pages or graph. The sidebar's Narrow (side/narrow.rs) is a separate typed buffer. The graph's own find field (facet/src/graph/view.rs:2081 graph-find-bounds) still exists, so the PLAN's "Retire the graph's field" did not happen.
missing/unknown: whole feature; the graph constellation (facet/src/graph/draw.rs:97) fires only from the graph's own field. Four search places still exist: Ask, Find page, graph field, sidebar narrow.

### Results scoped: here, then this package, then everywhere
surface: Ask/search
class: PARTIAL
intent: PLAN.md:65 "Results are scoped: here, then this package, then everywhere."
code: desktop/src/shell/ask.rs:41-48 Group::{Here, Everywhere} only (two tiers, "here" = the package you are reading); PER_GROUP 8 (ask.rs:37).
missing/unknown: no "this module/here" tier below package; no scope chip or way to widen/narrow scope by key.

### "All results" (⌘↵) as a place in the same grammar
surface: Ask/search
class: PARTIAL
intent: PLAN.md:67 "'All results' (⌘↵) becomes a place in the same grammar."
code: desktop/src/shell/ask.rs:206-209 `all_results` opens BrowseRoute::Find(query); the Find page is still its own reader route with a FIND eyebrow (facet/src/browse/find.rs:290).
missing/unknown: PLAN said retire "the Find page's header"; header, narration and the Compare/Held buttons (find.rs:434-439) and the mint primary "Explore declaration" button (find.rs:499) remain. Whether ⌘↵ is bound: grepped keys.rs TABLE and ask.rs; the row at ask.rs:416 is a click target, no ⌘↵ chord in the table (J11 cannot cover it).

### Find by shape ("path -> maybe text", "Invocation -> list of text", in steps)
surface: Ask/search
class: GALLERY-ONLY
intent: gui-plan.md:472-500 §8.2 Find: by shape, traits count, in steps (chains), the empty box teaches.
code: built in the graph: facet/src/graph/discovery.rs (1707 lines), discovery/capability.rs, discovery/proof.rs, recipes at facet/src/semantics/recipes/chain.rs; reachable only in the desktop graph body over the fixture world (bodies/graph.rs:180 `fixture_world::blocking()`). Ask (shell/ask.rs) and the Find page search names only: grepped "shape", "->", "teach" in ask.rs and facet/src/browse/find.rs (the Find page renders a `pipe` per answer but does not search by shape).
missing/unknown: shape search on the real index; browse-plan S2b "Shape, like-X, pasted code" (package-browsing.md:§5) is unstarted; J2 (`toml Value` -> shape `Invocation -> list of text`) is not written (no J2.journey) and cannot pass on a fixture world.

### "The empty box teaches" (three quiet rows)
surface: Ask/search
class: PARTIAL
intent: gui-plan.md:478-481 §8.2.
code: implemented in the graph's own find box (facet/src/graph/view.rs, hints at facet/src/graph/view/hints.rs); Ask's empty state: desktop/src/shell/ask.rs:362-437 shows placeholder "Find a name, or ask" only.
missing/unknown: the one place people type (Ask) never teaches shape search, name search or hop-in-steps.

### Find page: filters as combs/marks, ranked mixed results, one reason per row, preview
surface: Find/browse/discover
class: PARTIAL
intent: gui-plan.md:181-182 §2.7 Ask + Search results rows.
code: desktop/src/shell/ask.rs (reason per row, MatchReason, underline matched range ask.rs:315); facet/src/browse/find.rs (inspector, candidates, coverage).
missing/unknown: no filters (combs/marks) exist on the results (grepped "filter" in facet/src/browse/find.rs: no filter controls); Find cannot show a crate only present in ~/.cargo/registry (production-readiness.md:126 G12, runtime/reads.rs:1126-1168).

### Find: "you already have this", std verdicts, words -> item (package-browsing S2)
surface: Find/browse/discover
class: NOT-BUILT
intent: package-browsing.md §5 S2 "find('parse toml') -> verdict Yours... find('read a file into a string') -> verdict Std".
code: none found: grepped "Verdict", "Yours", "you already have this", "Std" in facet/src/browse and desktop/src/runtime/browse_*.rs; only S1 tree is shipped.
missing/unknown: S2 through S5 (Find words, Judge, Compare with incumbent uses, Adopt) were never started per the brief and there is no later record in the tree.

### Compare across packages (2-4 packages side by side)
surface: Find/browse/discover
class: SUPERSEDED
intent: COHESION.md:5-12 (owner corrections: Compare only across versions) owner correction: "The comparison page should only be for comparing between versions not between packages."
code: still built and reachable: desktop/src/navigation/browse.rs:8-40 CompareSet (2-4), shell/bodies/browse.rs:36-42, facet/src/browse/compare.rs (630 lines), reached from Find's "Compare"/"Held" button (facet/src/browse/find.rs:434-439).
missing/unknown: the owner said not between packages; nothing removed it, and the replacement (version compare: "What changes for you" first, per-module added/changed/removed, verdict sentence) is unbuilt as a page (see releases records). Compare's costs band forbidden-header rule (package-browsing ruling 2) not re-verified.

### Adopt (write the dependency into Cargo.toml with an exact diff)
surface: Find/browse/discover
class: NOT-BUILT
intent: package-browsing.md §5 S5 + ruling 5 (workspace.dependencies aware, show diff before writing, never silent); MOMENTS.md "Add" (open question 2: add to project vs library).
code: none found: grepped "dependency-plan", "dependency-add", "toml_edit" in desktop/src: no writer. The only add is "Add to library" (runtime/acquire.rs, shell/acquire.rs) which indexes a release for reading, it does not edit any manifest.
missing/unknown: OPEN owner question MOMENTS.md:56 (open question 2: Add means which?) ("Add means which?"): the product implements add-to-library (reading only) and never asked.

### Add a package from browsing (Find rows, package page header, dependency rows)
surface: Find/browse/discover
class: PARTIAL
intent: W-Acquire.md §1; production-readiness.md:121-127 (G10/G12).
code: desktop/src/shell/acquire.rs:1-92 `add_actions` and `page_offer` (mounted at shell/bodies/package.rs:132-138); facet/src/browse/acquire.rs + acquire/tests.rs; runtime/acquire/work.rs; find.rs imports Offer/AddActions (find.rs:6).
missing/unknown: the offer distinguishes Unpacked / Archive / Download (facet/src/browse/acquire.rs:34-52) and the Add control is disabled for Download, so a release only in the registry cannot be added and nothing says how to enable downloads; the package-page offer's label is the uppercase 'NOT IN YOUR LIBRARY' (shell/acquire.rs:83); dependency rows still only navigate (shell/bodies/package.rs:291-296); `cargo add NAME` in the ecosystem mark only copies to the clipboard (marks/eco.rs:116,361); no journey J7; the offline/would-download distinction is undecided (owner decision 1).

### Discover: the registry browsable with a dock, aisles, categories, registry cards
surface: Find/browse/discover
class: NOT-BUILT
intent: MOMENTS.md:59-96 (Discover) "Discover" (dock of your direct deps, aisles per category, registry card with fingerprint/comb/licence seal/heads-up/cost/+, flight to a ghost slot, toast "Added tonic to desktop ... Undo").
code: none found: grepped "aisle", "Discover", "dock", "ghost slot", "categories.json" in desktop/src and facet/src: no product code (only the board under Nudox-Design-System/v6/moments). A package page's category shown in the byline (shell/bodies/package/data.rs:670 byline) is text, not a link to an aisle.
missing/unknown: entire surface; requires a registry catalogue (registry.json/categories.json exist only as regenerated board data) and a network policy; also the "add play a card" hand animation (MOMENTS.md:40-45 (Add: play a card)) is board-only.

### Registry results under your library's answers in the jump bar (each with a legit strip and +)
surface: Find/browse/discover
class: NOT-BUILT
intent: MOMENTS.md:65 (Discover: the jump bar) Add verb spot 1.
code: none found: ask.rs choices() returns only index search rows (ask.rs:235-256); no registry rows, no legit strip, no "+".
missing/unknown: needs G12 (Find over the cargo cache) first.

### Toast "Added X to Y - brought N - shares M - Undo"
surface: overlays/popovers/hover
class: PARTIAL
intent: MOMENTS.md:92 (toast).
code: facet/src/overlay/toast.rs exists (476 lines); grepped desktop/src for "toast::" / "Toast": no product use for Add; the acquire flow reports progress via the Library seam and package page offer (shell/acquire.rs), not a toast.
missing/unknown: no undo semantics for add-to-library; no "brought / shares" cost numbers (needs the dependency closure vs your lock, package-browsing S3).

### Cancel indexing
surface: Library/home
class: NOT-BUILT
intent: W-Install.md §2 (uses `CancelIndex`); gui-plan onboarding.
code: Intent::CancelIndex exists (desktop/src/navigation/intent.rs:216, workspace_reducer.rs:271) but no surface dispatches it: ProjectCommand::for_phase for Indexing is [Reveal, Remove] only (desktop/src/shell/onboard/commands.rs:82-92); grepped desktop/src for CancelIndex dispatch sites: reducer and journey_specs only.
missing/unknown: a person cannot stop a running index short of Remove; Cancelling/Cancelled phases render but nothing leads there.

### Toast (says a fault once, undo)
surface: overlays/popovers/hover
class: GALLERY-ONLY
intent: gui-plan.md:145 Overlays: toast; MOMENTS.md:92 (toast) Add toast with Undo; production-readiness.md:180 (⌘⇧C needs a toast).
code: facet/src/overlay/toast.rs (476 lines) with gallery scenes; grepped desktop/src for "toast" / "overlay::toast": zero hits. Notices go to the foot text line (shell/status.rs) instead.
missing/unknown: the product has no toast; copy address, add-to-library, hold, remove-project, errors that should be "said once, in place" all have no toast/undo channel.

======== SIDEBAR (COHESION §"The sidebar: primitives" 1-10) ========

### 1 Scope: one scope, way out, title; path only in the jump bar
surface: sidebar
class: BUILT-REAL
intent: COHESION.md:129-131 (primitive 1 Scope) primitive 1.
code: desktop/src/shell/side/scope.rs:1-24, listing.rs header; view.rs paints the step-out and title.
missing/unknown: each jump-bar segment should be "a menu of its siblings with their rolled-up state (type to filter, up/down, return)": desktop/src/shell/titlebar.rs:689-740 builds plain MenuItem::new(name) entries (folding test modules into "tests"); no rolled-up state glyphs, no type-to-filter (grepped titlebar.rs, facet/src/overlay/menu.rs for "filter": none).

### 1b Hoist (→ or double-click on a row with children; ← pops)
surface: sidebar
class: BUILT-REAL
intent: COHESION.md:129-131 (primitive 1, hoist) primitive 1.
code: desktop/src/shell/side/scope.rs:19-24, mod.rs:344 `hoist`.
missing/unknown: hoisting "into a row" of a symbol other than a module/type (a method's members?) untested; drawer instance (phone widths) has no keyboard (production-readiness.md:171).

### 2 Lens strip: Contents · Versions · Rests on · Used by, chords G C / G V / G R / G U
surface: sidebar
class: PARTIAL
intent: COHESION.md:132-136 (primitive 2 Lens) primitive 2; table "Scopes and lenses" COHESION.md:173-188 (Scopes and lenses table).
code: desktop/src/shell/side/lens.rs, input.rs:49, listing.rs:292-352 (Library), 592-680 (package).
missing/unknown: Library-scope Versions lens is a single note "Newer releases across the library are not indexed yet." (listing.rs:351); Library Rests on is "The library rests on nothing." (matches the table's "none"); Library Contents groups only "Yours / In the library" not "grouped by how they relate to you" (listing.rs:297-345); Item-scope lenses (members / its history / what it is made of / callers by crate) exist only partially: no Rests on for an item, no "its history" content beyond the fixture. Versions lens content is BUILT-FIXTURE (side/mod.rs:242 reads fixture_releases::release_data).

### 2b Versions lens: releases newest first, pin mint, target periwinkle; choosing a target makes the reader show the diff and sets amber everywhere
surface: releases/time travel/upgrade
class: BUILT-FIXTURE
intent: COHESION.md:132-136 (primitive 2, Versions).
code: desktop/src/shell/side/mod.rs:242-278 builds rows from fixture_releases::release_data; row glyph amber state from shell/side/state.rs:169; Intent::SetRelease from the ticker and the lens.
missing/unknown: releases for any package other than toml/smallvec are undated fixtures or absent; real data path is desktop/src/runtime/releases.rs (untracked, NOT declared in runtime/mod.rs, so not compiled); GAPS G14.

### 3 Narrow in place ("9 of 191"; searches collapsed rows; "query in the whole library ↵")
surface: sidebar
class: BUILT-REAL
intent: COHESION.md:137-140 (primitive 3 Narrow) primitive 3.
code: desktop/src/shell/side/narrow.rs, listing.rs:199-210 (widen row with "↵"), view.rs:216 count.
missing/unknown: "widen" goes to Ask, whose results are names only; real-data "9 of 191" count untested on a 10k-declaration package; the narrowing hint had 2.59:1 contrast (GAPS D1; F-Shell 1 claims the sidebar on real data).

### 4 State on rows (mint uses, amber changes, coral gone, ink members)
surface: sidebar
class: PARTIAL
intent: COHESION.md:141-147 (primitive 4 State on rows) primitive 4.
code: desktop/src/shell/side/state.rs:1-30 StateBook, glyph.rs.
missing/unknown: mint use counts come from the index's reference sites (relation + byte span, model/pages/symbol.rs references; the line text is read from your file by runtime/workspace_lines.rs), so they are only as good as the owner's resolved references (Resolution::Resolved vs matched-by-name); amber/coral come from the fixture releases (toml, smallvec only); other ecosystems have no state.

### 5 Peek (arrows move selection, peek follows beside the sidebar, Space toggles, ↵ opens, → members, H holds, G G graph)
surface: sidebar
class: BUILT-REAL
intent: COHESION.md:148 (primitive 5 Peek) primitive 5.
code: desktop/src/shell/side/peek.rs (rides the float layer, morphs row to row); input.rs.
missing/unknown: "per crate, with bars" for who uses it; and "what the target release does to it" depend on fixture releases; 120 ms dismissal bar vs 220 ms grace (production-readiness.md:183 FEEL D13).

### 6 Twins (hover a sidebar row lights its twin in the reader and vice versa)
surface: sidebar
class: PARTIAL
intent: COHESION.md:149 (primitive 6 Twins).
code: desktop/src/shell/side/twin.rs (ring layer over registered targets with `source`).
missing/unknown: only targets registered with a `source` twin. Verified: the symbol page's doors (bodies/symbol/host.rs:74) and the package page's open-module cards (bodies/package/folio.rs:679 `source: Some(symbol)`) do; the package page's regions and doors (folio.rs:303,324), the Library chips and tiles (orbit.rs:94,270), and the resume line (orbit.rs:225) register `source: None`, and crest cells, features bar, ticker and berg blocks are not targets at all (production-readiness.md:179). So hovering a sidebar module/package row lights nothing on the package intro.

### 7 Hold (chips above the lenses, ⌘1-⌘9)
surface: hand/holds
class: PARTIAL
intent: COHESION.md:150 (primitive 7 Hold) primitive 7 "chips with ⌘1-⌘9 (Arc favourites)".
code: desktop/src/shell/side/hold.rs:1-20 says ⌘1-⌘5 only (the hand holds at most five, model/hand.rs); keys.rs:234-238 HandCard1-5.
missing/unknown: six to nine slots do not exist; the chip cap is a decision made by the hand model, not recorded as an owner decision.

### 8 Trail (history visible and clickable at the sidebar foot)
surface: sidebar
class: BUILT-REAL
intent: COHESION.md:151 (primitive 8 Trail) primitive 8.
code: desktop/src/shell/side/hold.rs (trail), listing.rs:95-109 `trail: Vec<Step>`.
missing/unknown: the older gui-plan "trail" button in the titlebar was dropped (see titlebar record); cross-session trail persistence unverified.

### 9 Sticky ancestors
surface: sidebar
class: BUILT-REAL
intent: COHESION.md:152 (primitive 9 Sticky ancestors).
code: desktop/src/shell/side/view.rs:79-120.
missing/unknown: not exercised on a 1,000-row real outline in any journey.

### 10 Follow (reveal the reader's current item; ⇧⌘J reveals on demand)
surface: sidebar
class: PARTIAL
intent: COHESION.md:153 (primitive 10 Follow).
code: automatic follow: side/scope.rs:12-16.
missing/unknown: ⇧⌘J has no key-table row (grepped keys.rs TABLE; production-readiness.md:169); "Settings > Keys" lists sidebar keys (settings.rs:177-188) but not this one.

### Contextual rule: on the package intro do not repeat the modules
surface: sidebar
class: BUILT-REAL
intent: W-Side.md §1, PLAN.md.
code: desktop/src/shell/side/outline.rs `intro_rows`, listing.rs:187-190.
missing/unknown: real toml data showed duplicate module names with no parent context (map x3, array x2) and `use` imports listed as contents (production-readiness.md:163-167); "Contents 747" unclear; `toml_pin` listed twice — data or view unresolved.

======== PACKAGE PAGE (MOMENTS §"package page top to bottom", COHESION §"Package page: the folio", W-Folio) ========

### Package lanes: Dependencies, Dependents, Releases, Security
surface: package page
class: NOT-BUILT
intent: gui-plan.md:177 Package (Territory) row: "package-versions, dependencies, dependents"; MOMENTS.md:5-12 (the owner's corrections) "That can be a separate page" (the relations).
code: PackageLane::{Overview, Dependencies, Dependents, Releases, Security} exist as a typed enum (desktop/src/navigation/route.rs:104-116) but every constructor in the tree passes PackageLane::Overview (desktop/src/shell/kit.rs:310, side/scope.rs:184, root.rs:1590, reader.rs:1902, runtime/ui_graph.rs:469; grepped PackageLane:: unbounded) and package::body ignores route.lane.
missing/unknown: four lanes are declared and unreachable; the dependency/dependent page ("can be a separate page", MOMENTS.md:5-12) was never designed beyond the sidebar rows and the hero's dependency line.

### Package page: dedicated dependencies/dependents view (fresh thinking: why here, how much you use, what breaks)
surface: package page
class: OPEN
intent: COHESION.md:5-12 (owner corrections) "Dependencies and dependents need fresh thinking: why it's here, how much of it you use, who uses what, and what breaks if it changes."; MOMENTS.md:47-51 (the relay became its own view) "The relay ... its own view (?v=graph)".
code: none found: grepped "why it's here", "what breaks", "How desktop uses", "relay" in desktop/src and facet/src; the hero's dep_line (facet/src/marks/deps.rs, mounted at shell/bodies/package.rs:216-237) shows chips with kind/req/resolved only (dep_facts sets uses None, purpose None, in_tree None, tree_note "your tree is not read yet", package.rs:262-282).
missing/unknown: "Rests on" strata, "Used by" ("How desktop uses toml": call sites grouped by item, used names underlined, "the rest of toml (24 items) it never touches"), removal what-if, duplicates callouts; the relay graph (MOMENTS.md:47-51 (Graph: the relay)). All unbuilt and need a dependency-usage producer that "has no producer reaching this page yet" (package.rs:258-262).

### Hero: gem, name (never cut), lede, byline (who, where, categories, edition + MSRV)
surface: package page
class: BUILT-REAL
intent: MOMENTS.md:20 (hero) item 1.
code: desktop/src/shell/bodies/package.rs:152-233 hero(); byline from source facts (bodies/package/data.rs:656-677).
missing/unknown: byline is read from Cargo.toml on disk (model/source_facts/manifest.rs:443); non-Cargo packages get no byline/categories/edition/MSRV (grepped source_facts and local_package for package.json/pyproject/go.mod/pom.xml/csproj/conanfile: zero hits). The hero lede/byline overrun the edge at 430 px and below (production-readiness.md:234).

### Release chip in the hero ("1.53.1 - your pin")
surface: package page
class: PARTIAL
intent: MOMENTS.md:20 (hero) "the release chip".
code: no chip in hero(); the pin is stated by the ticker's rider label "your pin X" (facet/src/folio/ticker.rs:461, folio/tests.rs:320) and by the time-travel banner (bodies/package/folio.rs:247-290).
missing/unknown: the version is not on the hero; on a page with no ticker (no dated releases, or `is_local()` packages, production-readiness.md:217 D3) the page states no version at all. J1 asserted "the package hero names the package and its pinned version" per gui-plan.md:229 and the journey instead checks the module regions (J1.journey:23-25).

### Five facts as components: Releases (ticker), Licence, Surface, Weight, Heads-up
surface: package page
class: PARTIAL
intent: MOMENTS.md:21-26 (five facts).
code: Releases: facet/src/folio/ticker.rs (fisheye, label rides inside, drag/click travel, ←/→ Home/End); Licence: folio/crest.rs (stamp unfolds in place, OR/AND/WITH via marks/spdx.rs, judged vs your project's licence, permits/asks/won't promise crest.rs:226-238); Heads-up: folio/heads.rs (compact stack, fans on rest, sheet on click); Weight: folio/berg.rs (iceberg glyph, full berg, keel lights); Features bar: folio/features.rs; Advisories: crest.rs + bodies/package/data.rs:405-455.
missing/unknown: the "Surface" fact is only a text line above the map ("N public names in M modules · documented X%", desktop/src/shell/bodies/package/folio.rs:362-388), the "facts as a line of text" pattern the owner rejected (DIRECTION.md:17; memory facts-are-components-not-text): no family bar (types/callables/contracts/values), no modules/lines-of-code cell, no "you use N", no tests/examples count, no hover component (grepped "family bar", "SurfaceCell" and crest cells: none). Weight bar "own code vs direct vs deep ('its own code is 3% of it')" is a berg, not the described bar; "hover shows where weight comes from per direct dependency" is the berg's block hover.

### Release ticker: tall major, mid minor, short patch, coral yanked, mint pin, amber newest
surface: releases/time travel/upgrade
class: BUILT-FIXTURE
intent: MOMENTS.md:21-26, W-Folio.md phase A/B.
code: facet/src/folio/ticker.rs; dates from data::ticker (bodies/package/data.rs:545-595) which reads the cargo sparse-index cache via model/source_facts/registry.rs and, for change marks, fixture_releases (data.rs:499,573).
missing/unknown: works only where the cargo index cache lists the crate (registry crates; not local roots); dates for non-Cargo packages have no source; change marks limited to toml and smallvec.

### Time travel: the page reads another release (banner, warmer tint, recoloured shingles)
surface: releases/time travel/upgrade
class: PARTIAL
intent: W-Folio.md phase A "Time travel"; COHESION.md:62-92 (folio interactions: scrub the release comb) "Scrub the release comb: recolour shingles new/gone/changed".
code: banner bodies/package/folio.rs:247-290 ("Reading X before/after your pin", "back to your pin" Esc); shingles wear amber/coral/ghost (facet/src/folio/shingles.rs:19-24); route.at via Intent::SetRelease; store.rs:245-280 route_package reads another release's tree if the library holds it.
missing/unknown: the recolour needs release diffs: only fixture data (toml, smallvec) or nothing; for any other package the banner reads "Its names are not read yet: only the release's date and size are known." (folio.rs:266); a local-root pin falls back to the pin's dossier so the page shows 0.8.23's names under "Reading 0.5.11" (production-readiness.md:131 / GAPS G13). Hover-a-tick-recolours-without-navigating (COHESION "Scrub the release comb: hovering a tick recolours the shingles") is not the behaviour: the ticker travels (navigates) on click/drag; hovering only widens.

### "What changes for you" verdict line (calls that parse into an owned value compile unchanged)
surface: releases/time travel/upgrade
class: NOT-BUILT
intent: COHESION.md:155-163 (Versions reader) Versions reader; MOMENTS.md "a line says what changes for you".
code: none found: grepped "what changes for you", "compile unchanged", "verdict" in desktop/src and facet/src; the banner gives counts only (folio.rs:249-262).
missing/unknown: requires your-code use sites per changed API (Crate.uses/impact); the real diff module leaves them empty (desktop/src/runtime/releases.rs:70-74 sets uses: Vec::new(), impact: Vec::new()) and is not even declared in runtime/mod.rs.

### Real release data behind release_data (runtime/releases.rs)
surface: releases/time travel/upgrade
class: PARTIAL
intent: W-Acquire.md §2 "Replace fixture_releases behind its existing API".
code: desktop/src/runtime/releases.rs (98 lines, untracked, NOT declared in desktop/src/runtime/mod.rs so it does not compile into the crate); it lists releases from the cargo index cache (dated, yanked, local-or-download) and asks the owner for SurfaceCommand::Diff per indexed release; but sets `before: None, after: None` (no signatures), `semver_slip: false`, `aliases: HashMap::new()`, `uses: Vec::new()`, `impact: Vec::new()`. Every product caller still calls fixture_releases::release_data (bodies/symbol.rs:112,179; symbol/history.rs:18; package/data.rs:499,573; side/mod.rs:242).
missing/unknown: unfinished wiring; signature diffs with plain-word respelling (gui-plan.md:635-637), the semver slip mark ("breaking in a minor release"), aliases for re-exports, and your-code impact are absent from the real path. Only Cargo registry releases; no other ecosystem.

### Upgrade lens on the symbol page ("Upgrading to 1.1.6", affected use sites, signature rows)
surface: releases/time travel/upgrade
class: BUILT-FIXTURE
intent: gui-plan.md:626-637 §8.4.
code: desktop/src/shell/bodies/symbol.rs:172-187 `upgrade` builds facet::data::release::lens over fixture_releases::release_data and renders facet::data::release::view::section.
missing/unknown: works only for toml and smallvec; journey J3 (comb -> 1.1.6 -> "82 added, 2 removed, 103 changed") is BLOCKED(data) by rule and not written (no J3.journey).

### Sidebar/shelf version comb (VersionComb.png) re-scoping page AND graph to a release
surface: releases/time travel/upgrade
class: PARTIAL
intent: gui-plan.md:625 "The version comb in the shelf header re-scopes page and graph to a release. In the graph, symbols absent at that release fade and ones added since your pin glow once."
code: sidebar comb: desktop/src/shell/side/view.rs (comb) + shelf listing.rs:531 facet::controls::Release; page re-scope via Route.at.
missing/unknown: graph does not re-scope: grepped facet/src/graph/*.rs for release scoping (only camera "release" as input release velocity, view.rs:1117); graph reads the fixture world regardless of route.at.

### Territory of shingles as the package intro (one region per module, one shingle per public name)
surface: package page
class: BUILT-REAL
intent: COHESION.md:62-92 (Package page: the folio) "The territory", MOMENTS.md "Contents at a release".
code: facet/src/folio/shingles.rs (justified rows, fluid size, hover region/shingle, past tint); mounted by bodies/package/folio.rs via ModuleFacts from data::modules. facet::data::territory (squarify treemap, 830 lines) is the older, unmounted version.
missing/unknown: with semantic rows lacking module paths the page draws one `lib` block as if true (production-readiness.md:82,215); outline modules come from Cargo `src` file layout, not a module tree, for other languages.

### Hover a shingle: swell, strands to related shingles, cross-package exits at the map edge, peek after 280 ms
surface: package page
class: PARTIAL
intent: COHESION.md:62-92 (folio: hover a shingle) "Interactions".
code: shingle swell + name plate: facet/src/folio/shingles.rs:19-20; strands module facet/src/data/strands.rs (218 lines) is not mounted.
missing/unknown: no strands between related shingles (needs item relations; the package page reads only outline), no "backend-library x4" exits, no relation-count peek "N relations"; peek after 280 ms replaced by cards; grepped desktop for data::strands: 0 hits.

### Hover a region lights the shelf module row (and vice versa); module lens after 280 ms
surface: package page
class: NOT-BUILT
intent: COHESION.md:62-92 (folio: hover a region).
code: none found: the region reads itself in the foot per shingles.rs doc ("reads itself in the foot"); on the package intro the sidebar deliberately does not list modules (side/outline.rs `intro_rows`), so there is no shelf module row to light; no module lens.
missing/unknown: contradiction between the contextual sidebar rule (no modules on the intro) and COHESION's region<->shelf twin; unresolved.

### Keyboard: arrows walk shingles in reading order, Tab moves between regions, ↵ opens, Space peeks, Back pulses the shingle you left
surface: keys/focus
class: PARTIAL
intent: COHESION.md:62-92 (folio: keyboard).
code: regions are host targets walked with j/k (shingles.rs doc; bodies/package/target.rs 138 lines, PageTarget); Back restores focus via ctx.targets.left_by (package.rs:118-123 reopens the module, folio.rs `reopen`).
missing/unknown: arrow keys inside the map, Tab between regions, Space peek on a shingle, the "pulses once" landing: none found (grepped folio/shingles.rs for arrow handling, "pulse"); crest cells, features bar, ticker and berg blocks are not keyboard targets (production-readiness.md:179 R-Folio D7a); Esc does not fold an open module (D7b).

### Module opens into cards with badges (no code blocks); modules over ~14 names get a dedicated view
surface: package page
class: BUILT-REAL
intent: W-Folio.md phase A; owner: "no more dumb code blocks... use badges/tags".
code: facet/src/folio/cards.rs, module.rs (dedicated view when > BIG=14, bodies/package/data.rs:25), marks/badges/{rust,other,glyph,view}.rs (signature -> typed badges), tests marks/badges/tests.rs.
missing/unknown: badge grammar is unit-tested on real signatures; "other.rs" covers non-Rust shapes only from signatures the source scanner supplies — for non-Rust packages the folio's module data (source_facts) is Rust-only so cards for TS/Python/Go carry no signature/summary/badges. module open/close are cuts, not motion (production-readiness.md:229 D11).

### Shingles fly to their cards' marks (CARRY)
surface: motion/transitions
class: PARTIAL
intent: W-Folio.md phase C; COHESION.md:164-172 (Browse -> package film) Browse -> package film.
code: facet/src/folio/flight.rs (227 lines), used at bodies/package/folio.rs:424,481.
missing/unknown: there is no FOLIO-C.md checkpoint (the lane died at the 08:55 rate limit; ls .local/lanes/wave6/folio shows only FOLIO-A/B); reflow 800-2560 and contrast>=3:1 acceptance was never reported; production-readiness.md:237 still lists it "planned but not built".

### Features bar (switches with lock animation)
surface: package page
class: BUILT-REAL
intent: W-Folio.md phase B; owner brief.
code: facet/src/folio/features.rs (482 lines), data::features (bodies/package/data.rs:641).
missing/unknown: features come from the Cargo manifest only; "reading aid, changes nothing on disk" — no affordance to actually pull the switch into a project (ties to the OPEN "Add means which?" question); feature chips overflow the window at phone widths (production-readiness.md:230).

### Heads-up stack and sheet (build script, proc macro, unsafe, process, network, files, env, C calls; file:line evidence)
surface: package page
class: PARTIAL
intent: MOMENTS.md:21-26 (fact 5: Heads-up) fact 5.
code: facet/src/folio/heads.rs; findings from model/source_facts/scan.rs (Cargo/Rust text scanning).
missing/unknown: source scan is Rust-only; heads-up tile is empty apart from its icon on some packages (production-readiness.md:217); the stack is unreadable at rest (chip covers half the previous glyph, D5); advisories go "honestly labelled while no feed is configured" — always the case here.

### Advisories (RustSec / OSV / GHSA) on the package page
surface: package page
class: OPEN
intent: MOMENTS.md:55 (open question 1: advisories) open question 1; production-readiness.md:149-152.
code: state machine desktop/src/shell/bodies/package/data.rs:405-455 (Found / Clear / Unknown with Silence::{NoFeed, Yours, NotRead}); the advisory crate is wired in crates/advisory (browse S1).
missing/unknown: no feed is configured or shipped; the owner has not answered whether to clone rustsec/advisory-db into .local/; the advisory comb marks ("the comb would mark advisory dates and affected ranges", MOMENTS.md:55 (open question 1: advisories)) are not built.

### Downloads and popularity
surface: package page
class: OPEN
intent: MOMENTS.md:57 (open question 3: downloads) open question 3 ("fetch-on-open from crates.io, or is 'used by N in your library' the honest local proxy?").
code: none found: grepped "downloads" in desktop/src/shell and facet/src/folio: model/pages/package.rs carries `downloads` in the record, no surface draws it.
missing/unknown: undecided; also gui-plan 6.2 rule 6 (usage secondary).

### About: README's first paragraphs, every package it names is a preview link
surface: package page
class: PARTIAL
intent: MOMENTS.md:27 (About) item 3, "Progressive disclosure is a primitive".
code: desktop/src/shell/bodies/package.rs:308-345 renders README blocks as plain words (headings, paragraphs, code, bullets) capped at 24 blocks; links are stripped (`clean_inline`).
missing/unknown: no links at all, so no preview links; link-reference paragraphs print as raw URLs; clips at 480 px (production-readiness.md:220-223).

### Progressive-disclosure previews (every package name a preview link; cards chain beside one another joined by a strand; Gwern/Wikipedia popups)
surface: overlays/popovers/hover
class: PARTIAL
intent: MOMENTS.md:16 (progressive disclosure).
code: chaining peeks exist in the float layer (facet/src/overlay/float.rs, model.rs "chain a child (three deep, crumb, connector)"; desktop/src/shell/peeks.rs builds the package card from the dossier); the hero dependency line's chips are peeks/links (facet/src/marks/deps.rs).
missing/unknown: package names in the README, "Used by" rows and the Library are not peek links; the chained card is not joined by a strand; the Rests-on tree does not unfold inline a layer at a time (no Rests-on section exists on the page).

### Hop: clicking a package name grows it into the title; the package you left flies to its row, ringed as "from"; jump bar keeps the trail as a sentence
surface: motion/transitions
class: PARTIAL
intent: MOMENTS.md:31 (Hop) "Hop".
code: shared title keys exist for symbol hops (bodies/symbol.rs:158 title shared_with; ctx.arrived_from ringing, bodies/mod.rs:118-119); dependency mark -> package navigates (package.rs:291-296).
missing/unknown: package -> package hop has no shared-element title flight; "from" ring is a symbol-page-only feature; the jump bar shows a path (`toml > de > from_str`), not the relation sentence `toml -> toml_edit <- cargo` (jump.rs segments only).

### Start line: three landmarks by fan-in, each shingle-mark + name + gloss (and gui-plan "Start here" strip / T fly it)
surface: package page
class: SUPERSEDED
intent: gui-plan.md:534-539 §8.2 "On the package page ... a Start here strip"; COHESION.md:62-70 (folio item 4, the Start line) item 4 "Start line: three landmarks by fan-in".
code: deliberately removed: apps/desktop/src/shell/bodies/package.rs:1-6 ("prototype fixture-world rankings never recommend a starting declaration"); tests assert its absence (bodies/package/tests.rs:115, shell/anatomy_tests.rs:333); apps/desktop/journeys/J1.journey:4-8 documents the removal.
missing/unknown: the replacement (a tour computed from index evidence, W-World) is unbuilt: production-readiness.md:91 (graph data). J1's "Start here stops" checkpoint no longer exists, so gui-plan.md:229 J1 is not the shipped J1.

### Locator (territory folded to 150x60 beside the symbol hero; click to unfold)
surface: symbol page
class: NOT-BUILT
intent: COHESION.md:49-59 (the geometry: locator), 125-127.
code: none found: grepped "locator" across desktop/src and facet/src: only package-locator ID uses.
missing/unknown: no locator on the symbol page, no hover-a-row-lights-its-shingle, and no Package<->Symbol unfold transition (COHESION transitions table rows 1-2).

### Object model: Module = "region" mark, Item = "shingle", Relation = "strand", one state grammar (rest / yours / lit / current / hover / changed / unread)
surface: theming/density/contrast
class: PARTIAL
intent: COHESION.md:28-48 (object model and state grammar).
code: shingles for the package page (folio/shingles.rs) and sidebar rows use item marks; mint/amber/coral states in side/state.rs; "changed in a release" states in folio shingles.
missing/unknown: strands appear only in the graph; the symbol page's rail/fork rows carry item marks from the kind icon set, not the 10 px chamfered shingle mark; "unread = dashed outline" is used for undeclared types in symbol derive, and for dep chips (marks/deps.rs) but there is no single component owning the state grammar; hover "swell x1.55 + lift 3 px, neighbours lift 1.5" (facet::hover) is not mounted on shingles (grepped facet/src/folio/shingles.rs for facet::hover: none; hover grammar used by desktop only at bodies/symbol/host.rs:16).

======== SYMBOL PAGE (MOMENTS §"simple form: what ships", W-Sym6; gui-plan §8.3; DIRECTION v5 §4) ========

### Simple-form page skeleton (header, the call, docs, if it fails, what it is, what you can do, in your workspace, context rail)
surface: symbol page
class: BUILT-REAL
intent: MOMENTS.md:205-248 (symbol page, simple form); W-Sym6.md "The page, top to bottom".
code: desktop/src/shell/bodies/symbol.rs:36-98 body() -> facet::anatomy::symbol::{compile, with_uses, page}; facet/src/anatomy/symbol/{page,body,call,side,workspace,card,derive/*}.rs; main column 860 + rail 280, rail under content below 1100 (page.rs:1-6, layout.rs).
missing/unknown: real-index fidelity gaps below; the 7 board pages were captured only on the board-model path (anatomy/symbol/board.rs) and the fixture set; no journey visits a symbol page in a non-Rust language.

### The call: ports with joints (required dot, optional hollow, rest double dot), receiver square by effect
surface: symbol page
class: BUILT-REAL
intent: W-Sym6.md item 2.
code: facet/src/anatomy/symbol/call.rs, derive/callable.rs (Port { name, joint, ty, default, note, options }, callable.rs:396), receiver effect mapped in desktop bodies/symbol/facts.rs:33-41.
missing/unknown: a receiver's effect (reads/changes/uses up) is only as reliable as the index's Receiver classification (Receiver::Unknown maps to Unknown); shown correct on toml/serde_json fixtures only.

### Outcomes at the rail's end: gives / or nothing / or fails-throws-raises-rejects / later / gives each
surface: symbol page
class: BUILT-REAL
intent: W-Sym6.md outcomes bullets; MOMENTS.md:222-232 (the grammar: outcomes).
code: facet/src/anatomy/symbol/call.rs:1-9, ink.rs glyphs, derive/callable.rs (undeclared-type handling at 433-544).
missing/unknown: "the Gives row drops an Option/Result output's written type" (production-readiness.md:255); async/Promise/Future ("later") and iterator ("gives each") are derived from signature text only — no real Python/TS/Go/Java page has been captured through the desktop harness (harness has TS (zod) and Go (pflag) roots only and both are named refusals: production-readiness.md:138-143).

### Types plain word first, written type quieter; generic parameter as violet pill; dotted underline for undeclared types
surface: symbol page
class: BUILT-REAL
intent: MOMENTS.md:222-248 (the grammar: types); W-Sym6.md.
code: facet/src/anatomy/symbol/derive/words.rs (829 lines), derive/text.rs, view.rs:141 `dotted()`, derive/rail.rs:34 "declares no types here... a dotted underline marks each one".
missing/unknown: on phone widths the written type runs past the plate edge and the case count overlaps it (production-readiness.md:244 D1).

### Options object collapsed ("4 options, 2 change what it gives")
surface: symbol page
class: PARTIAL
intent: W-Sym6.md item 2 "Options object (JavaScript opt, Python **kwargs when documented)".
code: derive/callable.rs:205,385 (Port.options); call.rs unfolds sub-rows.
missing/unknown: needs documented option shapes; only covered by board fixtures (board.rs `&opts=opt` state); an index that records JS/Python docs with option tables has never been run through the desktop.

### Docs with folds (Example, N more paragraphs), # Errors not repeated
surface: symbol page
class: BUILT-REAL
intent: W-Sym6.md item 3.
code: facet/src/anatomy/symbol/body.rs, derive/docs.rs, desktop bodies/symbol/facts.rs:102-150 blocks() (paragraphs, code, links as [label](coordinate)).
missing/unknown: "code inside doc links prints its backticks" (production-readiness.md:255); Example fences only when the Rust doc-comment lowering keeps them (symbol-page.md:§5 "wired but unconfirmed").

### If it fails: error type, when prose, kinds as coral ✕, impossible kinds dimmed with one line why
surface: symbol page
class: PARTIAL
intent: W-Sym6.md item 4; MOMENTS.md "An error type gives the error's kinds"; gui-plan.md:558-570 "How it fails".
code: body.rs:131-150 (dim = kind.impossible.is_some()); derive from # Errors section; desktop facts.rs sections.
missing/unknown: the gui-plan makers/readers model ("Io, Spawn, DaemonExited or StartTimeout", "MissingExecutable through locald_executable", "N calls in this world can fail with it") is NOT on this page: facet/src/semantics/fails.rs (508 lines, tested) is referenced by no desktop file (grepped semantics::fails in desktop/src and anatomy/symbol: none); J5 (failure pages, maker link) has no journey. The kinds come from the doc text, not from tracing variant mentions in bodies.

### What it is: enum fork (loop mark for self-holding case), struct bracket ("you read it" in mint), trait "What you write"
surface: symbol page
class: PARTIAL
intent: W-Sym6.md item 5; DIRECTION v5 §4 specimen (fork/bracket/contract).
code: facet/src/anatomy/symbol/body.rs, view.rs ("you read it" string), desktop facts.rs:180-190 made_of/implements.
missing/unknown: enum case rows carry bare numbers with no words (production-readiness.md:247 D5); "83 % derive" needs a derived flag the index lacks (line 258); Python `Literal`/`Enum`, TS unions, Java sealed, Go struct fields as forks/brackets: kind mapping in facts.rs:12-25 folds Class/Struct/Union to Struct and Interface/Trait to Trait, Variable to Constant; no union/sealed/Literal detection.

### v5 specimens: value = stone with its value printed; module = tray of kind marks; contract satisfiers as "486 types do it · 12 in yours" (Go computed fan)
surface: symbol page
class: SUPERSEDED
intent: DIRECTION.md:97-105 (the specimen) §4 "The specimen".
code: the v5 drawn page lives in facet/src/anatomy/{page,plan,fork,holds,pipe,contract}.rs (gallery-only; grepped desktop for anatomy::page/plan/fork/holds/pipe: only page::{title_key,words_w,Door,Doors,Fold} and history::*), replaced by the simple form.
missing/unknown: constants render as a bare row with a value only if the signature carries it; no stone; a package's modules are shingles not trays; the Implementors line (Facts.implementors: total/crates, facts.rs:195) exists but "12 in your code" and Go implicit satisfaction as the headline are not built.

### What you can do with it: verb groups (makes / reads / changes / uses up), six shown then "N more", From collapsed
surface: symbol page
class: BUILT-REAL
intent: W-Sym6.md item 6.
code: body.rs, derive/shape.rs:200 (From/overloads become one row), known.rs:214.
missing/unknown: ordering "methods your workspace uses come first with a mint dot and count" depends on references; the look-alike fold ("visit_... one of bool, i8, ... 22 of them", gui-plan.md:611) exists only in the old semantics path (facet/src/semantics/members.rs:8) not the simple form.

### In your workspace: package picker, 12 verb chips, include tests, groups per package, click opens file:line in the editor
surface: symbol page
class: PARTIAL
intent: W-Sym6.md item 7.
code: facet/src/anatomy/symbol/workspace.rs, derive/uses.rs (607 lines), desktop bodies/symbol/uses.rs (sites from page.references + UseLine text), host.rs:108 dispatches Intent::OpenSource; runtime/workspace_lines.rs reads the line from your file off the UI thread.
missing/unknown: verbs are line-local (production-readiness.md:248: Value reads "305 places (names 170...)" where the board has reads 368 / makes 47 / changes 1); "matched by name rather than resolved" marker exists (view.rs `approximate`) but the owner-side resolution level is the limit; the configured editor is never passed (runtime/ui_graph.rs:240 calls editor::open(..., None, ...)) and Settings has no editor page.

### Generic pill card: role in words / It must be (each bound with meaning) / Your workspace chooses (click filters) / Elsewhere in the registry
surface: overlays/popovers/hover
class: PARTIAL
intent: W-Sym6.md item 9; MOMENTS.md:132-140 (generics as threads) generics as threads.
code: facet/src/anatomy/symbol/card.rs:55,191-194 (elsewhere line drawn if data.elsewhere non-empty), derive/callable.rs bounds.
missing/unknown: "Elsewhere in the registry" is never drawn from a real source (production-readiness.md:257; there is no registry corpus to count "about 250 distinct types were chosen for from_str's T"); bound meanings ("Deserialize: can be read by serde") come from a fixed vocabulary in derive/known.rs (Rust-leaning).

### Error-type card lists the error's kinds
surface: overlays/popovers/hover
class: PARTIAL
intent: W-Sym6.md item 9 last bullet.
code: card.rs "an error type says which kinds it tells you".
missing/unknown: same source limit as If it fails (doc text, not variant tracing).

### Rail: Source (file:line opens), Your packages that use it, Next to it, It can, Across releases, How we know
surface: symbol page
class: PARTIAL
intent: W-Sym6.md item 8.
code: facet/src/anatomy/symbol/side.rs:59-171 (all six blocks exist).
missing/unknown: "Next to it" shows name tails and repeats with NO outcome glyphs (desktop bodies/symbol/facts.rs:197 builds Beside { signature: None }); "Across releases" needs release data (fixture: toml/smallvec) so it is hidden elsewhere (facts.rs: only when read.len() > 1); "How we know" only for undeclared-type languages; "It can" chips are Rust capability chips; glance at 'derive/rail.rs' shows unknown traits fall back to their name.

### Family / sibling matrix ("parse / safeParse x now / later"; hover a cell and the plate becomes that sibling)
surface: symbol page
class: SUPERSEDED
intent: MOMENTS.md:151-160 (the family) lab section 2; reduced to "Next to it" in the simple form (MOMENTS.md:210-218 (simple form: Next to it)).
code: rail's "Next to it" (side.rs:113-137); sibling rows from page.outline.siblings (facts.rs:191-199).
missing/unknown: the keyed-FLIP plate morph between siblings is not built; siblings are by proximity in the file outline, not by "what differs" axes (`differs` word comes from derive).

### Lab instruments dropped by the owner: crash statistics, word trees (the ways), ledger, reach matrix, knobs, the crowd (8.3k implementors)
surface: symbol page
class: SUPERSEDED
intent: MOMENTS.md:210-218 (what we took / what we dropped) "What we dropped".
code: none in the product by decision; lab remains at Nudox-Design-System/v6/moments/Lab.html; the crowd's data (Implementors) survives as one number.
missing/unknown: nothing to build unless the owner reverses ("You're being too ambitious").

### Getting one / Calling it (rails from what you have; ⌥ spells the code)
surface: symbol page
class: SUPERSEDED
intent: gui-plan.md:574-592 §8.3; DIRECTION.md:112 (reading order item 3: Getting one).
code: engine built (facet/src/semantics/recipes.rs 1591 lines, recipes/chain.rs) and used by the graph's chains (facet/src/graph/discovery.rs); no reference from the simple page or desktop bodies (grepped "recipes" in desktop/src: only fixture_world tables).
missing/unknown: the answer to "how do I get one of these" is not on any symbol page; only reachable by searching by shape inside the fixture-world graph.

### Its cousins (same idea in another package; to it / from it roads)
surface: symbol page
class: NOT-BUILT
intent: gui-plan.md:593-605 §8.3.
code: none found: grepped "cousin" case-insensitively across desktop/src and facet/src (excluding semantics/tests/fixtures): zero hits; recipes.convert exists (facet/src/semantics/recipes.rs) but nothing calls it for cousins.
missing/unknown: the classic "turn a toml Value into a serde_json Value" question is unanswered anywhere in the product.

### The prism (relations sentence / groups) and "made by / used by" rows on the page
surface: symbol page
class: SUPERSEDED
intent: gui-plan.md:606-608 static prism; symbol-page.md brief verb-rows.
code: facet/src/anatomy/prism.rs (364 lines) and semantics/relations.rs are not called from the desktop (grepped anatomy::prism in desktop/src: none); the graph draws its own prism (facet/src/graph/prism.rs).
missing/unknown: relations reach the simple page only as the rail's "Next to it" and the uses list; the door "G" into the graph exists as a key (keys.rs:208) not as an in-page drawing.

### Does / In use / can line (gui-plan §8.3) and the v5 reading order (What can go wrong tree, Who uses it strip with code excerpt on hover, What changed strip along the comb, Its own words last)
surface: symbol page
class: SUPERSEDED
intent: DIRECTION.md:109-118 (reading order) reading order 1-8.
code: replaced by the simple form's sections; the what-changed strip is the upgrade section (bodies/symbol.rs:172) and the rail's Across releases; "Who uses it: yours first, strip of real call sites with the code excerpt on hover" became the workspace list.
missing/unknown: "Its own words: the docs, last, typeset well" — docs sit third (after the call) in the shipped page; nobody recorded that ordering decision; the "graph is the page zoomed out" left/right stubs in the margins (DIRECTION.md:120-123 (the page is the node, opened) "The page is the node, opened") are unbuilt.

### Symbol history strip ("Across releases" dots) and history tooltip
surface: releases/time travel/upgrade
class: BUILT-FIXTURE
intent: gui-plan.md §8.4; DIRECTION.md:117 (reading order 7: What changed) item 7.
code: desktop/src/shell/bodies/symbol/history.rs (61 lines) via fixture_releases; facet/src/anatomy/history.rs.
missing/unknown: toml/smallvec only.

### Symbol page for a symbol at another release (route.at)
surface: releases/time travel/upgrade
class: NOT-BUILT
intent: gui-plan.md:625; W-Acquire.md §2.
code: desktop/src/runtime/store.rs:282 Unread::ReleaseNotHere -> "Release X is not in this index; only your working copy is. Esc returns to it." (desktop/src/shell/bodies/mod.rs:129-131). The uncommitted working-tree change (runtime/store.rs release_tree) lets a registry root be read at another release only when that release's tree is already in the library.
missing/unknown: no on-demand indexing of an earlier release (GAPS G13); J8 not written.

### Hover cards timing: 120 ms in, stays while hovered, 160 ms out; folds unfold with a height spring
surface: overlays/popovers/hover
class: PARTIAL
intent: W-Sym6.md phase C.
code: facet/src/overlay/float.rs:148 QUICK_REST 120 ms; card.rs doc.
missing/unknown: SYM6-C (motion and fit) checkpoint was never written (.local/lanes/wave6/sym6 has A and B only per production-readiness.md:243 "SYM6-B (08:36) addressed some"); hover/cursor feedback missing on the Example fold, "N more in workspace" and the rail source link (D6); layout changes are cuts (rail disappears in one frame, the SYMBOL_RAIL edge at 1344 moves 1084 px in one frame: D2/FLUID-C).

### Symbol page icons and marks match the board (function mark vs disclosure caret; enum diamond)
surface: theming/density/contrast
class: PARTIAL
intent: W-Sym6.md "colors, shapes and icons"; DIRECTION.md:168-175 (Shape) "Shape: chamfered plates open, round stones values, diamonds contracts".
code: facet/src/icons/* kind set; production-readiness.md:252 D9: the function kind mark reads as a disclosure caret, the enum mark doesn't match the board's diamond.
missing/unknown: the shape law (plates/stones/diamonds) is not applied uniformly: kinds are gems by family (gui-plan rule 3) while DIRECTION v5 wants shape = capability class; the two rules were never reconciled.

### Contrast: Glacier chip 4.23:1, ≥3:1 on marks and ≥4.5:1 on text in both themes
surface: theming/density/contrast
class: PARTIAL
intent: W-Sym6.md phase C; gui-plan.md:213 lints.
code: facet/src/gallery/lint.rs, probe rules; production-readiness.md:251 D8 (quiet 'imports' chip dims by opacity).
missing/unknown: full `verify` gate never gave a verdict this wave (production-readiness.md:303); Contrast::High mode coverage per page unverified.

======== MOTION / TRANSITIONS (DIRECTION v5 §3; transitions.md; COHESION transitions table) ========

### Open / Close: the row becomes the page (plate opens; old page clipped away; back reverses; "where you were" tint)
surface: motion/transitions
class: BUILT-REAL
intent: DIRECTION.md:73 (the row becomes the page) "The row becomes the page"; transitions.md §5 T1.
code: desktop/src/shell/reader.rs:1-32 (plates, never fades; Verb::{Open, Close, Fold, Unfold} at :118-139; CARRY spring); tests in reader.rs `transit_tests`; .local/lanes/wave6/transitions/CP1.md.
missing/unknown: only symbol titles are shared elements (kit.rs:319 shared_id takes a SymbolRef): a package hero name, a Library name, a Find row are keyed by nothing, so "the clicked name travels and grows into the new title" holds for declaration pages only (grepped "shared" in bodies/package.rs, package/folio.rs, bodies/orbit.rs: none). Across (settings, compare, symbol->symbol) plays Open/Close by history where Push was designed (CP2.md:120).

### Push along the relation's stroke (in-relations enter from the left, out-relations from the right)
surface: motion/transitions
class: NOT-BUILT
intent: transitions.md §2 table "Related: Push"; §5 T5; DIRECTION.md:65-71 (spatial map) "Related is along the relation's stroke".
code: none found: grepped "Push" in facet/src/motion/*.rs (only a test struct `Pushed`) and desktop/src/shell/reader.rs (Verb has Open/Close/Fold/Unfold only, reader.rs:118-139).
missing/unknown: sibling hops, dependency hops, Settings and Compare all use Way::Across which plays Open/Close by history.

### Peel: page <-> code anchored on the declaration line
surface: motion/transitions
class: NOT-BUILT
intent: transitions.md §2 "Code: Peel"; §5 T5.
code: reader.rs:22 "A view switch cuts (the Peel is a later slice)"; Way::View is a cut (reader.rs:709).
missing/unknown: ⌘. and S are cuts; the code view is reached without spatial continuity.

### Reel (jump-bar segments, siblings) and Odometer (counts), theme Sweep
surface: motion/transitions
class: NOT-BUILT
intent: transitions.md §5 T5.
code: none found: grepped "Reel", "Odometer", "Sweep" (theme) in facet/src/motion and desktop/src/shell: no hits except unrelated `controls/sweep.rs` (facet sweep hover on buttons).
missing/unknown: counts change by cut; theme change is a cut; the jump bar's segment menu (titlebar.rs:689-740) has no reel.

### FLIP in the desktop: Find narrowing, shelf roll, text-size Anchor, Pin travels into the column
surface: motion/transitions
class: PARTIAL
intent: transitions.md §5 T3; D4/D5/D6.
code: Flow is mounted for the Library ring (bodies/orbit.rs:127-149), the titlebar modes (titlebar.rs:11-16) and sidebar rows; facet/src/motion/flow.rs (1955 lines) and presence.rs (1728) exist.
missing/unknown: CP2.md:116-121 lists T3 unstarted: rewrite flow/presence to the three-phase rules (baseline flow-list 11 overlap runs, flow-reflow 57), Find narrowing re-lay (70 overlap runs), the shelf roll (shelf rows cross-fade at side/view.rs:521 `.hover(opacity)` and root.rs `shelf-w`), text-size Anchor, pin travel (float-pin). No record they were done; 'In-flight Flow chips cross each other after a fast step' (production-readiness.md:188).

### The fade purge (nothing crossfades; reduced motion is a cut plus a 1.2 s mark)
surface: motion/transitions
class: PARTIAL
intent: DIRECTION.md:83-84 (durations, reduced motion); transitions.md §5 T4 (D7, D9).
code: route transitions are plates (reader.rs); remaining fades: facet/src/motion/flight.rs:73 CROSSFADE 120 ms + State::Fading (reduced-motion graph flights), facet/src/motion/lab.rs:909 `crossfaded_name` (transitions.md said "Delete crossfaded_name"), titlebar compaction uses `div().opacity(fade)` (desktop/src/shell/titlebar.rs:152,171), CP2.md:122 baseline for toast, dialog, comb, controls, presence, float, rose.
missing/unknown: `motion::mark(key, hue)` (1.2 s mark then 160 ms settle) does not exist (grepped `fn mark` and "1_200"/"1.2 s" in facet/src/motion: only unrelated lab timings). Reduced motion in the reader is a cut with only the Close row tint.

### Semantic zoom page <-> graph (sections fold into the node's edges; stubs slide to ports; camera pulls back)
surface: motion/transitions
class: PARTIAL
intent: DIRECTION.md:74-78 (semantic zoom); COHESION.md:104-115 (transitions table: Symbol -> Graph); transitions.md §5 T2.
code: Fold/Unfold verbs in reader.rs:1013-1051, 1469-1525; the node is followed every frame (follow_node); CP2.md:22.
missing/unknown: CP2.md:109-115 leftovers: stubs travelling to their ports (needs facet::anatomy::page::Anchors from the retired v5 page, so no longer applicable to the simple page: the page has no stubs to fold), `GraphView::enter_from` camera pull-back (graph enters by flying in), title -> node label (needs R1 shared package/symbol title, done for symbols), prism labels at 60-70 % ink at rest (label LOD law); "the page's sections fold into the node's edges" was replaced by "body folds up, plate closes into the node".

### Graph label LOD legible-or-absent
surface: graph/world
class: PARTIAL
intent: transitions.md D8; DIRECTION.md law 2.
code: facet/src/graph/draw.rs label LOD three hunks and constants (CP2.md:151); prism labels still fade in at 1056-1136 ms per CP2.md:108.
missing/unknown: gate `legibility --catalog` as a standing RULES gate: facet/src/gallery/cli.rs:25,535 implements `--catalog FILE`; no record it is wired into `verify` (production-readiness.md:303: the full verify gate never gave a verdict this wave).

### Transition catalog as a test
surface: motion/transitions
class: PARTIAL
intent: DIRECTION.md:86-88 (the transition catalog) "Enumerate every transition... The catalog is a test."
code: facet/src/gallery/legible.rs (884 lines), film scripts under .local/lanes/wave6/transitions/film; reader.rs `transit_ledger` ignored test.
missing/unknown: D10 films (keyboard peek/pin, graph tour T, "to world" Esc, jump menus, drag reorder, desktop toasts and dialogs, reduced-motion variants of every film) not exercised; the catalog file itself lives in .local (git-ignored), not in the repo.

### One driver (CARRY spring, response 0.28 s) and Input responds in the same frame
surface: motion/transitions
class: BUILT-REAL
intent: transitions.md §2; DIRECTION.md:82 (input responds in the same frame).
code: facet/src/motion/carry.rs (172 lines); durations micro 90/quick 160/std 240/emph 380/scene 620 per gui-plan.md:113 in facet/src/motion tokens (tokens.rs motion).
missing/unknown: the 120-240 ms band in DIRECTION vs 380/620 ms tokens are both in the tree; hero title shared-element runs 460 ms (bodies/symbol.rs:158 `Duration::from_millis(460)`, ".timing(460ms, GLIDE)") which exceeds DIRECTION's 240 ms ceiling and is a survivor of the gui-plan.md:623 "page rises in (460 ms)".

### Ambient Pulse (ground twinkle, running bevel, flowing strands, working gem)
surface: motion/transitions
class: PARTIAL
intent: gui-plan.md:108-110 §2.3 Pulse; §2.4 Ground "twinkles on the pulse clock".
code: facet/src/motion/pulse.rs leased by controls/button.rs:516 (busy), data/progress.rs:263,496 (working gem/seam), data/rose.rs:508; desktop mounts the ground at shell/root.rs:1432 as `ground()` with phase 0.0 and never drives its phase (paint/ground.rs:267-282, 287-291 "drive it from the pulse clock").
missing/unknown: the faceted ground is static in the app; twinkle exists only in the gallery. No journey or storm checks the pulse's idle cost in the desktop.

### Stroke grammar: dashed = inferred / matched by path, dotted = optional, solid = compiler-verified
surface: theming/density/contrast
class: PARTIAL
intent: DIRECTION.md:160-167 (Stroke) §5 Stroke; "every count names its tier".
code: facet/src/tokens.rs:665-677 stroke::{HAIR, RELATION, FOCUS, INFERRED, OPTIONAL, PRUNED}; symbol page dotted underline (view.rs:141), workspace "approximate" marker.
missing/unknown: uses counts in the sidebar and package page never name their tier (approximate vs resolved) — glyph.rs mint counts carry no tier mark; package-browsing D2 "at least N places - matched by path" is unbuilt.

### Kind hues (types teal, callables periwinkle, contracts violet, modules slate, values neutral; amber = caution only; coral = failure only; mint = yours)
surface: theming/density/contrast
class: PARTIAL
intent: DIRECTION.md:151-158 (Colour roles).
code: facet/src/tokens.rs Palette families; folio/shingles.rs:5-8 uses exactly this mapping.
missing/unknown: the amber-means-caution-only rule is violated by "changed" state (COHESION.md:28-48 (state grammar: changed = amber) uses amber for changed signature, and the sidebar draws amber for changes); DIRECTION and COHESION disagree (caution vs change) and nobody reconciled them; the Library uses no kind hue.

### Type scale "one scale, no exceptions" (Display 40/44, Lede 19/28, Section 13/16 600, Body 14/22, Mono 13/20, Label 12/16)
surface: theming/density/contrast
class: PARTIAL
intent: DIRECTION.md:136-145 (Type scale) §5.
code: facet/src/tokens.rs:567-620 ty:: has 20 roles (HERO 44/46, DISPLAY 30/36, PROSE 14.5/23, BODY 13.5/21, ROW 13/18, MONO_ROW 12.5/18, MONO_SMALL 11.5/16, LEDE 19/28, LABEL 10.5/14 ...).
missing/unknown: the scale in DIRECTION was never adopted as the token set; there is no 13/16 600 "section heading with 0.02em tracking + kind-hued gutter mark" role (anatomy/page/ink.rs:422 has a gutter mark for the retired drawn page). 8 px grid, 48 px section gap, 16 px group gap, 32 px row exist as tokens::rhythm (tokens.rs:640-672) but the wave-6 compactness laws use ROW_PITCH 24 and ROW_TIGHT 28.

======== HOVER / PEEK / LENS / LADDER / X-RAY (gui-plan §6, DIRECTION §3) ========

### One hover grammar (facet::hover: ink rises, bevel stroke, related occurrences underline, peek at 350 ms)
surface: overlays/popovers/hover
class: PARTIAL
intent: DIRECTION.md:89-93 (One hover grammar); "Ad hoc hover styles are deleted."
code: facet/src/hover.rs (413 lines); desktop mounts it once: desktop/src/shell/bodies/symbol/host.rs:16 (Subject). Ad hoc hover backgrounds remain: desktop/src/shell/ask.rs:422,489, bodies/orbit.rs:289, onboard/failure.rs:181, side/view.rs:156,402,521 (`.hover(|style| style.bg(palette.tint))`), plus 19 `.hover(` sites in non-gallery facet code.
missing/unknown: package page (shingles, cards, ticker), the sidebar, Library, Find and graph nodes do not use it; the peek delay is 350 ms in the doc, 120 ms QUICK_REST in float.rs:148, 280 ms in COHESION.md:28-48 (hover row), 62-92 (folio hover); three numbers, no recorded decision.

### Peek (symbol card) anatomy, chained three deep with a crumb, pinned into a column or a ⌘P stack, follow with ⌥→
surface: overlays/popovers/hover
class: PARTIAL
intent: gui-plan.md:298-300, 391-393.
code: facet/src/overlay/peek.rs (623), float.rs/model.rs (chain, pin_top, pinned_column); desktop/src/shell/peeks.rs builds package/declaration cards; pins.rs mounts the column.
missing/unknown: ⌘P stack and ⌥→ follow: none found (grepped "⌘P", "alt-right", "follow" in overlay/float*.rs, keys.rs); the pin column is at 1900+ only; peek card rule 7 "cards are short" unverified for package/file/version peeks (gui-plan v4 Peeks.png lists package/file/version peeks: desktop peeks.rs builds symbol and package cards only — grepped "file peek", "version peek": none).

### Lens (aggregate breakdown card): release-comb lens by family with "touches your code" first; module tile lens; mosaic stone peek; language-comb lens
surface: overlays/popovers/hover
class: GALLERY-ONLY
intent: gui-plan.md:275 Lenses.png; 6.1 "Lens (aggregate breakdown)".
code: facet/src/overlay/lens.rs (465 lines) used only by facet::data marks (door.rs:27) and gallery; desktop calls neither (grepped desktop/src for overlay::lens, lens_card: none).
missing/unknown: the release-comb lens by family, module-tile lens and language-comb lens reach no desktop surface; the package page's ticker has its own label not a lens card.

### The ladder (Mark -> Tag -> Row -> Card) and ⌥ x-ray raising everything one rung
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:293-298; Ladder.png.
code: facet/src/measure.rs Rung/Needs; facet/src/data/comb.rs:329-390, compass, facts, caps, progress all rung-aware; desktop reveal.rs sets Reveal.xray (root.rs:452-465).
missing/unknown: the components that implement rungs are the unmounted data marks (comb, compass, facts, caps, spell, mosaic); in the desktop only the symbol page anatomy, dep_line and the release section honour xray, so ⌥ visibly changes little on the package page, Library and sidebar.

### Compass mark on every row (relational shape 16-30 px: up is / down made of / left from / right to)
surface: overlays/popovers/hover
class: GALLERY-ONLY
intent: gui-plan.md:299-300.
code: facet/src/data/compass.rs (919 lines); desktop uses none (grepped compass_row, compass_bar, Compass in desktop/src: 0). gui-plan 6.2 rule 2 additionally says compass appears on hover/⌥ only.
missing/unknown: whether a compass belongs on rows at all after the sidebar/state-glyph design took its place is undecided.

### Rose (up is / down made of / left from / right to) on the page
surface: symbol page
class: SUPERSEDED
intent: gui-plan.md:144, 177, 422 ("The rose is retired").
code: facet/src/data/rose.rs (807 lines, pulse-leased) unused by desktop; the read model still computes it (desktop/src/model/pages/symbol.rs:28 `rose: Rose`, runtime/page_mapping.rs:889 `rose()`); the simple page reads page.rose.up / implemented_by for "Implements" and Implementors (bodies/symbol/facts.rs:184-190).
missing/unknown: dead code in facet and a live read-model field named for a retired design; not deleted.

### Mosaic (stones public/new/gone/gate/lit) and stones generally
surface: package page
class: SUPERSEDED
intent: gui-plan.md:140-141; owner "stones awful/unusable, wants the v4 territory shingle map" (memory stones-rejected-shingles-wanted).
code: facet/src/data/mosaic.rs (537 lines) unused by desktop; replaced by folio/shingles.rs; facet::data::territory (830 lines, squarify) is the intermediate version, also unmounted.
missing/unknown: three generations of the module map live in facet (mosaic, territory, folio/shingles); two are dead.

### Density (Comfortable / Compact / Dense) truly changes pages ("dense folds summaries away")
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:274 Density row; Measure::Density.
code: settings.rs:81-97 (control), facet/src/measure.rs (density scaling of rows/roles), shell/facet_sync.rs:40.
missing/unknown: dense mode "folds summaries away" is a data-mark behaviour; the folio's cards and the symbol page's sections have no dense variants recorded; no density sweep in the responsive matrix runs on the desktop (gui-plan §3.6 lists it, harness `matrix` handles widths and scale).

### High contrast mode ("every ink one step stronger, firmer lines and bevels")
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:302-303.
code: Contrast::High threaded via facet_sync.rs:36; Settings control settings.rs:71-79.
missing/unknown: no journey asserts high-contrast frames pass lints; FEEL matrix covers `contrast high` per W-Feel.md but its FEEL-A/B reports are not in the repo (lane files git-ignored).

======== GRAPH / WORLD (gui-plan §8.2, COHESION "Graph: the same page zoomed out", MOMENTS relay, W-World) ========

### The world graph fed by the index (packages, modules, symbols, relations -> World::new)
surface: graph/world
class: BUILT-FIXTURE
intent: PLAN.md:52-54 W-World "projected from the index, laid out once per content hash. It kills 'Graph fixture' and the two universes."; gui-plan.md:652.
code: desktop/src/shell/bodies/graph.rs:1-4,180-186 (`fixture_world::blocking()`), runtime/fixture_world.rs:1-15 (16 MB world.json parsed on a thread, joined to index identities); status text "Laying out the graph fixture…" (graph.rs:1244); errors "This graph fixture has no guided tour of X" (graph.rs:407).
missing/unknown: W-World was queued and never launched (PLAN.md:52-53; not among the 2026-09-29 roster). The path to the file is env!("CARGO_MANIFEST_DIR")/../../Nudox-Design-System/v4/graph (runtime/fixture_world.rs:160), a source-tree path: a bundled Nudox.app has no such folder, so the graph shows a WorldFault. Only packages in the prototype snapshot (serde, serde_json, toml, smallvec, csv, this workspace) appear; your own project never does.

### Graph at a release (comb re-scopes the graph; absent symbols fade; added since your pin glow once)
surface: graph/world
class: NOT-BUILT
intent: gui-plan.md:625.
code: desktop/src/shell/bodies/graph.rs:413 "Graph fixture is pinned; release {} is not re-scoped by this map."; :636,:699 refuse to focus/open a symbol at a viewed release.
missing/unknown: no release model in facet::graph (grepped facet/src/graph for release scoping: none).

### Graph performance: cached layout per content hash, off the UI thread; 120 Hz flights
surface: graph/world
class: PARTIAL
intent: gui-plan.md:438,451; DIRECTION.md:46-54 (world as columns) (world as columns, mmap).
code: facet/src/graph/layout.rs:17,544,958-964 (layouts memoised in-process by content hash); model.rs:355 Csr adjacency in memory.
missing/unknown: no on-disk cache and no mmap'd columnar world (grepped mmap/rkyv/memmap in desktop/src and facet/src: none); the layout is recomputed each process start (graph.rs:182 `layout_of` on a background executor). Budget "graph open <= 100 ms to a laid-out frame" (instant-open.md §1) never measured in release.

### Graph: focus gathers the prism (left comes-from, right goes-into), reach (R), tour (T), find by name/shape, constellation, road/chain hold, where-line, trail
surface: graph/world
class: BUILT-FIXTURE
intent: gui-plan.md:456-541 §8.2.
code: facet/src/graph/{prism,interaction,discovery,road,draw,camera}.rs; keys facet/src/graph/keys.rs; mounted in the desktop at Route::World and View::Graph (bodies/graph.rs).
missing/unknown: all of it runs on the prototype snapshot; J4 (the map: G -> flight -> R -> Esc -> T -> -> ↵) is unwritten; "Below 720 px free width the prism becomes one column" (view width logic still hard-coded at facet/src/graph/scene.rs:834,875 and view.rs:1715,3385 per W-Fluid.md, migration status unrecorded); the graph's find field duplicates Ask (PLAN "Retire the graph's field").

### Package "Start here" tour from index evidence
surface: graph/world
class: NOT-BUILT
intent: gui-plan.md:508-539 tour; PLAN.md:52-54 W-World "brings back the package tour from index evidence".
code: computation exists (facet/src/semantics/tour.rs, 568 lines) and drives T in the graph over the fixture world; the package page strip was deleted (bodies/package.rs:1-6; tests assert absence).
missing/unknown: no tour for any package outside the prototype snapshot; no in-page Start here.

### Graph <- Library: package hulls as territories seen from far away; regions become hulls as you zoom
surface: graph/world
class: NOT-BUILT
intent: COHESION.md:98-103 (Graph: the same page, zoomed out); Package -> Graph transition (COHESION table row 5).
code: none found: the graph's hulls are drawn from world.json layout (facet/src/graph/layout.rs hull), not from package-page regions; no shingle -> node flights (Package -> Graph is Fold on symbol routes only).
missing/unknown: Package -> Graph (G on a package page), Home -> Package shelf arrival, Any -> Query plate transitions from COHESION.md:104-115 (transitions table) are not implemented as described.

### The relay graph (?v=graph): what it rests on left, who uses it right, strands with counts, hop grows chip into hero, arrival "How toml uses toml_edit"
surface: graph/world
class: NOT-BUILT
intent: MOMENTS.md:47-51 (Graph: the relay).
code: none found: grepped "relay", "How toml uses", "arrival through the relation": no code; the world graph is symbol-level not package-level.
missing/unknown: needs package-to-package usage (who-uses-what-of-whom) which the index does not serve.

### Graph is hand/roads arrangement on the fixture world
surface: hand/holds
class: BUILT-FIXTURE
intent: D-Hand: "the ones that feed each other joined by a hairline"; road sentence "from text to Table, in two steps".
code: desktop/src/shell/hand.rs:1-12; runtime/fixture_world.rs hand_view/HandView (Memo keyed by held set); model/hand.rs (max five).
missing/unknown: cards outside the snapshot stand apart with no road; the chain engine's results (facet::semantics::recipes) run on world.json packages only.

======== INSTANT / ARCHITECTURE (DIRECTION v5 §2, instant-open.md, gui-plan §2.2) ========

### Snapshot-first boot (paint the last frame from a zero-copy mmap archive, revalidate, morph only the difference)
surface: shell/frame
class: PARTIAL
intent: DIRECTION.md:45 (snapshot-first boot); instant-open.md §3.2, I2.
code: window-first launch: desktop/src/host/launch.rs:1-12 (I1), host/window_first_tests.rs; snapshot file runtime/snapshot.rs:1-40 (magic, schema, sha256 table, per-section hash, `.bad` on mismatch; JSON payloads of the pages last shown; seeded at the unserved root, `PageStore::seed`, quiet revalidation).
missing/unknown: format is JSON with sha256, not the "zero-copy archive, memory-mapped" (grepped rkyv, memmap, mmap: none); it stores page models only — not "shelf outlines, hand, world layout, graph camera"; cold-boot < 150 ms budget is not measured in release (production-readiness.md:266: release budgets never judged this wave).

### Pages as plans (flat render-ready plan cached by content hash; drawing never walks DTOs)
surface: shell/frame
class: NOT-BUILT
intent: DIRECTION.md:55 (pages as plans); instant-open.md §3.4 `PlanKey`.
code: facet::anatomy::plan::{PagePlan, compile} exists for the retired drawn page (facet/src/anatomy/plan.rs:414,456); the simple symbol page compiles a `View` per frame from Facts (bodies/symbol.rs:66-69 `compile` inside body()); grepped desktop/src for PlanKey / PagePlan: none.
missing/unknown: no plan cache, no measured-anchor cache; `compile` is pure but re-run per render (instant-open.md targeted <= 16 ms route change from cache — unmeasured).

### Typed guarantee: the UI consumes only Now<T> (available this frame, carries revision, may be stale)
surface: states (loading/empty/failure/stale)
class: NOT-BUILT
intent: DIRECTION.md:60 (typed guarantees: Now<T>); instant-open.md §2.5 `enum Now<T> { Ready, Stale, Pending{reserve} }`.
code: none found: grepped "Now<" and "enum Now" in desktop/src and facet/src: zero. The store hands `Resource<T>` and `Shown::{Ready, Pending, Fault, Unavailable}` (shell/bodies/state.rs); orbit.rs:153-156 still draws a `pending(...)` placeholder for the packages ring.
missing/unknown: "no spinners, no skeletons, no 'loading...' for anything already on this machine" is not a type-enforced property; skeleton-like placeholders (`kit::pending`) and "Laying out the graph fixture…" text remain.

### Prefetch by intent (80 ms rest or fast approach; keyboard focus; back/forward neighbours; hand cards; Start-here stops)
surface: keys/focus
class: PARTIAL
intent: DIRECTION.md:56-59 (prefetch by intent); gui-plan.md:89-90 (120 ms).
code: desktop/src/shell/kit.rs:20 PREFETCH_DELAY 120 ms; HoverIntent (kit.rs:210); region.rs:290-296 prefetch/cancel; orbit.rs:222 hover_link; package/folio.rs:712 prefetch on shingle hover; Urgency::Warm for titlebar marks (region.rs:89-119).
missing/unknown: 120 ms not 80; no motion-toward-target prediction; keyboard focus does not prefetch (grepped shell/focus.rs for prefetch: none); back/forward neighbours and hand cards not kept warm (no code).

### Batched owner round trips (one request returns N outlines; parallel over the 3 read sessions)
surface: shell/frame
class: PARTIAL
intent: instant-open.md §3.5.
code: READ_SESSIONS = 3 in host/launch.rs:35 with a read pool (runtime/reads.rs, offload.rs); I3 commit ff2f04dab per PLAN.md roster.
missing/unknown: whether `probe.outline` still runs serially 40-74 ms each is unrecorded in the tree; "one round trip for N outlines" has no test I could find.

### Main-thread watchdog (>4 ms logs backtrace; harness fails the run)
surface: shell/frame
class: NOT-BUILT
intent: DIRECTION.md:35-43 (budgets: main-thread task <= 4 ms); instant-open.md §3.8 I5.
code: none found: grepped -i "watchdog" over apps/ and tools/ (*.rs): zero hits.
missing/unknown: no keystroke p99 <= 8 ms test for Find/Ask/Filter either (I5).

### Event-driven wake, keyed resources with LRU bounds, region entities, background CPU
surface: shell/frame
class: BUILT-REAL
intent: gui-plan.md:74-95 §2.2.
code: desktop/src/runtime/wake.rs (224), store.rs (1319), shell/region.rs (318, cached region views), shell/mod.rs:9-23 rules; runtime/offload.rs Memo/Asker.
missing/unknown: `warm_anatomy` starts a world computation on every visit whose result nothing reads (production-readiness.md:267 D1); every hand touch re-runs the producer walk on the UI thread (D4); a panic on the world thread leaves is_loading() true forever (D5, fixture_world.rs:491).

### Debug prints and stale scaffolding (MARKS_DEBUG, NUDOX_PAGE_DUMP, [w-pages-review])
surface: other
class: PARTIAL
intent: PLAN.md:35.
code: grepped MARKS_DEBUG and NUDOX_PAGE_DUMP across apps and tools (*.rs): the two env-var prints are gone from desktop bodies (only a comment at apps/facet/src/anatomy/plan/tests.rs:195 remains); `[w-pages-review]` eprintln! survives at apps/desktop/src/harness.rs:63 and apps/facet/src/gallery/cli.rs:1475,1485,1489 (harness/gallery only); apps/desktop/src/runtime/debug_page.rs (566 lines) is compiled into the product (runtime/mod.rs `pub mod debug_page`).
missing/unknown: the shipped product still links a debug page module; a "diagnostics" tracing story is missing (production-readiness.md:101: the desktop has almost no tracing instrumentation, 2 files).

======== SETTINGS / SOURCE / STATES / INBOX ========

### Settings pages: Appearance, Editor, Agents (MCP), Connections, Privacy, Diagnostics, Index, Registry, Legend, Help
surface: settings
class: PARTIAL
intent: gui-plan.md:184 Settings "settings, health, capabilities: appearance, index and registries, keys"; SettingsPage has 10 closed variants (navigation/route.rs:229-290).
code: desktop/src/shell/bodies/settings.rs:25-32 maps Index|Registry -> index(); Help|Legend -> keys(); Diagnostics|Connections -> about(); Appearance|Editor|Agents|Privacy -> appearance(). The settings sidebar lists four rows only: Appearance, Index & registries, Keys, Diagnostics (shell/side/listing.rs:213-220).
missing/unknown: no Editor page, no Agents / MCP page (Intent::TestConnection exists at runtime/ui_graph.rs:228-241 and nothing dispatches it: GAPS L1), no Privacy / data-residency page, no Connections policy page, no Registry selection (Registry shows the index report), no Legend (semantic legend: what the colours and marks mean). Five typed pages are reachable only by persistence/route strings and all render another page's body.

### Settings > Editor: choose the editor template ({path} {line})
surface: settings
class: NOT-BUILT
intent: host/editor.rs:3-6 "The configured editor first (a command with {path} and {line}... when Settings has one)"; W-Sym6.md "Use the configured editor if Settings has one".
code: the launcher accepts a `configured` template but the only caller passes None: desktop/src/runtime/ui_graph.rs:239-240 `editor::open(launch.as_ref(), None, &path, line)`; grepped desktop/src/model for an editor setting: none (SettingsState, model/workspace.rs:389-415, has no editor field).
missing/unknown: no setting, no page, no persistence; a person without VS Code or Zed opens files with `open path` (no line).

### Settings > Index & registries (compiler found, declarations, files indexed, languages, capabilities)
surface: settings
class: PARTIAL
intent: gui-plan.md:184; W-Install.md.
code: settings.rs:154-192 index(): Compiler (host::toolchain::report), Declarations, Files indexed, Files without declarations, Capabilities ready, per-language declaration counts.
missing/unknown: read-only report; no registry list, no cache location, no "re-index", no network-policy control (owner decision 1), no advisory-feed control (owner decision 2), no toolchain override for Go/Python/TypeScript/Clang.

### Settings > Keys (from the key table; sidebar and graph keys)
surface: settings
class: BUILT-REAL
intent: gui-plan.md:184 "keys".
code: settings.rs:194-232 keys(): every TABLE row + side::KEYS + facet::graph::keys::KEYS.
missing/unknown: read-only (no rebinding: gui-plan lists none, `keybindings.json` support absent); missing rows: ⇧⌘J, ⌘↵, ⌥ x-ray and ⌘ reveal chords themselves.

### Settings > About / Diagnostics (version, service mode)
surface: settings
class: PARTIAL
intent: gui-plan.md:184 "health".
code: settings.rs:234-241 shows Version (CARGO_PKG_VERSION) and Service embedded/attached only.
missing/unknown: no health coverage view, no log location, no "copy diagnostics", no crash reporting (production-readiness.md:101); "transport health" (SettingsPage::Diagnostics doc) is not shown.

### Settings: a text-size control
surface: settings
class: OPEN
intent: gui-plan.md:166 "Text scale 85-200 %"; settings.rs:56-57 "Text size is not a setting: the system sets it, ⌘+ / ⌘- / ⌘0 adjust it."
code: keys.rs:222-225; ZoomTo follows the system text size (production-readiness.md:313).
missing/unknown: deliberate; but J10 (brief) expects "text scale" to persist across restart and there is no assertion path since it is not a setting.

### Inbox (followed releases, subscriptions)
surface: overlays/popovers/hover
class: NOT-BUILT
intent: gui-plan.md:185; PLAN.md:69 "one new-release line".
code: desktop/src/shell/bodies/inbox.rs (23 lines) prints "Nothing followed yet. Releases you follow arrive here once the local service publishes a release feed."
missing/unknown: no release feed, no follow action, no unread mark ("unread" state in the state grammar is used for un-indexed packages only).

### Source / code view (code with gutter, peek card, doc in margin)
surface: code view
class: PARTIAL
intent: gui-plan.md:180 §2.7 Source; §8.3 "the real source, soft-wrapped at token boundaries with the item's lines marked. It never clips."
code: desktop/src/shell/bodies/source.rs:78-190: crumb, numbered lines (the declaration's numbers lit mint), 24 lines before and 240 after (CONTEXT_BEFORE/AFTER), `text_fit::wrap_code(&shown, &[], columns)` wraps at token boundaries; margin = doc sentence + up to 5 callers (source.rs:108-126).
missing/unknown: NO syntax colour: the text is drawn ink1 (source.rs:150-163) although facet::code::highlight supports all seven languages (facet/src/code.rs:381-416, tree-sitter) and PLAN.md:26 listed "no syntax colour" as a walk defect; no symbol peek inside code; not the whole file (a 241-line window and "N lines above"); the item's lines are marked by lighting the gutter numbers mint only; a second breadcrumb duplicates the jump bar (PLAN.md:26) and remains (source.rs:47-56); the margin uses `page.rose.left` (the retired rose) for callers.

### Peel: S peels to source; "S" key
surface: keys/focus
class: PARTIAL
intent: gui-plan.md:167.
code: keys.rs:206 PeelSource; ⌘. toggles code/page (keys.rs:210).
missing/unknown: both cut (no Peel motion), see motion record.

### Hint mode (F): labels on every target, type to go
surface: keys/focus
class: BUILT-REAL
intent: gui-plan.md:167; overlay/hint.
code: desktop/src/shell/hints.rs (160 lines), facet/src/overlay/hint.rs.
missing/unknown: `codes(count)` silently leaves targets past the 256th without a code (hints.rs:43, FEEL D15); hint targets exclude the package page's crest/ticker/berg blocks (not targets).

### States: offline banner, loading skeletons, empty rooms, fault anatomy, health coverage
surface: states (loading/empty/failure/stale)
class: PARTIAL
intent: gui-plan.md:186 "States: health coverage, faults: offline banner, loading skeletons, empty rooms, fault anatomy".
code: desktop/src/shell/bodies/state.rs:1-141 (still arriving = a comb of hairlines; stopped = one coral plate with what happened, the code, one way to try again; not served = one quiet line); kit::pending; onboard::failure; status.rs Notice + Try again.
missing/unknown: no offline banner (there is no network use, so nothing to say offline about — but no "this needs the network" state either: undecided with the network policy); no per-language health coverage view (health.ingest.languages is printed in Settings only); the 'still arriving' comb is a skeleton, against DIRECTION law 1 ("no skeletons").

### Package outline states: "The library's packages are still being read", "not read yet", gaps in words
surface: states (loading/empty/failure/stale)
class: BUILT-REAL
intent: DIRECTION law 1 honest empties; gui-plan rule 5 "say it once".
code: shell/kit.rs gap_words; side/listing.rs:349 notes; package/folio.rs:256 "Its names are not read yet".
missing/unknown: gap phrasing is prose per site rather than one typed fault anatomy; owner-failed Try again inside page bodies only (GAPS G7).

======== JOURNEYS AND VERIFICATION (gui-plan §3) ========

### J1 first look (launch -> Orbit -> project -> dependency package -> Start here stop 1 -> page -> ⌘. code -> back x3)
surface: other
class: PARTIAL
intent: gui-plan.md:229.
code: apps/desktop/journeys/J1.journey (81 lines): on a real install of toml_pin -> your project -> a module region -> read_settings -> code -> back x2 -> toml 0.8.23 -> from_str -> code -> back.
missing/unknown: "Start here, stop 1" removed from the journey and the product; no report in the tree shows J1 PASS on the production path (production-readiness.md:290 lists it as required, F-Shell); gui-plan.md:229 not updated.

### J2 find (⌘K toml Value -> shape Invocation -> list of text -> chain row -> hold -> grammar)
surface: other
class: NOT-BUILT
intent: gui-plan.md:230.
code: none found: apps/desktop/journeys has J0, J1, J9, J10, J12 and seams-760-menus only (ls). Shape search exists only in the fixture graph.
missing/unknown: needs shape search on the real index (owner) or the fixture world's `Invocation` (locald is this workspace's own crate, not in a bundled app).

### J3 upgrade (scrub comb to 1.1.6 -> upgrade lens -> a use site -> esc)
surface: other
class: NOT-BUILT
intent: gui-plan.md:231.
code: none found (no J3.journey); would be BLOCKED(data) per rule since real diffs are fixture-only.
missing/unknown: the lens counts "82 added, 2 removed, 103 changed" and `from_str` respelled come from the fixture.

### J4 the map (G -> flight -> R -> esc -> T -> → -> ↵)
surface: other
class: NOT-BUILT
intent: gui-plan.md:232.
code: none found (no J4.journey).
missing/unknown: would run on the prototype world only.

### J5 failure (ensure_locald -> "or fails with" kinds -> Spawn -> RuntimeError's How it fails -> maker link)
surface: other
class: NOT-BUILT
intent: gui-plan.md:233.
code: none found (no J5.journey); the maker/reader model is not on the page (semantics::fails unreferenced by desktop).
missing/unknown: unbuildable until How it fails makers reach the page.

### J6 weather / J14 weather (resize storm 1440 -> 480 -> 2560, text scale 85 -> 200 mid-flight, reduced motion for the second half)
surface: fluid widths
class: NOT-BUILT
intent: gui-plan.md:234; W-Journey.md J14 "J1 with a resize storm (2560 -> 320 -> 1440)".
code: none found as a journey; the fit/fluid rig tests (desktop/src/shell/fit_tests.rs, fluid_tests.rs, 416+414 lines) sweep widths in-process, harness `matrix`/`sweep.py` under .local.
missing/unknown: the only end-to-end weather run is a lane script under .local (git-ignored).

### J7 add a package from browsing; J8 an earlier version; J13 failure and retry
surface: other
class: NOT-BUILT
intent: W-Journey.md §3.
code: none found (no J7/J8/J13 files). J13 also needs a fixture with a real compile error under apps/desktop/journeys/fixtures/ (directory does not exist: ls apps/desktop/journeys shows J*.journey, parts/, seams-760-menus.journey).
missing/unknown: blocked on G10-G13 (add affordance, typed source, Find over the cache, on-demand earlier releases).

### J9 search, J10 settings, J11 every key (generated), J12 crawler
surface: other
class: BUILT-REAL
intent: W-Journey.md §3.
code: apps/desktop/journeys/J9.journey, J10.journey, J12.journey; J11 generated from KEY_TABLE with an exhaustive match (apps/desktop/src/harness/journey/keys.rs:1-12); crawler harness/journey/crawl.rs.
missing/unknown: J9 asserts ⌘K "toml Value" -> `every result, as a page` -> ↵ but not the zero-result state or "a symbol only reachable through search"; production-readiness.md:290 says each key journey still needs a product mutation that makes it fail (brief §5 proof) and none is recorded in the tree; J11 excludes the sidebar's typed/G-chord keys and ⌘↵ (not in the table).

### Journey composition model (parts, cached states, Rust API, cycle detection)
surface: other
class: BUILT-REAL
intent: W-Journey.md §2.
code: apps/desktop/src/harness/journey/{parts,plan,state,machine,script}.rs; apps/desktop/journeys/parts/install.part, lifecycle.part.
missing/unknown: navigation/journey_specs.rs (497 lines, ColdEmpty/PickerCancelled/Indexing/ReadyMultiProject/FailureRetry/PersistedRestart/McpSetup) was to be unified with the runner or retired with a reason; it is still in the tree (navigation/mod.rs) and nothing records the decision.

### Gallery captures, filmstrips, motion probes, settle == fresh, storms, matrix, lints, frame budget, content truth
surface: other
class: PARTIAL
intent: gui-plan.md:192-225 §3.1-9.
code: facet/src/gallery/{verify,storm,matrix,lint,perf,bench,soak,legible,native_trace}.rs and tools/gui-harness; `backend-desktop-gui-harness` (apps/desktop/src/bin) with capture/film/journey commands.
missing/unknown: the full `verify` gate never gave a verdict this wave (production-readiness.md:303); release-mode budgets (page open <= 120 ms, search <= 50 ms after the last keystroke, flight <= 1500 ms, p95 frame <= 8 ms at 1440x900 / 12 ms at 2560x1440) have never been judged (line 266); the harness lint skips wholly clipped/offscreen text and button labels are not probe texts (lines 191-193); several tests "prove nothing" (line 298-301).

### Cross-platform (Windows and Linux)
surface: other
class: NOT-BUILT
intent: cross-platform-and-ci-gate memory; production-readiness.md:308-310.
code: the `.#cross` shell compile-checks; window options include macOS titlebar options (host/launch.rs:411); gpui-ce macOS renderer patches (vendor/gpui_ce_macos) carry compositing and blurred shadows.
missing/unknown: the group-opacity and blurred polygon shadows are Metal-only patches (NUDOX-PATCHES.md:53-125); no Windows/Linux equivalent recorded; fonts, titlebar, traffic-light inset and folder picker on other OSes unexercised.

### Accessibility (screen reader, focus order, reduced motion, high contrast)
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:213 lints; production-readiness.md:310-314.
code: contrast lints and Contrast::High; MotionPreference; keyboard reach via focus.rs Targets.
missing/unknown: VoiceOver / AX tree never exercised; icon-only marks (kind gems, modifier marks) have tooltips but no AX labels checked; focus ring jumps (GAPS D6).

### Localization / RTL / long words
surface: other
class: NOT-BUILT
intent: production-readiness.md:315-316.
code: none: every string is inline English (settings.rs, ask.rs, onboard/*); no message catalog; text_fit wraps identifiers at hump/underscore boundaries only (shell/text_fit.rs).
missing/unknown: bidi, CJK wrapping and non-Latin fonts (the four faces in facet/src/fonts.rs are Latin) never tried.

### Settings state with no surface: privacy (LocalOnly), advisories on/off, registry cache on/days, context panel, connection status
surface: settings
class: NOT-BUILT
intent: gui-plan.md:184 Settings "index and registries"; Intent::{TogglePrivacy, ToggleContext, TestConnection} and SettingsPage::{Privacy, Connections, Registry}.
code: desktop/src/model/workspace.rs:409-419 SettingsState.{privacy: PrivacyPreference (default LocalOnly), advisories: bool (true), cache_enabled (true), cache_days (14), connection: ConnectionStatus, context_open (true)} with reducers (navigation/workspace_reducer.rs:27,86) and persistence (model/persistence.rs); grepped the whole of desktop/src for consumers of .privacy/.advisories/cache_days outside reducers/persistence: none; no control draws them (settings.rs has Theme, Contrast, Density, Motion only).
missing/unknown: state that looks like a privacy/network policy exists but nothing reads it: the owner-level "network policy" decision (production-readiness.md:361) has a placeholder field with no semantics; the context panel toggle (ToggleContext, bound to a shortcut only in the unbound action.rs table) has no panel; `context_open` defaults true with no context panel in the shell.

======== LANGUAGES / ECOSYSTEMS (seven ecosystems first class; DIRECTION v5 §4 "Every ecosystem is first class") ========

### Rust / cargo: the only ecosystem with end-to-end desktop evidence
surface: languages
class: BUILT-REAL
intent: gui-plan.md:15-16 seven ecosystems; DIRECTION.md:124-127 (every ecosystem is first class).
code: every journey (J0, J1, J9, J10, J12) runs on frontends/rust/fixtures/toml_pin and the cargo cache; source facts (model/source_facts/*), release data (runtime/releases.rs, cargo sparse-index cache in model/source_facts/registry.rs:41-49), registry source (host/registry.rs) and dependency acquisition (runtime/acquire/work.rs) are Cargo-only.
missing/unknown: everything Rust-specific listed elsewhere; Rust is the only language whose package page has heads-up, weight, features, byline, licence-from-manifest and release ticker.

### npm / TypeScript-JavaScript: symbol page and code highlight only
surface: languages
class: PARTIAL
intent: DIRECTION.md:120-127 (page is the node; every ecosystem); MOMENTS.md:128-131 (how we know: grades) (JS has no types: "how we know" dotted).
code: pipeline exists: facet/src/anatomy/symbol/view.rs:17-55 Lang::{TypeScript, JavaScript} (JS detected by file extension, desktop bodies/symbol/facts.rs:79-83); "How we know" rail block (derive/rail.rs:34); harness root apps/desktop/tests/fixtures/lang/ts/zod and scene desktop-record-ts (harness.rs:1637); highlight grammar (facet/src/code.rs:386-391).
missing/unknown: zod is a NAMED OWNER REFUSAL (`Authority { Open, Binding }`, production-readiness.md:140), so the TS scene shows the refusal not a page; no npm package source resolver (registry/acquire is cargo-only, NOT_CARGO refusal at runtime/acquire/work.rs:183); no package page facts for npm (package.json unread: grepped); js-which.sync (board page symbol6) exists only via the board model in the facet gallery (anatomy/symbol/gallery.rs:38 sym6-js-which); no TypeScript page at all is among the seven board pages (sym6-* lists Rust x5, Python x1, JavaScript x1: gallery.rs:32-38).

### PyPI / Python
surface: languages
class: PARTIAL
intent: MOMENTS.md:222-232 (py-re.match grades) py-re.match (no type hints, "in its docs"/"in its code" grades).
code: Lang::Python in derive; scene sym6-py-match (facet/src/anatomy/symbol/gallery.rs:37) from the board's JSON model only.
missing/unknown: no Python fixture root in the desktop harness (harness.rs roots: Rust, zod (TS), pflag (Go)); no pip/venv/site-packages source resolver; specimen "Python Literal/Enum as a choice" not detected; Python `**kwargs` options object only on the board model.

### Go modules
surface: languages
class: PARTIAL
intent: DIRECTION.md:103 (a contract: Go computed fan) "For Go... the fan is computed, and it's the headline."
code: Lang::Go; scene desktop-record-go (harness.rs:1643-1646) over apps/desktop/tests/fixtures/lang/go/pflag; Go "named type's constants" companions (bodies/symbol/companions.rs, facts.rs:172-182).
missing/unknown: pflag is a NAMED OWNER REFUSAL (`Authority { Resolve, Authority }`, production-readiness.md:141); Go implicit interface satisfaction computed fan not built; Go module cache root not discovered for a Finder launch (production-readiness.md:59); no GOPATH/go.mod package resolver.

### Java / Maven and C# / NuGet
surface: languages
class: NOT-BUILT
intent: gui-plan.md:15; DIRECTION.md:124-127 (every ecosystem: fixtures for six languages) "pinned fixtures and scenes for Python, TypeScript, Go, Java, C# and C++".
code: Lang::Java / Lang::CSharp exist in derive/view (view.rs:28-31) and in the highlighter (code.rs:406,411); Eco::Maven / Eco::Nuget install lines (facet/src/marks/eco.rs:121-124: Maven prints a Gradle `implementation("...")` line). No desktop fixture, scene, journey or resolver: harness.rs mentions no Java/C# root (grepped harness.rs for java, csharp: none); tests/fixtures/lang has go and ts only.
missing/unknown: never rendered through the desktop on real data; sealed-class fork, annotations and `virtual/override` badges (DIRECTION.md:106-107 (badges: virtual/override)) not evidenced.

### C / C++ (and Conan)
surface: languages
class: NOT-BUILT
intent: gui-plan.md:15 lists Conan as the seventh registry; DIRECTION.md:124-127 (every ecosystem: fixtures for six languages) C++.
code: Lang::Cpp exists and is documented as "Conan" (facet/src/code.rs:44, icons/set.rs:724,764 name "conan"), but facet/src/marks/eco.rs:48-49 `Cpp` = "C and C++: no registry" with install line `git clone {package}` (eco.rs:125). Grepped desktop/src and facet/src for "conan" (case-insensitive): only those icon/code labels.
missing/unknown: the seventh ecosystem is spelled two ways (Conan in the highlighter/icons, "no registry" in the ecosystem mark) and no Conan resolution or C++ fixture exists in the desktop; Clang sysroot probing is an index-side concern (memory).

### Specimen plates across all seven languages (a choice, a record, a callable, a contract each)
surface: languages
class: SUPERSEDED
intent: DIRECTION.md:95-127 (Pages: specimen, reading order, ecosystems) §4 "The specimen"; "A page that only shines on Rust fails review."
code: replaced by the simple form; derive covers language idioms as far as text can (facet/src/anatomy/symbol/derive/{callable,shape,words,known}.rs with tests on TS/Python/Go signatures, derive/tests.rs).
missing/unknown: the review rule "a page that only shines on Rust fails" is still unmet operationally: only Rust reaches a real symbol page in the desktop; other languages are proved by unit tests and board-model gallery scenes.

### Language idioms as badges (decorators, annotations, async, unsafe, pub(crate), @deprecated, feature gates, virtual/override, readonly)
surface: symbol page
class: PARTIAL
intent: DIRECTION.md:106-107 (language idioms as badges).
code: package-page card badges from signatures (facet/src/marks/badges/{rust,other,glyph}.rs, 522+334 lines) and the simple symbol page verbs/outcomes; deprecation from facts (desktop model DeclRef.facts.deprecation).
missing/unknown: the simple symbol page draws no badge row for modifiers (`async`/`unsafe`/gates appear only as outcome glyphs or not at all); feature-gate "Off in your build" dimming (symbol-page.md ruling; needs R3 gates fact) not built.

### Ecosystem mark and install line in the package hero
surface: package page
class: PARTIAL
intent: MOMENTS.md:20 (hero) "where it lives"; marks/eco.rs.
code: shell/bodies/package.rs:186-203 ecosystem_mark with the ecosystem's install line (facet/src/marks/eco.rs:114-127).
missing/unknown: the mark copies its install line to the clipboard (facet/src/marks/eco.rs:361) but performs no add (no manifest edit, no add-to-library); Maven line is Gradle `implementation(...)` syntax only, no pom.xml form; Conan absent as an ecosystem name.

### Syntax highlighting for the seven languages in the code view
surface: code view
class: GALLERY-ONLY
intent: gui-plan.md:150-151 (tree-sitter features rust, typescript, javascript, python, go, java, c_sharp, cpp).
code: facet/src/code.rs:381-416 maps all eight; grepped `crate::code` / `facet::code` over desktop/src and facet/src: the only reference is code.rs's own test (code.rs:448). desktop/src/shell/bodies/source.rs draws ink1.
missing/unknown: the highlighter has no product consumer at all (not the code view, not peeks, not the symbol page's docs/examples); the Syntax palette roles (tokens.rs `Syntax`) are unused outside it. Doc "Example" blocks render as plain mono.

======== COMPONENT INVENTORY / PAINT / HOUSE RULES (gui-plan §1, §2.4, §2.5, §7) ========

### facet::chrome (titlebar thread, shelf rows + book header, kspine, status bar, pins frame) in the desktop
surface: shell/frame
class: GALLERY-ONLY
intent: gui-plan.md:146-147, 356 W-Controls owns "chrome pieces".
code: facet/src/chrome/{titlebar,shelf}.rs (834 + 809 lines) with gallery scenes (chrome/gallery.rs); grepped desktop/src for facet::chrome / chrome:: outside tests: zero. The shell draws its own titlebar (shell/titlebar.rs), sidebar (shell/side/view.rs, kspine inside it), status (shell/status.rs) and pins (shell/pins.rs).
missing/unknown: two parallel implementations of the same chrome; the facet one (beads, here capsule with altimeter, Book header, Row tones) is dead for the product; the design-system gallery no longer shows the real chrome.

### Controls not used by the desktop: switch, diamond check, diamond radio, select trigger, splitter, comb slider
surface: other
class: PARTIAL
intent: gui-plan.md:133-138 controls.
code: facet/src/controls/{toggle,field,splitter}.rs; desktop uses button, icon_button, kbd, seg, field (Add dialog), version_comb, theme_toggle, density_toggle (grepped facet::controls:: imports in desktop/src).
missing/unknown: no product screen has a switch/check/radio/select (settings.rs uses segmented controls only); "comb slider" (as a value control) is not present in the module list at all (facet/src/controls.rs exports VersionComb only).

### Data marks not used by the desktop: comb (as data), compass, facts line, lens bar, strands, caps, spell, mosaic, rose, territory, spatial grid
surface: other
class: GALLERY-ONLY
intent: COHESION.md:18-27 (What is wrong today: 1 of 15 components) "The desktop uses 1 of the 15 components in facet::data... none is mounted."
code: facet/src/data/* (comb 1218, compass 919, territory 830, rose 807, spell 711, progress 656, mosaic 537, facts 376, caps 374, door 306, spatial 304, strands 218 lines); the desktop mounts only data::progress (onboard/library.rs gem_progress/seam) and data::release::view::section (bodies/symbol.rs:185); comb via controls::version_comb in the sidebar.
missing/unknown: roughly 6,000 lines of design-system code are unreachable from the product; the cohesion board's "one set of objects, three distances" is realised by folio shingles + anatomy/symbol instead, so the 15-component family was never adopted or retired by decision.

### Paint: Cut states (focus doubled peri, hot, run, amber, coral, ghost hatched, two, deep, weave, lift)
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:119-126 §2.4; house rule 2 (bevel is the state channel: mint yours, running light, amber waiting, coral stopped, hatched pending).
code: facet/src/paint/cut.rs (767 lines); desktop uses `Bevel, Chamfer, cut` for the fault plate (bodies/state.rs) and the hand (shell/hand.rs); the hatch pending texture is not used by any desktop file (grepped hatch/Hatch in desktop/src: only a model comment).
missing/unknown: "pending = hatched" is not the pending look in the product (kit::pending is a comb of hairlines, orbit.rs:153-156); the running-light bevel for "working" is used through gem_progress only.

### Gem (12 facets, kind glyph, progress 0..12; todo/working/stalled/cracked/glint)
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:127-128.
code: facet/src/paint/gem.rs (526); desktop: project tiles (onboard/library.rs tile_gem), package hero gem (package.rs:225 gem(Kind::Package)), symbol hero gem.
missing/unknown: the "glint" state on arrival (new item just added) is unused in the product; row marks elsewhere are kind marks not gems.

### House rule 1: no gradients, flat tones, depth from bevel and faceted ground
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:26-27.
code: grepped `linear_gradient` / `gradient(` across desktop/src and facet/src: only doc comments in paint/cut.rs:6,96 and hatch.rs:5.
missing/unknown: the group-opacity/blurred-shadow compositing is a Metal-only patch (vendor NUDOX-PATCHES.md).

### House rule 2: no status dots, no flat pills
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:28-31.
code: two rounded_full mint dots on the symbol page's method/use rows: facet/src/anatomy/symbol/body.rs:352,394 (W-Sym6.md item 6 itself specifies "a mint dot and count"); the rail's "Across releases" draws square 8 px release dots (side.rs:150-153); sidebar mint notch (side/glyph.rs).
missing/unknown: spec and rule contradict; gui-plan.md:408 also forbids any `rounded()` ("chips and key caps are cut stones"): two `rounded_full()` calls remain.

### House rule 3: icons all the way down (no uppercase property labels; modifiers are marks with tooltips)
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:32-34.
code: uppercase small-caps labels still drawn: sidebar headings ("YOURS", "DIRECTLY", listing.rs:770, outline.rs:581 `to_uppercase()`), the sidebar peek's "GONE IN x" (side/peek.rs:135), the symbol call's port labels via `caps(words)` (anatomy/symbol/call.rs:118), rail block heads (`caps(title)`, side.rs:59), the Find page's "FIND" eyebrow (facet/src/browse/find.rs:290), "ADVISORIES" crest label (production-readiness.md:225).
missing/unknown: unresolved whether section-eyebrow caps count as "property labels"; DIRECTION.md:136-145 (Type scale: section heading 13/16 600) wants a 13/16 600 section heading with a kind-hued gutter mark instead (unbuilt).

### House rule 4: plain until touched (timelines are tick combs that wave and pop a card; packages are mosaics)
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:35-37.
code: the ticker (folio/ticker.rs) is the wave/fisheye comb; packages are shingle maps not mosaics (superseded).
missing/unknown: the "neighbour wave" tick comb (data/comb.rs) is not the mounted comb; the sidebar version comb (controls::version_comb) is a different implementation from the ticker; two comb families, both in use.

### House rule 5: say it once, quiet until asked
surface: states (loading/empty/failure/stale)
class: PARTIAL
intent: gui-plan.md:38-39.
code: fault plate (state.rs), Notice in the foot.
missing/unknown: DIRECTION.md:11-16 (walk defects: filler prose) walk defects ("Show more entries", "Choose a row to inspect the declarations recorded beneath it", "Value has 7 variants. Explore ways to make one...") were removed with the old bodies; remaining narration: "Type to narrow" hint, the Library's arrival lines (orbit.rs:170-190 "N declarations from X of Y files"), empty-state paragraphs (onboard/library.rs:160-165), the package-page banner sentence (folio.rs:249-262); law 4 ("prose is for meaning only") is judged by nobody automatically.

### House rule 6: usage and source are secondary metrics
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:40.
code: sidebar counts are quiet mint notches; the package page shows no download counts; workspace uses sit under the docs.
missing/unknown: none recorded.

### House rule 7: four faces, one job each (Bricolage Grotesque display, Geist UI, Geist Mono identifiers, Newsreader italic ledes/captions)
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:41-43.
code: facet/src/fonts.rs (456 lines) embeds the fonts via include_bytes! (fonts.rs:78).
missing/unknown: DIRECTION.md:136-145 (type scale: serif rule) "serif is for the lede and at most one sentence per section. Nowhere else." vs tokens ty::MARGIN, ty::CAPTION, ty::AXIS are serif roles (tokens.rs:604-612) used for captions and margin notes; no lint enforces "serif only for lede".

### House rule 8: motion is meaning; every animation interruptible, retargetable, reduced-motion aware, settles to layout position
surface: motion/transitions
class: PARTIAL
intent: gui-plan.md:44-46, §3 items 2-4.
code: motion engine facet/src/motion/{curve,spring,keys,store,element,flow,presence,shared,flight,carry,print}.rs; probe ledger facet/src/probe.rs (1151 lines).
missing/unknown: settle == fresh and continuity checks are run per lane via harness; no aggregate gate has passed (verify never gave a verdict).

### Trait / class pages: Implements / Inherits / Overrides edges, blanket arrivals dashed, implementors mosaic
surface: symbol page
class: PARTIAL
intent: gui-plan.md:179.
code: facet/src/anatomy/symbol/derive/shape.rs trait "What you write"; Facts.implements / Implementors (desktop bodies/symbol/facts.rs:184-190, one summary number "N types, M crates").
missing/unknown: no implementors mosaic/list, no blanket arrivals (R1/R2 facts phase 2: `Arrival::Blanket` not built, symbol-page.md §5), no Inherits/Overrides drawing, no list of "your" implementors; the Rust `impl` rows facts (ImplementationFacts) are R1, unstarted per symbol-page.md.

### Search results page (search paged; filters as combs/marks)
surface: Find/browse/discover
class: PARTIAL
intent: gui-plan.md:182 "Search results: search (paged); filters as combs/marks".
code: BrowseRoute::Find via ⌘K "every result, as a page"; SearchContinuation paging (bodies/graph.rs imports it; model/pages/search.rs).
missing/unknown: filters (combs/marks) absent; is the results page paged in the UI? facet/src/browse/find.rs shows candidates + "Explore N more packages" widening, not pages.

======== HAND / HOLDS ========

### Hold reasons: ⌘D pin, ⌘-click, Space-Space, a copied signature, a compare, an added package
surface: hand/holds
class: PARTIAL
intent: desktop/src/model/hand.rs:1-8 (the hand: at most five things you held with intent); D-Hand.
code: HeldWhy::{Pin, Copy, Compare, Add} exist and persist (model/persistence.rs:770-915) but every construction in the product uses HeldWhy::Pin: desktop/src/shell/root.rs:1037,1053 (⌘D) and shell/side/mod.rs:547 (sidebar H). Grepped HeldWhy:: over desktop/src: no other producer.
missing/unknown: copied signatures, Compare, "Add" (a package added from browsing), ⌘-click and Space-Space never enter the hand; the Compare/Held button on Find rows (facet/src/browse/find.rs:434-439) holds inside Find's own component state, not the hand.

### Hand rung Row (H): each road as cards with the verb between them and a sentence ("from text to Table, in two steps")
surface: hand/holds
class: BUILT-FIXTURE
intent: shell/hand.rs:1-12; gui-plan D-Hand.
code: desktop/src/shell/hand.rs (279 lines), roads from fixture_world::hand_view.
missing/unknown: roads exist only for cards the prototype world.json knows; cards from your own project or packages outside the snapshot stand apart with no verb; the sentences come from the recipe engine's plain-word types (facet::semantics::recipes).

======== FLUID WIDTHS (W-Fluid; 320 px phone to 2560) ========

### facet::fluid primitive (Room, Fluid ramps, Modes with hysteresis, Ladder)
surface: fluid widths
class: BUILT-REAL
intent: W-Fluid.md design §1.
code: facet/src/fluid.rs + fluid/{room,ramp,modes,ladder,tests}.rs (561 lines of tests; a scan test forbids width-vs-number compares outside fluid: fluid/tests.rs:375-561, KNOWN lists only retired drawn-page files: anatomy/{gallery,page,page/gallery,prism}.rs).
missing/unknown: the legacy `Measure::room` enum (Narrow/Slim/Regular/Wide/Vast) and `Measure::columns` remain (facet/src/measure.rs:111-117, production-readiness.md:261) beside the new API.

### 320 x 480 minimum window and phone-width usability on every screen
surface: fluid widths
class: PARTIAL
intent: W-Fluid.md "done looks like"; owner: "even phone widths".
code: host/launch.rs:35 LEAST 320x480; drawer mode with scrim, titlebar compaction (titlebar.rs:11-16), fit_tests.rs / fluid_tests.rs rig at 320/360/390.
missing/unknown: 94 of 285 sweep widths still have text past the edge on the package page (feature chips and repository URL from 416 px down: production-readiness.md:230); symbol page phone widths overrun (D1, line 244); README clips at 480 px; hero lede/byline overrun at 430 px and below; Find name column squeezes to 19-32 px at narrow widths (line 198); Library at 200 % text and 360x900 has a cut caption (line 202). Hit targets under 24 px: Library chips at 85 % text (orbit.rs:257), Find inspect controls, package badges 21 px (line 200).

### Continuous resize with no cliffs: blocks under a mode change jump
surface: fluid widths
class: PARTIAL
intent: W-Fluid.md verification §2 cliff metric.
code: Flow/Modes exist; FLUID-B/C reports under .local.
missing/unknown: package page has 16 hard thresholds and the symbol page 5 that move a block in one frame (production-readiness.md:187); worst: SYMBOL_RAIL edge at 1344 moves 1084 px in one frame; the crest flips between one and two rows on alternate frames during live resize (a 150 px jump, line 225); a reader-level reflow helper was proposed (FLUID-C "Next" 1) and not built.

### 2560: the page column is sparse
surface: fluid widths
class: OPEN
intent: W-Fluid.md §2 "fill the empty space... with fluid gutters and a wider measure for wide content".
code: Ctx::wide measure (shell/bodies/mod.rs:82-90, tokens::fluid WIDE_FOLIO); package PAGE_MAX 1800 (bodies/package.rs:152); symbol main column capped at 860 + 280 rail (anatomy/symbol/page.rs:1-6).
missing/unknown: production-readiness.md:201 says the Library, package and symbol pages are an 800 px column in an empty window (R-Fit 8) and FLUID-B added wide leaves — re-check pending; owner decision listed at line 364.

### Find at phone widths (floating panel -> full-width sheet) and the Ask plate
surface: fluid widths
class: PARTIAL
intent: W-Fluid.md "Find as a floating panel -> a full-width sheet"; R6 "⌘K Ask invisible".
code: tokens::fluid::FIND / FIND_INSPECTOR (facet/src/browse/find.rs:16); ask.rs plate over shelf column.
missing/unknown: ask plate at narrow widths and drawer instance keyboard (production-readiness.md:171); "the inspector glides through the results, text over text for 3+ frames" (line 199).

======== v4 TARGETS (gui-plan §6) ========

### Symbol page target (hero, facts line, context margin, lens bar, signature x-ray, rose, ledgers)
surface: symbol page
class: SUPERSEDED
intent: gui-plan.md:271 v4/SymbolPage.html.
code: replaced by the v6 simple form; no `facts` line, no `lens_bar`, no rose in the product page.
missing/unknown: gui-plan.md:271-277 targets are stale for the symbol page and Lenses/Ladder (they describe the unmounted data marks).

### Flow target: one page at five widths and 200 % text (third column of pins at vast, margin folds < 1100, rose -> grouped list < 560, thread keeps only "here")
surface: fluid widths
class: PARTIAL
intent: gui-plan.md:273.
code: pins column at 1900 (tokens PINS ladder), margin notes fold under blocks (Leaf::with_note, bodies/mod.rs), rail below content < 1100 on the symbol page.
missing/unknown: "thread keeps only here" is the jump bar dropping segments (titlebar.rs:11-16), not a thread; no rose to convert.

### Density target: three densities at the same width; dense folds summaries away
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:274.
code: Density in Measure; Settings control.
missing/unknown: see density record; sidebar rows and folio cards have no "summary" to fold.

### Peeks target: symbol peek anatomy, chained child with crumb, pinned column, package/file/version peeks, four peek rules
surface: overlays/popovers/hover
class: PARTIAL
intent: gui-plan.md:275.
code: facet/src/overlay/peek.rs, float chain/pin; peeks.rs (desktop) builds declaration and package cards.
missing/unknown: file peek and version peek do not exist as cards in the desktop; the release comb has its own label (folio ticker) rather than a version peek.

### Lenses target: release-comb lens by family with "touches your code" first; module tile lens; language-comb lens
surface: overlays/popovers/hover
class: GALLERY-ONLY
intent: gui-plan.md:276.
code: facet/src/overlay/lens.rs; facet/src/data/gallery/lenses.rs; not mounted.
missing/unknown: the Library's "language comb" (gui-plan.md:175 Orbit) is unbuilt.

### Ladder target: mark -> tag -> row -> card for a symbol, a package, a release
surface: theming/density/contrast
class: PARTIAL
intent: gui-plan.md:277.
code: Rung + Needs (measure.rs), data/gallery/ladder.rs.
missing/unknown: no product surface renders a package or release at four rungs; a package appears as a chip (Library), a row (sidebar), a peek card (peeks.rs) and a page, unrelated to `Rung`.

======== DIRECTION v5 LAWS (§1) ========

### Law 1 Instant: nothing local waits; no spinners, skeletons or "loading..."
surface: states (loading/empty/failure/stale)
class: PARTIAL
intent: DIRECTION.md:27 (law 1).
code: window-first launch, snapshot seed, background world/anatomy threads; remaining waits: state.rs "still arriving" comb, "Laying out the graph fixture…", owner blocks reads while indexing (production-readiness.md:69-77), 15-minute owner reply timeout.
missing/unknown: budgets in DIRECTION §2 not enforced as tests (no watchdog, no perf tests in a release build).

### Law 2 Legible at every frame
surface: motion/transitions
class: PARTIAL
intent: DIRECTION.md:28 (law 2).
code: gpui TextTrace patch and `legibility` command (gallery/legible.rs); route transitions built as plates.
missing/unknown: catalog of every transition unfinished (T3-T5), graph label LOD partial.

### Law 3 One space (world -> package -> module -> symbol -> source with a fixed spatial map; graph is that space zoomed out)
surface: motion/transitions
class: PARTIAL
intent: DIRECTION.md:29 (law 3).
code: Verb::{Open,Close,Fold,Unfold}; depth keys ⌃1-⌃4.
missing/unknown: module has no page/route of its own (the module opens in place in the folio, and modules over 14 names get a dedicated view within the package route); world -> package transition does not exist.

### Law 4 Show, don't narrate
surface: states (loading/empty/failure/stale)
class: PARTIAL
intent: DIRECTION.md:30 (law 4).
code: shingles, seam, gem progress, fork/bracket rails replaced sentences.
missing/unknown: no automated check; see house rule 5 record for surviving narration.

### Law 5 One hand (one hover, one stroke, one kind-shape)
surface: overlays/popovers/hover
class: PARTIAL
intent: DIRECTION.md:31 (law 5).
code: see hover grammar record; kind marks in facet/src/icons.
missing/unknown: hover grammar adoption 1 of ~10 surfaces.

======== PACKAGE PAGE: YOURS, USES, JUDGE (more) ========

### Mint "yours" on shingles and cards (the names your code reaches)
surface: package page
class: PARTIAL
intent: COHESION.md:28-48 (state grammar) state grammar "yours = mint"; shingles.rs:5 "Mint is yours: the names your code reaches"; W-Folio.md.
code: the component supports it (facet/src/folio/state.rs:18-24 `Use::{Yours, Elsewhere}`), but the desktop hard-codes Use::Elsewhere for every shingle and card: desktop/src/shell/bodies/package/folio.rs:143 (ShingleFacts.yours) and :618 (CardFacts.yours(Use::Elsewhere)).
missing/unknown: nothing on the package page is ever mint; "you use N" and mint-first module ordering are unbuilt though the sidebar's Used-by lens and the symbol page's workspace list already know per-item uses (side/state.rs StateBook, references).

### Package-page dependency facts: uses, features gating, purpose, tree standing
surface: package page
class: NOT-BUILT
intent: package-browsing.md §1 "every dependency's role derived from evidence"; DepFacts fields.
code: desktop/src/shell/bodies/package.rs:262-282 dep_facts sets `newest: None, features: [], on_by_default: None, on_in_tree: None, uses: None, items: [], purpose: None, in_tree: None, tree_note: Some("your tree is not read yet")` (comment: "no producer reaching this page yet").
missing/unknown: every one of the dependency chip's hover facts other than name/kind/req/resolved is empty; the browse-S1 role vocabulary (crates/library/browse/roles.rs) that would supply `purpose` is not wired to the page.

### Judge: measured churn, release cadence and stability from real API diffs, closure cost vs your lockfile ("adds 2 crates")
surface: package page
class: NOT-BUILT
intent: package-browsing.md §5 S3; false-equivalence rule; ruling 7 "Measure the last 12 releases when a package is judged".
code: none found: grepped "closure", "brings", "shares with you", "churn", "cadence" in desktop/src and facet/src/folio: only cadence as ticker geometry; no judge route (harness scene desktop-judge was planned; harness.rs scenes list has none).
missing/unknown: whole slice; "cost to you: brings 1 new - shares 20" (MOMENTS Add/Discover) depends on it.

### Roles, alerts, duplicates in the Library tree (unmaintained crate with why-path, "60 crates are here twice")
surface: Library/home
class: BUILT-REAL
intent: package-browsing.md §2 S1.
code: crates/library/browse/{tree,roles,cargo}.rs, facet/src/browse/library.rs, desktop/src/runtime/browse_reads.rs.
missing/unknown: unreachable in the product (see Library tree record); Cargo-only; advisory alerts depend on the unconfigured advisory feed.

### Adoption preview "only in name" vs "reads differently" lines
surface: Find/browse/discover
class: NOT-BUILT
intent: package-browsing.md ruling 1; S4.
code: none found: the Compare page (facet/src/browse/compare.rs) shows evidence columns; grepped "only in name", "reads differently", "matched by name" in facet/src/browse: no equivalence engine.
missing/unknown: the false-equivalence defect is avoided by not attempting equivalence.

======== DATA THE INDEX MUST PROVIDE (gui-plan §8.5) — as consumed by the desktop ========

### Impl-block generics with bounds attached to every member
surface: symbol page
class: NOT-BUILT
intent: gui-plan.md:643 (`impl<R: Read> Deserializer<R>` -> new reads "from any Read").
code: none found: model/pages/symbol.rs carries SignatureText tokens and Member.signature; grepped "bounds", "generics" in desktop/src/model: no field; the simple page derives bound meanings from the written signature text (facet/src/anatomy/symbol/derive/callable.rs, known.rs).
missing/unknown: impl-level bounds are invisible to the page; Getting one / search by shape depend on it (both off the product page).

### Derives and resolved impls (trait id + member names); blanket and auto arrivals
surface: symbol page
class: PARTIAL
intent: gui-plan.md:642.
code: Arrival::{Direct, Blanket, Auto, NotReported} exists in model/pages/symbol.rs:299; Rose.implemented_by consumed at bodies/symbol/facts.rs:184-190.
missing/unknown: symbol-page.md §5 says blanket/auto arrival construction (R1/R2) was never started; "derived vs written" flag missing ("83 % derive" needs it: production-readiness.md:258); `can` capability chips come from implemented traits by name.

### Members with receiver kind, required/provided, trait-via
surface: symbol page
class: BUILT-REAL
intent: gui-plan.md:641.
code: desktop/src/model/pages/symbol.rs (Receiver::{Reads, Changes, Consumes, Makes, Unknown}); facts.rs:44-53 Obligation::{Required, Optional, Provided}.
missing/unknown: "trait-via" (which trait a method comes through: "through its traits" group) not surfaced on the simple page.

### Per dependency: every release with publish time and yanked flag
surface: releases/time travel/upgrade
class: PARTIAL
intent: gui-plan.md:644-645.
code: read from the cargo sparse-index cache on disk by the app itself (model/source_facts/registry.rs:41-49; data::ticker), not from the index; owner-side purl history exists (product_state.rs:436 per GAPS G13).
missing/unknown: cargo only; the cache exists only for crates cargo has fetched; production would need the registry index (network policy).

### Public API of each release present locally, keyed by stable path, re-exports resolved, normalized signatures
surface: releases/time travel/upgrade
class: PARTIAL
intent: gui-plan.md:645-646.
code: owner Diff (backend_library SurfaceCommand::Diff -> DeclarationChange::{Added, Removed, Changed, Indeterminate}) consumed by runtime/releases.rs:44-59 (uncompiled).
missing/unknown: paths are declaration labels with the tree prefix stripped (releases.rs item_path), not "stable path with re-exports resolved to the shortest alias"; signatures before/after not carried; whitespace/Self/lifetime normalization absent; `respelled` cannot be computed (no before/after).

### Workspace use sites of each dependency's items (path expressions, imports plus bare names, turbofish); method calls need type inference and the lens must say so if asked
surface: releases/time travel/upgrade
class: PARTIAL
intent: gui-plan.md:647-648.
code: SymbolPage.references (relation + confidence + byte span) mapped by runtime/page_mapping.rs and read into lines by runtime/workspace_lines.rs; Resolution::{Resolved, ByName} marks approximate rows on the symbol page.
missing/unknown: the release lens's `uses`/`impact` (Crate.uses, Crate.impact) come from the fixture; the real releases.rs leaves them empty; no "method calls not covered" disclosure string exists (grepped "type inference", "not yet covered" in facet/src/data/release: none).

### Typed relations with kinds (has, takes, gives, is, derives, impl, calls, uses, type) at member granularity; rolled-up edges derived in the view model
surface: graph/world
class: PARTIAL
intent: gui-plan.md:650-651.
code: SemanticLinkKind (Calls, MethodCall, TypeReference, Reads, Writes, Imports, Implements, Overrides, Reexports, Inherits, Documents; bodies/symbol/uses.rs:14-25) are the index's kinds; facet::graph::model rolls edges up from a World.
missing/unknown: the index's kinds are call/usage-shaped; the graph's has/takes/gives/derives/type relation vocabulary has no producer other than the prototype extractor (world.json), so the graph and the page disagree on what a "relation" is (production-readiness.md:91).

### Tour, cousins, chains, "how it fails" need whole-world tables (producer table, call graph)
surface: graph/world
class: PARTIAL
intent: gui-plan.md:487-509, 559-570, 593-605.
code: engines are in facet/src/semantics/{recipes,tour,fails}.rs and run on `World`; only recipes and tour are called from the desktop (fixture_world.rs: tour, PreparedRecipes).
missing/unknown: they need a `World` projected from the index (W-World unbuilt).

======== PLATFORM / PACKAGING / DISTRIBUTION ========

### macOS bundle (Info.plist, app icon, URL scheme, version)
surface: other
class: PARTIAL
intent: production-readiness.md:94-105.
code: apps/desktop/package-macos.sh (one binary + Info.plist), apps/desktop/macos/Info.plist: CFBundleShortVersionString 0.1.0 and CFBundleVersion 1 hard-coded, bundle id dev.nudox.desktop, no CFBundleIconFile, no CFBundleURLTypes, no document types; fonts embedded (facet/src/fonts.rs).
missing/unknown: no icon, no signing/notarization/hardened runtime, no updater, no crash reporter, no Gatekeeper story, no uninstall notes; the script calls `cargo build` directly; no Homebrew cask (only Formula/backend-mcp.rb).

### `nudox://` addresses open something
surface: foot/status
class: NOT-BUILT
intent: PLAN.md:72 "The address (nudox://...)", shell/jump.rs address; gui-plan trail/shared trails.
code: addresses are built and displayed/copied (shell/jump.rs:324-345, keys.rs CopyAddress, root.rs:1125) but no URL handler: grepped `open_urls`, `on_open_urls`, `CFBundleURLTypes` in desktop/src and Info.plist: none.
missing/unknown: pasting `nudox://present/glyph/RelationLabel` anywhere cannot open the app at that place; CLI/MCP links to places don't exist.

### Editor open for Windows/Linux (xdg-open) and configured template
surface: other
class: PARTIAL
intent: W-Sym6.md "Open in editor".
code: desktop/src/host/editor.rs:63-72 code -g, zed, then open/xdg-open (path only, no line).
missing/unknown: JetBrains, Sublime, Vim, Emacs need the Settings template that does not exist.

### Team spaces, shared trails, admin boards
surface: other
class: OPEN
intent: gui-plan.md:188 "Team spaces, shared trails and admin boards have no backend and are not built."
code: none (by decision).
missing/unknown: still explicitly out of scope; no plan.

### Hop trail sentence in the jump bar (`toml -> toml_edit <- cargo`)
surface: titlebar/jump bar
class: NOT-BUILT
intent: MOMENTS.md:31 (Hop).
code: none found: jump.rs builds path segments (package > module > declaration) and a settings/inbox "here" only.
missing/unknown: relation-aware trail unbuilt; the sidebar Trail lists places not relations.

======== W-WORLD AND W-SURFACES (queued in PLAN.md, wave-4 brief) ========

### W-World CP0-CP3 (owner-side World surface, desktop projection, delete fixture, columnar mmap format + LOD)
surface: graph/world
class: NOT-BUILT
intent: .local/lanes/wave6/world/BRIEF.md (checkpoints CP0 measurements + owner surface shape, CP1 owner surface + projection, CP2 desktop reads it and the fixture leaves the product path, CP3 columnar mmap + LOD, graph open <= 100 ms); PLAN.md:52-54 "Queued (Opus, when a slot frees)".
code: no lane ever ran (not in PLAN.md's 2026-09-29 roster; no .local/lanes/wave6/world/CP*.md); the index serves only "bounded graph neighbourhoods per symbol: Command::Graph(GraphNeighborhoodQuery)", Probe::Related and SurfaceCommand::{References, Dependencies, Dependents, Explore} (BRIEF.md:13-21); desktop still reads fixture_world.rs.
missing/unknown: whole lane; the same decision (ship the graph labelled, hide it until real, or schedule the data work) is owner decision 5 (production-readiness.md:365).

### W-Surfaces (wave-4 brief, minus Orbit): lenses wired to the release comb / a module row / a language mark
surface: overlays/popovers/hover
class: NOT-BUILT
intent: .local/lanes/wave4/surfaces/BRIEF.md item 1; PLAN.md:54.
code: facet/src/overlay/lens.rs has "zero call sites in apps/desktop" (BRIEF.md item 1) and still has none (grepped desktop/src for overlay::lens / lens_card: zero).
missing/unknown: never assigned after the brief; module rows and language marks have no lens; the release comb's hover shows the ticker label instead.

### Peeks for package (done), location and version (not done); peek "your uses" fact with its tier
surface: overlays/popovers/hover
class: PARTIAL
intent: BRIEF.md item 2; gui-plan.md:275 Peeks.
code: package peek built (desktop/src/shell/peeks.rs package_peek, "Dead end #16"); facet has PackagePeek/LocationPeek/VersionPeek (facet/src/overlay/peek.rs) but PageKey has no Location or Version variant (desktop/src/model/pages/key.rs:42-57: Symbol, Source, Package, Search, Orbit, Health, Browse) and peek_of falls to an empty SymbolPeek for other keys (peeks.rs:36-42).
missing/unknown: no location (file:line) or version peek anywhere; the peek's "your uses" line with the tier ("at least N places - matched by path" from ReferenceSite.confidence) is unbuilt for the peek card.

### Ask lists actions as rows (command palette) or the action catalog is deleted
surface: Ask/search
class: OPEN
intent: BRIEF.md item 4 (dead end #17): "Decide which, and write the decision down."
code: desktop/src/navigation/action.rs (414 lines: ActionId catalog with labels, accessibility metadata and 18 chords) is consumed only by Intent::Action in navigation/reducer.rs:214; Ask's rows are search results only (shell/ask.rs:235-256); grepped desktop/src for Intent::Action / ActionId:: outside action.rs and tests: reducer only.
missing/unknown: no decision recorded; the catalog's shortcuts are dead metadata (L2); Ask cannot "Toggle appearance", "Open settings" or "Add project" by name.

### Ask trailing hint folds at narrow widths; populated state matches Ask4
surface: Ask/search
class: PARTIAL
intent: BRIEF.md item 4.
code: desktop/src/shell/ask.rs:362-437 draw() and the plate over the shelf column.
missing/unknown: R6 "⌘K Ask invisible" at some widths was routed to W-Fluid (PLAN.md roster); no FLUID-C evidence for Ask at 320 px in the tree.

### The spine below 900 px: ancestors then siblings as kind marks, hover shows the name label, click goes there, never the flat module list
surface: sidebar
class: PARTIAL
intent: BRIEF.md item 7.
code: desktop/src/shell/side/view.rs:506-536 spine_column: a kind mark per top-level row (up to 24), the current one gets a 2 px mint bar, others 0.62 opacity that rises on hover (`.opacity(0.62)`/`.hover(opacity(1.0))`, view.rs:520-521), click performs the row's action.
missing/unknown: no hover name label or tooltip on a spine mark (the brief wanted a label, not a peek); marks are the sidebar's top-level rows (the lens's own rows), not the "ancestors then siblings" arrangement; opacity dimming is the ad hoc hover style the one-grammar law deletes.

### The naming rule (a package is named by its manifest name everywhere)
surface: shell/frame
class: BUILT-REAL
intent: BRIEF.md item 8; DIRECTION.md:22 (the shelf flattens nested modules; packages named by directory) "Packages are named by directory (toml-0.8.23)".
code: commit 8766d6460 "packages carry their manifest names (F-Data 2)"; orbit.rs:170-230 names with `apart` (release or folder) to tell twins apart.
missing/unknown: journeys assert `toml-pin-fixture` in the reader (J1.journey:19) i.e. project name vs package name differ (toml_pin folder vs toml-pin-fixture); ⌥ alias spelling for workspace short names is unbuilt.

### Orbit4 details: two rings of names, "new release" line, Map / List, OrbitRoute::Project(id) using its id
surface: Library/home
class: NOT-BUILT
intent: BRIEF.md item 5 and #6.
code: OrbitRoute::Project(ProjectId) exists (navigation/route.rs:73-78); the body ignores it (bodies/mod.rs:247 `Route::Orbit(_) => orbit::body(...)`); project tiles navigate to the project's package page instead (orbit.rs:83-98).
missing/unknown: a project has no page of its own besides its package page; per-project Library filter unbuilt.

### Source4: neighbours folded to one line, mint ticks on lines from the newest release, ⌥ spells ages and fold facts
surface: code view
class: NOT-BUILT
intent: BRIEF.md item 6; Source4-xray.png.
code: none found: source.rs draws one uniform text column with the declaration's gutter numbers mint; grepped "newest release", "ages", "fold facts" in source.rs and facet/src/code.rs: none.
missing/unknown: no per-line age, no folded neighbours, no ⌥ meaning in the code view (decision "what ⌥ x-ray means here" not recorded).

### Settings4: Text size row (a comb slider with a percent)
surface: settings
class: NOT-BUILT
intent: BRIEF.md item 3; Settings4.png.
code: none found: settings.rs comment says text size is not a setting (settings.rs:56-57); no comb slider control in facet::controls.
missing/unknown: contradicts the board target; the choice was made in code, not recorded as an owner ruling.

### Settings controls for RetryIndex / CancelIndex / TestConnection on Index and Connections pages
surface: settings
class: NOT-BUILT
intent: BRIEF.md item 3 (#17).
code: settings.rs index() has read-only rows; Retry lives on the Library failure card (onboard/failure.rs); Cancel and TestConnection have no controls (see records above).
missing/unknown: no re-index, cancel, or connection-test control on Settings.

======== INSTANT / HARNESS ITERATION (DIRECTION §2 last bullet) ========

### Harness fixture index cached by content hash (captures back to seconds)
surface: other
class: PARTIAL
intent: DIRECTION.md:61 (iteration speed) "Iteration speed is a feature... Cache it by fixture content hash."
code: instant-open.md §2.4 records 123 s -> 22-27 s from claim-index and journal fixes; harness state dirs cloned per lane (.local/harness/*, git-ignored); apps/desktop/src/harness/startup.rs (527 lines), harness/refusals.rs (486 lines: refusal record keyed by source mtimes).
missing/unknown: production-readiness.md:294-297: the refusal record is keyed by source mtimes, not the compiled owner and toolchain; every ClientError is recorded as an owner refusal; "13 of 13 refused, failed() empty" hides behind a Ready row.

======== FOUNDATIONS CHECKED AGAINST gui-plan §2.1-2.3 (built; recorded so they are not re-planned) ========

### Deterministic clock (every animation reads the executor clock; frames reproducible byte for byte)
surface: motion/transitions
class: BUILT-REAL
intent: gui-plan.md:65-72 §2.1.
code: vendor/gpui-ce NUDOX-PATCHES.md "Why: one clock"; facet motion reads `motion::now(cx)` (desktop status.rs:172 uses it for the whisper).
missing/unknown: none recorded.

### Motion engine (Curve, Spring, Keys, per-entity store, Pulse, Offset/Reveal, probe ledger)
surface: motion/transitions
class: BUILT-REAL
intent: gui-plan.md:97-115 §2.3.
code: facet/src/motion/{curve,spring,keys,store,element,pulse,flow,presence,shared,flight,carry,print}.rs; facet/src/probe.rs publishes tracks.
missing/unknown: Pulse leased by only button/progress/rose in practice (see Pulse record); durations table includes 380/620 ms tokens that DIRECTION.md:83 (durations 120-240 ms) contradicts.

### Read concurrency: pool of read sessions, latest-wins per view key, cancellation on navigate; index/admin on its own lane
surface: shell/frame
class: BUILT-REAL
intent: gui-plan.md:85-88 §2.2.
code: desktop/src/runtime/reads.rs (ReadPool, 3 sessions: host/launch.rs:35), ask.rs SETTLE 90 ms latest-wins, runtime/acquire.rs own worker.
missing/unknown: the owner is still one loop, so reads queue behind an index job up to a 15-minute timeout (production-readiness.md:69-77).

### Components read palette roles, never raw hex
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:302-303, 406-409.
code: grepped desktop/src and facet/src for `rgb(0x` / `hex(0x` / `rgba(0x`: only facet/src/tokens.rs and graph/sprite_checks.rs (a check).
missing/unknown: none.

### Hero name wraps at identifier boundaries and is never cut to "…"
surface: symbol page
class: BUILT-REAL
intent: gui-plan.md:546-547 §8.3.
code: desktop/src/shell/text_fit.rs (fit_name, name_lines, wrap_identifier) used by bodies/symbol.rs:120-160, bodies/package.rs:158-165, status.rs.
missing/unknown: gem above name below 560 px (40 px gem): PACKAGE_GEM/EMPTY_GEM fluid tokens exist; the symbol page's compact hero at 320 px overruns (D1).

### Every link on the symbol page peeks on hover (the graph's peek card, through the float layer)
surface: overlays/popovers/hover
class: BUILT-REAL
intent: gui-plan.md:618.
code: desktop/src/shell/bodies/symbol/host.rs:60-64 door(): `peek: Some(peeks::request(...))`; peeks.rs builds the card from the store.
missing/unknown: the card for an unindexed target is an empty SymbolPeek with only its name (peeks.rs:52-58).

### Peek on Space, again to pin; Esc peels the topmost transient
surface: keys/focus
class: BUILT-REAL
intent: gui-plan.md:168-169.
code: keys.rs:205 Peek "peek; again to pin", float::step_back/pin_top (gui-plan.md:386-387).
missing/unknown: peek and menu dismissal (grace + roll-up 220 ms) exceeds the 120 ms bar by construction (production-readiness.md:183).

### Tooltip (chamfered, delayed, one per window), menu, dialog, scrim, hint labels
surface: overlays/popovers/hover
class: BUILT-REAL
intent: gui-plan.md:143-144.
code: facet/src/overlay/{tooltip,menu,dialog,hint}.rs; desktop uses tooltip (onboard/library.rs stage tips), menu (titlebar siblings menu), dialog (onboard/add.rs), scrim (drawer, root.rs).
missing/unknown: toast unused (separate record); the add dialog is a float-layer modal but was reported not publishing a stack entry so the harness misreads its scrim (GAPS D4).

### Reader measure: folio max 1080 with a 250 px margin column and 34 px gutter
surface: shell/frame
class: SUPERSEDED
intent: gui-plan.md:157-158 §2.6.
code: desktop/src/shell/reader.rs:108-114 FOLIO = 784 px at 100 % text; MARGIN = 250, GUTTER = 34; only the code view uses a margin note (bodies/source.rs:62); the symbol page carries its own 860 px column + 280 px rail (anatomy/symbol/page.rs); the package page grows to 1800 (bodies/package.rs:152).
missing/unknown: the 1080 folio never shipped; three page widths (784 / 860+280 / 1800) coexist with no shared rhythm.

### No tabs / lens tabs on the symbol page (Reference / Relations / Usage / History)
surface: symbol page
class: SUPERSEDED
intent: DIRECTION.md:109-110 (reading order: no tabs) "There are no tabs; the section heading is the navigation, and the jump bar's › lists sections"; gui-plan.md:179 lens (Reference/Relations/Usage/History).
code: legacy enum Lens{Reference, Relations, Usage, History} survives: desktop/src/shell/bodies/mod.rs:107 `Ctx.lens`, reader.rs:384,438,550-674 `set_lens`, per-Place `lens: Lens::Reference`; no body reads ctx.lens (grepped `ctx.lens`: zero reads).
missing/unknown: dead plumbing; the jump bar's `›` section list ("the jump bar's › lists sections") is not built (jump segments list siblings, not sections: titlebar.rs:689-740).

### Contents unfurled by default: "symbol words all at once" (docs.rs All items)
surface: package page
class: PARTIAL
intent: MOMENTS.md:13-14, 30 "Words, all at once... Contents lists every public name by module, unfurled by default... shingles remain the browse overview and the folded state. Opening a package unfurls them."
code: shell/bodies/package/folio.rs: the territory shows shingles (one per name, no words); a module opens on click into cards; modules over 14 open a dedicated view.
missing/unknown: the words are not visible until a module is opened; there is no flat all-names view; owner: "on package pages it needs to either come already unfurled or be easy to do so (like docs.rs)" — easy to do so is one click per module, not one for all; no "unfurl all" control found.

### Contents and versions are the same thing; no Versions lens
surface: sidebar
class: SUPERSEDED
intent: MOMENTS.md:15 (Contents and versions are one thing) "The sidebar's lenses are Contents at 1.53.1 - Rests on - Used by. There is no Versions lens."
code: W-Side built four lenses including Versions (desktop/src/shell/side/lens.rs:2, listing.rs:351); the sidebar version comb (side/view.rs) plus ticker on the package page both re-scope.
missing/unknown: the MOMENTS ruling (2026-09-28 evening) postdates COHESION's four-lens strip that W-Side implemented; the Versions lens is built and the board's replacement (Contents "at 1.53.1" with words recolouring in place) is not; nobody reconciled the two.

### A region opens in place into the module's discoverable section (kind, shape, one-line doc, members on demand)
surface: package page
class: PARTIAL
intent: COHESION.md:5-12 (owner corrections) owner correction.
code: facet/src/folio/module.rs, cards.rs (kind mark, name, kind word, first sentence, badges).
missing/unknown: "members on demand" (expand a type to its members inside the card) not found (grepped cards.rs for members/expand: none); card badges come from signatures only.

### Release comb popover: hover a tick gives the list with dates; bars widen apart; label rides inside
surface: releases/time travel/upgrade
class: BUILT-REAL
intent: MOMENTS.md:21-26 (five facts: Releases); W-Folio.md owner words on the ticker.
code: facet/src/folio/ticker.rs (fisheye, rider label).
missing/unknown: dates exist for cargo-cached releases only; undated ones are spaced evenly.

### Pointer cursor over hand-written elements (shingles, ticker, berg, crest cells)
surface: overlays/popovers/hover
class: NOT-BUILT
intent: standard affordance; production-readiness.md:236.
code: grepped `cursor_pointer` in facet/src/folio and shell/bodies/package: zero hits.
missing/unknown: the arrow cursor over clickable package-page parts.

### Remove-project confirmation or undo
surface: overlays/popovers/hover
class: NOT-BUILT
intent: gui-plan rule 5 toast "says it once, undo"; onboard/commands.rs Remove is Weight::Danger.
code: none found: grepped desktop/src for "confirm" / "are you sure": none; ProjectCommand::Remove dispatches Intent::RemoveProject immediately (onboard/commands.rs:78-82); the owner keeps what it indexed.
missing/unknown: destructive action without confirm or undo; no toast channel to offer Undo.

### Copy signature / copy path actions (hand reason Copy)
surface: hand/holds
class: NOT-BUILT
intent: model/hand.rs:1-8 "a copied signature"; HeldWhy::Copy.
code: clipboard writes exist only for the address (shell/root.rs:1128) and the install line (facet/src/marks/eco.rs:361).
missing/unknown: no copy-signature, copy-path or copy-as-link on the symbol page, rails or uses rows.

### Jump bar `›` lists sections of the page
surface: titlebar/jump bar
class: NOT-BUILT
intent: DIRECTION.md:109-110 (reading order: no tabs).
code: none found: jump.rs segments = package > module > declaration; grepped "sections" menus in titlebar.rs: none.
missing/unknown: no in-page section navigation for long pages (symbol page, package page).

### Keyboard hint for peek/holds/graph in the foot
surface: foot/status
class: OPEN
intent: gui-plan.md:158 status bar "hold ⌘ for keys".
code: see foot record.
missing/unknown: no decision whether ⌘ caps alone suffice.

### Ask "or ask": natural-language / semantic search
surface: Ask/search
class: PARTIAL
intent: ask placeholder "Find a name, or ask" (desktop/src/shell/ask.rs:81); gui-plan.md:181 "Ask (⌘K): ranked mixed results, one reason per row, preview"; PLAN.md one query.
code: ask.rs:330-350 semantic_search_label and :384 print "semantic search available/unavailable/stale · reason" beside results; rows carry MatchReason::{ExactName, Name, Signature, Docs, Producer} (ask.rs:274-287).
missing/unknown: semantic search needs a configured embedding provider (SemanticSearchReason::Unconfigured is the default state); there is no UI to configure one (no settings page), so "ask" resolves to the same lexical search; "one reason per row" is said only for signature/docs (name matches say nothing).

### Ask preview: walking results previews each place in the reader without history
surface: Ask/search
class: BUILT-REAL
intent: gui-plan.md:181 "preview"; PLAN.md:59-66.
code: desktop/src/shell/ask.rs:1-12 (Intent::Preview / CommitPreview; Esc and Back restore).
missing/unknown: zero-result and searching states have words ("Nothing matches that yet.", "Searching the library…", ask.rs:405-413) but J9 does not assert them.

### Graph "yours" (mint footprint: your code and every dependency symbol it reaches; "you use N" on dependency packages)
surface: graph/world
class: BUILT-FIXTURE
intent: gui-plan.md:445 §8.2 Colour.
code: facet/src/graph/model.rs:191 `Package.yours` and yours_in (model.rs:442,556-576) come from the loaded World; the desktop's World is the prototype's (world.json flags the extraction workspace's own crates as yours); desktop/src/shell/bodies/graph/identity.rs:362,369 sets yours only in test worlds.
missing/unknown: in the product, mint in the graph means "one of the backend workspace's own crates" as of the snapshot day, not the active project's code; the person's own project (a toml_pin, say) does not appear at all.

### UI icon set with no product consumer (21 of 50)
surface: other
class: GALLERY-ONLY
intent: gui-plan.md:135 "UI icons (50)".
code: grepped `Icon::X` over desktop/src and facet/src excluding icons/, gallery and tests for each of the 50 ui icons: no consumer for Bell, Bookmark, Comb, Copy, Crown, Cursor, Grid, Heart, History, Link, List, Lock, More, Msg, Note, Peek, Rose, Tag, Target, User, Users (a few, such as Lock, may reach the screen through a different glyph enum: features.rs draws its padlock itself).
missing/unknown: the unused set is a map of unbuilt affordances: Grid/List (Home Map/List toggle), Bell (notifications/inbox unread), Copy (copy actions), History (a history page beyond the sidebar Trail), Link (copy link), More (overflow menus), Users/User/Heart/Bookmark/Crown/Msg/Tag (teams, favourites, discussion, labels: gui-plan.md:188 says team features are not built).

### Kind marks 20, modifier marks 17, capability marks 14, language marks 7
surface: theming/density/contrast
class: BUILT-REAL
intent: gui-plan.md:135-136.
code: facet/assets/icons/{kind,mod,cap,lang} contain 20, 17, 14 and 7 svg files (counted); Kind/Mod/Cap/Lang enums in facet/src/icons/set.rs.
missing/unknown: modifier marks (abstract, async, auto, blanket, changes, const, consumes, crate, deprecated, derived, generic, inherited, makes, override, reads, static, unsafe) reach the product only through the symbol page's rail/verb glyphs and package-card badges; a "modifier is a mark with a tooltip" law (house rule 3) is not checked by a lint.

### Native application menu bar, ⌘Q, ⌘H, Edit menu (copy/paste), Window menu
surface: shell/frame
class: NOT-BUILT
intent: not specified in the design documents; a shipping macOS app needs it (production-readiness.md:94-105 packaging list omits it).
code: none found: grepped desktop/src and facet/src for `set_menus`, `cx.quit`, `Quit`, `cmd-q`/`secondary-q`: zero hits; the platform layer offers `App::set_menus` (vendor/gpui-ce/src/app.rs:2443) and vendor/gpui_ce_macos/src/platform.rs builds NSMenu, but nothing in the product calls it.
missing/unknown: whether the bundle shows a default application menu with Quit is not evidenced in the tree; no Edit menu wiring (copy/paste work only inside gpui_component inputs); no About panel; no "Add Folder…" File menu item to mirror ⌘O.

### MCP / agents setup guidance (open setup, copy the command, test the connection)
surface: settings
class: NOT-BUILT
intent: navigation/journey_specs.rs McpSetup (retired spec); GAPS L1; Settings > Agents.
code: Intent::TestConnection at desktop/src/runtime/ui_graph.rs:228-241 and SettingsState.connection: ConnectionStatus exist; Formula/backend-mcp.rb ships the MCP server for Homebrew; no surface in the app shows the setup or dispatches the test.
missing/unknown: the desktop never tells a person that an MCP server exists or how to attach an agent to the same local index.

### Budget contradiction: search results <= 50 ms after the last keystroke vs the Ask debounce
surface: Ask/search
class: OPEN
intent: gui-plan.md:224 (search <= 50 ms after the last keystroke); DIRECTION.md:35-43 (budgets: main-thread task <= 4 ms) (keystroke to repaint <= 8 ms p99).
code: desktop/src/shell/ask.rs:34 `SETTLE = 90 ms` "How long typing rests before the query is asked"; the read pool then answers.
missing/unknown: by construction results cannot land within 50 ms of the last keystroke; either the debounce or the budget must change; no release measurement exists either way.

### Budget contradiction: hover prefetch 120 ms (built) vs 80 ms or fast approach (DIRECTION), peek 350 ms vs 280 ms vs 120 ms
surface: overlays/popovers/hover
class: OPEN
intent: DIRECTION.md:56 (prefetch: 80 ms), :92 (350 ms peek); COHESION.md:28-48 (state grammar: hover) (280 ms); gui-plan.md:89 (120 ms).
code: desktop/src/shell/kit.rs:20 PREFETCH_DELAY 120 ms; facet/src/overlay/float.rs:148 QUICK_REST 120 ms; hover.rs doc says 350 ms; W-Feel bar: peek visible <= 150 ms after rest, gone <= 120 ms.
missing/unknown: no single owner-approved set of timings; production-readiness.md:364 item 4 asks the owner for the 120 ms dismissal ruling only.

======== RESTS ON / USED BY (MOMENTS item 5, COHESION "Versions, dependents, dependencies") ========

### Rests on: sorted by weight with version, proc-macro / build.rs pills, dependency count and lines; ▸ unfolds its own dependencies in place, a layer at a time
surface: sidebar
class: PARTIAL
intent: MOMENTS.md:29,31 (Rests on | Used by, Hop) item 5; COHESION.md:155-163 (Rests on).
code: only the sidebar lens exists: desktop/src/shell/side/listing.rs:592-622 rests_on(): a "Directly N" heading, one row per dependency with its requirement and scope word (dev / build / optional), dimmed when not resolved to an indexed release; no page section.
missing/unknown: no weight order, no version-resolved text (shows the requirement, e.g. ^1), no proc-macro/build pills, no dependency count or lines, no in-place unfold of transitive layers (nothing lists a dependency's own dependencies), no indexed-vs-dashed distinction beyond dim.

### Used by: your crates first with how often, then the rest folded; "as a graph" opens the relay; "How desktop uses toml"
surface: sidebar
class: PARTIAL
intent: MOMENTS.md:29 (Rests on | Used by); COHESION.md:155-163 (Used by).
code: desktop/src/shell/side/listing.rs:625-680 used_by(): "Yours N" (crate rows with "N items" and mint use count, choosing one narrows Contents) then "In the library" dependents with their version.
missing/unknown: no "as a graph"; no per-item call-site view ("How desktop uses toml": call sites grouped by item with the used name underlined and "the rest of toml (24 items) it never touches"); the dossier's dependents are rows from the registry index (empty for local roots: GapReason::LocalProject, shell/tests.rs:153).

### Strata: hover a block lights the chain from your code down to it; one line says why it is here ("desktop ... use toml -> toml reads through toml_edit -> toml_edit parses with winnow")
surface: package page
class: NOT-BUILT
intent: COHESION.md:155-163 (Rests on strata); MOMENTS.md:33-38 (Browse: strata, hover, loupe, type-to-light) core drawn up through the layers.
code: none found: no path-to-root computation in desktop/src (grepped "why it's here", "chain from your code", "path to your"): none; the owner serves Dependencies / Dependents surfaces but no path.
missing/unknown: "why is this here" is the strongest cohesion claim (NAV.md research: no other tool shows every path); entirely unbuilt.

### Duplicates called out; what removing a package would drop
surface: package page
class: NOT-BUILT
intent: COHESION.md:155-163 (duplicates, removal what-if) "Duplicates are called out, and so is what removing a package would drop."
code: partial precedent only in the unreachable Library tree page (facet/src/browse/library.rs "packages that are here twice"); grepped desktop/src for "would drop", "here twice" outside browse tests: none.
missing/unknown: no removal what-if anywhere.

======== MARKS FAMILY (D-Marks) ========

### license_mark, version_mark (Rider / Baseline / band), semver 'how far behind', dep card 'what you use each for'
surface: package page
class: SUPERSEDED
intent: facet/src/marks.rs:1-22 (wave-4 D-Marks); DIRECTION.md:128-133 (The package page) "hero carries the marks: ecosystem, license, version comb and dependencies (W-Marks' components)".
code: desktop mounts only ecosystem_mark and dep_line from facet::marks (bodies/package.rs:15,186-237) and LicenseFacts as the crest's input (package/data.rs:17,397-403); grepped desktop/src for license_mark / version_mark / VersionFacts: zero. The folio's crest stamp and ticker replaced them.
missing/unknown: marks/version.rs (593 lines), marks/semver.rs (331), marks/license.rs LicenseMark and marks/card.rs remain alive in facet with the gallery; the "how far behind you are" measure (semver.rs) is computed nowhere in the product page (the ticker colours newest amber but says no distance).

======== PROCESS EVIDENCE GAPS (claims the plans require that the repo cannot show) ========

### Journey mutation proofs (each of J0, J7, J8, J11, J12: a marker-guarded product mutation makes the journey fail, REPORT line quoted)
surface: other
class: OPEN
intent: W-Journey.md §5; production-readiness.md:292 "Each key journey needs a product mutation that makes it fail."
code: none in the repo: the lane files (.local/lanes/wave6/journey/) are git-ignored; apps/desktop/journeys contains no mutation scripts.
missing/unknown: unknown whether any journey has ever been proved to bite; J7/J8 do not exist.

### Journey budgets are judged in release only
surface: other
class: OPEN
intent: gui-plan.md:224; W-Journey.md §6.
code: apps/desktop/journeys/J1.journey:20,32,50,60 `budget page-open <= 120ms` and :81 `budget frame-p95 <= 8ms`; the runner measures virtual time in debug and only judges in release (W-Journey brief).
missing/unknown: no release build has been judged (production-readiness.md:266).

### Responsive matrix and storms as standing gates
surface: fluid widths
class: PARTIAL
intent: gui-plan.md:205-216 §3.5-3.6.
code: facet/src/gallery/{storm,matrix,soak}.rs; `backend-desktop-gui-harness matrix|storm|lint|verify` binary.
missing/unknown: `matrix --scene desktop-graph` and `lint --scene all` end with "the product never went quiet within 120s" under load (production-readiness.md:302); `desktop-record-rust` panics at boot with "no exact candidate" and stops `capture --scene all` after 16 scenes (line 82).

### Marks explain themselves: a tooltip on every glyph, and a Legend page
surface: overlays/popovers/hover
class: NOT-BUILT
intent: gui-plan.md:32-34 house rule 3 ("a modifier is a mark with a tooltip"); v4 "breakdowns on every bar"; SettingsPage::Legend ("Semantic legend").
code: tooltips reach the desktop only through onboard/library.rs stage tips (facet::overlay::tooltip, library.rs:24,58-68) and facet's badges/icon_button/chrome; the sidebar's state glyphs (mint notch, amber diamond, coral words, member count: desktop/src/shell/side/glyph.rs) have no tooltip or hover (grepped side/glyph.rs, row.rs, view.rs for tip/tooltip: none); the symbol page's outcome and verb glyphs (anatomy/symbol/ink.rs) and rail cap chips carry words but no tooltip (grepped anatomy/symbol for Tipped / .tip(: none); SettingsPage::Legend renders the Keys page (settings.rs:25-32).
missing/unknown: no place explains "what does the dashed ring / coral block / clock / amber diamond / mint notch mean"; the design-system semantic legend was never drawn as a page or reachable overlay.

### Package page lens tabs (Map / Readme / Depends / Used by / Changes) and symbol lens tabs
surface: package page
class: SUPERSEDED
intent: gui-plan.md:177-179 §2.7 (lens tabs on Package and Page); DIRECTION.md:109-110 no tabs; COHESION.md:62-92 "There are no tabs".
code: none on the pages (bodies/package.rs draws one column: hero, folio, README); legacy Lens enum remains in shell/bodies/mod.rs:147-171 and reader.rs (see no-tabs record).
missing/unknown: the README is the only "Readme" content and sits last, unconditionally; a "Changes" view is the ticker/banner; "Depends / Used by" moved to the sidebar lenses.

### Every sub-part of every mark hoverable into a lens or tip (W-Data ownership)
surface: overlays/popovers/hover
class: PARTIAL
intent: gui-plan.md:357 W-Data "making every sub-part of every mark hoverable into a lens or tip"; facet/src/data.rs:1-14 Door.
code: implemented for facet::data marks via Door (data/door.rs, 306 lines); desktop data marks are not mounted, and the package page's parts are hand-drawn (ticker bars and crest cells have their own hover, berg blocks surface a plate).
missing/unknown: the Door contract (rest -> exact sub-rect -> float layer, keyboard walks parts, Space opens, Enter descends) is honoured by no mounted desktop component except the dependency chips and dep-line; the ticker's ticks, the berg's blocks, the crest's cells, the shingle regions and the sidebar's glyphs each invented their own hover/keyboard behaviour.

======== OPEN DECISIONS MADE EXPLICIT (owner-only calls the code is waiting on) ========

### Network policy: when may the product download a crate or read the registry index, and how is consent shown
surface: Find/browse/discover
class: OPEN
intent: production-readiness.md:119,361 (decision 1); W-Acquire.md ("agents never download"); SettingsState.privacy exists.
code: RegistrySource resolves offline only (desktop/src/host/registry.rs, registry/archive.rs); Availability::Download renders as "needs a download" and a disabled Add (facet/src/browse/acquire.rs:34-52); crates/local-service/src/builtin/registry.rs:802 `acquire` (purl path) FETCHES over the network and is what the owner would use if allowed.
missing/unknown: nothing decided: no consent surface, no "available offline vs would download" copy beyond the label, no Settings control; Discover, Find over the registry, earlier releases not in the cache, advisories and downloads/popularity all wait on it.

### A shell launch inside a project folder auto-admits it and skips the first run
surface: onboarding/first run
class: OPEN
intent: production-readiness.md:207,363 (decision 3); GAPS G9.
code: desktop/src/host/launch.rs:227-229, model/persistence.rs:1045-1097, host/paths.rs:74-86.
missing/unknown: keep or change is the owner's call; journeys use the ambient (Finder) launch so the difference is untested.

### Quit leaks the owner watch's handles (debug builds panic on quit)
surface: other
class: PARTIAL
intent: GAPS D2; production-readiness.md:208.
code: desktop/src/runtime/owner.rs `watch` holds root and store strongly.
missing/unknown: a real process exits there so users don't see it; the journey machine works around it (harness/journey/machine.rs `quit`); no fix recorded.

### Reader widths: three column systems (784 folio, 860 + 280 rail, up to 1800 territory)
surface: fluid widths
class: OPEN
intent: gui-plan.md:157-158 (folio 1080 + 250 margin); W-Fluid.md §2 "wider measure for wide content".
code: shell/reader.rs:108-114 FOLIO 784; anatomy/symbol/page.rs 860 + 280; bodies/package.rs:152 PAGE_MAX 1800; Ctx::wide (bodies/mod.rs).
missing/unknown: no single rhythm or owner ruling; the 2560 look is on the owner's list (production-readiness.md:364).

### Amber means caution (DIRECTION) vs amber means changed (COHESION, sidebar)
surface: theming/density/contrast
class: OPEN
intent: DIRECTION.md:154 vs COHESION.md:28-48 state grammar and primitive 4.
code: desktop/src/shell/side/state.rs and folio shingles use amber for "changes in the target release"; symbol page amber for "changes it" (receiver) and undeclared-type notes (side.rs:150 `how` in amber).
missing/unknown: three meanings of amber (caution, changed-in-release, receiver mutates) share one hue; no ruling.

### Foot vs jump bar: where the address lives and what clicking it does
surface: foot/status
class: OPEN
intent: PLAN.md:72 vs shell/status.rs:1-5.
code: status.rs moved the address out; jump.rs shows it on hover; ⌘⇧C copies.
missing/unknown: whether the foot should regain a clickable address with Copy link / Open in editor / Reveal (PLAN) or stay empty at rest.
