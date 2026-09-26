# Nudox desktop: the GUI plan

Owner of this contract: the lead session. Lanes build against it; the lead
vets every lane at a checkpoint (screenshots, filmstrips, stress output, code
read) before it is merged into the plan's "done" column.

The design source of truth is the FACET v3 board set ("cut, not painted"):
`Nudox-Design-System/boards/*.dc.html` (serve the folder with
`python3 -m http.server 47811 --bind 127.0.0.1` and open
`http://127.0.0.1:47811/boards/<Board>.dc.html`). The extracted token spec,
icon paths and research notes live beside it in `Nudox-Design-System/spec/`.

## 1. What we are building

A local-first desktop docs reader for code across seven ecosystems (cargo,
npm, PyPI, Go, Maven, NuGet, Conan) that is better than docs.rs: every symbol
is a page, every page shows its relations, every depth is one zoom away, and
the whole thing is playful without ever being slow.

Depth model, one zoom: **Orbit** (your projects and everything around them)
› **Package** (one package as a mosaic) › **Page** (one symbol and everything
it touches) › **Source** (the deepest depth). The titlebar is the trail.

### House rules (non-negotiable; the owner's taste, learnt over three rounds)

1. **Cut, not painted.** Flat tones only. No decorative gradients. Depth comes
   from the two-tone bevel and the faceted ground, never from a gradient fill.
2. **The bevel is the only state channel.** Focus = doubled periwinkle bevel;
   yours = mint; working = light travelling the edge; waiting = amber;
   stopped = coral; pending = hatched. **No status dots, ever. No flat status
   pills** ("approved", "closest version").
3. **Icons all the way down.** No uppercase property labels (`CONST`, `ASYNC`);
   a modifier is a mark with a tooltip. Kinds are 12-facet gems: hue = family,
   shape = kind.
4. **Plain until touched.** Timelines are tick combs that wave and pop a card
   on hover; packages are mosaics; progress is a gem filling facet by facet or
   a seam. Hover reveals meaning.
5. **Say it once, quiet until asked.** A fault is said once, in place, in plain
   words, with the exact operand; everything else only dims or hatches.
6. **Usage and source are secondary metrics** — present, never shouting.
7. **Four faces, one job each.** Bricolage Grotesque (display, ≥16px),
   Geist (UI), Geist Mono (every identifier, path and version), Newsreader
   italic (ledes, captions, margin notes).
8. **Motion is meaning.** Things arrive (drop, land, make room), seal, wave,
   descend, peel. Every animation is interruptible, retargetable, reduced-motion
   aware, and settles to exactly the layout position.

## 2. Architecture

```
vendor/gpui-ce                 gpui 0.2.2 + deterministic-clock patch (+ small APIs we need)
vendor/gpui_ce_components_base gpui_base 0.2.0 + deterministic-clock patch
vendor/gpui_ce_components      gpui_component 0.2.0 (already vendored)
apps/facet   (backend-facet)   the design system: tokens, fonts, icons, motion,
                               paint primitives, controls, overlays, data marks,
                               the gallery binary (`facet-gallery`)
apps/desktop (backend-desktop) shell, routes, pages, runtime (engine actor), harness adapter
tools/gui-harness              generic headless capture, input, filmstrips, stress, lints
```

`backend-facet` depends only on gpui, gpui_component and small utility crates.
It never depends on the engine. It builds and links in seconds, so all
component work iterates in `facet-gallery`, not in the 600 MB desktop binary.

### 2.1 Clock and determinism (load-bearing)

GPUI's `with_animation` reads the wall clock (`web_time::Instant::now()`), so
headless captures cannot step it. The vendored gpui and gpui_base are patched
so every animation reads `cx.background_executor().now()`, which is real time
in the app and the controllable `TestClock` in the harness. Facet's motion
engine reads the same clock. Consequence: any frame at any virtual time is
reproducible byte for byte.

### 2.2 State and async

Keep (audited solid): `runtime/{actor,mailbox,coordinator,mapping,client}`,
`model/{persistence,workspace,local_package,selectors,viewport}`,
`navigation/*`, `core/*`, `host/*`. Change:

- **Event-driven wake.** The actor wakes the UI through an async channel that
  a `cx.spawn` task awaits; no per-frame polling while requests are in flight.
  An idle window requests zero frames.
- **Keyed resources.** Page, package, source, graph, references and search
  results are separate `Resource<T>` maps keyed by identity with LRU bounds —
  fixes the shared `CatalogState` slot corruption.
- **Read concurrency.** A small pool of read sessions (latest-wins per view
  key, cancellation on navigate); index/admin work on its own lane.
- **Hover-intent prefetch.** Hovering a link for 120 ms prefetches its page;
  the descent animation hides the rest.
- **Region entities.** The window root composes region views — titlebar,
  shelf, reader, overlays — each its own `Entity` that re-renders only when its
  slice of the snapshot changes (equality-gated `notify`). Animated regions
  are isolated so a hover wave never re-renders the page.
- CPU work (highlighting, markdown, layout of large code) runs on the
  background executor; results land through `WeakEntity::update`.

### 2.3 Motion engine (`facet::motion`)

- `Curve`: cubic-bezier evaluator for the tokens `glide (.22,1,.36,1)`,
  `snap (.3,0,0,1)`, `spring (.2,.9,.25,1.18)`, `bounce (.34,1.56,.64,1)`;
  overshoot allowed.
- `Spring`: analytic, frame-rate independent, velocity-preserving on retarget.
- `Keys`: keyframe tracks for compound shapes (drop-in squash/stretch).
- `Motion` store per entity keyed by stable ids; `animate(key, target, spec)`
  in render; frames are requested only while a track is live; one OR-gate per
  window decides whether another frame is needed.
- `Pulse`: a leased shared ambient clock (≤ 12 fps) for slow ambient motion
  (ground twinkle, running bevel, flowing strands, working gem). Released when
  nothing leases it, paused when the window is inactive, off under reduced
  motion, frozen at a fixed phase in the harness unless a scene samples it.
- Elements: `Offset` (translate without affecting layout), `Reveal` (clip),
  opacity; custom-painted primitives take a scale.
- Durations: micro 90, quick 160, std 240, emph 380, scene 620 ms.
- **Probe ledger**: every live track publishes `(key, value, target, velocity,
  started, budget)` per frame to a probe the harness reads.

### 2.4 Paint primitives (`facet::paint`)

- `Cut`: the chamfered plate (top-left + bottom-right 45° cuts; 9/14/22 px),
  flat fill, 1 px hard two-tone bevel (hi top-left, lo bottom-right), states
  focus (doubled peri), hot, run (light travels the edge), amber, coral, ghost
  (hatched), two, deep, weave, lift. Shadows for floating plates are painted
  outside the clip. Point-in-polygon hit testing.
- `Hatch`: the universal pending texture (diagonal and 90° variants,
  optionally running).
- `Gem`: 12 facets with the fixed top-left lighting map, table, kind glyph,
  progress 0..12, states todo/working/stalled/cracked/glint.
- `Ground`: the faceted low-poly field behind every window; twinkles on the
  pulse clock; one custom element, cached.

### 2.5 Component inventory (`facet`)

Controls: button (primary/edge/ghost/danger, sizes, facet sweep hover, press,
busy hatch, disabled), icon button, kbd (hot/hint), segmented control, switch,
diamond check and radio, comb slider, text input + select trigger (wrapping
gpui_component input for IME), splitter.
Marks: UI icons (50), kind marks (20), modifier marks (17), capability marks
(14), language marks (7), logo, chevron/twisty.
Data: comb (horizontal/vertical/down, neighbour wave, popup card), mosaic
(stones: public/new/gone/gate/lit, dimmed), seam (done/now/stall/bad/todo),
strands + nodules (written/via/flow), rose (up is / down made of / left from /
right to).
Overlays: tooltip (chamfered, delayed, one per window), float/popover, menu,
toast (says it once, undo), dialog, peek card, scrim, hint labels.
Chrome: titlebar thread of beads + here capsule + altimeter, shelf rows and
book header, kspine, status bar.

Reuse from gpui_component (behaviour, restyled): `Root`, input/IME engine,
resizable panels, virtual list, tree, popover positioning, highlighter
(tree-sitter features for rust, typescript, javascript, python, go, java,
c_sharp, cpp enabled), markdown `text` for readmes and docs prose.

### 2.6 Shell (`apps/desktop`)

Titlebar 50 px (traffic lights, shelf toggle, altimeter, bead thread with the
here capsule, ⌘K ask field on Orbit, trail/inbox buttons). Shelf 264 px,
resizable 200–420, collapses to a 42 px kspine. Reader: folio max 1080 with a
250 px margin column (34 px gutter). Status bar 26 px (mono address, "hold ⌘
for keys").

Responsive rules: < 1100 margin notes fold under their paragraph; < 900 shelf
becomes the kspine; < 640 the spine becomes an on-request overlay; ≤ 480 the
rose becomes a list and the thread keeps only "here". Text scale 85–200 % via
rem size; every rule is re-resolved from measured widths, never from the
window size alone.

Keys: J/K or ↑↓ move focus (the bevel light walks), Space peek, S peel to
source, F hint mode, ⌘K ask, ⌘[ / ⌘] walk the thread, ⌘- surface one depth,
⌘. zen, ⌘\ shelf, Tab zones, Esc closes the topmost transient.

### 2.7 Pages and their data

| Route | Backend | Notes |
|---|---|---|
| Orbit | projects, explore, tree | rings map + list; language comb; add flow |
| Add flow | index-search, add, health progress | rows drop in, list makes room, gem fills by stage, seam |
| Package (Territory) | package, package-versions, dependencies, dependents, outline | hero, release comb, lens (Map/Readme/Depends/Used by/Changes), mosaic per module |
| Page (symbol) | show/read, related/graph, references, outline | hero gem, lens (Reference/Relations/Usage/History/Source), rose, signature x-ray, members ledger, in your code |
| Trait/class pages | same, Implements/Inherits/Overrides edges | blanket arrivals dashed; implementors mosaic |
| Source | source excerpt + spans, references | code with gutter, peek card, doc in margin |
| Ask (⌘K) | search, index-search, names | ranked mixed results, one reason per row, preview |
| Search results | search (paged) | filters as combs/marks |
| Settings | settings, health, capabilities | appearance, index and registries, keys |
| Onboarding | projects, index, health | dependencies arrive on the rings; seam of stages |
| Inbox | releases, subscriptions | followed releases, calm |
| States | health coverage, faults | offline banner, loading skeletons, empty rooms, fault anatomy |

Team spaces, shared trails and admin boards have no backend and are not built.

## 3. Verification

Nothing is done until the harness says so and the lead has looked.

1. **Gallery captures.** Every component state is a gallery scene, captured
   headless at a fixed virtual time and composed into contact sheets beside
   the matching board crop.
2. **Filmstrips.** Every motion is sampled at fixed virtual times into a strip
   (plus an onion-skin overlay) and read frame by frame.
3. **Motion probes.** From the probe ledger: no track jumps between frames
   (continuity), every track settles within its budget, the settled value
   equals the laid-out position, and a settled scene requests zero frames.
4. **Settle equals fresh.** After any journey and settle, the frame must be
   pixel-identical to a fresh boot into the same state. Catches stuck hover,
   stale overlays, half-finished transitions.
5. **Storms.** Seeded random action sequences: open/close overlays dozens of
   times per frame, resize storms, text-scale changes mid-animation, navigate
   spam, rapid hover sweeps. Invariants per frame: no panic, focus on a
   visible element, overlay stack consistent, no text overflowing its box,
   frame budget met, no leaked tasks or entities.
6. **Responsive matrix.** Widths 480/640/760/900/1100/1280/1440/1920/2560 ×
   text scale 85/100/125/150/200 × Abyss/Glacier × motion on/off.
7. **Layout lints.** Text clipped without ellipsis, sibling text overlap,
   focusables off-viewport, hit targets under 24 px, contrast under 4.5:1.
8. **Frame budget.** Render+layout+paint timings per frame in release:
   p95 under 8 ms at 1440×900, under 12 ms at 2560×1440.
9. **Content truth.** Page scenes assert the rendered text against the backend
   reply (signature, doc fragments, member names), not just structure.
10. **Journeys: the end-to-end acceptance (binding).** A journey is a person's path through the real desktop app (`backend-desktop`, not a gallery stand-in) over a pinned local index.
    - It is scripted as acts (key, type, hover, click by probe id, resize, text-scale, wait-settle) with **content assertions** at named checkpoints: the probe text that must be on screen, the route, the focused element.
    - Each journey is filmed (strip + motion report).
    - It passes only when:
      - every checkpoint's content is true;
      - continuity and settle are clean, settle == fresh at the end, and there are no lints on any checkpoint frame;
      - the budgets hold in release: page open ≤ 120 ms to the first complete frame, search results ≤ 50 ms after the last keystroke, any flight ≤ 1500 ms, p95 frame ≤ 8 ms at 1440×900.
    - A journey that passes on a stand-in, or that asserts counts instead of content, is not a journey.

    | # | journey | checkpoints (content that must be true) |
    |---|---|---|
    | J1 | **first look**: launch → Orbit → your project → a dependency's package page → Start here, stop 1 → its page → ⌘. code → back ×3 | Orbit names your project; the package hero names the package and its pinned version; Start here lists the tour's stops in order; the page hero names stop 1; the code shows its real first line; each back lands on the previous route with focus restored |
    | J2 | **find**: ⌘K → `toml Value` → ↵ → the shape `Invocation -> list of text` → the chain row → ↵ hold → click `grammar` | the name results show toml's `Value` first; the shape results show `Invocation::positional`, then "or, in steps"; the held plate reads "Invocation *to* list of text, *in two steps*"; the page opens on `Invocation::grammar` |
    | J3 | **upgrade**: toml's package page → scrub the comb to 1.1.6 → the upgrade lens → a use site → esc to pin | the line reads "viewing 1.1.6 · you pin 0.8.23"; the lens counts 82 added, 2 removed, 103 changed; `from_str` shows as respelled; esc restores the pin with no lens |
    | J4 | **the map**: from a page, G → the graph flight → R reach → esc → T tour → → → ↵ | the camera lands on the symbol (focus card names it); reach shows "If it changes — …" with nonzero waves; the tour plate reads "toml *in six stops*"; ↵ opens the stop-2 page (`Value`) |
    | J5 | **failure**: the `ensure_locald` page → the "or fails with" kinds → click `Spawn` → RuntimeError's How it fails → the maker link | the pipe names "Io, Spawn, DaemonExited or StartTimeout" and "MissingExecutable through locald_executable"; the error page's Spawn row lists `ensure_locald`; the maker link opens `ensure_locald` again |
    | J6 | **weather**: J1 re-run with a resize storm (1440 → 480 → 2560) and text scale 85 → 200 mid-flight, reduced motion on for the second half | the same checkpoints, plus no text cut to "…" where §8.3 forbids it; nothing is offscreen that isn't inside a scroll |

    `facet-gallery journey <name>` runs one journey and writes `journeys/<name>/{strip.png,motion.json,REPORT.txt}`. `verify` gains a `journeys` column.

    **Data.** Journeys run on the desktop harness's fixture index (`apps/desktop/src/harness.rs`, which today indexes `crates/present` and `frontends/rust/fixtures/rich_project`). Its roots grow to cover every journey's subject:
    - `crates/runtime` (J5);
    - a fixture project that pins `toml = "0.8.23"` and uses `toml::from_str`, indexed offline from the local registry cache (J2, J3).

    Where the index cannot yet serve a checkpoint's data (release history and diffs, §8.5), the journey reports **BLOCKED (data)** and names the missing reply. It never passes on a stand-in, and the lead routes the gap to the data plane.

## 4. Phases

- **P0 Foundations** — vendor patches, facet crate, fonts, tokens, motion
  engine, Cut/Hatch/Gem/Ground, icon pipeline, gallery + harness capture,
  filmstrip and contact-sheet tools. Checkpoint: Plates, Language and
  DataMarks gems reproduced in the gallery; a filmstrip of drop-in.
- **P1 Components** — three lanes: controls & inputs; marks & data (comb,
  mosaic, seam, strands, rose); overlays & chrome. Checkpoint: Controls,
  Plates, DataMarks boards reproduced; storms pass on the gallery.
- **P2 Shell** — runtime wake/keyed resources, region entities, titlebar,
  shelf, reader, status, keys, focus walk, hint mode, responsive rules.
- **P3 Pages** — Orbit + add flow + onboarding; Package; Page (+ trait/class);
  Source; Ask + search; settings, inbox, states.
- **P4 Hardening** — storms, perf budgets, matrix, lints, cleanup of dead
  code (old views/theme/ui modules are deleted as their replacements land).

## 6. FACET v4 — "more in less" (supersedes where it conflicts)

The owner's second brief: components must be more ambitious than the boards,
highly responsive with a constant sense of flow across screen and text sizes,
better at communicating information, alive on hover everywhere (breakdowns on
every bar, excellent popups around symbols, gorgeous symbol pages), a compact
mode, strong theming abstractions, and deep verification across situations.
The lead drew the v4 targets; **lanes implement against these screenshots,
not against prose**:

| Target | File (render with `Nudox-Design-System/v4/snap.sh <name> W H`) | What it fixes |
|---|---|---|
| Symbol page | `v4/SymbolPage.html` → `v4/shots/SymbolPage.png` (1440×1500) | hero + facts line + context margin (in your code, history) + lens bar + signature x-ray + rose + single-line ledgers grouped by receiver |
| Flow | `v4/flow-{2560,1440,1100,760,480}.html`, `flow-1440-200pct.html`, sheet `v4/shots/flow-sheet.png` | one page at five widths and 200 % text: third column of pins at vast, margin folds into the flow < 1100, shelf → kspine < 900, rose scales, rose → grouped list < 560, thread keeps only "here" |
| Density | `v4/density-{comfortable,compact,dense}.html`, sheet `v4/shots/density-sheet.png` | three densities, same width; dense folds summaries away |
| Peeks | `v4/Peeks.html` → `v4/shots/Peeks.png` | symbol peek card anatomy, chained child peek with crumb, pinned column, package/file/version peeks, the four peek rules |
| Lenses | `v4/Lenses.html` → `v4/shots/Lenses.png` | release-comb lens (change breakdown by family, "touches your code" first), module tile lens, mosaic stone peek, language-comb lens, the four lens rules |
| Ladder | `v4/Ladder.html` → `v4/shots/Ladder.png` | mark → tag → row → card for a symbol, a package, a release; x-ray (⌥) spelling every mark |

The CSS in `v4/v4.css` is the executable spec of the responsive rules
(container queries = `Measure`/`Room`; `clamp()` = `Measure::fluid`; density
vars = `Density`). The generators (`v4/*.py`) show the data each piece needs.

### 6.1 New abstractions (in `facet`)

- **`measure::Measure`** (landed): container-relative sizing. Effective width
  = width ÷ text scale drives `Room` (Narrow < 480 ≤ Slim < 760 ≤ Regular <
  1100 ≤ Wide < 1600 ≤ Vast); `fluid(small, large)` interpolates
  continuously; `space(Space)`, `role(TypeRole)` (display type fluid, UI/mono
  density-scaled with floors), `row()`, `control()`, `icon()`, `columns()`.
  `Density` Comfortable/Compact/Dense. `Reveal { keys, xray }`. Every component
  takes a `&Measure` for the width it actually gets; nothing sizes from the
  window. Typeset with `.set(role, &measure)`.
- **The ladder** (`Rung::{Mark, Tag, Row, Card}`, `Needs`, `Measure::rung`):
  every datum renders at the rung its room affords; **rest climbs one rung**
  (peek/lens), **activate descends** to its page, **⌥ x-ray raises everything
  one rung in place**.
- **Compass** mark: a symbol's relational shape in 16–30 px (arms: up is,
  down made of, left from, right to; length = log count). Used on every row.
- **Peek** (symbol card) and **Lens** (aggregate breakdown): one shared
  anatomy each (see targets); peeks chain three deep with a crumb, pin into a
  column (Vast) or a ⌘P stack, follow with ⌥→.
- **Theme roles**: `Contrast::{Normal, High}` (high = every ink one step
  stronger, firmer lines/bevels), `Density`, `Reveal` in the `Facet` global.
  Components read palette roles, never raw hex.
- **Flow** (next foundation lane): FLIP layout motion keyed by stable ids —
  when a keyed element's laid-out position changes across a *layout epoch*
  (room class, density, text scale, route, list order, data), it springs from
  where it was painted to where it now is; continuous width drags track
  directly. **Presence**: keyed enter/exit for lists (drop-in, make room, leave)
  so removed items finish their exit. **Shared**: an element keyed the same on
  two routes morphs between them (descent).
- **gpui-ce additions** (vendored; no hacks around missing APIs): subtree
  transform (translate + scale) applied to all primitives, a group-opacity
  layer (offscreen composite), and blurred polygon shadows for cut plates.

### 6.2 Restraint (binding; the owner rejected the first v4 targets as cluttered)

"More in less" means a calm surface with depth behind it, not more marks on
the surface.

1. **One thing speaks.** Each view has one focal element (the hero, the code,
   the query). Everything else recedes to ink2–ink4 or waits.
2. **Rows at rest: mark + name.** At most one quiet descriptor when it is the
   row's reason to exist. Counts, compass, caps, history, file combs and
   locations appear on hover or focus (in the row's trailing zone), in the
   peek, or under ⌥ x-ray — never all at once by default.
3. **One accent.** Mint means yours or current; periwinkle means focus or
   selection. Family hues live only on kind marks and code tokens. No
   multi-hued legends or strips.
4. **Space, not lines.** Group by spacing; hairlines only between regions.
   No boxes around groups, no borders around previews that don't need them.
5. **Serif is the voice, used sparingly.** The lede and the one sentence of a
   card. Not captions, counts or labels.
6. **Keys appear when asked.** Key feet and key caps show while ⌘ is held,
   not by default.
7. **Cards are short.** A peek at rest: mark, name, where, one sentence, and
   the single fact that answers "why care" (e.g. your uses). The rest is one
   more rest or ⌥ away.
8. **Targets show the product, not notes about it.** No explanatory margin
   annotations in boards.

## 7. Wave 2 — ownership and interfaces (binding)

Wave 1's component lanes were reviewed and restructured (2026-09-25):
controls and data marks are kept as bases; the overlay module is rewritten
(duplicated marks, rounded chips, hardcoded colours, text scale ignored,
signatures built from one div per token, peeks that could not chain, a popup
model that could not represent an exit, tautological tests).

**One authority per thing.** Nothing is implemented twice. If you need a
variant of something another lane owns, add a builder option to it (listed
in your report) or ask the lead; never copy it.

| Owner | Paths | Owns |
|---|---|---|
| W-Controls | `facet/src/{controls,chrome}/**` | button, icon button, **kbd (the only one)**, seg, diamond switch/check/radio, comb slider, input + select trigger (IME via gpui_component), splitter, density toggle; chrome pieces: titlebar thread, here capsule, altimeter, shelf rows + book header, kspine, status bar, pins-column frame |
| W-Data | `facet/src/data/**`, `facet/src/overlay/lens.rs` | compass (mark, row, bar), comb (+ file comb), mosaic, seam, strands, **rose**, caps row, gem progress, facts line, lens bar, territory map; the **lens card**; making every sub-part of every mark hoverable into a lens or tip |
| W-Float | `facet/src/overlay/**` except `lens.rs` | the float layer (hover intent, placement, chain, pin, exits), tooltip, **peek card**, rich text (`StyledText` runs, hoverable words, syntax roles), menu, toast, dialog, hint labels |
| W-Flow | `facet/src/motion/{flow,presence,shared}.rs`, `vendor/gpui-ce/**`, highlighter features | FLIP on layout epochs, Presence, shared elements, gpui compositing (transform + group opacity), blurred polygon shadows, tree-sitter highlighting |
| W-Harness | `facet/src/{probe.rs,gallery.rs,gallery/**,bin/**}`, `apps/facet/tests/**`, `tools/gui-harness/**`, `apps/desktop/src/{harness.rs,bin/backend-desktop-gui-harness.rs}` | storms, lints, matrix, perf, motion alignment, the desktop capture adapter, one `verify` command |
| W-Shell | `apps/desktop/src/**` (not the harness files) | window root, region views on `DataStore`/`Watch`, routes + thread + descent, responsive regions, keys, focus walk, reveal (⌘ keys, ⌥ x-ray), deleting the old views/ui/theme |

`lib.rs`, `gallery.rs` (`all()`) and `Cargo.toml` take one-line additive edits
from anyone. A module is wired into `lib.rs` only once it compiles; every lane
keeps `backend-facet` compiling at all times (check after each edit batch; a
break in a file you do not own means wait and retry, never edit it).

**The float layer** (W-Float provides; W-Data, W-Controls and W-Shell call it).
One layer per window, rendered once as the root's last child. Triggers never
own popups; they report rest and leave:

```rust
// facet::overlay::float
pub enum FloatKind { Tip, Peek, Lens, Menu }
pub enum Side { Above, Below, Right, Left }          // preferred; flips and shifts at edges
pub struct FloatRequest {
    pub key: ElementId,                               // the exact trigger: a tick, a word, a row
    pub anchor: Bounds<Pixels>,                       // window coords of that sub-rect
    pub kind: FloatKind,
    pub side: Side,
    pub content: Rc<dyn Fn(&Measure, &mut Window, &mut App) -> AnyElement>,
}
pub fn rest(request: FloatRequest, window: &mut Window, cx: &mut App); // hover intent applies
pub fn open(request: FloatRequest, window: &mut Window, cx: &mut App); // keyboard: no delay
pub fn leave(key: &ElementId, window: &mut Window, cx: &mut App);
pub fn step_back(window: &mut Window, cx: &mut App) -> bool;          // Esc
pub fn pin_top(window: &mut Window, cx: &mut App) -> bool;            // Space
pub fn layer(window: &mut Window, cx: &mut App) -> AnyElement;        // the root's last child
```

A `rest` on a trigger inside an open float chains a child (three deep, crumb,
connector). Content is built with a `Measure` for the card's own width (its
base width × text scale, clamped to the viewport), so cards scale with text.

**Presence** (W-Flow provides first, then FLIP/shared). Keyed enter/exit on the
executor clock: `sync(keys)` diffs; survivors keep order; leavers hold their
slot until their exit settles; a key that returns while leaving reverses from
where it is, never restarts; `is_settled()` is true only when nothing is
entering or leaving.

**Measure flows down explicitly.** Every component takes the `&Measure` for
the width it actually gets. No fallback to a zero-width or window-width
measure inside a component.

**Text.** Anything with mixed styling or hoverable words is one `StyledText`
or `InteractiveText` with runs, never a row of divs. Code colours come from
`palette.syntax`; no raw hex or HSL outside `tokens.rs`. No `rounded()`
anywhere: chips and key caps are cut stones (`geo::CUT_STONE`).

**Quality bar for every wave-2 report.** Real command output quoted for every
claim (a narrated "would pass" is not evidence). Side-by-sides against the
targets at 1440, 760 and 480, at 100 % and 200 % text, Abyss and Glacier, read
and annotated with what still differs. Filmstrips for every motion. Storms for
everything with state. No tautological tests: a test must be able to fail for
a real defect in the code under test.

## 8. The graph view and the symbol page (binding for wave 3)

The live prototype is the target: `Nudox-Design-System/v4/Graph.html` over the real
workspace (`graph/extract.mjs` → `graph/layout.mjs` → `graph/app.js` + `graph/page.js`),
with stills in `v4/shots/graph/` (TARGETS.md). Where this section and the prototype
disagree, the prototype's behaviour wins and this text gets fixed. The rose is retired.

### 8.1 One rule for relations, every language
Left is what a symbol **comes from**; right is what it **goes into**. Group words say how:
- type: `made of`, `made by`, `implemented by` ← T → `taken by`, `held by`, `calls it`, `used by`;
- callable: `takes`, `called from` ← f → `gives`, `calls`, `used by`.

`is` is not a direction: it is the capability line (§8.3). One function computes the
groups (`relationsOf`), used by the graph's prism, the focus card and the page. A view
drops any group its own anatomy already shows, so each thing is said once per view.

### 8.2 The world graph (`facet::graph`)
- **Layout is nested and deterministic.** world → packages → modules → symbols → members.
  - Each level is a small force simulation (links + exact collision + gravity), seeded on a phyllotaxis spiral by importance (PageRank over rolled-up symbol edges). No level exceeds ~250 bodies.
  - Members sit on **shells** around their type: parts (variants, fields) on the inner shells, methods on the outer. Spacing is even, so a shell reads as a cut polygon.
  - Territories are the faceted convex hulls of their contents.
  - The whole 54 k-symbol workspace lays out in ~6 s in JS. In Rust it runs off the UI thread, is cached per (package, version), and gives the same positions for the same input.
- **Semantic zoom by on-screen size, never by zoom step.**
  - Package hulls always.
  - Module hulls and labels once the package is ≥ 160 px.
  - Symbols as 1 px stars, becoming kind shapes past ~1.6 px core: ◆ types, ◇ traits, ■ callables.
  - Member shells once the item is ≥ 12 px.
  - Labels by importance through an occupancy grid, with a budget of 40–160 per frame.
- **Colour.** Monochrome at rest. Mint marks your code *and every dependency symbol your code reaches* (the footprint; dependency packages say "you use N"). Periwinkle marks focus. Direction is motion: dashes flow source → target on lit edges.
- **Paint cost follows what is visible.**
  - Modules are culled by box.
  - Shapes are batched per (shape, tone, brightness), so ~20 fills draw 15 k symbols.
  - A uniform grid serves picking.
  - Hover edges are bundled through module and package centres (β = 0.72).
  - Budget: 120 Hz at 1440×900@2x during flights (the prototype measures p50 8.3 ms, p99 16.7 ms).
- **Camera.** `(cx, cy, w)`.
  - Wheel zooms about the pointer, with a 70 ms exponential ease in log-width. Drag pans with inertia.
  - Every jump is a **van Wijk–Nuij flight** (`motion::flight`, W-Flow), ρ = √2, duration `clamp(210·S + 260, 320, 1500)` ms, sine ease, re-planned from the current camera when interrupted.
- **Focus gathers the prism.**
  - Click (or ↵ on a search result): the camera flies to the symbol at reading scale, centred in the space the focus card leaves. The symbol's relations then fly out of their home positions into two named columns beside it (left: comes from, right: goes into), 22 px rows, at most 6 per group plus "and N more".
  - Faint dashed tethers run back home. Names from another package carry the package name, and duplicate names carry their module.
  - ↑↓←→ walk the proxies, ↵ goes to the selected proxy (a flight), Esc releases the prism, then backs out.
  - Below 720 px of free width the prism becomes one column.
- **Reach (R on a focused symbol): what would feel it if this changed.**
  - Dependents are in-edges (every relation points from the dependent to what it depends on), followed breadth-first for up to 8 waves.
  - A method passes the change on to its callers. A field or variant passes it to its owner as well, because the shape changed.
  - Waves are counted per top-level symbol.
  - The camera frames the source plus the nearest 90 % of what it reaches. The waves then light 240 ms apart: ◆ at 95 / 66 / 34 / 20 % alpha by depth, mint for your code, everything else dimmed.
  - Faint threads run from the 48 most important first-wave symbols back to the source, and first- and second-wave labels are promoted. The prism steps aside while reach is shown.
  - The focus card gains one quiet block: "If it changes — 3 direct · 8 within two steps · 151 in all · across 4 packages · 111 in your code", then "most in desktop 104 · present 38 · …".
  - R again or Esc hides it.
- **Hover** raises the peek (W-Float) anchored to a moving node; the anchor is re-read every frame.
- **Find:** `/` or ⌘K; ↵ flies there.
  - **By shape.** A query with an arrow searches callables by what they take and give, in plain words or Rust:
    - examples: `path -> maybe text`, `takes bytes gives Value`, `text, number → Span`;
    - inputs match in any order, the result matches through maybe/fails, and each extra parameter costs a little;
    - same-shaped overloads fold into one row (`From<String>`, `From<&str>` and `From<Cow<str>>` all read "(text) → Value");
    - each row shows its shape in words under the name.
  - **The constellation.** While the find box holds a query, every match (up to 400) is lit in the graph as a periwinkle diamond with a soft halo. Its label is promoted and everything else dims to 42 %, so the answer has a *place*, not just a list. The panel ends with "N lit in the graph, across P packages". Blur or Esc restores the map.
  - **The empty box teaches.** Focused and empty, the panel shows three quiet rows (click fills the box):
    - `Value`: a name;
    - `path -> maybe text`: a shape (what it takes, what it gives);
    - `Invocation -> list of text`: what you have → what you need, in steps if it takes more than one call.

    Nobody guesses that `->` searches by shape; the box says so once, where the eye already is.
  - **Traits count.** A value of yours also matches a parameter typed by a trait it implements: `WireSchema -> text` finds `serde_json::to_string(value)` as a one-call answer, through "any Serialize" (it scores below a concrete match). A generic's associated type (`V::Value`, `T::Err`) is "anything", never a same-named type elsewhere.
  - **In steps (chains).** When fewer than three single calls answer a shape, the panel adds "or, in steps" (or "no one call does it; in steps") with up to three **chains** from what you have to what you need:
    - `RustEdition -> text` → `Language::from(LanguageProfile::Rust(edition)).name()`;
    - `Invocation -> list of text` → `invocation.grammar().aliases()`.

    The engine is the Getting one table with a second least-cost pass. It first runs Knuth with your inputs free alongside the plain values (R0). Then a Dijkstra over keys counts only derivations that *consume* what you have. A producer fed by a had key pays:
    - its own weight;
    - that key's cost;
    - each other input's R0 cost + 0.3 (what you must bring along is not free here);
    - +0.6 for a step outside *home* (the packages of the input and output types).

    Rules that keep chains honest, each found by reading real output:
    - **No hollow detours.** Never call a method on a value you just wrapped in a variant (`Value::String(t).as_table()` always gives nothing).
    - **No round trips.** A getter named like a parameter of the constructor that made its receiver is an echo (`HeadExpectation::new(root, sequence).sequence()`).
    - **Not a step:** views of the same thing (`as_ref`, `borrow`, `deref`, `clone`, `into`, `index`) and comparisons (`eq`, `cmp`, …).
    - **Plain values alone** (`text -> number`) get no chains: from text, every road leads somewhere.
    - **Caps:** at most 3 calls; cost ≤ 3.6 and ≤ best + 1.5; one chain per distinct spine.

    A chain row shows its steps as small diamonds (one per call) and one line: `from RustEdition › LanguageProfile::Rust › from › name → text`.
  - **The road.**
    - **Selecting a chain row previews its road in the graph.** The road runs through *types as places*: calls on the same type fold into one stop labelled `Language · from › name`, and a free function is its own place.
      - Your value's stop is a mint hollow diamond, the answer's a larger periwinkle one. Stops are joined by gentle arcs that all bend the same way.
      - A mint bead carries the value along the road once (380 + 260 ms per arc, sine ease), and stops brighten as it passes.
      - Everything else dims to 42 %. The chain labels its own stops (right, else left, below or above; always inside the view), and the graph's own label for those nodes is suppressed.
    - **↵ holds the chain.** The camera flies to frame its stops (wider margin below 640 px). A plate at the foot says "RustEdition *to* text, *in three steps*", then the rail (Getting one's grammar; its lead reads "from your RustEdition", or "from your WireSchema, a Serialize" when a trait carried it). The foot reads "your value is the spine; the rest rides along · ⌥ for code · esc to let go".
    - ⌥ swaps the rail for the code. In the code, a step that may give nothing reads `?` inside the chain; the answer itself stays maybe.
- **Start here (T): a package's reading path, computed and flown.** docs.rs lists a crate's items alphabetically; we give the five or six a newcomer should read, in the order they meet them (`graph/tour.js`):
  - **start here:** the door. A free function used from other packages, preferring one that hands you the heart; failing that, the heart's own most-used maker ("how you get one").
  - **what you hold:** the heart, the type with the most weight (importance + 0.22·ln(1 + packages that use it) + 0.15 if your code does).
  - **inside it:** at most two public types the heart's fields and variants are made of.
  - **what it promises:** the trait with the most implementors + outside users.
  - **when it fails:** the `…Error` the door or the heart returns, else the weightiest one.

  A package whose idea is a trait (its top trait outweighs the heart by 2× + 5) starts with it ("the idea · 486 types do it"). It adds the second trait when that weighs at least a third as much ("and the other half"), and drops the heart unless it carries a quarter of the trait's weight. The results:
  - toml: `from_str → Value → Table → Array → Index → Error`;
  - serde_json: `from_slice → Value → Map → Number → Read → Error`;
  - serde: `Serialize → Deserialize → Error`.

  A tour needs at least three stops.

  In the graph:
  - The where-line says "T start here" while a package with a tour is under the camera (packages and modules altitudes).
  - T starts the tour of the focused symbol's package, else the one under the camera. → / space: next; ←: back; ↵: open the stop's page; esc: end.
  - Each step is a flight (van Wijk) to the stop at 2.6× its focus width, sitting 12 % above centre because the plate covers the foot.
  - The road is dashed periwinkle arcs. The legs into and out of the current stop are drawn at 55 %, the rest hinted at 12 %.
  - Stops are numbered and labelled by the road itself. The current one is a filled periwinkle diamond with a soft halo; past stops are ink, upcoming ones periwinkle outlines.
  - The plate at the foot reads:
    - "toml *in six stops* · 2 of 6";
    - the strip of stops (diamond + name, the current one filled, each a button);
    - "*what you hold* **Value** everything turns on it", then the lede as prose (markdown marks and link targets dropped);
    - the foot "→ next · ← back · ↵ open its page · esc end".

  **On the package page** the same tour is a **Start here** strip under the tabs, replacing the old "Start with X, then Y" line (target `PackagePage.png`):
  - "Start here", then each stop as an 18 px gem, the name (mono) and its role beneath (serif italic), joined by 34 px periwinkle legs;
  - the first stop is underlined in periwinkle, because that's where you begin;
  - "T fly it" at the right starts the graph tour;
  - below 760 px the strip becomes rows: gem, name, role, why;
  - every stop is a link and peeks on hover.
- **Where:** a quiet line bottom-left names the package › module under the camera and the altitude.
- **Trail:** the symbols you visited are joined by a faint mint line. This is the titlebar's thread, drawn on the map.

### 8.3 The symbol page: anatomy instead of a code block
- **Hero:** gem, name, lede, then one facts line (`kind in path · used in N places · N in your code`).
  - The name wraps at identifier boundaries (humps, `_`, `::`) and is **never** cut to "…".
  - Below 560 px the gem sits above the name at 40 px.
- **Anatomy, one visual grammar for all languages:**
  - **fork** (enum / union / sum type): "one of", a rail with a branch per variant.
  - **holds** (struct / record / class fields): a bracket; notes how many fields are private.
  - **pipe** (function / method): inputs stacked on the left → the output, with "or fails with E" as its own exit, and generic bounds as sentences ("T is any Deserialize").
  - **contract** (trait / interface): "you write" (required, dashed bracket) and "you get" (provided).
- **Types in plain words:**
  - `maybe X`, `list of X`, `set of X`, `map K → V`, `X or fails with E`, `shared X`, `locked X`, `text`, `path`;
  - `its Value` for associated types;
  - generics as italic variables, never links.

  ⌥ spells the exact source type beside each one. Every named type is a link.
- **How it fails** (`graph/fails.js`). docs.rs says `Result<Workspace, RuntimeError>` and stops. The index records every mention of a variant in a body (uses/calls/type/has edges).
  - **Makers and readers.** A mention by a callable that *returns* the error (its `Result<_, E>`, or the package's `type Result<T> = Result<T, E>`) builds that kind: a **maker**. A mention by E's own methods that take `&self` (fmt, source, is_retryable), or by a callable returning something else (`Fault::from_client_error`), only tells kinds apart.
  - **Spread through calls.** A callable returning E that calls g, which also returns E, can give whatever g gives (`?` carries it up). This is memoised over the call graph; a cycle sees what is known so far.
  - **On a callable's pipe,** under "or fails with RuntimeError":
    - the kinds it builds itself, as links ("Io, Spawn, DaemonExited or StartTimeout");
    - then each group carried up from a call: "MissingExecutable *through* locald_executable";
    - then "5 of its 10 kinds" (or "any of its 10 kinds").
  - **On an error type,** a **How it fails** section (after `can`), one row per kind:
    - the kind (mono, a link; underlined mint when your code tells it apart), then its makers (yours first, in mint, then importance; four, then "+ N");
    - one column for every row, sized to the longest kind;
    - below 560 px each row stacks, the makers indented under the kind;
    - the foot: "12 calls in this world can fail with it · your code tells 1 of its 10 kinds apart".
  - A struct error (toml's `Error`) has no kinds, so the page shows neither.
- **`can`:** capabilities in words, marked `derived` (hollow), `written` (solid) or `via Display` (dashed).
  - Implied derives drop out: Copy covers Clone, Eq covers PartialEq, Ord covers PartialOrd and Eq.
  - Example: `copies freely · sorts · hashes · debug-prints · prints · to text`.
- **Getting one** (types) and **Calling it** (callables): how to obtain the thing from plain values, computed rather than written.
  - The engine is one table of producers: every public callable, variant, open struct literal and `Default`. Their inputs and outputs are keyed by the plain-word types. `From` impls are producers, so conversions come for free.
  - Plain values (text, path, number, bool, bytes) cost 0. A producer costs one step plus its inputs (+0.45 if it may fail, +0.35 if it may give nothing).
  - A least-cost derivation over that AND-OR graph (Knuth's generalisation of Dijkstra) gives every type its cheapest recipe. It runs once per package perspective: public items, plus everything inside the target's own package.
  - The page shows at most three routes, one per distinct maker, ranked:
    1. the type's own makers first;
    2. then its package's;
    3. then helpers elsewhere, preferring fewer arguments;
    4. a route over 6 steps or 3 cost units longer than the best is dropped.
  - Same-named makers fold into one route: "from a number · also from yes or no, text, Number and 2 more".
  - An enum's own variants are the fork above, so the foot says "or pick one of its N variants above".
  - **Display: a rail.** It reads left to right like a transit line:
    - the plain-value source in words (`from text what`), then each step as a boxed link, then the intermediate type as a station ◇ (a link), ending in the target's ◆;
    - the spine follows the costliest input, and the other inputs ride along as `+ a number n` / `+ a DeclarationKind kind`;
    - `?` marks a step that may fail.
  - **⌥ spells the code** the way a person writes it: nested steps become `let` bindings named after their type, shared sub-steps are bound once, and `?` marks each fallible step.
  - For a callable, "Calling it" is the same tree rooted at the callable itself: how to get each argument. It is omitted when every argument is plain.
  - The foot counts the makers ("257 ways in this world make one"). When nothing public makes one, the section says so in one sentence ("You receive it; the prism shows from where").
  - Getting one sits above the prism and replaces its "made by" group on type pages.
- **Its cousins:** the same idea in another package, and the road each way. "How do I turn a toml Value into a serde_json Value" is a classic question that docs cannot answer; the world can.
  - **Finding cousins.** A cousin is a public top-level struct, enum or union in *another* package with:
    - the same name and ≥ 3 shared public method names, or
    - a different name with ≥ 6 shared names at Jaccard ≥ 0.35.

    Names every type has don't count (new, default, from, into, fmt, clone, eq, len, iter, get, serialize, deserialize, …). Same-name cousins rank first, then Jaccard; at most two are shown.
  - **Each cousin row:**
    - "**serde_json::Value** · the same idea in serde_json · both have `get_mut`, `as_bool`, `as_str` and 8 more";
    - then *to it* and *from it*, each a rail from `recipes.convert(i, j)`: the chain engine with single-call answers kept, so trait roads count (toml's Value is a Deserializer, so `serde_json::Value::deserialize(value)?`; serde_json's Value is a Serialize, so `toml::Value::try_from(value)?`);
    - the lead says whose value it is ("from toml's Value, *a* Deserializer");
    - with no road: "no road between them in this world".
  - **Code** spells two same-named types with their crate (`serde_json::Value::…`), and your value's variable is named for its type.
  - The section renders after first paint (each road costs a derivation) and sits after Does, before In use.
- **The prism** (static): the same groups as the graph, minus those the anatomy and Getting one already show. Its gem opens the graph (G).
  - With four or more groups it shows 3 per group.
  - A one-sided prism fans from the content edge instead of stranding its gem mid-page.
- **Does:** members grouped by what they do to it (`reads it`, `changes it`, `uses it up`, `makes one`).
  - A by-value receiver on a Copy type is `reads it`.
  - **Look-alikes fold:** ≥ 4 members that share a name prefix and a result become one row ("visit_… (one of bool, i8, … and 16 more) → its Value or fails with E · 22 of them").
  - Trait-provided methods group under "through its traits". Names starting `__` are hidden.
- **In use:** up to three real statements that use the symbol, mined from the bodies of the callables that refer to it.
  - Callables only; type declarations only "hold" it, and the prism already says so.
  - Your code first, then other packages, then importance, at most two per package.
  - The search skips the caller's signature, takes the statement until it closes (at most three lines), and underlines the symbol in periwinkle.
  - Each is captioned with its caller (a link) and `package · file:line`.
- **Every link peeks on hover:** the same peek card as the graph, through the float layer.
- **Code** is one keystroke away (⌘. or the view switch): the real source, soft-wrapped at token boundaries with the item's lines marked. It never clips.

### 8.4 Moving between them
- The titlebar's altimeter slot is the view switch: **Graph · Page · Code**, with only the active view named. See `Route::Symbol { id, at, view }` in W-Shell; switching views replaces the history entry.
- **Graph → page (↵ / double-click):** the focus gem flies (FLIP) into the hero gem while the page rises in (460 ms).
- **Page → graph (G):** the page lifts away, the camera flies from wherever you last left the map (or from the package's altitude the first time) to the symbol, and the prism gathers on arrival.
- The version comb in the shelf header (`VersionComb.png`) re-scopes page and graph to a release. In the graph, symbols absent at that release fade and ones added since your pin glow once.
- **The upgrade lens (the comb scrubbed away from your pin).** docs.rs shows one version in isolation; this shows the *difference* against the version you pin, and against the places your code uses the crate.
  - **Data.** Releases come from the registry index (every version with its publish time; ticks for versions not on the machine are dimmed, with date only). The API comes from each local version, keyed by stable path with re-exports resolved.
  - **Diffs** are pinned → each local version and consecutive, each change marked `breaking` or `additive`. `semverSlip` marks breaking changes shipped in a minor (after 1.0) or a patch.
  - **Uses** are your workspace's use sites of the crate's items, resolved through imports. **Impact** is the uses whose item changed.
  - **Shelf, under the comb:** "viewing 1.1.6 · you pin 0.8.23 · esc", then one quiet line: "14 breaking · 31 added · 2 of your 9 uses change", plus "breaking in a minor release" when semver slipped.
  - **Page, above the anatomy:** "Upgrading to 1.1.6" (or "Going back to …").
    - One line on your code: "2 of the 9 places your code uses toml change".
    - Up to 4 affected use sites: file:line, the change, and the statement with a periwinkle rule.
    - Then this symbol's own changes as rows (`added` / `removed` / `changed` / `deprecated` / `variant added` …).
  - **Signatures in the rows are spelled in the page's plain words**, and a change marks exactly what differs: new parts underlined mint, old parts struck.
    - When both sides read the same in plain words (a lifetime or spelling-only change), the row says "reads the same in plain words; only its Rust spelling moved" instead of alarming you.
    - The Rust spelling is in the tooltip and under ⌥.

### 8.5 Data the index must provide
- Per package closure: symbols (kind, name, parent, visibility, file:line span, doc first sentence).
- Members with receiver kind, required/provided, and trait-via.
- Derives, and resolved impls (trait id + member names).
- **Impl-block generics with their bounds**, attached to every member of the impl (`impl<R: Read> Deserializer<R>` → `new(read: R)` reads "from any Read", not "from anything"). Getting one and search by shape both depend on it; the prototype's extractor records only item-level generics.
- **Per dependency:** every release with its publish time and yanked flag (the registry index has them), and the public API of each release present locally.
  - The API is keyed by stable path, with re-exports resolved to the shortest public alias.
  - Signatures are normalized: whitespace, trailing commas, `Self` → the owner, and lifetime parameters canonicalized by position.
  - The workspace's use sites of each dependency's items come from path expressions, imports plus bare names, and turbofish. Method calls on values need type inference and are not yet covered; the lens must say so if asked.
  - The prototype's `graph/releases.mjs` is the reference implementation, and its header documents the schema.
- Generics and where-clauses as text.
- Typed relations with kinds (has, takes, gives, is, derives, impl, calls, uses, type), at member granularity. Rolled-up symbol edges are derived in the view model.

The prototype's extractor is a stand-in with name-based resolution; the product reads the index. The graph and page lanes use `graph/world.json` as their fixture until the query exists.

## 5. Lane rules

- Only `git add <new file>`, `git status`, `git diff`, `git log` are allowed.
  Never stash, checkout, restore, reset, clean, commit, or `cargo clean`.
- Build only through `.local/devenv/cargo … -p <your crate>` (the cached
  `.#development` environment). Never `--workspace`.
- Stay inside your lane's files; touching another lane's file needs the lead.
- A checkpoint report ends with: files changed, what was verified with which
  command, screenshots/filmstrip paths, and what is still wrong.
