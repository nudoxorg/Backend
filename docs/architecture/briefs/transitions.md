# Brief: transitions between views, and the FLIP defects

**For:** the agent who takes this over (one Opus lane).
**From:** the GUI lead, 2026-09-28.
**Status of the tree:** HEAD f9359e942, clean.

## 0. What the owner asked for

- 2026-09-28 03:15: "animations still feel incredibly weak throughout, often overlaying the past and temporarily being hard to read … could still be more unique and creative … especially in transitioning to and fro the graph."
- 2026-09-28 15:30: "Smoother transitions between views is still an issue. … dispatch an agent … to work on … the FLIP defects."

**Done means:**
- every move between views reads as one continuous object going somewhere you can predict;
- no frame ever shows two legible texts on top of each other;
- nothing fades, whether in, out or across.

## 1. The binding law, and the machine that checks it

**DIRECTION v5 law 2** (`Nudox-Design-System/v5/DIRECTION.md` §1 and §3; `v4/motion/MOTION.md`): at any frame of any motion, each text is either fully legible and unoverlapped, or not drawn. Old content never lingers under new content. **No entrance or exit is carried by opacity.** Opacity survives only as the reduced-motion mark's colour settle.

**The check is built and in HEAD:**
- `gpui::TextTrace` (vendor patch, `vendor/gpui-ce` "Text trace" in `NUDOX-PATCHES.md`). Every text line painted per frame is recorded with its window box and its effective alpha.
- `facet-gallery legibility` and `backend-desktop-gui-harness legibility` (`apps/facet/src/gallery/legible.rs`, the `legibility` command in `gallery/cli.rs:282`):
  ```
  <binary> legibility --scene S --scale 2 [--input-file SCRIPT] [--from MS] [--to MS] [--frames] [--dump] --out DIR
  ```
  - It samples a real draw every 16 ms and exits non-zero on any finding.
  - `overlap`: two readable texts crossing on a shared ground. An opaque plate covering text is legal.
  - `faded`: translucent text visible below 80 % of its best contrast for more than 2 frames.
- Tests `gallery::legible::tests` pass 3/3.

**The ledger** is W-Motion's `CATALOG.md`, copied beside this brief as `transitions/CATALOG.md`. Every film script is in `transitions/films/`. At 959f8911f it showed:
- **every desktop route transition fails:** 15–210 overlap runs each, and old text lingers 228 ms (1.4–1.6 s into the graph);
- 23 of 26 facet gallery films fail.

Re-run the catalog first. The numbers are your baseline.

## 2. The approved design: "plates, not fades"

The full plan is W-Motion's `PLAN.md`, copied as `transitions/PLAN.md`. The lead approved it on 2026-09-28. Read it whole. In one line: **every surface is an opaque cut plate. Motion is plates opening, closing, folding and unfurling along a travelling edge, and ink never lies on ink.** An opaque plate covers what it passes, so law 2 holds by construction, not by timing.

**Storyboards** are the reference for every film. Compare at the same `t`.
- The board is `Nudox-Design-System/v5/motion/Signature.html?demo=<id>&t=<ms>[&reduced=1]`. Serve it with `python3 -m http.server 47811 --bind 127.0.0.1` from `Nudox-Design-System/`.
- The strips are in `v5/shots/motion/sig-{open,close,fold,unfold,peek}{,-reduced}.png`. The failure to beat is `today-route-down.png`.

**The spatial map.** Every relation has one direction:

| Relation | Verb |
|---|---|
| Deeper | **Open**: in, rightward. The row's plate opens into the page, and the title rides the edge. |
| Back | **Close** |
| Related | **Push** along the relation's stroke |
| Time | **Scrub** along the comb |
| Graph | **Fold**: the page closes into its node |
| Code | **Peel**: the declaration line holds still, and the file unrolls around it |

**One driver per transition:** CARRY, a critically damped spring with response **0.28 s** (the lead's ruling), reaching 97 % at 240 ms. Every pose is a band of `p`. An interruption retargets the one spring from its painted value and velocity.

## 3. What is landed, and what is not

**Landed** (HEAD; don't redo):
- `68cd76857`, **`facet::hover`**. It paints the hover grammar: at 0 ms every other occurrence of the subject lights, and relation strokes step 1.2 → 1.5.
- `0fc2166e9`, **Unfurl** for floats (`overlay/float/unfurl.rs`). Peeks grow out of the token's underline.
- `a61636e01`, the legibility check.
- `7bc389d1a`, the storyboards.

**Not landed. This is the work:**

| # | Defect | Where today (HEAD) |
|---|---|---|
| D1 | Every route change fades. The arriving page rises 24 px and fades in over 380 ms, while the leaving page fades out **painted underneath it** for 240 ms. | `apps/desktop/src/shell/reader.rs:449` `enter_act` (Down/Up/Across/View), `:503` `exit_act`, and the leaving page kept under the arriving one at `:690`, `:759` and `:789` (`.occlude()`) |
| D2 | Page ↔ graph is a 460 ms fade with a gem fading `1 − morph.t` over it | `reader.rs:487` `graph_enter`, `:494` `graph_exit`; `bodies/graph.rs:1116-1124` `FocusMark` |
| D3 | View page ↔ code is a pure cross-fade (QUICK) | `reader.rs:470-477` `Way::View` |
| D4 | **FLIP (`facet::motion::flow`) crosses text.** `flow-list`: 11 overlap runs; rows cross each other, and arriving rows rise under the next row. `flow-reflow`: 57; card descriptions cross while cards spring across column changes. `flow-descent`: the page body fades over the list. `flow-graph`: `tokio × tokio`, the shared name's two faces cross-fading (`motion/lab.rs` `crossfaded_name`). | `apps/facet/src/motion/flow.rs` (Flow), `motion/presence.rs:170-233` (fade acts), `motion/lab.rs` |
| D5 | **The desktop uses FLIP nowhere.** Find narrowing re-lays results through each other (70 overlap runs, up to 496 ms). A text-size change reflows by cut. The shelf rows and spine cross-fade with the shelf's width spring. | `reader.rs:531-537` find re-render; `shell/shelf.rs:242,254` `opacity(open)` / `opacity(1 − open)`; `root.rs` `shelf-w`/`pins-w` springs |
| D6 | Pin/unpin: the pin row rises and fades while the card fades to 1.49:1 | `overlay/float.rs` pin row (`float-pin` film) |
| D7 | The remaining fades: toast, dialog, key caps, splitter, comb rungs, the away note, mosaic, rose, territory, the graph's reading room and hover, and ad-hoc row hovers | `CATALOG.md` §4 lists every site; the ones in `apps/desktop` are `shelf.rs:750-751` (rows at 0.62 with `.hover(opacity 1)`) and `root.rs:1107` |
| D8 | Graph LOD labels are painted at partial alpha **at rest** (for example `LanguageGlyph` at 1.29:1 for 276 frames). Under law 2 a label is legible or absent. | `apps/facet/src/graph/view.rs` label LOD; the camera in `graph/camera.rs` |
| D9 | Reduced motion is a 120 ms cross-fade | `motion/flight.rs:73` `CROSSFADE`, `State::Fading` |
| D10 | Not exercised by any film: keyboard peek and pin, graph tour (T), "to world" Esc, jump menus, sibling (⌥↓ doesn't exist), drag reorder, desktop toasts and dialogs, and reduced-motion variants of every film | CATALOG §5 |

## 4. The FLIP rules (new; this lane writes them into MOTION)

FLIP is how things that *stay* move when layout changes. Law 2 still applies to them. Each rule has its test.

1. **Three phases, never overlapping in time:**
   - leavers **roll up** (a clip to their top edge, 0–90 ms);
   - survivors **travel** (FLIP on CARRY, from 60 ms);
   - arrivals **unroll** into the gap once it is open (clip, from the frame their slot is clear).

   No arrival is drawn where a survivor still is.
2. **A travelling row is a plate.** It carries its own opaque ground at full row height, and it is painted above the rows it passes. Two survivors crossing is then legal (one covers the other) *only if* the covered one's text is fully hidden. If both are partly visible at the crossing, stagger them instead: the row moving up goes first.
3. **Text never scales.** `Resize::Scale` is for gems, heroes and marks. A card whose width changes reflows its text by snap at the epoch, under a clip that grows with the card. The description is never drawn at two widths. *The one exception (ruling, 2026-09-28):* the shared name of an Open/Close is re-set in the title face at each frame's size as it grows from the row into the title. It is re-shaped each frame, never bitmap-scaled, so it is legible at every frame (PLAN §2a).
4. **A shared name has one face per frame.** The row's name and the page's title are one text. The face swaps on frame one (the one allowed face change), and the text travels. Delete `crossfaded_name`.
5. **Width-driven layout** (shelf collapse, pins column, text size) keeps the line you are reading still (the "Anchor" verb). Row names **roll** rightward with the width. They never cross-fade with the spine.

## 5. Slices (in order). Each one needs:

- the catalog entries it covers turned green (two identical runs);
- a film set beside its storyboard at the same `t` values (a contact sheet);
- a test that asserts frames: geometry plus the legibility law;
- a trap-restored mutation with its real panic line quoted.

1. **T0: re-baseline.** Re-run the whole catalog at HEAD and write `$S/wave6/transitions/BASELINE.md`. Add a `--catalog` mode to `legibility` that runs every film and prints the ledger, then wire it into `RULES` as a standing gate. **Stop for review.**
2. **T1: Open / Close** (M2 in PLAN §8).
   - New primitives: the CARRY driver; `shared` driven by an external progress (the title rides the plate edge); and `print`, which reveals sections in reading order by clip.
   - Back reads `Place.way` plus the origin row the history entry remembers. The row you return to holds a periwinkle "where you were" tint (α .09, 180–700 ms, then 240 ms to let go).
   - Delete the fade poses, and the leaving page painted underneath (D1).
   - Tests:
     - frame 0's title box equals the clicked row's box;
     - the plate rect is monotone;
     - no old-page text is visible inside the plate;
     - the title lands by 240 ms;
     - catalog: route-down, route-up, back and forward are all green.
3. **T2: Fold / Unfold** (M3).
   - The page closes into its node: sections clip along their rules, stubs slide to their ports in stub order, the spine contracts into the gem, and the camera pulls back. The anchors come from `facet::anatomy::page::Anchors` (`page.rs:81`); the instant lane caches them with the plan.
   - Delete the graph fades and the gem fade (D2).
   - Fix label LOD so labels are legible or absent (D8).
   - Catalog: graph-enter, graph-exit, route world and world→package go green.
4. **T3: FLIP.**
   - Rewrite `flow`/`presence` acts to the §4 rules, so the gallery's `flow-list`, `flow-reflow`, `flow-descent` and `flow-graph` pass (D4).
   - Wire Flow into the desktop for Find narrowing (survivors travel, leavers roll up first), the shelf roll, and text size with Anchor (D5).
   - Pin: the card **travels** into the pins column (D6).
5. **T4: the purge** (M4). Clip acts replace every fade in CATALOG §4 (D7). Reduced motion becomes cut + mark: `motion::mark(key, hue)` holds 1.2 s, then settles over 160 ms (D9).
6. **T5: Push, Peel, Reel, Odometer** (M5).
   - Push goes across along the relation's stroke. In-relations enter from the left and out-relations from the right, plate edges touching.
   - Peel is page ↔ code, anchored on the declaration line.
   - Reel covers jump-bar segments and ⌥↓ siblings. Odometer covers counts.
   - Add the theme Sweep.
   - Add films for everything in D10.

## 6. Interfaces with the other lanes

- **Instant lane** (`briefs/instant-open.md`). It renders the destination on the click frame, from `Now<T>`, and a section whose data lands later prints when it lands. **Don't build a loading state**; if you need one, the instant lane's contract is wrong, so tell the lead.
- **Page lane** (`briefs/symbol-page.md`). Page sections are wrapped in `print(order)`, and titles are keyed `shared` by the symbol's address. `facet::anatomy::page::anchor` gives you the section and stub rects.
- **The lead** owns `apps/desktop/src/shell/**` outside `reader.rs`'s transition code and the graph body's enter/exit. For anything else in the shell, write a request into your checkpoint.
- **Hover:** one grammar through `facet::hover`. Nobody paints their own hover.

## 7. Rules (binding)

- **Git.** Only `git status`, `git diff`, `git log`, `git show`, and `git add <a NEW file you created>`.
  - Never stash, checkout, restore, reset, clean, commit, rebase, switch, merge or filter-repo, including inside scripts and sub-agents.
  - The owner commits.
- **Build.** Only through `.local/devenv/cargo <cmd> -p <crate>`. Never use `--workspace`, and never run `cargo clean`. Other lanes share the build lock, so run one cargo process at a time.
- **Harness.**
  - Build: `.local/devenv/cargo build -p backend-desktop --features visual-harness --bin backend-desktop-gui-harness`.
  - Capture: `.local/target/debug/backend-desktop-gui-harness capture --scene <id> --out <dir> --size 1440x900 --time 1500`.
  - Scenes: `desktop-{orbit,package,symbol,code,graph,world,tree,find,compare,value,serialize,smallvec,error}`.
  - The facet gallery binary is `facet-gallery`.
- **Tests.**
  - Trust a result only when two consecutive runs agree.
  - Tests pin fixtures, never the live `world.json`.
  - Assert on rendered content and frames, not counts.
  - Mutations are applied, run and reverted inside one Bash command with a trap, then `touch`ed. Quote the real panic line; a narrated "would fail" is not evidence.
  - A stale mutated artifact can survive a revert. If a test fails exactly at your mutation's line after the revert, touch the file and rebuild.
- **Scope.** Lints go to a later Sonnet pass. Every claim cites `path:line` at the current tree.
- **Checkpoints** go in `$S/wave6/transitions/`, where `$S=/private/tmp/claude-501/-Users-mileswirht-Downloads-backend/4821aaf6-28dc-4f75-b07b-d8fc41618656/scratchpad`.
