# v6 · Cohesion: one set of objects, seen from three distances

The lead, 2026-09-28. This supersedes the rejected "stones" (see `memory: stones-rejected-shingles-wanted`). Board: `v6/cohesion/Cohesion.html`. Data: `v6/cohesion/data/present.json`, extracted from `v4/graph/world.json` by `extract.py`. `present` is the one package that exists both in the fixture index and in the graph's world, so every item on the board is the same item in every view.

## Corrections from the owner (2026-09-28, second pass). These override anything below.

> "Shingles aren't some kind of central design primitive, it's just for browsing. Once you've wanted to dig into the package more, the shingles should expand into something far more discoverable. The comparison page … should only be for comparing between versions not between packages. More creativity on dependents/dependencies … improvements to the sidebar may be more worthwhile, and figuring out better interactions primitives there."

- **Shingles are a browse overview only.** They appear in Find, the Library and Tree, and in a versions overview. On a package page they are the first glance, and they *open*: a region expands in place into that module's discoverable section (items with kind, shape, one-line doc and members on demand). A shingle becomes its row's mark, so identity carries.
- **Compare is versions of one package**, not packages against each other. For example, toml 0.8.23 → 1.1.6: what changed, and what it means for your code, first.
- **Dependencies and dependents need fresh thinking**: why it's here, how much of it you use, who uses what, and what breaks if it changes.
- **The sidebar is the priority for interaction design.** Research is in `.local/lanes/wave6/research/NAV.md`.

## The owner, 2026-09-28

> "all the UI components need to tie together and feel cohesive, so against the graph and everything, and transitions need to make sense between them … special and magical in the aims of a more useful experience … a lot of this is in subtle interactions and presentation of key views."

## What is wrong today

- **Each view invents its own marks.**
  - The package page draws an outline list.
  - The symbol page draws the v5 spine-and-rails page.
  - The graph draws hulls, squares and diamonds.
  - Find draws cards with Compare buttons.
- **Nothing carries from one view to the next.** A place change is a page swap, not a move through one space. The map that shows a package, the *territory* (the owner's "shingles", `facet::data::territory`), is built and unmounted.
- **The desktop uses 1 of the 15 components in `facet::data`.** The comb, facts, lens bar, territory, strands, caps, compass, spell, progress and mosaic are all built and none is mounted.

## The object model: four things, one mark each, everywhere

| Thing | Its mark | Package page | Symbol page | Graph | Shelf | Query |
|---|---|---|---|---|---|---|
| **Package** | the package gem | the hero | the locator's frame | the world's package hull label | the book header | a result's `package ›` |
| **Module** | a *region*: a rectangle with a quiet mono label | a territory region | the lit region in the locator | a hull around its nodes | a module row | a result's `› module` |
| **Item** | a *shingle*: a 10 px chamfered square; at ≥ 14 px its kind shape (◆ type, ■ callable, ◇ contract, ▪ value) | a shingle in its region | the hero gem, and every related row's mark | a node | a row under its module | a result row's mark |
| **Relation** | a *strand*: a 1.5 px stroke, solid = written, dashed = arrives | strands between shingles on hover | the spine's rails (inputs left, outputs right) | edges | none | none |

One state grammar applies to all four, in every view:

| State | Treatment |
|---|---|
| rest | quiet ink |
| **yours** (your code reaches it) | mint |
| **lit** (query, uses, selection set) | periwinkle fill |
| **current** (you are here) | peri-hi ring and the 2 px left bar in lists |
| **hover** | swell ×1.55 + lift 3 px; neighbours lift 1.5 px; peek after 280 ms (`facet::hover`) |
| **changed in a release** | new = mint, gone = coral hatch, changed signature = amber (caution) |
| **unread** | dashed outline |

## The geometry that makes the views one space

- **Left is where a thing comes from; right is where it goes.**
  - The drawn page already does this: "Getting one" rails enter the spine from the left, "What it does" leaves to the right.
  - The graph's focus view adopts the same sides: makers and takers on the left, what it holds and gives on the right, members below along the spine.
  - Zooming out from a page to its graph is then the rails growing into edges. No node jumps to the other side of the screen.
- **In is down and right; out is up and left** (DIRECTION law 3). A shingle opens *into* the page: its gem sits where the shingle was, then moves up-left to the hero slot as the page unrolls below it. Back reverses it exactly.
- **The territory is the package's floor plan and the symbol page's locator.**
  - Entering a symbol folds the territory into a 150 × 60 locator beside the hero. It is the same regions and the same shingles at 1/5 scale, with your shingle lit.
  - Clicking the locator unfolds it back. You can always see where in the package you stand.

## Key views

### Package page: the folio

1. Hero: gem + name + lede.
2. One facts line (`facet::data::facts`).
3. The release comb (`comb`), only when there are releases.
4. The Start line: three landmarks by fan-in, each a shingle-mark + name + a four-word gloss.
5. **The territory**: the reader column's width; one region per module; one shingle per public item.
6. "Rests on" and "Used by" as dependency rows.
7. Read me.

There are no tabs. "Recorded outline" is deleted: the shelf already lists the modules, and the territory draws them.

**Interactions.**
- **Hover a shingle.**
  - It swells.
  - Its **strands** draw to every related shingle on the map, which light peri. Solid strands are written relations (has, takes, gives, type); dashed ones arrive (calls, uses).
  - Relations that leave the package exit at the map's right edge, labelled with the package they reach ("backend-library ×4").
  - After 280 ms the peek opens with kind, name, signature, one line of doc and "N relations".
- **Hover a region.**
  - Its outline brightens and the shelf's module row lights.
  - After 280 ms the module lens lists its landmarks.
  - Hovering the shelf row does the same to the region.
- **Hover a Start-line name** to lift its shingle on the map.
- **Keyboard.**
  - ←→↑↓ walk shingles in reading order.
  - Tab moves between regions.
  - ↵ opens the shingle, Space peeks.
  - Back restores focus to the shingle you left, which pulses once.
- **Scrub the release comb.** Hovering a tick recolours the shingles by what that release changed: new, gone, changed. The map becomes the changelog, and nothing moves.
- **Query.** Typing in the jump bar lights matching shingles in place, per keystroke. ↑↓ moves *current* among the lit ones and ↵ opens one (the same transition as a click).

### Symbol page: the drawn page (W-Page2) plus the locator

- The locator sits at the hero's right edge: the territory folded to about 150 × 60, with the item's module outlined and its shingle current.
- Every related name on the page (rails, forks, "made by") carries its item mark. Hovering a row lights that item's shingle in the locator, so you see where it lives without leaving.

### Graph: the same page, zoomed out

- The focus view keeps the page's sides: inputs left, outputs right, members down the spine.
- Nodes are item marks at graph size. Hulls are the regions' modules.
- The world view's packages are territories seen from far away. The regions become hulls as you zoom in (W-World).

## Transitions (W-Flip implements; the board films them)

| From → to | Carries | Choreography (CARRY spring, 0.28 s response; text never scales) |
|---|---|---|
| Package → Symbol (click/↵ a row) | row → the page's plate (PLAN §2a); the row's name → the title | Frame 1: the map begins folding toward the locator slot and the shingle detaches, growing along the path into the gem. The name plate travels to the title line and lands at title size in one swap. The page unrolls beneath in reading order, staggered by y. The shelf's module row opens, and its item row takes *current*. Map labels hide at frame 1 and return in the locator only as shingles (law 2). |
| Symbol → Package (Back, or click the locator) | gem → shingle; locator → territory | The exact reverse. The page clips upward behind the rising map, and focus lands on the shingle. |
| Symbol → Graph (G) | gem → focus node; each rail row → its neighbour node | The spine shortens into the node. The rails lengthen into edges on the same sides. Section headings become edge-group labels. The locator unfolds into the module hull. |
| Graph → Symbol (↵ on the focus) | the exact reverse | none |
| Package → Graph (G) | each shingle → its node; each region → its hull | Shingles fly to their node positions, staggered by distance from the pointer, and the regions reshape into hulls. |
| Home → Package | the package entry → hero gem | The shelf arrives from the left as the package's modules. The territory unrolls region by region, largest first. |
| Any → Query plate | nothing moves | The plate opens over the shelf column and results light in place. Esc restores. |

## Order of work

1. **The board** (the lead): the package folio on `present` with real items, shingle hover with strands and peek, and region hover. Then Package → Symbol with the locator, then Symbol → Graph. Film each at 60 fps and review frame by frame against law 2.
2. **Mount the territory in the desktop** (the lead):
   - `bodies/package.rs` becomes the folio: territory regions from `dossier.outline`, shingles open symbols, peeks through `facet::hover`, facts, comb.
   - Delete "Recorded outline".
3. **Territory states** (the lead, `facet::data::territory`): lit / current / changed, strands on hover, keyboard walking, and a locator size.
4. **W-Page2**: the locator and item marks on rails. **W-Flip**: the transition table above. **W-World**: graph sides and hulls = regions.

## The sidebar: primitives (board: `?v=side-contents|side-narrow|side-users|side-versions|side-rests|side-hoist|side-jump`)

Grounded in `.local/lanes/wave6/research/NAV.md` §Q1: Xcode's jump bar, VS Code's breadcrumbs and Explorer sticky scroll, Figma's layer↔canvas hover, Things' narrow-then-widen search, WorkFlowy zoom, Arc's positional favourites, Linear's chords and multi-select.

1. **Scope.** The sidebar shows one scope: the Library, a package, a module or a type.
   - The path lives **only** in the jump bar, and each segment is a menu of its siblings with their rolled-up state (type to filter, ↑↓, ↵). The sidebar shows only the step out ("‹ Library") and the scope's own title.
   - → (or double-click) on a row with children **hoists** into it: the list becomes its contents, all of them, used ones first. ← or the step-out pops. Hoisting is browsing, not navigating: the reader doesn't move.
2. **Lens.** Contents · Versions · Rests on · Used by, one tab strip. The active lens shows its count; the chords are G C / G V / G R / G U. Each lens is a list with the same row grammar:
   - **Contents**: the outline.
   - **Versions**: releases newest first, pin in mint, target in periwinkle. Choosing a target makes the reader show the diff and sets amber state everywhere.
   - **Rests on**: dependencies, indexed or dashed.
   - **Used by**: your crates first, with counts. Choosing one narrows Contents to what it uses ("only what desktop uses", Esc clears).
3. **Narrow.** No field. Typing while the sidebar has focus narrows the current scope in place.
   - It searches **collapsed** rows too, and keeps each match's parents as context. This avoids JetBrains' documented "speed search only searches expanded nodes".
   - A count ("9 of 191") shows how much matched.
   - The last row, "*query* in the whole library ↵", widens the same query to the jump bar's search (Things' narrow-then-widen).
4. **State on rows.** Glyphs sit on the right edge, and modules roll them up:
   - mint count: how often your code uses it;
   - amber: it changes in the target release;
   - coral text: gone or deprecated there;
   - quiet ink: member count when nothing else applies.

   Nothing needs opening to know what matters.
5. **Peek.** Arrow keys move a selection, and a peek follows it beside the sidebar (Finder column view + Quick Look). Space toggles it. The peek shows the signature, who uses it (per crate, with bars) and what the target release does to it. ↵ opens, → shows members, H holds, G shows the graph.
6. **Twins.** Selecting or hovering a row lights its twin in the reader (the same item's row or chip), and hovering in the reader lights the sidebar row (Figma's layer↔canvas hover).
7. **Hold.** Held items sit above the lenses in every scope as chips with ⌘1–⌘9 (Arc favourites).
8. **Trail.** Where you've been, at the foot of the sidebar: the history made visible and clickable.
9. **Sticky ancestors.** In a long list, the chain of parents of the first visible row pins to the top (VS Code Explorer).
10. **Follow.** The sidebar reveals the reader's current item when you navigate; ⇧⌘J reveals on demand.

## Versions, dependents, dependencies (the readers beside the lenses)

- **Versions** (toml 0.8.23 → 1.1.6):
  - A comb across all releases, with the pin and the target marked.
  - **"What changes for you" first**: every API your code touches that changes, with the signature diff, each call site, and a verdict in one sentence ("calls that parse into an owned value compile unchanged").
  - Then the rest, per module: added (mint), changed (amber), removed or deprecated (coral).
- **Used by** (a dependent chosen in the lens): "How desktop uses toml". Its call sites grouped by item, the used name underlined in each line of code, and "the rest of toml (24 items) it never touches".
- **Rests on**: **strata**. Your crates on top, what they rest on as layers beneath. Width = size, unread = dashed. Hovering a block lights the chain from your code down to it, and one line says why it's here ("desktop … use toml → toml reads through toml_edit → toml_edit parses with winnow"). Duplicates are called out, and so is what removing a package would drop.

## Browse → package (film: `?v=film-browse`)

Choosing a browse result opens its shingles into the package page:
- each shingle flies to its item's row mark;
- each region's outline grows into its section's frame;
- the answer's gem and name become the hero.

Everything else unrolls top-down behind them. **Only marks and frames travel**: rows never fly as text in crowds (law 2), and a single name plate may travel in its old face.

## Scopes and lenses, all the way up and down (boards: `side-library`, `side-contents`, `side-item`)

The same four lenses at every scope; the meaning scales:

| Lens | Library | Package | Item |
|---|---|---|---|
| Contents | packages, grouped by how they relate to you | modules → items | members |
| Versions | newer releases across the library, and which change something you use | release diffs | its history |
| Rests on | none | dependencies (strata) | what it is made of |
| Used by | your projects | dependents, by how much they use | callers, by crate |

At Library scope, the reader is the one place shingles live as the primary view: a card per package with its overview. Mint shows what you use; amber shows what a newer release changes. Opening a card plays the Browse → package film.

Research (NAV.md §Q3–Q4) confirms two gaps this fills:
- **No API-diff tool scopes a diff to the consumer's own call sites.** Our "What changes for you" is new.
- **"Why is this here" is always a path, never a boolean**, but tools don't say whether they show one path or all of them. The strata light *every* path and say how many.

## Ruling: the travelling name (2026-09-28)

The shared name of an Open/Close is re-set in the title face at the row's size on frame one, then grows to the title size while riding the opening plate's edge. It is re-shaped at each frame's size, never bitmap-scaled (PLAN §2a; brief rule 3's named exception). Everything else that is text never scales and never flies in crowds: rows and heads unroll in place, and only marks and frames travel. (The board's films swap the face on landing; the product follows this ruling.)
