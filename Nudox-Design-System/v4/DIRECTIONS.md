# FACET v4: the next directions (2026-09-26)

These directions come from the owner's review of the running desktop. Every board still obeys `docs/architecture/gui-plan.md` §6.2 (restraint): one thing speaks, rows at rest are mark + name, one accent (mint = yours/current, periwinkle = focus), space not lines, serif only for a lede or a card's one sentence, keys only while ⌘ is held, and boards show the product rather than notes about it. The owner asked for work that is **creative and playful, and still calm**.

## What the owner said, in brief

1. **The package hero's facts line is what we don't want.** Today it reads "0.1.0 · cargo · MIT OR Apache-2.0 · 1278 declarations".
   - *Liked:* the version slider (the comb, `shots/VersionComb.png`).
   - *Wanted:* the ecosystem is its own icon, and the license is its own component with a GitHub-style hover card. Version UI should be inventive. Depends-on is hyperlinked, with clever hovers.
2. **"The trail you walked" is rejected outright.** The owner wants a cleaner, more useful abstraction.
3. **Motion is "sweet but nonsensical"; the same fade everywhere is the worst of it.** Each animation should fit its context.
4. **The follows/relations idea is good, but the rose/"octopus" on the symbol page is not flexible.** It is also not consistent across languages and needs a more creative presentation. It fits the graph view better.
5. **Symbol pages are less comprehensive than docs.rs.** This needs a lot of work.
6. **Package browsing should be first class**, better than lib.rs, crates.io and npm, and more elegant.
7. **The graph has its own agent now.** Lanes do not edit `apps/facet/src/graph/**` or the graph prototype's `app.js`; boards may *link* to it.

## 1. Identity marks: each fact becomes its own component

Replace the text line with a row of **marks**. Each mark is a component with a state at rest, on hover (a card) and on press. At rest a mark is a glyph plus at most one short word. The facts line becomes a line of objects.

- **Ecosystem mark.** A cut glyph per ecosystem, in the house style (octagons, diamonds, one lit facet), not the registries' logos. Hover: "Rust crate · crates.io", the install line in mono (`cargo add toml`), and a press-to-copy. On press, the line "drops" into the clipboard with a small bounce, and the mark ticks once.
- **License mark.** The glyph's *shape encodes the family*:
  - permissive: open ring;
  - weak copyleft: ring with one closed arc;
  - strong copyleft: closed ring;
  - public domain: dotted ring;
  - unknown or custom: dashed.

  An SPDX `OR` draws two rings you can choose between; `AND` draws them linked. The hover card is GitHub-style, with three columns:
  - *Permissions:* commercial use, modification, distribution, patent use.
  - *Conditions:* notice, state changes.
  - *Limitations:* liability, warranty, trademark.

  The card then goes further than GitHub with **one line on fit with *your* project's license**. For example, "Fits your MIT project", or "GPL-3.0: shipping it would make present GPL". For `OR` it also says which choice fits.
- **Version: the comb *is* the version.** The comb is the owner's favourite, so version UI grows *from* it rather than beside it:
  - The version number sits on the comb's baseline.
  - Releases after your pin trail off to the right, faint.
  - A major bump since your pin is a taller tick.
  - Yanked releases are hollow.
  - Hovering the number gives a semver reading in words: "3 releases behind · one of them breaking · 14 months".
  - Explore beyond that. One option is odometer digits that roll when you scrub. Another is honest semver: a release that broke API without a major bump gets a notch, a fact we compute from `releases.json` `semverSlip`.
- **Depends on: hyperlinked, and each link says *why*.** Each dependency is a link, and its hover peek says **what this package uses it for**, computed from which of the dependency's items it uses. Examples: "toml → serde: Serialize/Deserialize on Value · 38 uses"; "toml → winnow: the parser · 12 uses". The peek also shows:
  - the feature that pulls it in (optional deps render hollow);
  - requirement versus the resolved version;
  - "already in your tree", which means adopting it costs nothing.

  Dev and build dependencies wait under ⌥.
- The **declaration count** leaves the line. It becomes the API-size fact in the package peek, or a gem facet count; the board decides.

## 2. Instead of a trail: *where you are* and *what you hold*

History is ordered by time, but people work by task. Drop the visible trail (beads, metro map) entirely and replace it with two things.

- **The jump bar (where you are).** The titlebar path `present › glyph › RelationLabel` becomes live segments. Each segment opens its siblings, the way Xcode's jump bar does, so moving sideways takes one gesture. Back and forward stay as ⌘[ / ⌘]. A long press on back lists the last ten places as plain rows, as a browser does. Nothing about history shows at rest.
- **In hand (what you hold).** The app keeps only what you touched *with intent*: a pin (⌘-click), a copied signature, an item you compared, a package you added. It sits in a small hand of cards at the window's foot, at most five marks.
  - Hover a card to see its peek.
  - Once two or more are in hand, the hand **connects them**: "from_str → Value → Table", using the chain finder (`recipes.js`). Gathering pieces turns into a recipe.
  - Cards leave when you drop them. They also fade to hollow after a day unused, and never pile up.

## 3. A motion grammar: motion says where things went

The rule: **no transition is a plain opacity fade**. Every motion encodes the relation between the before and the after.

| Relation | Motion |
|---|---|
| deeper (open a row) | container transform: the row's mark flies to become the hero gem, the name grows into the title from where it sat, the list recedes (0.98 scale, dims), and the body unfolds downward from the hero |
| back out | the reverse: the hero shrinks home into its row and the list re-forms around it |
| sibling (next or previous) | a short lateral slide in list order; the shelf's selection slides along with it |
| across (another package) | the gem turns (its facets rotate 60°) as the page swaps: new ground |
| version scrub | nothing fades. Unchanged text holds still; changed tokens roll like an odometer; added rows grow from zero height and push neighbours; removed rows collapse through a strike; counts roll |
| peek | unfurls from the hovered word's baseline; the link's hairline becomes the card's top edge |
| results re-rank | FLIP: rows travel to their new places; new rows rise 4 px in rank order |
| fold or unfold | height with a clip; content never fades |
| add a package | drop, bounce, and the list makes room (the owner asked for playful here) |
| progress | the gem's facets light one by one |
| a fault | the exact operand pulses once, in place |

Durations stay short: 120–240 ms, 320 ms for "across". Use spring curves, not linear. Under reduced motion every row of the table becomes an instant cut.

## 4. Relations as a sentence (replacing the rose/"octopus")

The relations should read as a **sentence** in the lede zone, in one grammar for every language. Example: "RelationLabel **is** an enum of three kinds, **made of** SemanticLinkKind and RelationDirection, **becomes** text through as_str, and **is used** in 4 places in present." Each noun is a link with a peek, and ⌥ expands the sentence into the full prism.

The verbs map from normalised relation kinds, so Rust, TypeScript, Python, Go, Java, C# and C++ read alike:

| Verb | Relation kinds |
|---|---|
| is | kind / implements / extends |
| made of | fields / variants / params |
| takes / gives | in / out |
| becomes | conversions |
| used by | callers |
| lives in | module |

The rose itself belongs to the graph view.

## 5. More than docs.rs

A symbol page must carry **everything docs.rs carries**, organised better, and then add what only we know:

- the full declaration, with generics, bounds and where-clauses (our extractor currently strips bounds);
- rendered docs whose intra-doc links are live;
- `# Errors`, `# Panics` and `# Safety` lifted into their own sections ("fails when…", "panics when…");
- methods grouped by impl block, with the block's bounds in words ("when T: Display");
- trait impls, with auto traits and blanket impls folded to caps glyphs;
- methods from `Deref<Target = X>`;
- for traits: required versus provided methods, implementors (here, in your tree, elsewhere), and dyn-compatibility;
- associated types and consts;
- feature gates, each with "how to enable" and whether your project enables it;
- deprecation;
- "since" from real release data ("added in 0.7, changed in 1.0");
- examples: the doc examples **plus** real calls from your tree and from dependents.

## 6. Package browsing, first class

The jobs are: **find** a package for a need, **judge** it, **compare** alternatives, **understand** one quickly, and **keep up** with the ones you use. The package page and the release lens already cover the last two. The new work covers find, judge and compare, built on what only we know:

- **Your tree.** "Already in your tree", so it is free to adopt. The marginal cost of adding a package *to this project* ("adds 3 crates: …"). License fit with your project.
- **API-level answers.** A result shows the entry point that answers the query (`toml::from_str(text) → T`), not a README blurb. Search by words, by shape (`text -> Value`), or "like serde_json".
- **Measured stability.** Churn comes from real API diffs rather than claimed semver: breaking releases per year, and a notch for dishonest releases.
- **Cousins across ecosystems.** The same idea in npm or PyPI.
- **Compare by capability.** Rows are what you can do (parse from text, write to text, preserve order, borrow input), columns are the candidates, and each cell holds the item that does it. Cost to your tree, license, churn and size are rows too.

Real data for prototypes:

- `~/.cargo/registry/index/index.crates.io-*/.cache/**`: 1,642 crates with every version, dependencies, features, yanked flags and rust-version.
- `v4/graph/registry/` → `~/.cargo/registry/src/…`: 2,028 unpacked crate sources, with Cargo.toml metadata and READMEs.
- `v4/graph/world.json`: the workspace's 37 packages and 56 k items.
- `v4/graph/releases.json`: real API diffs for toml and smallvec.
- The workspace `Cargo.lock`: your tree.
