# W-Motion PLAN: plates, not fades (for the lead's review; nothing below is built except the check)

## The idea in one line

**Every surface is a cut plate, and motion is plates opening, closing, folding and unfurling along a travelling edge. Ink never lies on ink.**

With the sound off, you would know Nudox by three things:
- a chamfered edge that sweeps across the reader with a periwinkle hairline;
- the one name that rides that edge and grows into the title;
- content printed beneath it, line by line, in reading order.

Nothing ever cross-fades, and nothing is ever drawn over something still legible.

The whole grammar follows from one physical fact: **a plate is opaque**. A plate that opens covers the old page as it goes, so the old page is gone from every region the new content enters (law 2, by construction rather than by timing). A plate that closes uncovers the page that was always behind it.

## 0. What exists now: the check (built this phase)

- **`gpui::TextTrace`** (vendor patch, documented in `NUDOX-PATCHES.md` "Text trace").
  - With the global set, every window records each text line it paints per frame: its content, its window-space box (after the layer transform, cut by the content mask) and its ink's effective alpha (run alpha × element opacity × group opacity).
  - It covers every text path: div text, `StyledText`, `InteractiveText` and raw `ShapedLine`s. No component needs wrapping; the 34 `probe::text` sites saw almost nothing.
  - Code: `vendor/gpui-ce/src/window.rs` (`painted_texts`, `trace_text`, `TextTrace`, `PaintedText`) and `vendor/gpui-ce/src/text_system/line.rs` (`trace_line`).
- **`facet-gallery legibility --scene S [--input-file F] [--from/--to] [--frames] [--dump] --out DIR`**
  - Code: `apps/facet/src/gallery/legible.rs` (new) and the `legibility` command in `apps/facet/src/gallery/cli.rs`. The desktop harness gets the same command.
  - Samples every 16 ms at 2x. Contrast is measured from the frame's pixels, as `lint.rs` does.
  - **overlap:** two texts intersect (>1 px on both axes), both are readable (≥1.8:1 in the part of each box the other doesn't cover), and both are *seen at the crossing*: the crossing's ground is each text's own ground.
    - The shared-ground rule is what makes the check trustworthy. An opaque card over page text hides that text, and is not counted.
  - **faded:** a translucent text (alpha < 0.95) is drawn (>1.15:1) but under 80 % of its own best contrast, for more than 2 frames.
  - Overlaps that also exist in a frame where nothing animates are reported as layout overlaps "(also at rest)", separate from motion.
- **Tests** (`gallery::legible::tests`, 3/3, two runs):
  - a crossing flight fails for exactly the frames it crosses;
  - a lingering fade fails and a cut passes;
  - pixels: a text on one ground crossing another fails, and the same text under an opaque card passes.
- **The catalog ledger** is in `CATALOG.md`: every desktop route pair, overlays, graph and appearance, plus the facet gallery films. Every desktop route transition fails today, with 15–210 overlap runs each, and the old page lingers for up to 228 ms (1.4 s into the graph).
- **What that looks like:** `Nudox-Design-System/v5/shots/motion/today-route-down.png`, the package → symbol route at 1120, 1152, 1184 and 1248 ms. `backend-present` and `RelationLabel` are printed over each other, and the old outline rows run through "Typed / Neighbourhood / Related". Set it beside `sig-open.png`.

## 1. The spatial map (DIRECTION §3), as plate physics

| Relation | Where it goes | The plate |
|---|---|---|
| **Deeper** (child, member, opening) | in, to the right | **Open.** The clicked row's plate opens into the page. The old page drifts 16 px left under it, and the new content settles from 16 px right. |
| **Up / back** | out, to the left | **Close.** The page's plate closes into the row it came from. The parent is uncovered, returning from 16 px left. |
| **Related** (across a relation) | along the relation's stroke | **Push.** The stroke from the clicked token extends to the reader's edge on its side: in-relations from the left, out-relations to the right. The page pushes along it with plate edges touching, never overlapping. |
| **Time** (versions) | along the comb, left = older | **Scrub.** The comb's head walks; changes roll (Odometer) and print where they land (MOTION §scrub). |
| **The graph** | out | **Fold.** The page's plate closes into its node. The sections fold into the node's edges, and the camera pulls back. |
| **Code** (same thing, other view) | in place | **Peel.** The declaration line holds still. The page folds away around it, and the source file unrolls above and below that same line. |

## 2. The signature moves (storyboards: stills at 0/40/80/120/160/240 ms)

- **Board:** `Nudox-Design-System/v5/motion/Signature.html?demo=<id>&t=<ms>[&reduced=1]`, served at `http://127.0.0.1:47811/v5/motion/…`. The page is a pure function of t, and its curves and springs use `facet::motion`'s maths.
- **Strips:** `Nudox-Design-System/v5/shots/motion/sig-{open,close,fold,unfold,peek}.png`, plus the `-reduced` variants. Regenerate with `python3 stills.py`.

### 2a. Open: "the row opens" (deeper), and Close (back)

**The bold idea: you don't go to a page, you go *into* the row.** The row's own plate is the new page's ground.

One driver `p` runs on **CARRY**, a critically damped spring with response 0.28 s: p = 23 % at 40 ms, 54 % at 80, 75 % at 120, 87 % at 160 and 97 % at 240. Every pose is a band of `p`, so an interruption retargets one spring with its velocity.

| t (ms) | What you see |
|---|---|
| 0 (the click frame) | The name is re-set in the title face at the row's size (the one allowed face change, on frame one). The row's plate lifts. |
| 0–140 | The plate's top edge rises to the reader's top, and its bottom edge falls to the floor. Being opaque, it covers the old page as it goes. The old page drifts 16 px left. |
| 10–170 | The name rides just below the rising edge, grows to Display 38/40 and lands in the title slot. It never crosses old text: the old text above the edge is not yet covered, and the name is below it. |
| 90–190 | The gem unfurls from the title's leading edge. |
| 100–270 | The page prints beneath the title in reading order, one line at a time. Each line is uncovered by a clip plus an 8 px settle in 32 ms, starting 8 ms after the line above. A line is part-drawn for at most 2 frames. |
| jump bar and shelf | The changed segments roll (Reel), and the shelf's mint bar travels (p .1–.9). |

**Close** (Back, from `Place.way` plus the origin row the history entry remembers):
- The body folds first, last lines first, all within 0–70 ms.
- Then the plate closes into the row's bounds (CARRY from 64 ms), uncovering the parent, which returns from −16 px.
- The title rides the closing edge down into its row.
- The row keeps a periwinkle tint (α .09) from 180 to 700 ms, then lets go over 240 ms: *where you were*.

**Interruption:** Back at any `p` retargets the one spring from its painted value and velocity.

**Reduced motion:** a cut. The new jump-bar segment holds a periwinkle mark (1.2 s + 160 ms settle). `sig-open-reduced.png`.

### 2b. Fold / Unfold: "the page closes into its node" (page ↔ graph)

**The bold idea: the node *is* the page, closed.** Row plate, page plate and node plate are one object at three scales. Zooming out doesn't swap views; it closes the plate you are on.

This supersedes the door-flight overlay. It builds on W-Shell's §15 S3 (`GraphView::enter_from(i, origins)`), with W-Page's `EdgeAnchors` as the origins (§5).

| t (ms) | What you see |
|---|---|
| 0–110 | Each section clips upward along its own rule, bottom section first. |
| 40–220 | The page's plate contracts onto the node's plate (the spine contracts into the gem). The graph was always laid out behind the page, and is uncovered around the shrinking plate; this is a clip, not a fade-in. |
| 60–210 | Each stub (the relation rows) slides along its y to its port on its side: in-relations to the left, out-relations to the right. Stub order is port order, so nothing crosses. The stub's stroke draws on as its label lands (120–230). |
| 40–210 | The gem becomes the node, and the title becomes the node's label (shared, uniform scale). |
| 200–480 | The camera pulls back (van Wijk, `Pacing::GRAPH_TRAVEL`) into the neighbourhood. Neighbour labels follow the graph's LOD, so no label is drawn inside the moving plate's path. |

- **Unfold** (zoom in) is the exact reverse. The camera comes in, the node's plate opens into the page, the strokes undraw into their stubs, and the sections unroll along their rules. `sig-unfold.png`.
- **Reduced motion:** a cut to the graph. The focus node's ring holds a periwinkle mark: `flight.rs`'s 120 ms `CROSSFADE` is deleted (S5).

### 2c. The hover grammar and Unfurl: "ink rises, then the word opens"

**The bold idea: the page answers the pointer before it opens anything.** At 0 ms everything *related* lights at once, so you see the relation before you read about it. The peek then grows out of the exact word, and leaves the way it came.

| t (ms) | What you see |
|---|---|
| 0 | The target's ink rises one step. Its hit shape draws its bevel (1 px, kind hue at 45 %). Every other occurrence on screen gets a 1.5 px kind-hue underline, and the relation stroke to it lights at 1.5 px in full hue. The target's own stroke goes 1.2 → 1.5. |
| 350 +0–60 | The underline draws under the exact token. |
| 350 +60–140 | The underline *becomes the card's top edge*: its x, width and y travel to the card's. |
| 350 +100–220 | The body unrolls down from that edge (a clip; content is never scaled or faded). As the body's edge passes a page line, that line steps down to ink4, so *one thing speaks*: no legible text is ever left cut in half beside the card. |
| leave (+0–120) | The body rolls up. |
| leave (+120–210) | The edge shrinks back to the word. |
| leave (+210–300) | The underline undraws. |

- **Warm swap** (resting on another word while a card is open): the card springs to the new anchor on SNAPPY, and the content *wipes* in reading order (boundary right to left, 12 px offsets).
- The same Unfurl serves the lens (from its tick), menus and jump menus (from the segment's plate), tips (from the underline), and Ask (the plate unfurls down from the jump bar).
- **Reduced motion:** the card cuts in and out, and the token keeps its underline while it is open. `sig-peek.png`, `sig-peek-reduced.png`.

## 3. Verbs per context (not one move everywhere)

| Transition (catalog id) | Verb | Today (the fade it replaces) |
|---|---|---|
| route-down (orbit→package→page, a row to its page) | **Open** | `reader.rs:449-457` `enter_act` Way::Down: 24 px rise, 0.985 scale, fade (EMPH 380 ms); `exit_act` `reader.rs:503-515`: fade (STD 240, DROP) |
| back / route-up (⌘[, ⌘↑) | **Close** | `reader.rs:458-464` Way::Up: −24 px + fade |
| across (another package or declaration, same depth) | **Push** along the relation stroke | `reader.rs:465-469` Way::Across: +24 px + fade |
| view page ↔ code (⌘.) | **Peel** (the declaration line anchors) | `reader.rs:470-473` Way::View: pure cross-fade (QUICK 160) |
| graph enter / exit, page ↔ graph morph | **Fold / Unfold** | `reader.rs:487-499` `graph_enter`/`graph_exit` 460 ms fades; `bodies/graph.rs:1086-1096` `FocusMark` `opacity(1 − morph.t)` |
| graph focus / tour / back-out (inside the graph) | camera flight (kept) + the prism **gather** from origins; labels by LOD | `view.rs:1298` prism `spec::DESCENT`; `view.rs:1433` reading-room `REVEAL` fade → clip |
| hover (rows, links, marks, chips, nodes, comb teeth, rose, territory, mosaic) | **the hover grammar** (instant) | `controls/state.rs` + `spec::HOVER` tints (button lift `LIFT` BOUNCE, icon-button tint/scale, seg lean); `data/rose.rs:560`, `data/territory.rs:480`, `data/mosaic.rs:401-432`, `view.rs:1510-1534` REVEAL/HOVER fades |
| peek / lens / tip / menu / jump menus / Ask | **Unfurl** (+ **Wipe** on warm swap) | `overlay/float.rs:1380-1520` group fade + 0.97 grow; `float.rs:1552` warm-swap 0.35→1 cross-fade; `menu.rs:252` plate fade; Ask cut (`root.rs:1091-1129`) |
| pin / unpin | the card **Travels** into the pins column (FLIP) | `float.rs:1861` pin row rise + fade |
| toast | **Unroll** from the foot's edge | `toast.rs:401-404` 14 px rise + fade (380/200) |
| dialog | **Unfurl** from the invoking control; the page quiets (ink steps down, no scrim fade) | `dialog.rs:292-304` scrim + plate fade |
| list arrivals / leavers (Presence) | **Clip acts:** UNROLL_DOWN, UNROLL_RIGHT, ROLL_UP (POP and DROP_IN stay for play) | `presence.rs` `act::{FADE_IN, FADE_OUT, RISE, PEEK, LEAVE}`, `keys::{RISE, TOAST, UNFOLD, PEEK}` |
| shelf collapse ↔ spine | the marks hold; the names **unroll/roll** rightward with the width | `shelf.rs:206,218` rows ↔ spine cross-fade driven by `open` |
| ⌘-held key caps | **Roll up** from the baseline, 90 ms, staggered by x | `controls/kbd.rs:292-331` KeyRise fade |
| comb scrub / rung change | **Timeline + Odometer + print** (MOTION §scrub) | `data/comb.rs:548-563` rung cross-fade; `controls/comb.rs:1051` note fade |
| find / filter narrowing | FLIP **Travel** of survivors; leavers roll up first | re-render cut (`reader.rs:531-537`) |
| sibling (⌥↓) | **Reel** (MOTION §sibling) | none today |
| hand take / drop | the take is a shared arc (kept); the drop **sinks** through the foot by clip, not fade | `hand.rs:69-80` fade + sink |
| theme | **Sweep** from the top-left (MOTION §theme) | instant swap |
| text size / density | **Anchor:** the line you read holds; the rest springs (FLIP epoch) | instant reflow (`Flow` is unused in the desktop) |
| reduced motion, everywhere | **cut + mark** (#16, below) | 120 ms `CROSSFADE` (`flight.rs:73`) |

## 4. The one hover primitive (API for W-Page and everyone)

I confirm W-Page's grammar (§2c). It is exactly what the primitive paints, and nobody paints their own. The API lives in `facet::hover` (new; mine):

```rust
/// What lights together: every hoverable with the same subject (a symbol's address, a
/// package id, a release). "Light every other occurrence of X on screen" is this key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Subject(pub SharedString);

/// Wraps an element in the grammar: its hit shape, subject and kind hue.
pub fn hoverable(id: impl Into<ElementId>, subject: Subject, hue: Hsla, child: impl IntoElement) -> Hoverable;

impl Hoverable {
    pub fn shape(self, shape: Shape) -> Self;          // Rect | Chamfer(px) | Diamond | Stone: where the bevel draws
    pub fn underline(self, rect: Rect) -> Self;        // the token's baseline (defaults to the text's), the peek's origin
    pub fn peek(self, card: PeekFn) -> Self;           // opens by Unfurl at 350 ms; hover intent and chain via the float layer
    pub fn on_open(self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self; // click / ↵ (Open, Push, Fold…)
}

/// For painters that draw their own strokes and underlines (rails, stubs, spines, edges):
pub enum Lit { None, Target, Related }
pub fn lit(subject: &Subject, window: &Window) -> Lit;
pub fn stroke(subject: &Subject, rest: f32, window: &Window) -> f32; // RELATION rest → lit (+0.3 px: 1.2 → 1.5)
```

- **Mechanics.** One `HoverField { target: Option<(ElementId, Subject)> }` per window.
  - It is set in the pointer-move handler (hitbox) and in the keyboard focus walk. Keyboard focus lights exactly like hover: one hand.
  - It is read by every `Hoverable` and by `lit()` in the **next frame, which is the same frame as the input response**. So an element painted before the target in paint order still lights correctly.
  - There are no tweens: colour and stroke steps are instant (DIRECTION: "at 0 ms"). The peek is `float::rest(FloatRequest { kind: Peek, anchor: underline })`, whose hover intent is already 350 ms for Peek (`float/model.rs:59-95`).
- **What it deletes.**
  - The ad-hoc hover tracks in `controls/state.rs` (`Touch` hover/press `spec::HOVER`/`PRESS`; press stays a 90 ms step).
  - `button.rs:387-431` lift.
  - `icon_button.rs:143-166` tints.
  - `data/rose.rs:494-560`, `data/territory.rs:444-481`, `data/mosaic.rs:336-432` dims.
  - `graph/view.rs:1510-1534` node hover fades.
  - `splitter.rs:321-339` reveal fades.
  - `menu.rs:252` plate fade (the bevel travels instead).

## 5. Answer to W-Page: `EdgeAnchors`

Accepted as proposed, with three additions I need:

```rust
pub struct EdgeAnchors {
    pub gem: Bounds<Pixels>,                 // was Point: the fold needs the size (gem → node, uniform scale)
    pub title: Bounds<Pixels>,               // + the title's box (title → node label)
    pub spine: (Pixels, Pixels, Pixels),     // (x, y0, y1)
    pub sections: Vec<SectionEdge>,          // + { rule_y, top, bottom, side: In | Out | Own }: sections clip along their own rule
    pub stubs: Vec<Stub>,                    // { side, verb, y, x0, x1, n, tier, subject: Subject } (+ subject: the far end's
                                             //   identity, so the stroke lands on the neighbour node when the camera pulls out)
    pub rails: Vec<Rail>,                    // { src, step }
    pub fan: Vec<Fan>,                       // { label, n }
}
```

- Their choreography is mine: sections clip along their rules, stubs slide along their y to their ports in stub order, and the spine contracts into the gem.
- I add that the page's *plate* contracts with the spine and uncovers the graph behind it. Nothing fades in.
- `EdgeAnchors` must be published on the frame of the keypress (G / ⌘− / pinch), from the page plan's measured anchors (W-Instant's `PagePlan`). No re-measure is needed.

## 6. The fade purge

- **Rule** (DIRECTION): no entrance or exit is carried by opacity. Opacity survives only as the reduced-motion mark's colour settle.
- **Inventory:** every fade and cross-fade by `path:line` is in `CATALOG.md` §3. There are 33 sites: 22 facet, 7 desktop and 4 primitives. Each is replaced by the verb in §3 above.
- **Guard.** When the purge lands, the harness's `faded` rule is the regression test. Any translucent text lingering for more than 2 frames, in any catalog film, fails the run.

## 7. Reduced motion: a cut plus a mark (MOTION #16, now mine)

- **The primitive.** `motion::mark(key, hue) -> f32`, where `key` is the element keyed to arrive, return or change.
  - It holds a colour for 1.2 s, then settles over 160 ms on the executor clock.
  - It never marks the first draw.
  - It requests frames only while it is holding.
- **What holds the mark:**
  - the new jump-bar segment: periwinkle, *arrived*;
  - the row you came back to: periwinkle, *where you were*;
  - the graph's focus ring;
  - the peek's token underline, held while the card is open;
  - the scrub's built-on line: mint;
  - a failure's operand: coral.
- `flight.rs` `CROSSFADE`/`Fading`/`Shot.fade` are deleted (W-Shell's S5 finding: the graph already cuts).

## 8. Build slices (after approval)

Every slice has four things, and two identical runs of each:
- catalog entries turned green;
- a film next to its storyboard at the same `t` values;
- a test that asserts frames (geometry and the legibility law);
- a trap-restored mutation, with its real panic line quoted.

1. **M1: hover grammar + Unfurl + Wipe** (facet `hover` new; `overlay/**`). This is the most frequent transition.
   - Unfurl already exists, but only as an opt-in: `FloatRequest::unfurl` (`float.rs:131-173`), `unfurl_bands` (`:218`) and `paint_unfurl` (`:2038`), used only by the comb and mark cards.
   - M1 makes it the only entrance for Peek, Lens, Tip and Menu, deletes the fade/grow path (`float.rs:1470-1472`, `:1616`, `:1648`), and adds the page-line quieting under the unrolling body.
   - Tests:
     - the underline's width at +60 ms equals the token's;
     - the card's top edge at +140 equals the card's rect;
     - no card line is painted before the clip uncovers it;
     - a covered page line reads ink4;
     - `legibility` passes on `float-*`, `menu-film` and `peeks`.
   - Mutation: restore the group fade.
   - W-Page adopts the API here.
2. **M2: Open / Close** in `reader.rs`.
   - It needs three new primitives: the CARRY driver, *Shared driven by an external progress* (#5), and *print* (a clip reveal of sections in reading order).
   - Back reads `Place.way` plus the origin row's key.
   - Catalog: route-down/up, back and forward go green (today 15–69 overlap runs each).
   - Tests:
     - frame 0's title box equals the clicked row's box (face swapped);
     - the plate rect is monotone;
     - no old-page text is visible inside the plate;
     - the title lands at 240 ms.
   - Mutation: paint the old page above the plate.
3. **M3: Fold / Unfold** (S3 `enter_from` with `EdgeAnchors`, graph enter/exit, S5 mark).
   - The GRAPH-AUDIT gate runs twice after every graph-file change.
4. **M4: Clip acts + the rest of the purge:** Presence acts, toast, dialog, shelf, key caps, comb rungs and mosaic.
5. **M5: Push** (across), **Peel** (page ↔ code), Roll/Odometer (jump bar and counts), Find narrowing, text-size Anchor, and theme Sweep.

## 9. Dependencies and requests

- **W-Instant.**
  - The destination page must render on the click frame (`Now<T>`). The title is shared from frame one, and the page prints as the plate opens.
  - If a section's data arrives later, that section prints when it lands: never a spinner, never a fade.
  - `EdgeAnchors` and section rects come from `PagePlan`'s measured anchors.
- **W-Page.** `EdgeAnchors` (§5). Sections are wrapped in `motion::print(order)` so the reveal follows reading order, and titles are keyed `shared` by symbol.
- **W-Seams.** `bodies/graph.rs` map requests only; enter motion is mine. The origin row's key rides on `Intent::Navigate` (for Close).
- **The lead.** Should CARRY's response be retuned from MOTION's 0.34 s to 0.28 s? At 0.34 s the tail runs past the 240 ms budget: 93 % at 240 ms, against 97 % at 0.28 s.

## 10. Housekeeping for this phase

- **Check and data.**
  - `gpui` text trace patch (additive).
  - `gallery/legible.rs` (new, `git add`).
  - `legibility` and `--dump` in `cli.rs`.
  - `gallery.rs` sets `TextTrace` when probing.
  - W-Instant's `gallery::declared` (applied, with test `a_declared_script_resolves_the_same_without_playing_the_scene`: passed in two runs).
- **Storyboards:** `Nudox-Design-System/v5/motion/{Signature.html, signature.css, signature.js, stills.py}` and the strips in `v5/shots/motion/`. All are new; `git add` them.
