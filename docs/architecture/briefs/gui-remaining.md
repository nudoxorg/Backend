# The GUI: what was never tried, never settled, or never finished

The lead wrote this on 2026-09-30, after the two finishing agents were stopped. It is the companion to `production-readiness.md`, which covers the data plane, first run, packaging and the defects known at the end of wave 6. This brief covers the desktop GUI as a whole. For every surface it says:
- what the design asked for;
- what the code has;
- what nobody ever tried;
- which questions are still open;
- which proofs never ran.

It is written for whoever continues the GUI, and for the owner, who holds the decisions in §5.

## How to read this

**Evidence.** Everything here was read, not re-run:
- the code at HEAD `525355f68`, plus 44 uncommitted paths. Those paths are the finishers' unreported milestone-3 work, including the search fix, the crawler, and an untracked `runtime/releases.rs`;
- the lane checkpoints, reviews and design documents listed under "Sources" at the end.

A claim marked "from code" was read in the source and has never been seen in a running app.

**Inventories.** Four raw inventories back this brief. They hold one record per item, with file:line, and are preserved in `docs/reviews/gui-checkpoint-2026-09-30/` so this checkpoint remains usable after a fresh checkout:
- `plan.md` (295 records): what the design documents intended against what the code has;
- `code.md`: routes, reachability, dead UI, deferral markers, exercise coverage, languages and platform, all from the code alone;
- `lanes.md` (329 records): every open item the lanes and reviewers wrote down, deduplicated, with its latest status;
- `final.md` (66 records): the finishing agents' milestone files and coordination log.

Ids in parentheses point into those files:
- `lanes.md`: `G…`, `L…`, `D…` (journey gaps), `FEEL-D…`, `RFit-…`, `RFolio-D…`, `RSym-…`, `ROpen-…`, `SIDE-…`, `FLUID-…`, `INST-…`, `INS-…`, `IDX-…`, `ACQ-…`, `TRN-…`, `TIS-…`, `GLYPH-…`, `PG-…`, `WLD-…`, `OLD-…`, `RES-…`, `PH-…`;
- `final.md`: `M…`;
- `code.md`: `ROUTE-…`, `ACTION-…`, `SETTINGS-…`, `LANG-…`, `PLAT-…`, `MAC-…`, `A1`–`A10`.

**How far each thing got.**

| Word | Meaning |
|---|---|
| Real | built, and seen working on the real owner |
| Rig | built; proven only by rig tests, gallery scenes or fixture captures |
| Fixture | built, but the product feeds it prototype data |
| Unbuilt | designed; not in the code |
| Superseded | designed, then dropped by an owner ruling |
| Decision | waits on the owner |

---

## 1. The short version

The desktop works on one path, and only that path has been walked for real. A person on this Mac adds a Rust project, and its crates arrive from the local cargo cache. They can then read a package page, a symbol page and its code, search with ⌘K, change the appearance settings, and relaunch. Everything else was either built and proven only on fixtures, or designed and never built. Of the 329 open items the lanes recorded, 38 were ever seen on the real owner or real packages.

The biggest things nobody got to:

1. **The graph is a prototype (§2.9).**
   - These read a 16 MB snapshot of *this repository's* crates: the graph, with its tour, chains, reach and "yours"; the hand's roads; and the Library's resume line.
   - The snapshot is loaded from a source-tree path that a bundled app does not have.
   - A person's own project has no node in it.
   - The lane that would project the world from the index (W-World) never launched.
2. **Release data is a prototype (§2.10).**
   - Time travel's diffs, the upgrade lens, symbol history, and the sidebar's Versions lens and row glyphs know only toml and smallvec.
   - The real replacement is written but untracked and not compiled in. It carries no signatures, no uses and no impact.
3. **Only Rust has been seen (§2.17).**
   - No non-Rust package page was ever drawn.
   - Two non-Rust symbol pages exist only as fixture scenes over checked-in excerpts, and the owner refuses both roots.
   - Six language taxonomies in the code disagree with each other.
4. **Motion was never finished (§2.14).** None of these ran:
   - both pages' motion phases (FOLIO-C, SYM6-C);
   - the transitions lane's T3 to T5 (FLIP, the fade purge, and Push/Peel/Reel/Odometer);
   - semantic zoom between page and graph;
   - the transition catalog as a desktop test.

   Page to code is a cut.
5. **The one hover grammar reaches one surface (§2.13).**
   - `facet::hover` is used only by the symbol page's host. The package page, sidebar, Find, graph and Library each hover their own way.
   - Lenses, location and version peeks, the peek chain and the ⌘P pin stack are not in the product.
6. **Whole surfaces are placeholders (§2.8, §2.11, §2.12).**
   - The Inbox is one fixed sentence.
   - Six of ten Settings pages can't be reached, and each shows another page's content.
   - The code view is plain text, although `facet::code` highlights seven languages.
   - The Tree page is reachable only from the harness.
7. **The browsing that was designed never started (§2.2, §2.5).** None of these has code:
   - the Home map;
   - the Library as strata;
   - Discover (dock, aisles, registry cards);
   - Add as a verb;
   - adopting a dependency into Cargo.toml;
   - the relay graph.
8. **A ready project can't be managed (§2.1).**
   - Remove, Reveal and Retry exist only on a stopped project's card.
   - There is no Stop, and no confirmation or undo for Remove.
   - A second project was never tried for real.
9. **Instant was only half built (§2.18).**
   - The launch snapshot covers pages already visited, nothing else.
   - Prefetch, pages as plans, `Now<T>`, the world as columns and the main-thread watchdog were never built.
   - No release-mode budget was ever judged.
10. **The proofs never ran (§3).**
    - J2 to J8, J13 and J14 have no files.
    - J11 and J12 exist but never reported a run.
    - The full `verify` gate never gave a verdict.
    - 23 of 25 scene captures were never opened.
    - Most reviewers' fix phases never ran.
11. **A possible P0 that nobody tested (§2.1, §4).**
    - Every harness run and journey forces umask 0077, but a normal macOS launch runs with 022.
    - A lane reproduced the owner refusing its own folders on the next start under 022.
12. **Off macOS, off the keyboard and off English, nothing was done (§2.19).**
    - There are no app menus and no URL handler.
    - Keys collide on Windows and Linux.
    - No accessibility tree is published.
    - There is no localisation.

---

## 2. Surface by surface

Each surface says what was **designed**, what **exists**, what was **never tried**, what is **unfinished**, and what is still **open** for decision.

### 2.1 First run, adding projects, lifecycle

**Designed.** Sources: gui-plan §2.7 ("dependencies arrive on the rings; seam of stages"), MOMENTS "Add", and the W-Install and W-Owner briefs. The design asks for:
- a first run that says what the app is for;
- adding a project by ⌘O, a typed path or the picker;
- indexing that says what it is doing, package by package, while reads are never blocked;
- a failure that names its reason and offers a way forward;
- a relaunch that restores everything;
- many projects.

**Exists (Real).**
- The first-run screen with Add a folder, ⌘O and the toolchain line.
- The typed-path dialog, with completion and inline refusals.
- A clean install that indexes the project's crates from the cargo cache (12 for toml_pin), with per-package additions.
- The stopped-project card with Try again, Reveal and Remove.
- Refusals that survive relaunch.
- Restart persistence of the route, theme and window size.
- Reads served while a package compiles.
- The owner-failed foot with Try again, proven only by an injected failure (M38).

**Never tried.**
- **Umask 022** (FIT-1, INST-1).
  - The owner creates its folders with the process umask, and `crates/platform/durable.rs` refuses a group-readable folder on the next start. The instant lane reproduced exit 70 on the product binary at `25fe3d18a`.
  - The harness and the journeys force umask 0077 (`apps/desktop/src/harness.rs:622`, `harness/install.rs:94`), so no run can see this.
  - `host::private_dir` is test-only.
- **A real Finder launch.** "Finder launch" has always meant `env -i HOME PATH=/usr/bin:/bin` (M13). From code, the project's own dossier loader runs a bare `cargo` from PATH rather than the discovered toolchain (`model/local_package/mod.rs:163`; A5).
- **The native folder picker.** The `install-by-picker` part exists, and no journey uses it (JRN-1).
- **A second project.** Switching, the ring's union of what several projects use, and removing one were never tried with two ready projects (G8, INS-3). The owner serves one flat package list, so the Library can't scope itself to one project.
- **Anything but toml_pin.** Nobody ever added through the dialog (IDX-9):
  - a workspace with members;
  - path or git dependencies;
  - build scripts or proc macros;
  - a non-Cargo project.
- **A real failure.** The owner refuses almost nothing small, so the failure card has only been seen with a replayed refusal (INS-2). J13 has no fixture.
- **The set-aside state.** When an update changes the index layout, a person gets:
  - a full re-index of 10 to 17 minutes;
  - a note;
  - their old state kept, with no way to delete or restore it.

  None of that has a capture (IDX-4, M56).
- **An install on a quiet machine.** Every measured install (826–1032 s) ran at load 18–62, while the copy promises "a few minutes" (INS-8, M35).

**Unfinished.**
- **Project controls** (from code, A8, `code.md` §2.2).
  - A ready or indexing project has no Remove, Reveal or Stop anywhere. `ProjectCommand::for_phase(Ready)` exists, and nothing renders it.
  - `CancelIndex` has no control, so the "stopping" and "paused" states can only come from the owner.
- **Remove** acts at once, with no confirmation and no undo (A10).
- **The owner.**
  - It reports no progress inside one package's compile (M34).
  - Reads still wait during the scan and publish phases (M33).
  - The 5 s Clang sysroot probe fails the whole owner start under load (M37).
- **The copy.** The install copy is out of date ("Pages open when it finishes", "a few minutes"; INS-8). "Your source stays on this machine" is a promise nobody has audited (INS-9).
- **Launch notes** appear only on the Library, so a person who relaunches onto another page never sees them (`code.md` §1.2).

**Open.**
- G9: a shell launch inside a project folder auto-admits it and skips the first run.
- What the first run says for a non-Rust folder. Today it says "Only a Rust project's packages are added for now".

### 2.2 The Library (home)

**Designed.** Three versions of home were designed, and none was chosen:
- **The map.** Sources: gui-plan §2.7, the Orbit4 board, and wave-6 PLAN "Home is the map". Projects sit at the centre with their dependencies in two rings, plus one resume line, one new-release line, and Map/List.
- **Strata.** Source: MOMENTS "Browse". Your crates sit on top, with what they rest on beneath as true layers. A loupe reads the dense lower layers; typing lights every matching name, and V shows versions.
- **Cards.** Source: COHESION. One card per package, each with its shingle overview.

**Exists.**
- **A list form (Real).** `shell/bodies/orbit.rs:3` says so: "The rings map is wave 3; this is its list form." It holds:
  - project tiles;
  - a flow-wrapped ring of package names;
  - arrival lines;
  - the install block;
  - failure cards.
- **The resume line (Fixture).** "Continue …" reads the fixture world whenever the hand holds cards.
- **The Tree page (Rig).** It shows roles, alerts and duplicates, and is reachable only from the harness (ROUTE-DEAD-3).

**Never tried.**
- A Library with more than about 13 packages.
- A ring with two versions of one crate (M31).
- 200 % text at phone widths on real data (FLUID-14, RFit-9).

**Unfinished.**
- **Motion.** When the list grows, chips overlap mid-glide and jump 1.1 px on arrival (M17, FLUID-4). The Library region differs between a settled frame and a fresh boot (M18). The install seam's fills stay live past their budget (M19).
- **The ring's Unavailable state** draws nothing (`code.md` §1.1).
- **`OrbitRoute::Project`** has no body, and no person can reach it (ROUTE-DEAD-2).
- **Chip hit targets** are 23 px at 85 % text (FEEL-D12).

**Open.**
- Which home: the map, strata or cards? The owner has not seen a native version of any of them.
- Hover popovers on the Library (FEEL-D9).
- Whether the Tree page gets an entry or goes.

### 2.3 The frame: titlebar, jump bar, foot, zen, pins, drawer

**Designed.**
- **gui-plan §2.6:**
  - a shelf that can be dragged to any width from 200 to 420 px;
  - a status bar that says "hold ⌘ for keys".

  The bead thread and altimeter it also specified were superseded by the jump bar.
- **Wave-6 PLAN:**
  - the jump bar is the path at rest and the query when you type;
  - the foot carries the address, with Copy link, Open in editor and Reveal;
  - the foot also shows the hand's marks and a live index rule.
- **v5:** the jump bar's `›` lists the page's sections.
- **MOMENTS:** the jump bar keeps the hop trail as a sentence (`toml → toml_edit ← cargo`).

**Exists (Real).**
- The titlebar, with the traffic-light inset, shelf toggle and view switch (Page / Code / Graph).
- The jump bar with sibling menus on each segment.
- Back, with a long-press list.
- The Ask field and the inbox button.
- Three width modes.
- The foot, showing the hand's marks and notices.
- Zen.
- The pins column at 1900 effective px and up.
- Shelf, spine and drawer modes, with hysteresis.

**Never tried.**
- The pins column. No test or journey ever pins anything (FIT-5).
- Deep jump-bar paths on real symbol pages (TIS-3, FLUID-16).
- The drawer's content and keyboard at phone widths (SIDE-5).
- The titlebar below 560 px at 200 % text (RFit-13).

**Unfinished or unbuilt.**
- **The shelf can't be dragged to a new width.** `controls::splitter` has no caller (`code.md` §2.7).
- **The foot.** Its address menu, live index rule and "hold ⌘ for keys" hint are unbuilt. A `nudox://` address can be copied, but nothing opens one.
- **The one query.**
  - Typing anywhere doesn't start it.
  - It doesn't light the shelf, the page or the graph.
  - ⌘↵ is unbound.
  - Search still lives in four places: Ask, Find, the graph's own "Find a symbol" field (`apps/facet/src/graph/view.rs:752`), and sidebar narrowing (PLN-1).
- **The jump bar** shows the current path, not the trail (GLYPH-8), and its `›` does not list sections.
- **The view switch** isn't drawn below 760 px, so at phone width the only way to switch view is the keyboard (FIT-4).

**Open.**
- Where the address lives, and what clicking it does.
- FIT-4: a pointer path to switch view at phone width.

### 2.4 The sidebar

**Designed.** COHESION's ten primitives (scope, lens strip, narrow, state on rows, peek, twins, hold, trail, sticky ancestors, follow), grounded in `research/NAV.md`. The owner named the sidebar the priority for interaction design.

**Exists.**
- **All ten primitives in code** (W-Side).
- **Real on the real install** (F-Shell-1):
  - narrowing ("25 of 665");
  - told-apart duplicate module names;
  - imports excluded;
  - toml_pin listed once.
- **Fixture:** the Versions lens and the row state glyphs read fixture releases (SIDE-2).

**Never tried on real data (SIDE-1, M54).**
- The peek on the float layer.
- Twins.
- Hoist.
- Sticky ancestors.
- Hold chips.
- The trail.
- The lens chords G C / G V / G R / G U.

W-Side never took a harness capture, and R-Side never launched.

**Unfinished.**
- **Two counts.** toml's Contents shows 88 while the page says "31 public names", because they come from two sources (SIDE-3, M29).
- **Repeats.** A type repeats once per impl block (`desktop-record-rust`; SIDE-10, M28).
- **Keys.** ⇧⌘J reveal has no key-table row, and the hold chips stop at ⌘5 (SIDE-4).
- **The drawer** has no keyboard (SIDE-5).
- **Twins on package cards** don't light, because the cards register targets with no source (SIDE-6).
- **The widen row** goes to Find, because Ask can't be opened with words already typed (SIDE-7).
- **The spine's content** under the new sidebar was never looked at (OLD-11).

**Open.**
- A query that begins with gc, gv, gr or gu can't be typed, because G starts a chord (SIDE-8).
- Whether Used by and Rests on say how complete they are (RES-14).
- The Versions lens contradicts MOMENTS, which ruled that there is no Versions lens, only contents at a release.

### 2.5 Find, Compare, the Tree, Discover and Add

**Designed.**
- **The package-browsing brief:** the S1 tree, the S2 judge card, and roles.
- **Browse rulings:** Add writes the dependency into Cargo.toml and shows the exact diff first; the rules for "only in name".
- **MOMENTS Discover:** a dock, category aisles and registry cards. `+` flies the card into the dock, and a toast offers Undo.
- **gui-plan §8.2:** find by shape, the constellation, and chains.

**Exists.**
- **Find (Real reads).** It gives ranked answers, held packages, and Compare of 2 to 4 packages.
  - It is reached only from Ask's "all results" or the sidebar's widen row. No control opens an empty Find (ROUTE-DEAD-4).
- **The add control (code only).** It sits on a Find row, and on a package page for a release the library lacks, and uses the offline cargo cache only. It was never seen drawn on the real library (G10, M10, ACQ-4).
- **Gallery only:** find by shape, chains and the constellation.

**Never tried.**
- The add control end to end on the real install. J7 is unwritten.
- The package header's "NOT IN YOUR LIBRARY" offer. No scene shows it.
- Compare on real data after the false-equivalence rules (OLD-32).
- Find at phone widths on real data (FEEL-D6, D7).

**Unfinished.**
- **G12 (M11).** The `desktop-find-offer` scene finds `anyhow`, a crate only in the cargo cache, which suggests W-Acquire fixed this. `production-readiness.md` still lists it as open. Verify.
- **Downloads.** A source that would need a download is shown disabled, with no words (ACQ-2).
- **Find layout** (FEEL-D6, D7, RFit-7).
  - The name column squeezes to 19–32 px with no ellipsis.
  - At 200 % text, the settled frame differs from a reduced-motion boot.
  - The inspector glides across the results.
- **Separators.** On browse pages they are ink4 at 2.6:1, and the ink4 guard test scans only `src/shell` (FIT-3).
- **Relaunch.** Find, Compare and the Tree are not persisted (ROUTE-PERSIST).

**Unbuilt.**
- Discover.
- Registry answers under the library's own answers in the jump bar.
- "You already have this".
- Adopting a dependency into Cargo.toml, with its preview.
- The judge card.
- The Add landing (RES-2).
- The consequence lens (RES-3).

**Open.**
- What "Add" means: read-only into the library (built), into a project's Cargo.toml (designed), or both. This is MOMENTS open question 2.
- The network policy.
- Popularity and downloads (MOMENTS open question 3).
- Compare across packages survives, although COHESION ruled that Compare is for versions of one package.

### 2.6 The package page

**Designed.**
- **The W-Folio brief and the v6 board (`?v=package`):**
  - the hero;
  - the crest: licence stamp, heads-up stack, weight iceberg, advisories;
  - a ticker with fisheye;
  - a features bar with locks;
  - the shingle territory, opening into cards;
  - a dedicated module view;
  - time travel with a warmer tint.
- **COHESION and MOMENTS:**
  - legit first;
  - contents at a release;
  - Rests on / Used by, rethought;
  - every package name a preview link.
- **Phase C:** the shingles fly to their cards.

**Exists (Real for Rust only).**
- The hero and the crest.
- The ticker, with release dates from the cargo index cache.
- The features bar with its lock animation.
- The shingles, module cards with badges, and the module view.
- The README.

The source facts are read from Cargo.toml and Rust source on disk.

**Never tried.**
- Any non-Rust package. Every crest cell would read Absent (LANG-PACKAGE-FACTS).
- A many-module crate on the real install after the module-path fix (M50).
- The 12 real cache packages beyond toml's intro (PH-2).
- Very long names, and 150 % text (RFolio-N5).
- A real advisory. None is configured anywhere.

**Unfinished.** R-Folio's list is the real backlog: its fix phase and FOLIO-C never ran.
- **Layout and resize.**
  - D1 (worst): live resize still flickers, and got worse after FLUID-B (39 → 51 A-B-A frames).
  - D2: ADVISORIES drops onto a row of its own at 1440–1600 px.
  - D10: the crest re-lays out on arrival.
  - FLUID-2: 16 hard thresholds, and 94 of 285 widths with text past the edge (feature chips and the repository URL, from 416 px down).
- **Missing parts and dead controls.**
  - D3: the ticker and banner are missing for an unpacked registry tree. This is unverified after the source-releases change.
  - D8: no pointer cursor on shingles, ticker or berg.
  - D14: the advisories cell is inert.
  - The four package lanes exist as route values, and the body ignores them (ROUTE-DEAD-1).
- **Motion.** D11: opening a module or the berg is a cut, and Phase C is unbuilt.
- **Visual defects.**
  - D12, the README: raw reference URLs, a dangling "(LICENSE-APACHE or )", no working links, a misaligned edge, and clipping at 480 px.
  - D13: region labels clip.
  - D14: badge hit targets are 21 px.
  - D15: the ticker label covers "your pin".
- **Data the page lacks.**
  - N1: "yours" never lights, because every shingle is hard-coded `Use::Elsewhere`.
  - OLD-17: on dependency marks, uses, features, purpose and tree standing are hard-wired unknown ("your tree is not read yet").
  - M16: the page says "17 packages beneath" while the Library says 12.
- **Unseen changes.** Code changes in the tree that nobody has looked at: crest doors, Esc folding a module, the badge tip on the float layer, a dwell before popovers, and a heads-up overlap of 6 px.

**Unbuilt.**
- Rests on / Used by as a page section, with "why is this here", duplicates, and what removing the package would drop.
- Package names in the README as preview links.
- Hovering a region lights its shelf row, and a module lens.
- The locator.
- Strands between shingles.

**Open.**
- The advisories feed.
- Downloads.
- The dependencies/dependents view, which the owner asked to be rethought.
- The 2560 layout: today a 784 px column in an empty window.
- Three reader width systems (784 px folio, 860 + 280 px symbol page, up to 1800 px territory).

### 2.7 The symbol page

**Designed.**
- **W-Sym6:** the simple form the owner chose, close to docs.rs.
- **The docs.rs parity list.**
- **gui-plan §8.3**, mostly superseded.
- **The v5 specimens across seven languages.** The simple form superseded the specimens, but the rule that every ecosystem is first class still stands.

**Exists (Real for Rust).**
- The header.
- The call, with ports and outcomes.
- Docs with folds.
- If it fails.
- What it is: an enum fork, a struct bracket, or a trait's required methods.
- Verb groups.
- In your workspace: a package picker, verb chips, a tests toggle, and click-to-editor.
- The rail: source, your packages, next to it, it can, across releases, and how we know.
- Generic and error cards.

**Never tried.**
- A page for a real Python, TypeScript, Java, C# or C/C++ package (RSym-N1, N6). From code, Java, C# and C/C++ get no call rail at all (LANG-SYMBOL-PAGE).
- The page at phone widths on real data.
- Keys in the real shell (RSym-K, OLD-36).
- A real editor launch (RSym-N3).
- Reduced motion, the first frame after launch, and restoring the scroll position on Back.

**Unfinished.** R-Sym6's list; its phase 2 and SYM6-C never ran.
- **Layout and motion.**
  - D1: overflow at phone widths.
  - D2: layout changes are cuts. At 1344 px the rail edge moves one block 1084 px in a single frame.
- **Words and data.**
  - D4: verbs are classified line by line.
  - D5: bare numbers on enum cases (unverified).
  - D10: Next to it has no signatures, doc links keep their backticks, and "Elsewhere in the registry" is never drawn.
- **Hover, contrast and icons.**
  - D6: no hover on Example, "N more", or the rail's source link.
  - D8: the Glacier chip was claimed fixed, but its lint was never quoted.
  - D9: the function mark reads as a disclosure caret, and two icon systems coexist.
- **Places.**
  - N2: places in files that can't be read are left out.
  - M27: the as_str page shows a false place inside a licence comment.
- **The rail and error kinds.**
  - N4: the rail is not sticky.
  - N5: error kinds never reach a real page, because Result alias → Error → Category is not chained.
- **Parity.** The docs.rs parity gaps were never re-scored after the rewrite (OLD-14):
  - required vs provided methods;
  - blanket and auto impls;
  - the deprecation payload;
  - cfg and feature gates;
  - Deref methods;
  - macros;
  - confidence, which is never rendered.
- **The hop.** The shared-element hop of the old page was probably lost in the rewrite (GLYPH-1).

**Superseded, but still in facet and the gallery.** The owner's simplicity ruling set these aside:
- Getting one;
- How it fails;
- cousins;
- the locator;
- specimens;
- the prism on the page;
- the lab instruments.

Whether any of them returns is a decision (§5).

**Open.**
- A sticky rail.
- "83 % derive" and "Elsewhere in the registry". Both need index data.
- How the person chooses their editor.

### 2.8 The code view

**Designed.**
- **gui-plan §2.7 "Source":** a gutter, a peek card, and the doc in the margin.
- **Source4:** the owner asked for "a specialized GUI format instead of code views". Neighbours fold to one line, mint ticks mark lines from the newest release, and ⌥ spells ages.
- **v5:** the Peel transition between page and code.

**Exists (Real).**
- Plain text around the declaration, with numbered lines and the declaration lit.
- "N lines above/below".
- A crumb.
- A margin with the first doc sentence and up to five callers.

**Unbuilt.**
- Syntax colour. `facet::code` highlights seven languages behind a feature and has no caller.
- Scrolling to and lighting `route.line`.
- Open in editor from this view.
- Copy, and search within the file.
- Links inside the text.
- Folding.
- X-ray.
- The Peel transition. Page to code is a cut (`shell/reader.rs:22`).

The callers in the margin are pointer-only.

**Never tried.**
- Any non-Rust file.
- A long file.
- The continuation indent of a wrapped signature at 760 and 480 px (OLD-6).

### 2.9 The graph, the world, the hand, the tour

**Designed.**
- **gui-plan §8.2:**
  - a nested, deterministic layout;
  - semantic zoom;
  - the prism;
  - reach;
  - find by shape;
  - the constellation;
  - chains and the road;
  - the Start here tour;
  - the trail drawn on the map.
- **gui-plan §8.4:** moving between graph and page.
- **v5:** the graph is the same space zoomed out, and semantic zoom folds a page's sections into edges.
- **The W-World brief:** the world projected from the index, a columnar mmap format, label LOD, and open in 100 ms or less.
- **MOMENTS:** the relay graph.

**Exists (Fixture).** Everything the graph does, the hand (5 cards, ⌘D, ⌘1–⌘5, roads) and the T tours all run over `Nudox-Design-System/v4/graph/world.json` (A1).
- **What it holds.** 37 packages and 55,988 nodes: 26 of this repository's crates, six of them marked "yours", plus five externals. So "yours" means this repository.
- **How it loads.** The file is found by `env!("CARGO_MANIFEST_DIR")`, so a bundled app would show the Unreadable fault (`code.md` §1.1).
- **On screen,** it is labelled "Graph fixture · …".

**Never tried.**
- The graph on a person's project. It can't have a node.
- Opening the graph within the 100 ms budget. 393–432 ms was measured (INST-9).
- A panic on the world thread, which would be silent and permanent (ROpen-D5).
- A long session's memory (ROpen-D8).

**Unfinished.**
- W-World's checkpoints CP0 to CP3 never ran, because the lane never launched (WLD-1, WLD-2).
- The Start here tour was removed from the package page and never returned.
- The graph at a release, and package hulls as territories.
- Leftovers from transitions T2 (TRN-5):
  - the prism's labels stay dim for 226 frames;
  - the gather fades;
  - stubs don't travel;
  - the camera doesn't pull back.
- The focus card jumps 588 px at mode changes. The glide is proven only by a rig test (RFit-15).
- The reduced-motion flight is still a 120 ms crossfade (OLD-20).
- Every hand touch re-runs the producer walk on the UI thread (ROpen-D4), and the MIGRATE move to `hand_view_for` was never done (ROpen-C4).
- The graph footer's two lines sit on different baselines (FEEL-D11).

**Open.**
- Ship the graph labelled, hide it until it is real, or schedule W-World (`production-readiness.md` §11.5).
- Whether the hand needs the world at all. Its roads come from the fixture's recipe table.

### 2.10 Releases, time travel, upgrade, the Inbox

**Designed.**
- **gui-plan §8.4:** the version comb re-scopes the page and the graph. The upgrade lens says things like "2 of the 9 places your code uses toml change".
- **MOMENTS:** contents and versions are one thing, and hovering a release recolours the words.
- **COHESION:** "What changes for you" comes first.
- **NAV research:** no existing tool scopes a diff to the consumer's own call sites, so this would be new.
- **The Inbox:** releases you follow.

**Exists.**
- **The ticker (Real),** with real dates from the cargo index cache.
- **Fixture, for toml and smallvec only:**
  - diffs;
  - the upgrade lens;
  - symbol history;
  - past tints;
  - the sidebar's Versions lens and its glyphs.
- **`runtime/releases.rs`** (M7, M8, `code.md` §2.5):
  - it is untracked, and not declared in `runtime/mod.rs`, so it doesn't compile;
  - no caller has switched to it;
  - its before/after, uses, impact and semver-slip fields are empty.
- **The Inbox:**
  - one fixed sentence;
  - no data;
  - no way to follow anything;
  - reachable only from a titlebar icon that is hidden at narrow widths.

**Never tried.**
- Reading an earlier release for real, such as toml 0.5.11 against 0.8.23. J8 is unwritten.
- A symbol at another release. It shows `Unread::ReleaseNotHere`.
- The upgrade lens on any package other than those two.
- The graph at a release.

**Unbuilt.**
- "What changes for you" verdicts.
- Impact on the consumer's own code.
- Semver-slip.
- The release chip in the hero.
- Location and version peeks. `PageKey` has no variants for them (OLD-2).
- Following a release.

**Open.**
- Whether the Inbox stays. It promises a feed the service doesn't publish.
- The Versions lens against "contents at a release".

### 2.11 Ask and search

**Designed.**
- **gui-plan §2.7:**
  - Ask (⌘K) ranks mixed results, with one reason per row and a preview;
  - a search results page, with filters drawn as combs.
- **gui-plan §8.2:**
  - find by shape, and chains;
  - an empty box that teaches.
- **Wave-6 PLAN:** one query.
- **Budgets:** search results within 50 ms (gui-plan), and a keystroke repainted within 8 ms at p99 (v5).

**Exists (Real, for one query).**
- ⌘K searches the real install. "toml Value" ranks toml's `Value` first, and ↵ opens it.
- Faults are said in words.
- Walking the results previews each place.

**Never tried.**
- Any other class of query (M23).
- A query with zero results on the real install.
- The widen-to-Find path on real data.
- Ask at phone widths with the real library (FLUID-11).
- The stall on the first text focus (INST-6).
- Fuzzy matching and recents (RES-13).
- Performance over 12,000+ declarations.

**Unfinished.**
- **Speed.** A search takes seconds in a debug build (M21, INST-12), against a 50 ms budget and a 90 ms debounce, which contradict each other.
- **Silent gaps.** Rows and evidence that don't pair are left out silently. Only stderr says so (M22).
- **Fault words.** The fault text is raw protocol prose (M24).
- **"Or ask".** The placeholder says "Find a name, or ask", but no natural-language path exists.
- **The command palette.** `Overlay::CommandPalette` is a search box, and the action layer for a command palette is dead (ACTION-LAYER).

**Open.**
- A command palette, or delete the action catalog (OLD-8).
- Scoping results: here, then this package, then everywhere.
- The search budget.

### 2.12 Settings

**Designed.**
- **gui-plan §2.7:** appearance, index and registries, and keys.
- **Settings4:** a text-size comb slider.
- **L1:** MCP setup.
- **W-Sym6:** a choice of editor.

**Exists.** Four bodies serve ten pages:
- **Appearance (Real; J10 PASS):** theme, contrast, density and motion.
- **Index & registries:** the compiler line and the health rows.
- **Keys:** a read-only list.
- **About:** version and service mode.

**Unfinished or unbuilt.**
- **Six unreachable pages.** Editor, Agents, Connections, Privacy, Registry and Legend have no entry, and each draws another page (`code.md` §2.4).
- **Stored settings that do nothing.** These have no control and no effect: privacy, advisories, cache on/off and days, the context panel, and connection status (SETTINGS-DEAD).
- **No editor template.** `host/editor.rs` is always called with None.
- **No MCP setup.** `TestConnection` is dispatched by nothing.
- **A stuck report.** When the health read faults, Index & registries says "on its way" forever (`code.md` §1.1).
- **No keyboard reach.** The Settings controls are not keyboard targets; J10 uses clicks (A10).
- **Density** has never been shown to fold summaries away as designed.
- **No coverage.** No test renders the Index or About pages.

**Open.**
- Text size as a control, or system-only (decided against, though Settings4 had one).
- Which pages ship.
- What privacy means while there is no network policy.

### 2.13 Peeks, lenses, cards, menus, toasts, dialogs

**Designed.**
- **gui-plan §6.1 and §6.2, the ladder:**
  - resting on a thing climbs one rung, and activating it descends to its page;
  - ⌥ raises everything one rung.
- **gui-plan §6.1 and §6.2, peeks and lenses:**
  - peeks chain three deep with a crumb, and pin to a column or a ⌘P stack;
  - lenses exist for the release comb, a module and a language.
- **gui-plan §7:** the float layer contract.
- **v5, one hover grammar:**
  - at 0 ms the target's ink rises and everything related lights;
  - at 350 ms a peek unfurls.
- **MOMENTS, progressive-disclosure previews:** Gwern- and Wikipedia-style popups, chained and joined by strands.

**Exists.**
- **Real:** the float layer, tooltips, menus, dialogs, the scrim and hint labels.
- **Peeks:** symbol peeks, package peeks and the sidebar peek.
- **Cards:** the symbol page's generic and error cards, and the package page's crest plates.

**Unbuilt or unmounted.**
- **Lenses.** `overlay::lens` has no desktop caller.
- **Peeks.**
  - Location and version peeks can't be named.
  - The ⌘P pin stack doesn't exist.
  - Chained peeks are unverified.
  - The "why care" fact on peeks is missing (OLD-3).
- **Toasts.** Nothing posts one, so ⌘⇧C gives no feedback (FEEL-D10).
- **Explanations.** Marks don't explain themselves, and the Legend page is an alias of Keys.
- **Cursors.** There is no pointer cursor on hand-drawn elements.
- **Confirmations.** No confirmation dialog exists anywhere.

**Not unified.**
- `facet::hover` reaches only the symbol page's host. Every other surface hovers ad hoc.
- Hovering opens a peek only on pages that build their own cards (OLD-4).
- The Library has no hover card at all (FEEL-D9).

**Unfinished.**
- Peek and menu dismissal takes 220 ms by construction, against a 120 ms bar (FEEL-D13).
- Moving from one popover to another was never measured on real surfaces (FEEL-N5).
- A peek on an index key shows only a name for about a second (INST-11).
- Nobody checked whether the shell swallows arrow keys in the other popovers: the Find inspector, the release picker, and the Add dialog's completion list (INS-7).

**Open.**
- Hover timings. The documents disagree: 80 or 120 ms before prefetch, and 120, 280 or 350 ms before a peek. The research suggests two timers (RES-9).
- Library popovers.
- The dismissal bar.

### 2.14 Motion and transitions

**Designed.**
- **gui-plan §2.3 and house rule 8.**
- **v5 §3:**
  - the spatial map;
  - the row becomes the page;
  - semantic zoom;
  - no crossfades;
  - reduced motion as a cut plus a 1.2 s mark;
  - the transition catalog as a test.
- **The transitions brief, T0 to T5:**
  - Open/Close;
  - Fold/Unfold;
  - FLIP;
  - the purge of fades;
  - Push, Peel, Reel and Odometer.
- **COHESION's transitions table:**
  - package → symbol, with the locator;
  - symbol ↔ graph;
  - package → graph;
  - home → package;
  - the film of a Library card becoming a package page.

**Exists (Real).**
- The deterministic clock.
- The motion engine.
- CARRY.
- Open/Close: the row becomes the page.
- J10's motion is clean since F-Shell's Course fix.

**Unbuilt or never ran.**
- **T3, FLIP.** `flow-list` and `flow-reflow` have failed since wave 2.
- **T4, the purge.** Fades remain in the toast, dialog, version comb, controls and presence.
- **T5:** Push, Peel, Reel and Odometer.
- **Designed moves never built:**
  - semantic zoom;
  - shingles flying to their cards;
  - the Library card → package film;
  - motion for hoist and lens switches;
  - the theme sweep.
- **Reduced motion as a cut plus a mark.** The graph still crossfades.
- **The ambient pulse.** The ground never twinkles, because its phase is never driven.

**Never tried.**
- The pixel transition catalog on the desktop harness (TRN-7).
- Catalog coverage of peek and pin, the tour, jump menus, toasts, dialogs and reduced motion (TRN-8).
- A reduced-motion film of each verb (OLD-13).
- The flash before first paint, which only a real window can show (FEEL-N6).
- The storm on `desktop-orbit` since the Course fix (FEEL-D5).

**Unfinished.**
- **Settings opening** shows two contexts, then a near-empty reader (FEEL-D17).
- **Route changes.**
  - A fast route change paints the skeleton of a route already gone (ROpen-D2).
  - Back and Forward dip through an empty reader for 48 ms (ROpen-D3).
- **Reflow.**
  - Blocks under a mode change jump: 16 thresholds on the package page, 5 on the symbol page.
  - Flow chips in flight cross each other.
- **Titles.** Whether the title is still a shared element after the rewrite is unrecorded (TRN-4). The travelling-name ruling in COHESION conflicts with PLAN §2a.

**Open.** RES-10 lists fifteen contradictions, for example whether a hop moves the world or holds it still, and 1 s against 250 ms.

### 2.15 Keys and focus

**Exists (Real).**
- The key table (35 commands).
- Hint mode.
- Peek and pin.
- The Esc chain.
- Zen.
- ⌃1–⌃4 for depth, and ⌘1–⌘5 for the hand.

**Never tried.**
- **J11.** It covers every key, generated from the table. It was written, and never reported a run.
- **The key sweep** in the symbol, Find and graph contexts (FEEL-N3).
- **The simple symbol page:** Tab, J and Space in the real shell (OLD-36).
- **Settings controls,** reached by keyboard (A10).
- **Hint mode past 256 targets** on the real 665-row sidebar (FEEL-D15).

**Unfinished.**
- **Contradicting shortcuts.** The dead action layer lists shortcuts that clash with the live table: ⌘0, ⌘-, ⌘←/→, ⌘⇧P, ⌘N and ⌘B (ACTION-SHORTCUT-CONFLICTS).
- **Silent keys.** ⌘⇧C is silent, and Hold on the Library gives no feedback (FEEL-N4).
- **The package page.** Its features, ticker and berg doors are unverified (RFolio-D7).
- **Focus restore.** Back restoring focus is proven only on the Library tile (TIS-1).
- **The continuity check.** It doesn't treat a switch between pointer and keyboard as a retarget (M20).
- **Provisional rows.** G, ⌘. and ⌘⇧. were never confirmed by the owner.

**Open.**
- Keep or delete the action catalog.
- The provisional keys.
- The G chord against typing.

### 2.16 Widths, text size, density, themes, contrast

**Exists (Real).**
- `facet::fluid`: Room, ramps, and Modes with hysteresis.
- The dock ladder.
- A 320×480 minimum window.
- The themes, high contrast and density.
- Text scale from 85 to 200 %.

**Never tried.**
- **The shipped install swept 320–2560 px** (FLUID-15, M53). FLUID-C swept a 13-package clone from before F-Data, so done-list item 8 was never shown.
- **Pictures after the merge** of the frame changes (FLUID-1).
- **Settings at phone width** (FLUID-13).
- **Density's effect** on real pages.
- **Audits.** The semantic colour audit, and icon crops at 1× and 2× (FEEL-N2).
- **Glacier and 200 % text** on the browse extras (FIT-6).
- **High contrast** on the systemic contrast failures (OLD-12).

**Unfinished.**
- **Layout decisions left undone.**
  - The 2560 layout.
  - Three reader width systems.
- **Leftover width code.**
  - Hand-rolled widths in facet (the KNOWN list).
  - The dead `Measure::room`.
- **Resize defects never re-checked.**
  - FEEL-N1: the cliffs.
  - RFit-3: the fast shrink, never measured after the merge.
  - RFit-6: shelf-collapse legibility, never re-run.
- **The type scale.** The migration from `tokens::ty` to `scale` covers one of 23 files (`code.md` §3.11).

**Open.** Does amber mean caution (DIRECTION) or changed (COHESION)?

### 2.17 Languages other than Rust

**Designed.**
- **gui-plan §1:** seven ecosystems.
- **v5:** "every ecosystem first class; a page that only shines on Rust fails review".
- **MOMENTS:** the symbol page across languages, with grades for how each fact is known.

**Exists (LANG-\*).**
- **Only Rust end to end:**
  - package facts;
  - acquire;
  - releases;
  - receiver verbs;
  - toolchain discovery;
  - the registry;
  - version semantics.
- **The symbol page's call rail** for Rust, Python, JS/TS and Go. Java, C# and C/C++ get none.
- **Doc sections** read by each language's own convention.
- **The ecosystem mark,** which renders all seven.

**Never tried.**
- **A non-Rust package page.**
- **A non-Rust project** added through the dialog.
- **TypeScript and Go** exist only as two fixture excerpts (zod, pflag), whose roots the owner refuses (IDX-3). Those scenes may show a preserved earlier generation.
- **Python, Java, C# and C/C++** were never indexed through the desktop (OLD-22).
- **Toolchain discovery** for anything except Rust, and Go through an environment variable.

**Unfinished.**
- **Taxonomies.** Six language taxonomies disagree (LANG-ENUMS), and Unknown falls back to a Rust badge.
- **Detection.** The add dialog doesn't recognise C#, or Python without pyproject.toml.
- **Refusals.** A C# refusal comes with no remedy text.
- **Versions.** Only crates.io semver is modelled.
- **Shapes.** Find and Compare's shape pipes return None for Go and C++.
- **Code.** The code view highlights no language at all.
- **Modules.** Placing names in modules on the package page works for Rust only.

**Open.**
- Which language ships next.
- Acquisition for npm, PyPI and Go, which runs into the network policy again.

### 2.18 Instant: startup, prefetch, budgets

**Designed.** v5 §2 sets budgets:
- cold boot under 150 ms;
- a route change within 16 ms;
- the graph within 100 ms;
- a keystroke repainted within 8 ms at p99;
- hover to peek within 16 ms;
- no main-thread task over 4 ms.

It also designs the architecture to meet them:
- a snapshot-first boot;
- the world as columns;
- pages as plans;
- prefetch by intent;
- `Now<T>`;
- a cached harness index.

**Exists.**
- The window opens first.
- A snapshot of visited pages.
- Reads off the UI thread.
- `Memo` for some caches.

**Never tried.**
- **Release-mode budgets.** None was ever judged, anywhere (M47, INST-19, PH-14).
- **A quiet machine.** Every number was taken under load 12–227.
- **The first exec of a signed bundle** (INST-2).
- **Relaunching the toml_pin install** from its snapshot (INST-4).

**Unbuilt.**
- **Prefetch by intent.** A shelf-row click still waits about a second for a symbol page (INST-7).
- **The page architecture:** pages as plans, `Now<T>`, and the world as columns.
- **The main-thread watchdog** (INST-10).
- **Startup speedups:**
  - prewarming the glyph atlas (INST-5);
  - building the Metal shaders at build time. `runtime_shaders` is still on, which costs a 140 ms stall (INST-3).

**Unfinished.**
- **Startup:** the owner takes 5.8–9 s to start (INST-8).
- **Memory:**
  - caches are unbounded (ROpen-D8);
  - `source_facts` never evicts (RFolio-C5).
- **Rendering:** a pointer move re-renders five regions (ROpen-N5).

### 2.19 Platform, accessibility, localisation

**Exists.**
- **macOS only.**
- **A hard-coded 78 px** traffic-light inset.
- **A minimal bundle:** one binary and an `Info.plist`.

**Unbuilt (from code).**
- **App menus.** There is no Quit, Hide, Minimise, Close window, About or Edit menu. Whether GPUI supplies defaults is unverified (MAC-QUIT-SAVES).
- **Dock behaviour:** no Dock menu and no reopen handler.
- **A `nudox://` handler.**
- **An app icon.**
- **Window position and display restore.**
- **A second window.**

**Never tried: Windows and Linux.** From code:
- **Keys.**
  - Ctrl-1..4 collide with the hand's secondary-1..5 (PLAT-KEY-COLLISION).
  - The key caps arm on the wrong modifier (PLAT-REVEAL).
  - Caps and copy say ⌘ everywhere.
- **Windows.**
  - The titlebar is transparent, with no window controls.
  - The editor fallback is `xdg-open`, which doesn't exist there.
- **Linux:** there may be two title bars.
- **Plumbing.**
  - The owner endpoint is a Unix socket.
  - The harness is Unix-only.

**Accessibility.**
- **No accessibility tree is published.** `AccessibilityRole` and `ActionNode` exist only in the dead action layer, and VoiceOver was never tried.
- **Hit targets under 24 px remain:**
  - package badges, at 21 px;
  - Library chips at 85 % text;
  - Find's inspect controls.

**Localisation.**
- **There is none.** Every string is inline English.
- **Untested:** right-to-left text and long words.

---

## 3. Proof that never ran

### 3.1 Journeys

- **Ran.**
  - J10 passed on the real owner (F-Shell-1).
  - J0 reached 8 of 11 checkpoints under the Finder-like environment at 19:53, before later fixes.
  - J1 and J9 were "running" at 00:08, and no verdict was recorded.
- **Exist, never reported a run.**
  - J11, generated from the key table.
  - J12, the crawler. `crawl.rs` is modified and uncommitted.
- **Stale gates.**
  - J0 (G15, G4), J1 (G15) and `install.part` (toolchain) are gated on things the product now does.
  - J9's gate is already removed in the tree.
  - A stale gate doesn't fail a run; it hides the fact that the check now passes.
- **No file:**
  - J2 find by shape;
  - J3 upgrade;
  - J4 the map;
  - J5 failure pages;
  - J6 and J14 weather;
  - J7 add from browsing;
  - J8 an earlier release;
  - J13 failure and retry.

  J3, J4 and J8 are blocked on fixture data, and J13 on a real failing fixture.
- **No journey mutation proof exists.** FINISH asked for proofs on J0, J11 and J12.
- **The parts library** has 6 parts. The brief asked for more: open-package, open-symbol, search, pick-result, view-release, set, chord, hover-sweep and back-to.
- **Every journey installs the same Rust project.**

### 3.2 Gates

- **`verify`** never gave a verdict this wave. It was killed after 63 minutes, and later after 50.
- **Never ran:**
  - `storm` with 8 seeds × 300 acts on every scene;
  - `matrix` on the world, graph, symbol and package scenes;
  - `lint --scene all`, which dies after the fifth scene on a wall-clock wait;
  - `legibility --catalog` on the desktop.
- **Settle equals fresh** failed in J0's Library region (M18).
- **Release budgets:** never judged.
- **Suites.** The last counts conflict: 542 passed 0 failed, against 538 passed 4 failed, and then three real-owner tests flaked under load. There is no run of every suite on one tree, and no CI.

### 3.3 Scenes

- **25 scenes,** all on fixture roots except `desktop-install`.
- **`capture --scene all`** ran end to end once, and 2 of its 25 PNGs were opened.
- **No scene for:**
  - Settings;
  - the Inbox;
  - the Add dialog;
  - Ask;
  - the hand;
  - a peek or a pin;
  - zen;
  - the drawer;
  - a failed, missing or paused project;
  - a package or symbol at another release.

### 3.4 Lanes that stopped short

- **Phases that never ran:**
  - SYM6-C;
  - FOLIO-C;
  - INSTALL-B and C;
  - JOURNEY-B and C;
  - OWNER-A, B and C;
  - ACQUIRE-B and C;
  - FEEL-B;
  - transitions T3–T5;
  - W-Surfaces;
  - W-World;
  - W-Green's routed list;
  - page CP3.
- **Reviewers never launched:** R-Side, R-Journey, R-Install, R-Acquire and R-Feel.
- **Fix phases never reported:**
  - R-Sym6;
  - R-Folio;
  - R-Fit (its code landed);
  - R-Open3 (only MIGRATE.md).

  Their defect lists are the real backlog (PH-7).
- **The finishers stopped** at F-Data milestone 2 and F-Shell milestone 1. Their milestone-3 work, 44 paths, is uncommitted and unreported (PH-13).

### 3.5 What the harness can't see

- **Unseen text:**
  - button labels (D3);
  - lines that mix typefaces (OLD-23).
- **Unseen state:**
  - the pointer cursor;
  - the flash before first paint;
  - element boxes and focus (FEEL-N7).
- **Misread motion:**
  - it can't tell a followed layout step from a jump (FLUID-9);
  - it lints cascades mid-reveal (RFit-18).
- **Its own setup hides defects:**
  - it forces umask 0077;
  - it uses fixture roots and pre-admitted projects;
  - its waits are wall-clock.

---

## 4. Experiments nobody ran

The cheapest ways to learn the most, roughly in order. Each names its question and what settles it.

1. **Does a normal launch survive its second start?** Build the bundle, launch it from Finder with umask 022, quit, and launch again. It passes if the Library comes back (FIT-1, INST-1).
2. **Does what the finishers left actually work?**
   - Commit the tree through `snapiso.sh`.
   - Run J0 in the Finder-like environment, then J1, J9, J10, J11 and J12, and read each REPORT.
   - Open all 25 captures from `capture --scene all`.
3. **Does a real Finder launch find cargo for the project's own dossier?** (A5)
4. **Two projects.** Add toml_pin and a second Rust project, switch between them, then remove one (G8, INS-3).
5. **A real workspace.** Add this repository, or any Cargo workspace with path and git dependencies, build scripts and proc macros, through the dialog (IDX-9).
6. **A non-Rust project.** Add a small npm project, a pyproject project and a go.mod project, then open each one's package and symbol pages. Record what the owner refuses and what the pages say (IDX-3).
7. **Real releases.** Put `runtime/releases.rs` behind `release_data`, index toml 0.5.11 from the cache, and see whether a real diff draws (G13, G14).
8. **A world from the index.** The smallest W-World spike: project the installed library's packages, modules, symbols and relations into `World::new`, and time the layout. The result decides §5.1.
9. **Budgets on a quiet machine,** in a release build: page open, search, flights, frame p95 and cold boot. Time the same install unloaded.
10. **The wide net on the real install:**
    - `popover.py` over every surface;
    - the key sweep in every context;
    - a storm on each scene;
    - the semantic colour audit;
    - icon crops at 1× and 2×.
11. **Syntax colour.** Wire `facet::code` into the code view. It is about a day's work, and it shows whether the highlighter holds its budget on a large file.
12. **VoiceOver.** Open the app with VoiceOver on and see what, if anything, GPUI exposes.
13. **Other platforms.** Compile-check Windows and Linux with `.#cross`. If possible, run one Linux window.
14. **A long session.** Run an hour of scripted navigation and measure memory (ROpen-D8, ROpen-N3).
15. **Scale.** Try:
    - hint mode over 665 rows;
    - Find and Ask over 12,000 declarations;
    - a crate with 90-character paths.

---

## 5. Decisions only the owner can make (GUI)

Some of these repeat `production-readiness.md` §11.

1. **The graph.** Ship it labelled as a fixture, hide it, or schedule W-World. Should the hand keep its roads?
2. **What "Add" means.** Read-only into the library (built), into a project's Cargo.toml with a diff (designed), or both.
3. **Network policy.** May the product download a crate or read the registry index, and how is that shown? This blocks:
   - adding anything not already cached;
   - earlier releases not in the cache;
   - Discover;
   - popularity.
4. **The advisories feed.** The request to clone `rustsec/advisory-db` is still unanswered.
5. **Home.** The list (built), the map (Orbit4), strata (MOMENTS) or cards (COHESION).
6. **Hover.** Library popovers, and one set of timings in place of the documents' competing values:
   - 80 or 120 ms before prefetch;
   - 120, 280 or 350 ms before a peek;
   - the 120 ms dismissal bar, against 220 ms for peeks and menus.
7. **Width.** The 2560 layout, and one reader width system in place of three.
8. **Amber.** Caution, or changed.
9. **Settings.** Which pages ship (Editor, Agents/MCP, Privacy, Registry, Legend); text size as a control; the Inbox.
10. **The command palette.** Build Ask actions, or delete the action catalog.
11. **Smaller calls:**
    - G9, the shell-launch auto-admit;
    - FIT-4, the view switch below 760 px;
    - SIDE-8, the G chord against typing;
    - the provisional keys (G, ⌘., ⌘⇧.).
12. **Superseded designs.** Which are dead for good:
    - the rose;
    - the mosaic;
    - stones;
    - the drawn page;
    - specimens;
    - the lab instruments;
    - Getting one;
    - How it fails;
    - cousins;
    - the locator;
    - the Start here tour;
    - the relay graph;
    - Compare across packages;
    - the Versions lens.
13. **Languages.** Which ships next.
14. **The research contradictions** (RES-10), if any hop or landing design goes ahead.
15. **Platforms beyond macOS,** and the app menus.
16. **Cleanup.** Whether the dead code in §6 may be deleted, now that no lane owns it.

---

## 6. Dead or misleading code to settle

Each item should be either deleted or wired. The rule against removing another lane's public API protected lanes that have now stopped, so every item here is the owner's call.

- **Navigation.**
  - The action layer in `navigation/action.rs`. It is dead, and its shortcuts contradict the key table.
  - `navigation/journey_specs.rs`, which is retired.
  - Intent variants nothing dispatches:
    - `Select` and `SelectDocument`;
    - `ToggleAdvisories`, `ToggleCache` and `SetCacheDays`;
    - `RejectProjectPath`;
    - `CancelIndex` and `Stop`;
    - `TestConnection`;
    - `Action`;
    - the `Refresh*` family.
  - `PackageLane` variants other than Overview.
  - `OrbitRoute::Project`.
  - The name `Overlay::CommandPalette`, for what is a search box.
- **Reader.**
  - The lens-tab plumbing (`set_lens`, `Ctx.lens`).
  - `package_outline_expanded` and `jump_symbol_section`.
  - Dead `SymbolFold` variants.
- **Settings.**
  - The six alias pages.
  - The SETTINGS-DEAD fields.
  - The `_palette` and `_key` stubs.
- **Runtime.**
  - `fixture_world`'s compile-time path.
  - `fixture_releases`, once `releases.rs` lands.
  - The MIGRATE groups (`hand_view`, `release_data`, `pool_load`).
  - The "Temporary" `NUDOX_DEBUG_PAGE` window.
- **Facet items with no product caller.**
  - **To wire rather than delete:**
    - `controls::splitter`;
    - `overlay::lens` and `overlay::toast`;
    - `facet::code`.
  - **Unused or duplicated:**
    - `chrome::{titlebar, shelf}`;
    - `controls::{toggle, select}`;
    - 21 of the 50 UI icons.
  - **Data marks and older page pieces never mounted:**
    - `data::{mosaic, compass, facts::lens_bar, territory, rose, strands, spell, caps, spatial}`, and the comb as a data mark;
    - `marks::{license_mark, version_mark}`;
    - `graph::peek::symbol_peek`;
    - the v1 anatomy elements;
    - the plan-based drawn page.
  - **Gallery tools and dead measures:**
    - `motion::lab`;
    - `Measure::room`, `Measure::columns` and `Room`.
- **Stale docs:**
  - `runtime/workspace_lines.rs:5-9`;
  - `shell/onboard/commands.rs:1-13`;
  - `host/editor.rs:1-6`;
  - the orphan doc comment at `shell/bodies/mod.rs:100-102`;
  - `navigation/route.rs:222`.
- **Leftovers:**
  - `apps/desktop/resources/fonts`, which nothing uses;
  - `.local/devenv/clang-sysroot-adapter`;
  - the "Graph fixture" strings and the tests that pin them (WLD-2).

---

## 7. A suggested order

1. **Settle what exists** (about a day).
   - Commit the finishers' work.
   - Run the journeys and read their reports.
   - Open the captures.
   - Run experiment 1.
2. **Stop the product from misleading anyone.**
   - Hide or label every fixture-fed surface a stranger can reach: the graph, releases and the resume line. Or put `releases.rs` behind `release_data`.
   - Give a ready project Remove, Reveal and Stop.
   - Add syntax colour.
   - Decide the Settings pages and the Inbox.
3. **Finish the two pages.** Work through the R-Folio and R-Sym6 backlogs, then FOLIO-C and SYM6-C (motion and fit), on the real install and with harness captures.
4. **The motion system.**
   - T3 to T5, with the transition catalog as a desktop gate.
   - One hover grammar on every surface.
5. **Languages.** One non-Rust ecosystem, end to end.
6. **The world from the index (W-World)**, and everything waiting on it:
   - the tour;
   - chains;
   - reach;
   - the hand;
   - the graph at a release.
7. **Browsing,** once the network policy is decided:
   - Add as a verb;
   - Discover;
   - an upgrade view scoped to your own code.
8. **Instant budgets** in release, then platforms, menus, accessibility and packaging.

---

## Sources

- **Design.**
  - `docs/architecture/gui-plan.md`
  - `Nudox-Design-System/v5/DIRECTION.md`
  - `Nudox-Design-System/v6/cohesion/COHESION.md`
  - `Nudox-Design-System/v6/moments/MOMENTS.md`
  - `docs/architecture/briefs/{symbol-page,package-browsing,transitions,instant-open}.md`
- **Wave 6.**
  - `.local/lanes/wave6/PLAN.md`
  - `.local/lanes/wave6/lead/briefs/W-*.md`
- **Lanes and reviews.**
  - `.local/lanes/wave6/*/*.md`
  - `.local/lanes/wave6/review/**`
  - `.local/lanes/wave6/journey/GAPS.md`
  - `.local/lanes/wave6/research/{MOMENTS,NAV}.md`
  - the checkpoints from waves 2–5 under `.local/lanes/`
- **The finishers.**
  - `.local/lanes/final/{FINISH,COORD,F-Data-1,F-Data-2,F-Shell-1}.md`
- **Inventories.**
  - `.local/lanes/final/gui-inventory/{plan,code,lanes,final}.md`
