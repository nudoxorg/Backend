# W-Motion CATALOG: every transition, filmed at 16 ms, against the legibility law

Captured at HEAD 959f8911f plus the working tree of 2026-09-28 (06:00–07:00 UTC). Every number below comes from a run whose output file is named.

## 1. The law, and the check that enforces it

DIRECTION v5 law 2: *at any frame of any motion, each text is either fully legible and unoverlapped, or not drawn at all. Old content never lingers under new content.*

**Command** (both binaries):

```
.local/target/debug/{facet-gallery|backend-desktop-gui-harness} legibility --scene S --scale 2 \
    [--input-file SCRIPT] [--from MS] [--to MS] [--frames] [--dump] --out DIR
```

- It samples a genuine draw every 16 ms of virtual time at 2x, from `--from` (default 0) to `--to` (default: the script's end + 600 ms).
- It writes `DIR/<scene>-legibility.{txt,json}`.
- `--dump` adds `<scene>-texts.jsonl`: every text's box, contrast and alpha, per frame.
- `--frames` adds the PNG of every frame that broke the law.
- It exits non-zero on any finding.

**What it sees:**
- Every text line gpui paints, through a new `gpui::TextTrace` (vendor patch, `NUDOX-PATCHES.md` "Text trace"). No component needs to be wrapped in a probe.
- Each line comes with its window-space box (after layer transforms, cut by content masks) and its ink's effective alpha (run alpha × element opacity × group opacity).
- Contrast is measured from the frame's pixels, exactly like `gallery/lint.rs`.

**Rules** (`apps/facet/src/gallery/legible.rs`):

| Rule | Fails when |
|---|---|
| `overlap` | Two lines intersect by more than 1 px on both axes. Both are readable: at least 1.8:1 in the part of each box the other doesn't cover. Both are also *seen at the crossing*: the crossing's pixels sit on each line's own ground, so neither is hidden under the other's opaque plate. Pairs that also overlap in a frame where nothing animates are counted separately as "layout overlaps at rest". |
| `faded` | A line painted translucent (alpha < 0.95) is visible (> 1.15:1) but under 80 % of its own best contrast in the film, for more than 2 consecutive frames. |

**Trust:**
- Tests `gallery::legible::tests` pass 3/3 in two runs:
  - `two_readable_texts_crossing_fail_and_a_text_under_its_ground_does_not`: a flight that crosses fails for exactly the frames it crosses (32..48 ms).
  - `a_fade_that_lingers_fails_and_a_cut_does_not`.
  - `a_card_covering_page_text_hides_it_and_text_on_one_ground_crosses`: on real pixels, text on a shared ground fails; the same text under an opaque card passes.
- **Mutation** (trap-restored, touched, two runs): with the shared-ground rule removed, the check starts failing a card that legitimately covers page text:
  - `panicked at apps/facet/src/gallery/legible.rs:584:9: a card hides the page text it covers: Report { … findings: [Finding { rule: Overlap, what: "`card` × `page`" …`
- **Coverage per desktop film:** 38–574 distinct lines drawn, and up to 226 lines per frame (A: `max 226 probed in a frame`). Before the trace, the 34 `probe::text` sites showed 0–17.

## 2. The ledger: desktop (real shell, fixture index)

- Films live in `wave5/motion/films/*.txt`. Their output is in `wave5/motion/legibility/desktop/<film>/` and the per-transition ledgers in `desktop/<film>.ledger`, aggregated by `wave5/motion/ledger.py`.
- Each transition is the window from its act to the next act.
- **"exercised"** means drawable text changed in that window; checked from `--dump` (the `+new −gone` counts).
- A PASS in which nothing changed is reported as **NOT EXERCISED**, never as PASS.

### Routes (film A, `desktop-orbit`, A-routes.txt, 626 frames, FAIL)

| Transition | Code today | Overlap runs | Faded runs | Old text lingers for | Worst example |
|---|---|---|---|---|---|
| route-down orbit→package | `reader.rs:449-457` Down, `:503-515` exit | 15 | 45 | 228 ms | `backend-client` × `toml_edit-0.22.27` 352..528 ms (the shelf list under the package page) |
| route-down package→symbol | same | 69 | 76 | 228 ms | `Typed` × `browse_tests.rs` 1120..1328 ms, both at 17.3:1 |
| back symbol→package (⌘[) | `:458-464` Up | 59 | 76 | 228 ms | `Reference` × `assemble.rs` 1920..2112 |
| forward (⌘]) | Down | 69 | 76 | 228 ms | `132 nested` × `Neighbourhood` 2736..2928 |
| view page→code (⌘.) | `:470-473` View crossfade | 51 | 82 | 228 ms | `/// The read declaration is…` × `SemanticLinkKind` 3504..3728 |
| view code→page | same | 75 | 86 | 228 ms | `Related,` × `Returns the words this grou…` 4304..4528 |
| graph-enter (G) | `:487-492` `graph_enter`, `bodies/graph.rs:1120` `FocusMark` | 210 | 265 | **1556 ms** | `Graph fixture · pages resol…` × `signature` 5200..5392; `signature` faded 5120..6480 |
| graph-exit (route view=page) | `:494-499` `graph_exit` | 15 | 72 | 448 ms | `RelationGroup.label` × `RelationGroup::label` 6464..6848 |
| route-up (⌘↑) | Up | 58 | 78 | 232 ms | `SemanticLinkKind × Relation…` × `browse_tests.rs` 7424..7632 |
| route world | graph enter | 50 | 204 | **1384 ms** | `Graph fixture · pages resol…` × `packages` 8480..8656 |
| world→package | graph exit + Down | 14 | 56 | 456 ms | `Show more entries` × `frontend-typescript` 9408..9856 |

**Every route transition fails.** The mechanism is the same each time:
- the leaving page is kept painted under the arriving page (`reader.rs:748-761`, `.occlude()`), fading over 240 ms (`exit_act`, DROP);
- the arriving page fades and rises in over 380 ms (EMPH, GLIDE);
- so for about 228 ms both pages are legible in the same place.

This is the owner's "overlaying the past", measured.

### Overlays, chrome and the hand (films B and B2, `desktop-symbol`)

| Transition | Code today | Verdict | Detail |
|---|---|---|---|
| hover a page link, then its peek opens | `overlay/float.rs:1470-1472` group fade + 0.97 grow; `:1616` card group fade | **FAIL** | 3 overlap, 6 faded. The translucent card shows page text through it: `Neighbourhood` × `SemanticLinkKind` 4960..4992; `enum in library::surface` faded 4976..5040 |
| peek leaves (pointer moves on) | same, exit | **FAIL** | 4 overlap, 5 faded. `Closed semantic relation vo…` × `SemanticLinkKind` 6336..6400; `Calls \| MethodCall…` faded to 1.58:1 |
| settings open (⌘,) | reader Across (Way::Across `:465-469`) | **FAIL** | 7 overlap, 57 faded, lingers 232 ms |
| back from settings | Across | **FAIL** | 3 overlap, 27 faded |
| ⌘ held: key caps | `controls/kbd.rs:312,330` KeyRise element opacity | **FAIL** | faded 1 run (48 ms), release 1 run (64 ms) |
| shelf collapse / expand (⌘\\) | `shell/shelf.rs:242,254` rows ↔ spine opacity `open` / `1−open` | **FAIL** | 13 faded runs each, 176 ms |
| hand take (⌘D) | shared arc `hand.rs` | PASS | "in hand" and the mark appear; no crossing |
| hand row (H), close (Esc) | cut | PASS | |
| hint mode (F) and close | cut (`root.rs:1107`) | PASS | |
| Ask open / close (⌘K) | cut (`root.rs` `ask_layer`) | PASS | The motion passes, but there are **6 layout overlaps at rest** inside Ask (ledger B). They are routed to W-Surfaces as a layout defect, not motion. |
| focus walk (Tab, J), keyboard peek (Space), pin | `focus.rs:369-372` springs | **NOT EXERCISED** | `tab, j, j, space` changed no text on `desktop-symbol` (B2 dump: `+0 −0`). The focus glow is not text, but the keyboard peek never opened. This needs a known focus path, or it is a dead end; routed to W-Seams to confirm. |

### Appearance (film C, `desktop-symbol`)

| Transition | Code today | Verdict |
|---|---|---|
| text size 100→150 % | reflow cut, plus shelf/pins width springs `root.rs` `shelf-w`/`pins-w` (`spec::SETTLE`) | **FAIL**: 15 faded runs, 128 ms (shelf rows cross-fading with the width) |
| text size 150→100 %, density both ways, resize 900×700 and back | instant reflow | PASS (cut) |
| theme glacier / abyss | instant palette swap (`theme.rs:128`) | PASS (cut; text unchanged, only colour). The sweep in MOTION §theme is new work. |

### Graph (films D and D2)

| Transition | Code today | Verdict | Detail |
|---|---|---|---|
| wheel zoom in / out | `graph/camera.rs` `Segment::Ease`; labels by LOD alpha | **FAIL** | 5 / 9 faded runs, the longest 1584 ms: LOD labels sit at partial alpha |
| pan (drag) | `camera.rs:591` glide | **FAIL** | 2 overlap, 5 faded |
| back out (Esc) to the package context | `view.rs` `Travel::Survey`; reading-room `REVEAL` `view.rs:1444` | **FAIL** | 3 overlap, 91 faded, the longest 4928 ms |
| to world (Esc again) | | NOT EXERCISED (no text change) |
| tour (T), stops (Space), end | `navigation.rs` tour | **NOT EXERCISED**: on `desktop-world`, `t` changed nothing (D2 dump `+0 −0`) |

**The graph's faded runs are mostly not motion.** Level-of-detail labels are painted at partial alpha at rest:
- `graph-flight-a`: `LanguageGlyph` at 1.29:1 of its 5.21:1 for 276 frames;
- `graph-hover`: `KindGlyph` at 1.81:1 for 297 frames.

Under law 2 a label is either legible or not drawn. These are listed as a defect of the graph's label LOD, which lives in the enter/draw paths I own. The fix (§3 of PLAN) is that a label is either drawn legibly or not drawn, and it is revealed by clip.

### Find and compare (films E and E2)

| Transition | Code today | Verdict |
|---|---|---|
| find typing (refine) | re-render in place (`reader.rs:531-537`) | **FAIL**: 70 overlap runs up to 496 ms. `src/value.rs:27` × `toml_edit-0.22.27`, `Value` × `basic-toml-0.1.10`: results re-lay out through each other. |
| route compare | Across | **FAIL**: 21 overlap, 119 faded |
| back from compare | Up | **FAIL**: 22 overlap, 118 faded |
| route find (a query) | Across | **FAIL**: 76 overlap runs, the longest 32 ms |
| open a result (click) | | PASS (the old list is cut) |

## 3. The ledger: facet gallery films (`legibility/facet/<scene>.log`, 301 frames each)

| Scene | Verdict | Overlap | Faded | Worst |
|---|---|---|---|---|
| float-reverse, hint-mode, graph-focus | PASS | 0 | 0 | |
| menu-film | FAIL | 0 | 12 | `Open` at 1.88:1 of 11.08:1, 544..608 (menu exit fade, `menu.rs:252` + float fade) |
| float-chain | FAIL | 0 | 4 | `MethodCall` faded 112..160 (chain child fade) |
| float-deepen | FAIL | 6 | 6 | `pub enum SemanticLinkKind {…` faded 368..416 |
| float-back / float-tip | FAIL | 0 | 4 / 2 | card fades |
| float-pin | FAIL | 5 | 8 | the card fades to 1.49:1 over 160..240 while the pin row rises (`float.rs:1957`) |
| float-sweep | FAIL | 2 | 6 | warm swap cross-fade `float.rs:1648` (`draw.swap`), 400..496 |
| toast-film | FAIL | 0 | 5 | `Undo` 16..176 (11 frames), `toast.rs:404` |
| dialog-film | FAIL | 0 | 8 | `Remove serde from the shelf?` 16..128, `dialog.rs:292,304` |
| flow-list | FAIL | 11 | 10 | FLIP rows cross each other, and arriving rows (RISE) rise under the next row |
| flow-reflow | FAIL | 57 | 0 | card descriptions cross while cards spring across column changes |
| flow-descent | FAIL | 2 | 2 | the page body (`BODY_IN`) fades over the list: `A framework for…` × `toml` 912..1040 |
| flow-graph | FAIL | 9 | 5 | `tokio` × `tokio`: the shared name's two faces, 2608..2656 (`lab.rs` `crossfaded_name`) |
| flow-presence | FAIL | 2 | 6 | `act::DROP_IN`/`LEAVE` fades |
| version-comb | FAIL | 7 | 34 | `3 months ago` × `you pin` 1424..1504; rung cross-fade `data/comb.rs:548-563` |
| controls-live | FAIL | 0 | 18 | the comb's `0.4.2` and `this week` fading (`controls/comb.rs:1170` `away`) |
| film-rose | FAIL | 4 | 18 | the rose's arrival fades, `data/rose.rs:560` |
| graph-flight-a / graph-journey / graph-hover | FAIL | 7 / 12 / 2 | 167 / 444 / 14 | LOD labels at partial alpha at rest (above), plus labels crossing mid-flight |
| film-comb, film-mosaic | PASS, but **NOT EXERCISED** | | | 0 texts drawn: marks only |

## 4. Every fade and cross-fade (`path:line` at the current tree)

Each entry names what carries opacity and the verb that replaces it (PLAN §3).

**Desktop (7)**
- `apps/desktop/src/shell/reader.rs:449-472`: `enter_act`. Down: +24 px, 0.985, opacity 0→1. Up: −24, 1.015. Across: +24. View: pure cross-fade (QUICK). EMPH/GLIDE. → Open / Close / Push / Peel.
- `reader.rs:487-492`: `graph_enter`, opacity 0→1 over 460 ms. → Unfold.
- `reader.rs:494-499`: `graph_exit`, −24 px + opacity 1→0 over 460 ms. → Fold.
- `reader.rs:503-515`: `exit_act`, 0.99 + opacity 1→0, STD DROP; the leaving page stays painted under the new one (`:748-761`). → gone (a plate covers it).
- `apps/desktop/src/shell/bodies/graph.rs:1120-1124`: `FocusMark` gem `opacity(1 − morph.t)`, 460 ms. → Fold (the gem *is* the node).
- `apps/desktop/src/shell/shelf.rs:242,254`: the row list `opacity(open)` against the spine's `opacity(1 − open)`, driven by the `shelf-w` spring. → roll/unroll with the width.
- `apps/desktop/src/shell/shelf.rs:750-751`: rows at 0.62 with `.hover(opacity 1.0)`. This is an ad-hoc hover, replaced by the hover grammar.

**Facet (22)**
- `apps/facet/src/overlay/float.rs:1470-1472`: card `with_group_opacity(fade)` + 0.97 grow when not `unfurl`. `:1616` paint group fade. `:1596,1604` underline and connector `opacity(t)`. `:1648` warm swap `with_element_opacity(draw.swap)` cross-fade. `:1957` pin row rise + `opacity(t)`. → Unfurl + Wipe.
  - Unfurl already exists, opt-in: `FloatRequest::unfurl` `:131-173`, `unfurl_bands` `:218`, `paint_unfurl` `:2038`. It is used only by `controls/comb/styled.rs:704,730,759` and `marks/card.rs:107`. The desktop peeks (`shell/peeks.rs`) still fade.
- `apps/facet/src/overlay/menu.rs:252`: the active-row plate `opacity(shown)`. → the bevel travels.
- `apps/facet/src/overlay/toast.rs:404`: rise + `opacity(t)`. → unroll from the foot's edge.
- `apps/facet/src/overlay/dialog.rs:292,304`: scrim and plate `opacity(t)`. → unfurl from the invoking control; the page's ink steps down.
- `apps/facet/src/controls/kbd.rs:312,330`: KeyRise `with_element_opacity`. → roll up from the baseline.
- `apps/facet/src/controls/splitter.rs:362,369,401`: bar, grip and readout `opacity(lit/readout_t)`. → hover grammar; the readout unrolls.
- `apps/facet/src/controls/comb.rs:1170`: the "away" note `opacity(away)`. → unroll.
- `apps/facet/src/data/comb.rs:722,731,748-751,807,850,854`: rung blend alpha (`548-563` computes it). → the rung swaps by clip at the threshold.
- `apps/facet/src/data/mosaic.rs:395-432`: stone `appear` and `dim` opacities. → stones print in reading order (clip); x-ray dims by ink step.
- `apps/facet/src/data/rose.rs:535-560`: strand and glyph alpha `0.7 + 0.3l`. → hover grammar.
- `apps/facet/src/data/territory.rs:480-481`: dim alphas. → ink steps.
- `apps/facet/src/chrome/shelf.rs:678`: spine mark `opacity(lit)` (gallery only). → hover grammar.
- `apps/facet/src/graph/view.rs:1444`: reading-room `REVEAL` alpha; `:1494` flow `REVEAL`; `:1523-1525` hover fades (`HOVER_KEY`). → clip reveal; hover grammar.

**Primitives (4)**
- `apps/facet/src/motion/presence.rs:170-176`: `LEAVE_POSE`, `FADE_IN_POSE`, `FADE_OUT_POSE`. `:195,207,213,220,226,233`: `act::{RISE, PEEK, FADE_IN, LEAVE, FADE_OUT, POP_OUT}`. → clip acts UNROLL_DOWN, UNROLL_RIGHT, ROLL_UP (POP and DROP_IN stay, for play).
- `apps/facet/src/motion/keys.rs:213,220,227,234`: `keys::{PEEK, RISE, TOAST, UNFOLD}` opacity channels. → the same.
- `apps/facet/src/motion/flight.rs:73`: `CROSSFADE` (120 ms), `State::Fading` `:886-1113`, `Shot.fade`. → cut + mark (MOTION #16).
- `apps/facet/src/motion/lab.rs` [demo]: `crossfaded_name` (the two faces of the shared name), `BODY_IN`, `FadeLab`. These are rebuilt with the new verbs as the storyboards' gallery twins.

## 5. What this catalog does not cover yet (stated so it isn't mistaken for PASS)

- **Not exercised:** keyboard peek and pin; graph tour; "to world" Esc; film-comb and film-mosaic (no text).
- **Not in any film yet:** jump menus (long-press ‹, segment menus); Inbox; the comb scrub in the desktop (the comb isn't wired into the shell); sibling (⌥↓, which doesn't exist); drag reorder; toasts and dialogs in the desktop (not wired, gallery only); reduced-motion variants of every film (next: the same films with `motion off`).
- **Harness scripts can't dwell exactly on a tip** (no dwell verb). Tip hover-intent was only exercised in `float-tip`.
