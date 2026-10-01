# D-Browse: concept checkpoint (before drawing)

> Copied verbatim from the lead's scratch folder
> (`$S/wave4/browse/CONCEPT.md`) into `docs/architecture/briefs/package-browsing/` by the
> W-Browse finisher, 2026-09-27. No `$S/...` paths appear in this document's own body.

## What the four registries get right and wrong

All four were read on 2026-09-26: lib.rs pages, the crates.io API (`/api/v1/crates`), the npm registry search API, and PyPI JSON. The npm and PyPI web UIs block fetches, so their page structure comes from memory and the API fields.

### lib.rs
**Right**
- Ranks by a quality blend, not raw downloads.
- Shows the *weight* of a crate: "~12–48MB ~836K SLoC" on reqwest. No other registry shows cost.
- "Used in 46,102 crates (30,561 directly)" separates direct from transitive use.
- A category rank ("#2 in HTTP client").
- Related and See-also lists: the only registry that names alternatives.
- It flags deprecated and unmaintained crates.
- Pages are server-rendered and fast.

**Wrong**
- Cost is absolute. reqwest's 12–48 MB ignores that you already have hyper and tokio, so the number is wrong for *you*.
- Each result is a README blurb plus version, downloads and hashtags ("serde_json - A JSON serialization file format - v1.0.151 115.0M #json …"). You can't see what the crate lets you *call*.
- Alternatives come from co-occurrence ("reqwest-middleware, diqwest, axum…" sits under reqwest). There is no basis for comparing them.
- Stability is a release count ("126 releases"), not an API diff.
- Pages are walls of text, and one crate can't be judged against another.
- Other ecosystems don't appear.

### crates.io
**Right**
- The exact match comes first (`exact_match`).
- A copyable `cargo add` line plus the `Cargo.toml` line.
- Per-version rust_version, edition, crate_size, license and features, plus the yanked flag and a versions tab.
- A security tab (RustSec) and trusted-publishing provenance.

**Wrong**
- Result rows lead with two large download numbers (All-Time, Recent). Popularity stands in for fitness.
- Search matches only name, description and keywords.
- Dependencies are a flat list. It doesn't say what each one is for, which feature pulls it in, or the requirement versus the resolved version.
- "Dependents" is a count and a list with no reason attached.
- The README *is* the page.
- Nothing knows about your project, and there is no compare.

### npm
**Right**
- A copyable install line; the TS-types badge sits in the header (a capability shown as a mark).
- Unpacked size and file count; provenance; dist-tags (latest / next); a Code tab for browsing files.

**Wrong**
- Keywords game the ranking. "toml" returns ESLint parsers and dprint wasm plugins in the top 5.
- The p/q/m score is opaque and was retired: the search API still returns `quality 1, popularity 1, maintenance 1` for every result.
- Cost lives on other sites (bundlephobia, packagephobia), and so does compare (npmtrends, which is downloads only).
- It doesn't tell you when the platform already does the job (`JSON.parse`).

### PyPI
**Right**
- Classifier facets (Development Status, License, Python version, Topic) give filtering real structure.
- The release history is a timeline with pre-release and yanked marked.
- `requires_python` is shown up front; project URLs are marked verified or unverified.

**Wrong**
- Search is weak: summary tokens only, with sorts limited to relevance, date and trending.
- The page shows no dependencies (`requires_dist` is hidden), no API, and no compare.
- Most importantly, it can't tell you that **you need no package**: `tomllib` is in the standard library, and PyPI will still sell you `toml`.

### Elsewhere
- Hoogle and docs.rs's in-crate `str -> T` search prove that shape search works, but only inside one crate.
- deps.dev has graphs and scorecards, but nothing about *your* tree.
- Nobody compares by what the code can *do*.

## Three concepts for the whole experience

### A. The query is the page (answer-first)
- Browsing is not a place you go; it's what the reader shows when the titlebar ask field holds a *need*. The field flies down into the reader and becomes the hero: big mono query, caret.
- The answers below are **items, one per package** (`toml::from_str(text) → your type`), each with one reason.
- A judge card unfurls from the selected answer.
- Compare is a capability ledger in the reader.
- Your tree is the shelf's Library, grouped by role.
- Add drops the gem into its role in the shelf, and the list makes room.

**For:**
- It is fully integrated: there are no new chrome regions.
- One thing speaks on every view (the query, the verdict, the project).
- It degrades cleanly to 480.

**Against:** the finding is only as good as the search. Our index makes that the strength, but ranking is real work.

### B. Start from what you hold (Library-first)
- The home is your tree, grouped by role ("speaks formats", "draws the window"). Every incumbent has an "instead of…" door.
- Find is scoped to a role, and compare is always *against the incumbent*: each cell reads "same / more / missing" relative to your code's actual calls.

**For:** radically grounded, and nobody else can do it.

**Against:**
- Finding something *new* (a need with no incumbent) is second-class.
- It turns into a dependency dashboard, which the owner already rejected.

### C. The constellation (graph-first)
- A query lights every answering item on the world map: territories are packages, and your tree is mint.
- You judge by hovering a territory and compare by pulling two territories side by side into a prism.

**For:** spectacular, and it shares the graph lane's engine.

**Against:**
- Maps are bad at ranking and at reading signatures.
- A compare laid out as geometry isn't a comparison.
- It's unusable at 480.
- The graph is owned by another lane (we may only link to it).

## Pick: A, with B's anchor and C as a door

I pick **A (the query is the page)**, and steal the best of the other two.

- **From B, the incumbent anchor.**
  - When you compare against something you already use, the ledger's first rows are *what your code does today* (from the real `uses`: `toml::Value` 29 places, `as_str` 20, `as_table` 11, `from_str` 4…). Each cell is the candidate's equivalent item, or honest empty space.
  - "Would it cover what I do now" is the question no registry can answer. This is the "beats lib.rs" moment.
- **From C, a door.** "N lit in the graph" at the foot of results opens `Graph.html?find=` (a link only; the graph lane owns the rendering).

### Why A
- The owner asked for browsing that is *integrated*, not a dashboard, and for one thing to speak.
- A is the only concept whose every view has a single hero:
  - Find: the query.
  - Judge: the answering signature.
  - Compare: the verdict sentence.
  - Tree: the project and its one number, which ticks on add.
- It reuses the frame (titlebar ask → reader hero, shelf Library, status seam) and the house components (peek, comb, gem, row, `drop-in`/`make-room` keyframes).
- Our three unfair advantages land in the *row's one reason* rather than in badges:
  - "in your tree";
  - "adds 2 crates: toml_writer, serde_spanned";
  - "in the standard library" for a PyPI cousin.

### Views and states to draw
- **Find**
  - Empty: three teaching rows, a need / a shape / a package you know.
  - Words (`parse toml`), shape (`text -> Value`), and like (`like serde_json`).
  - Each row: eco mark + name + answering item + ≤ 1 reason.
  - Cousins from npm/PyPI/Go/… follow under "the same idea elsewhere".
  - Re-ranking uses FLIP.
- **Judge**: the card for the selected row, with these parts:
  - why care (tree / cost / fit);
  - the comb *as* the version;
  - one serif sentence;
  - the answering signature.
  - ⌥ adds churn, API size, MSRV and local dependents.
- **Compare**: 2–4 columns by capability.
  - The verdict lede, then rows in three bands: *what your code does today* (only with an incumbent), *what else it can do*, and *what it costs you* (tree, license fit, churn, API size, MSRV).
  - Hovering a row folds its signatures open; hovering a column lifts it; × removes a column (FLIP); ⌥ spells each cell as the call.
- **Tree**
  - The Library grouped by the role each dependency plays *in this project*. Roles are derived from the dependency's categories × which of your packages use it, with the derivation recorded in the data.
  - Direct deps are rows; transitive deps are a quiet "and N that come with them" per role.
- **Adopt**
  - Add lifts the gem off the card, arcs it to its role's row in the shelf, and drops it in (squash and bounce) while the list makes room.
  - The tree count rolls like an odometer (1,189 → 1,192).
  - A slim, four-stage indexing seam (fetch · unpack · read · seal) runs under the new row, and the gem's facets light one by one.
