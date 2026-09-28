# v6 · Stones: every package has a face

The lead, 2026-09-28. Board: `v6/gems/Gems.html?state=cuts|atlas|hover|query|package|symbol|indexing` (served from `Nudox-Design-System/` on :47811). Data: `v6/gems/data/packages.json`, extracted from the harness fixture index by `v6/gems/extract.py`.

## Why

The app draws names. Every surface (home, shelf, package page, Find, Compare, Tree) is a list of names, and the only graphic is the graph. The information has shape — how big, where the mass is, what kind of thing, what rests on what, where a name occurs — and none of it reaches the eye. The owner, 2026-09-28: "GUI still doesn't communicate information as well as it could, or as playfully/prettily as it could. really think big."

## The idea

**A package is a stone, cut by its modules.**

- **The outline is the language.** There are seven cuts, one per ecosystem:

  | Language | Cut |
  |---|---|
  | Rust | octagon |
  | Go | emerald (a chamfered 1.36:1 rectangle) |
  | TypeScript | shield |
  | Python | oval |
  | Java | marquise |
  | C# | hexagon |
  | C/C++ | trillion |

  These silhouettes stay distinct at 12 px.
- **The facets are the modules.**
  - Recursive weighted bisection with straight cuts means every piece is convex, and area is proportional to declarations (floor: 1.8 % of the stone).
  - Cut angles come from a hash of the package name, so a stone never reshuffles and you learn to recognise it.
  - Second-level cuts, one hairline lighter, are submodules. They appear only when the facet is large enough.
- **The stone is one object at every scale:**

  | Size | Where |
  |---|---|
  | 12–14 px | shelf, results, jump bar |
  | 18–26 px | dependency rows, shelf header, locator |
  | 46–64 px | "Used in", the package hero |
  | ~150 px | the atlas |
  | the reader's width | the package page's floor plan, with labelled facets |

  The floor plan is the hero stone, opened. Clicking a facet zooms into that module; it is the same object and the same space (DIRECTION law 3).
- **Light.**
  - Facets are silver (ink1) at 4.5–23 % by how squarely they face the light.
  - A facet turned fully to the light shows **fire**: the kind hue of its module's landmark. Fire is where colour on a stone comes from, and it says what the stone is made of.
  - The light follows the pointer, so gems glint as you move.
  - Reduced motion pins the light at the upper left, the same light the ground uses.
- **Colour means one thing at a time.**
  - Periwinkle facets = *lit by the query* (matches, uses). While anything is lit, fire goes out everywhere.
  - Mint outline = yours.
  - Peri-hi edge on one facet = hovered (bevel = state).
  - Dashed outline = rough: not indexed, or not read yet.

## Where stones replace lists

1. **Home = the atlas (replaces Orbit's chip cloud; the shelf hides at home).**
   - Your projects are in the left column, in mint. Everything they rest on runs right by distance from the ground ("deeper is to the right").
   - Libraries outside your tree sit on a quiet shelf below.
   - At rest, dependency strokes are hairlines. Hovering a stone lights its whole chain, carries the requirement on each stroke, and draws unindexed dependencies as dashed ghosts (the honest empty, and the door to "index these").
   - Arrow keys walk the stones. ↵ opens one, the stone grows into the package hero, and the shelf arrives as its outline.
2. **The query lights the world.**
   - Typing in the jump bar puts results on a plate over the left, and every facet holding a match lights, per keystroke.
   - A result row carries the micro-stone and `package › module`.
   - ↑↓ previews, ↵ keeps, ⌘↵ shows all results as a page, Esc goes back (the preview model in `navigation::reducer`).
3. **The package page = the stone, opened.**
   - The hero (64 px stone, name, lede, one facts line) sits above the floor plan (784 × 250) with labelled facets: name, count, and the landmark with its kind mark.
   - Below that, two columns: *Start here* (the landmarks, one 26 px row each, docs on hover) and *Rests on / Rests on it* (stone rows, requirement in ink4, dashed = not indexed).
   - It all fits in the 1440 × 900 fold. "Recorded outline" is deleted: it repeated the shelf.
4. **The shelf gets weight.** Module rows carry a **tape**, a 3 px bar at the right edge whose length is proportional to declarations. Submodules are nested (not flattened), and imports are gone.
5. **The symbol page gets two pieces** (W-Page2 owns the page; these are drop-in components):
   - **Locator**: a 22 px package stone with this declaration's facet lit, placed beside the path. It is also the door back to the floor plan.
   - **Used in**: uses are a query, so they draw like one. Each using package appears as a 46 px stone with the using facets lit and the count under it. Hover a lit facet to see that module's call sites.
6. **Indexing is cutting the stone.**
   - An added project is a rough stone (a dashed outline).
   - As the index reads, the final cut tree is revealed biggest-split first. Each cut is one straight stroke drawn across in 120 ms, and the unread part stays rough.
   - This replaces every spinner and percentage on project rows.
7. **Also, next:**
   - Compare = three stones side by side, with the shared names lit in each.
   - Tree = the atlas focused on one project.
   - The graph's module hulls become facets (W-World: the world's outermost LOD *is* the atlas).

## Laws

- Geometry is a pure function of `(outline, aspect, weights, seed)`, cached by content hash. Painting is N convex polygons batched by colour, with no per-frame geometry.
- Areas are honest: each facet's share of the stone is within 1 % of its weight share, after the floor.
- A facet label is drawn only if its box fits inside the facet. Otherwise it is omitted, never clipped or overlapped (law 2, legible at every frame).
- No gradients, no glow, no blur: flat facets, straight edges (v3 "cut, not painted").
- At 12–14 px, only the outline and the first two cut levels are drawn, and the stroke is 0.6–1 px.

## Build order

- **S1** (the lead): `facet::paint::stone`.
  - Geometry (`Outline`, `cut`, `Facet`) and the element: sizes, light, fire, lit, hot, yours, rough, and depth-limited reveal.
  - Tests: areas honest, convex, deterministic, stable under small weight changes.
  - A gallery scene.
- **S2** (the lead): the atlas as home (`bodies/orbit.rs` → `bodies/atlas.rs`); query lighting through `shell::ask`.
- **S3** (W-Page2): the floor plan + Start here + stone dependency rows on the package page; the locator + Used in on the symbol page.
- **S4** (W-Tissue/Sonnet): shelf tapes, nesting, no imports; micro-stones on shelf/results/jump bar.
- **S5** (W-Flip): stone → hero → floor plan; facet → module zoom; the cut-reveal timing.
- **S6** (W-World): real per-module counts and landmarks (by fan-in, not name frequency) from the index; the atlas and the graph share one layout.
