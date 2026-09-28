# Nudox v5: instant, legible, graphical

The lead's direction, 2026-09-28. It is binding on every GUI lane and replaces nothing in `gui-plan.md` §6.2 (restraint) or `v4/motion/MOTION.md` (motion grammar). It raises the bar on both.

## 0. Where we are (captured at HEAD 959f8911f, `wave5/walk/`)

The owner's verdict: "a poor start… decent work, but could be infinitely more creative, feels a little cluttered… animations incredibly weak, often overlaying the past… not really fully async… hover effects inconsistent, visual hierarchy weak, poor use of colors, stroke, shapes… typography and layout feels weird, poor separation."

What the captures show:

- **Filler prose where a picture should be.**
  - "Value has 7 variants. Explore ways to make one, operations, connections, and author notes."
  - "Choose a row to inspect the declarations recorded beneath it."
  - "Show more entries".

  Sentences that describe the page instead of being it.
- **The rejected facts pattern is back.** The package hero is a table of italic labels: VERSION / ECOSYSTEM / OUTLINE READ. The marks were built but never wired in.
- **Hierarchy by accident.**
  - Every section is a bold 17 px heading with rows under it, so all sections look the same.
  - Tabs, headings and inline labels ("one of", "you write", "done by") compete at similar weights.
  - The serif is used for the lede, the variant docs and the captions at once.
- **The shelf flattens nested modules.** It shows `map, array, map, mod, array, map, mod` on toml and `mod, fmt, impls, mod, fmt, impls` on serde_core. Packages are named by directory (`toml-0.8.23`, `serde_core-1.0.229`), not by name.
- **Slow everywhere.** A harness capture costs 110–180 s of CPU, and it was about 10 s a day ago. The graph loads visibly. Nothing is instant.

## 1. Five laws

1. **Instant.** Nothing local ever waits. Every frame paints real content from memory; the index only revalidates. No spinners, no skeletons, no "loading…" for anything already on this machine.
2. **Legible at every frame.** At any frame of any motion, each text is either fully legible and unoverlapped, or not drawn at all. Old content never lingers under new content. Automated checks enforce this, not taste.
3. **One space.** World → package → module → symbol → source form one continuous space with a fixed spatial map. Every transition travels in that space, and the graph is simply that space zoomed out.
4. **Show, don't narrate.** Where a sentence describes structure, draw the structure. Prose is for meaning only: the lede, a doc, one sentence per section at most.
5. **One hand.** Every hoverable thing hovers the same way, every relation is stroked the same way, and every kind has one shape and one hue. Consistency is the creativity budget. Spend novelty on big moments, never on inconsistency.

## 2. Instant (lane W-Instant)

### Budgets
These are enforced as harness perf tests, measured quiet in a release build.
- Cold boot to a meaningful frame: under 150 ms, painted from a snapshot.
- Route change to the new page's first real frame: 16 ms or less, from cache.
- Graph open (world or focus): 100 ms or less to a laid-out frame. Refinement may follow.
- Keystroke to repaint in Find, Ask or Filter: 8 ms or less at p99.
- Hover to peek: 16 ms or less after the hover delay.
- No main-thread task over 4 ms. A debug-build watchdog logs the backtrace, and the harness fails the run.

### Architecture (be adventurous)
- **Snapshot-first boot.** On quit and at idle, persist the visible state and the read models behind it: page models, shelf outlines, hand, world layout. Use a zero-copy archive, such as rkyv or a hand-rolled format, memory-mapped. At launch, map it, paint the last frame, then revalidate in the background. Whatever changed morphs in; only the difference moves.
- **The world as columns, not JSON.**
  - Today a 16 MB `world.json` is parsed per process.
  - Replace it with a content-hashed binary built beside the index:
    - node columns (SoA: x, y, kind, package, name offset);
    - CSR adjacency, with edges sorted by kind;
    - an interned string arena;
    - a spatial grid for picking and culling;
    - precomputed label placement per zoom band (LOD).
  - Load it with mmap, zero parse. Layout is computed once per content hash and cached on disk.
- **Pages as plans.** A page's read model compiles once into a flat, render-ready plan: rows, spans and glyph runs with measured widths, cached by content hash. Drawing a page never walks DTOs.
- **Prefetch by intent.**
  - A pointer resting on a link for 80 ms, or moving toward it fast, prefetches the target's plan.
  - Keyboard focus prefetches.
  - Back/forward neighbours, hand cards and the Start-here stops are kept warm.
- **Typed guarantees.** The UI can only consume `Now<T>`: a value that is available this frame, carries its revision and may be stale. The store's contract is that a `Now<T>` always exists: the snapshot, the last known value, or an honest empty. Anything async lives behind that type and can never block a frame.
- **Iteration speed is a feature.** The harness rebuilds the fixture index on every capture, at 110–180 s of CPU. Cache it by fixture content hash so captures return to seconds. This comes first, because every lane's loop depends on it.

## 3. Motion (lane W-Motion)

### The spatial map (fixed and never contradicted)
- Deeper (a child, a member, opening) is **in and to the right**.
- Up (the parent, back) is **out and to the left**.
- Related (across a relation) is **along the relation's stroke**.
- Time (versions) runs **along the comb, left = older**.
- The graph is **out**: the page shrinks into its node.

### Signature transitions
- **The row becomes the page.** The clicked name travels and grows into the new title as a shared element, on the first frame. The old page clips away behind the title's path in the direction of travel, and must be gone from every region the new content enters. The new sections unroll beneath the title in reading order, staggered by y. Back reverses it exactly: the title shrinks into its row on the parent page.
- **Semantic zoom, page ↔ graph.**
  - Zooming out (⌘−, a pinch, G): the page's sections fold into the node's edges. "Getting one" folds into the incoming strokes, and "What it does" and "Who uses it" into the outgoing ones. The hero gem becomes the node, and the camera pulls back into the neighbourhood.
  - Zooming in reverses it: the edges unfold into sections.
  - This replaces the door-flight-as-overlay with one continuous object.
  - Build on W-Shell's §15 S3 plan (`enter_from` with origins).
- **Holding, peeking, lensing** unfurl from their origin mark (the MOTION verbs Take, Unfurl, Wipe) and return to it.

### Rules
- **Input responds in the same frame.** Motion never delays interaction, and every motion is interruptible and retargetable.
- **Durations are 120–240 ms, and nothing crossfades.** The fades that remain are defects; search for `opacity(` driven by `t`.
- **Reduced motion** is a cut plus a 1.2 s mark on what changed (MOTION #16).

### Enforcement: the transition catalog
Enumerate every transition in the app: each route pair, overlay, menu, card, hover, hand act, lens, and graph enter/exit. Film each one in the harness at 16 ms, and fail the run if two text boxes above the contrast threshold intersect in any frame, or if a text is drawn mid-transition below legibility for more than 2 frames. The catalog is a test.

### One hover grammar (a facet primitive, used everywhere)
- **At 0 ms:** the target's ink rises one step, and its hit shape draws its bevel stroke (1 px, the kind's hue at low alpha).
- **At 0 ms:** everything *related* on screen lights: every other occurrence of the same symbol, and the relation stroke to it. Nothing else changes.
- **At 350 ms:** a peek unfurls from the target (Unfurl), and leaves the way it came.
- Rows, links, marks, chips, graph nodes and comb teeth all use this same primitive. Ad hoc hover styles are deleted.

## 4. Pages (lane W-Page)

### The specimen
Each declaration is presented as a **specimen plate**: its shape drawn large and exact in the centre of the page, annotated like a technical drawing. That diagram *is* the reference section, not a picture beside a list.

- **A choice** (Rust enum, TS union, Java sealed, Python `Literal`/`Enum`) is a **fork**. The tines are the cases. Each payload is drawn as the small shape glyph of its type, coloured by kind, and hovering a payload traces it.
- **A record** (struct, class fields, dataclass, TS interface fields, Go struct) is a **bracket**. The fields are stacked rungs, each showing a type glyph plus its name in words, with optional fields hollow.
- **A callable** (fn, method, Python def, TS function, Go func) is a **pipe**. Inputs enter at the left as typed ports, the output leaves at the right, and each failure mode branches down: `Result`, `(T, error)`, `throws`, raises-from-the-doc. Generic bounds are written in words on the port.
- **A contract** (trait, interface, Protocol, abstract class) is an **outline** with notches: required members as open notches, provided members as filled ones. The satisfiers sit beside it as a count and a fan: "486 types do it · 12 in your code". For Go, which satisfies interfaces implicitly, this fan is *computed*, and it's the headline.
- **A value** (const, static, enum constant) is a **stone** with its value printed.
- **A module or namespace** is a **tray** of kind marks: a territory, not a list.

Language idioms appear as badges on the specimen, never as prose: decorators, annotations, `async`, `unsafe`, `pub(crate)`, `@deprecated`, feature gates, `virtual`/`override`, `readonly`. ⌥ spells the exact source beside the drawing.

### Reading order
One column, graphical, top to bottom. There are no tabs; the section heading *is* the navigation, and the jump bar's `›` lists sections.
1. The hero: gem, name, one-sentence lede, and marks (facts as components).
2. The specimen.
3. Getting one: rails from what you have.
4. What it does: capabilities as a grid of glyphs grouped by contract, not a list.
5. What can go wrong: the failure tree.
6. Who uses it: yours first, as a strip of real call sites with the code excerpt on hover.
7. What changed: a strip along the comb.
8. Its own words: the docs, last, typeset well.

### The page is the node, opened
- The incoming relations ("made from", "getting one") sit on the left edge and the outgoing ones ("does", "used by") on the right edge.
- At wide widths they're drawn as the stubs of real strokes leaving the page's margins. That's the same geometry the semantic zoom folds into.

### Every ecosystem is first class
- The harness gets pinned fixtures and scenes for Python, TypeScript, Go, Java, C# and C++, each with a choice, a record, a callable and a contract.
- The specimen grammar is proved on all seven languages. A page that only shines on Rust fails review.

### The package page
- The hero carries the marks: ecosystem, license, version comb and dependencies (W-Marks' components, already in `facet::marks`).
- "Start here" is the tour strip.
- The territory is a tray of the package's modules, sized by the number of public items, with your uses lit. It is not a second copy of the shelf.
- No instructional prose.

## 5. Visual language (all lanes; W-Page owns the tokens it needs)

### Type scale (one scale, no exceptions)
- Display: 40/44, 700, tight.
- Lede: serif italic 19/28.
- Section heading: 13/16, 600, ink2, with 0.02 em tracking and a kind-hued section mark in the gutter.
- Body: 14/22.
- Mono: 13/20.
- Label: 12/16, ink3.

The serif is for the lede and at most one sentence per section. Nowhere else.

### Rhythm
- The grid is 8 px.
- Section gap: 48 px. Group gap: 16 px. Row: 32 px.
- Separation comes from space plus a hairline rule that starts at the gutter mark. No boxes, no filled cards at rest.

### Colour roles
- Ink carries text, in steps 1–4.
- **Kind hues** carry structure: marks, strokes, ports and section marks. Types are teal `#46d2dc` (they moved off mint, which reads as "yours"). Callables are periwinkle, contracts violet, modules slate. **Values have no hue**: their glyphs and stones are neutral ink, and the printed value is the information.
- **Amber means caution, only.** Examples: "wraps the text; does not parse it", or a trap on a rail.
- **Coral means failure**, only: failure drops, "can panic", "fails with".
- **Mint** means yours.
- **Periwinkle** used as a stroke means focus. When a callable is focused, the focus is the bevel, not the hue.
- Colour is never decoration. Every hue answers "what kind" or "whose".

### Stroke
- Structure: a 1 px hairline.
- Relation: 1.5 px in the kind hue.
- Focus: 2 px.
- Dashed means inferred or matched by path. Dotted means optional or maybe. Solid means compiler-verified.

This is how "every count names its tier" becomes visible without words.

### Shape
The kind glyph is the only icon a thing has.
- Chamfered plates are things you can open.
- Round stones are values.
- Diamonds are contracts.

The specimen diagrams reuse the same shapes at scale.

### Hover
See §3. One grammar.

## 6. Lanes

- **W-Instant (Opus):** §2. It owns runtime/store, snapshot, world format and loading, page plans, prefetch, the budgets and the watchdog. For graph files it coordinates with W-Motion, which owns graph enter and exit.
- **W-Motion (Opus):** §3. It owns the transition catalog and its check, every transition, the hover primitive, semantic zoom (taking over W-Shell S3/S5) and the fade purge.
- **W-Page (Opus):** §4 and §5. It owns the specimen, the reading order, the package page, the language fixtures and the type, rhythm, colour, stroke and shape tokens. Its design comes first: stills across three languages at 1440/760 for the lead's review, then the build.
- **Sonnet lanes:**
  - W-Shell's §15 seams S9, S8, S7a/b, S1, S2 and S7c, which are desktop-only and fully specified.
  - The marks wired into the package hero.
  - Contrast calibration (W-Ink).
  - The Ask leak (W-Leak).
  - Shelf nesting and package naming.
