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

## 5. Lane rules

- Only `git add <new file>`, `git status`, `git diff`, `git log` are allowed.
  Never stash, checkout, restore, reset, clean, commit, or `cargo clean`.
- Build only through `.local/devenv/cargo … -p <your crate>` (the cached
  `.#development` environment). Never `--workspace`.
- Stay inside your lane's files; touching another lane's file needs the lead.
- A checkpoint report ends with: files changed, what was verified with which
  command, screenshots/filmstrip paths, and what is still wrong.
