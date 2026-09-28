# D-Page concept: relations that read, and a page that answers questions

The prototype is `Nudox-Design-System/v4/page2/Page2.html?page=value|from_str|serialize|smallvec|error`. Its data comes from `page2/extract2.mjs`, which reads the world, the registry sources, releases.json, rust-src and Cargo.toml. Working stills are in `wave4/page2/work/`.

## 1. Why the octopus fails on the page

The prism-as-flow is a picture of a graph neighbourhood. On a reading column it has four problems:

- **Its size is set by geometry, not content.** One relation still costs a 300 px fan. With 500 relations it shows "and 477 more" and nothing else.
- **Its words depend on the kind of item.** A type gets "taken by / held by / calls it / used by", while a function gets "called from / calls".
- **You read it sideways.** The eye has to follow a bezier to find a group's name.
- **It sits beside the prose, not in it.** Nothing else on the page reads left-to-right-from-a-gem.

The fan is the right form in the graph, where position means something. On the page, position means reading order.

## 2. Three presentations on real data

Each one was drawn on `serde::Serialize`, which is done by 486 types (17 of them yours) across 13 packages, and on `toml::Value`.

| | Presentation | Still | Verdict |
|---|---|---|---|
| A | **Verb rows.** A right-aligned verb, then the top names inline (mark + name, yours first, as many as fit on one line), then "and N more". "and N more" unfolds in place, grouped by band (yours / in this crate / elsewhere) and then by package, one line per package. | `work/r-rest.png`, `work/r-open.png`, `work/r-480.png` | **Picked.** It reads as a sentence. It is one line per verb at rest, whether there is 1 relation or 500. It degrades to stacked rows at 480. It is identical for every language. |
| B | **Scales.** "Where it comes from" is right-aligned on the left, "where it goes" is on the right, with a spine and a small gem between them: the prism without its curves. | `work/who-b.png` | Rejected. Most items are one-sided: types have few *in* relations, and functions are called more than they call. That leaves half the column empty. It also splits reading direction, and below 620 px it collapses into A anyway. |
| C | **Combs.** Each verb gets a tally: one tick per relation, grouped by package, yours in mint. Only one name is shown; the rest appear as you scrub. | `work/who-c.png` | Rejected at rest, because names hide behind a gesture. **Kept as the row's trailing detail** on hover and under ⌥ (`work/r-hov.png`, right edge). At 486 the ticks become a band per package. |

**The pick in full.** A is the row. C's comb is the row's hover and ⌥ zone. B survives as ordering only: *in* rows first, then a gap, then *out* rows.

## 3. The page is ordered by the reader's questions (adopted)

A reader arrives with a question, so the page is laid out as answers, in the order people ask:

| Question | Section | What answers it |
|---|---|---|
| What is it? | the hero plus the top block (no heading) | the sentence, the declaration strip with bounds in words, the anatomy (fork / holds / pipe / contract), and the author's prose. For a function, "calls" sits at the anatomy's foot. |
| How do I get one? | **Getting one** / **Doing it** (trait) | the rails; a route through a method behind a feature you don't enable is dimmed with "not in your build" (see below); then the **comes from** row, which holds all makers and replaces the old foot count |
| What can I do with it? | **What it does** | the "can" line (one arrival axis), **acts like** (Deref/base/embedding), **becomes** (conversions away), and Does grouped by receiver, with impl-block conditions as their own groups |
| What can go wrong? | **When it fails** | one component: *fails when* / *fails with* / *panics when* / *you promise* (Safety). An error type also gets its kinds, which are told apart by the method that classifies them. |
| Who uses it, and how? | **Who uses it** | the verb rows (done by · called by · taken by · held by · called on by · asked for by · used by), then real code: doc examples first, then your tree |
| What changed? | **What changed** | since, from real releases; changes per release read; the release comb with your pin in mint. Where release data is missing, the section says so. |

**The lede sentence is one clause per question, and each clause scrolls to its answer.** For toml::Value it reads: *"Value **is** one of 7 TOML kinds · **comes from** text, any Serialize and 36 more ways · **panics** in 2 places · **used in** 59 places, 7 in yours · **since** ≤ 0.5.11"*. A clause is omitted when its section is empty; nothing is drawn empty. The sentence replaces the facts line, and the quiet marks line under it (path · alias · since · gate · deprecated) holds identity. Each mark is a glyph plus at most one word, with a card on hover.

**The relation verbs are distributed by question, not collected in one place.** *comes from* answers "how do I get one", *becomes* and *acts like* answer "what can I do", *fails with* answers "what can go wrong", and the rest answer "who uses it".

This is also what makes the page read the same in seven languages. The questions are language-neutral, and only the rows under them vary. For example, Go never shows "Doing it: derive", and Java never shows "the usual 13".

## 4. The door into the graph

The "Who uses it" heading carries a small drawing of the item's actual fan, 88×30:

- one strand per relation group;
- *in* strands on the left and *out* strands on the right;
- strand length grows with log(count);
- a strand is mint if the group holds any of your code.

At rest the drawing is ink4. On hover the strands spread, the gem fills periwinkle, and "see it in the graph" slides in. The key cap "G" shows only while ⌘ is held.

Pressing it runs the flight (`work/r-fly.png` mid-flight, `work/r-fly1.png` at the end):

1. Every visible name leaves its row on an arc.
2. The door's gem grows into the focus gem at the column's centre.
3. The names settle into the prism: groups on each side under italic-serif heads, which is the graph's own voice.
4. Strands draw out from the gem. The page recedes to 18 %.
5. The graph opens on that prism (`../Graph.html?focus=…`).

The motion is 480 ms, spring-eased, with staggered starts. It is not a fade: the rows *become* the prism. Under reduced motion it is an instant cut.

## 5. Hover gives a peek at the joint

Resting on a name raises **the joint**: the exact place the relation happens, with the item underlined in periwinkle (`work/r-hov.png`). Examples:

- held by: `ManifestPackage.version: Option<toml::Value>`;
- called on by: the calling line;
- done by: `#[derive(…, Serialize, …)]` or `impl Serialize for X` with its line;
- taken by: the signature.

The card also carries one serif sentence and "yours" or the package.

Resting on a **verb** shows the verb's definition and how it is spelled in all seven languages (`work/r-vb.png`). The normalised table is visible in the product, not only in a document.

## 6. docs.rs parity plan: everything it carries, one fold down

The top of `PARITY.md` is folded in. Each row below is at-rest / hover / ⌥ / fold.

| docs.rs | Here | At rest | Deeper |
|---|---|---|---|
| declaration, generics, bounds, where | declaration strip (exact code) | head plus bounds; enum bodies fold to `{ … }` | hover a bound: it in words ("T can be built by a deserializer, borrowing for 'a"); **source** flips to the file |
| docs + intra-doc links | the author's prose under the anatomy | full, with live links to world items (`gtl`) and external links marked ↗ | long code blocks fold after 14 lines |
| `# Errors` `# Panics` `# Safety` | **When it fails**: one component for all three, and for `@throws` / `Raises:` / `<exception>` | a verb column (fails when / panics when / you promise), one serif sentence each | member-level sections collected (SmallVec: 8 panics, 3 safety) |
| methods by impl block | Does, grouped by receiver; a conditional impl block becomes its own group titled with its bounds in words ("when its items copy freely") | rows are mark + name + plain-words signature + one sentence | ⌥ shows the exact `impl<…>` header; hover shows since · file:line · gate |
| trait impls | the **can** line on one arrival axis: hollow = derived, solid = written, dashed = through another trait (blanket), dotted = from its parts (auto) | only what is special | "and the usual 13" folds autos + universal blankets; exceptions stand alone ("not unwind-safe" on serde_json::Error); impls behind features you don't enable fold to "5 behind features you don't turn on" |
| auto traits | dotted caps | folded unless an exception | card: each auto trait in words, plus why ("its buffer holds raw pointers when it spills") |
| blanket impls | dashed caps | ToString, ToOwned and DeserializeOwned shown with their reason ("through Display") | the universal 7 are in the fold |
| methods from Deref | **acts like** — one line: "acts like a slice &[its items], through Deref and DerefMut: 125 more methods, like first, split_first…" | one line | unfolds to all methods from rust-src with their Rust `since`; the ones SmallVec shadows are struck through |
| trait: required / provided, implementors | **Doing it** (derive / write the one method / "not yours? check for a serde feature" from `on_unimplemented`), contract anatomy, "not usable as dyn Serialize, because serialize is generic over S" | | implementors are the **done by** row: yours / in serde_core for std types (numbers 31, text 9, collections 11 …) / elsewhere by package |
| associated items | on the trait card (`type Err = crate::de::Error`) and the contract | | |
| feature gates | a key mark plus the feature, shown **only when it is off for you**. When it is on, it just works and the mark waits for hover or ⌥. | dimmed row + key mark | card: "Off in your build" plus the Cargo.toml line to copy; or "On — through serde_json’s std" (feature unification, computed). Engine state drawn: "behind feature X, not indexed in this build". |
| deprecation | struck name + strike mark (e.g. ExtendFromSlice, a deprecated trait SmallVec implements) | | card: since + note from `#[deprecated(since, note)]` |
| since / changed-in | "since" clause + mark, per-row since on hover, **What changed** section with the release comb | | unknown shown as unknown (serde_json and serde_core have no history read) |
| examples | doc example first, then real calls (In use), under **Who uses it** | first 14 lines | ⌥ reveals `# ` hidden lines |
| source links | every section has **source** (hover or ⌥), and the section flips in place to its exact lines | | |

Next: complete pages for all five items, the flip, the gate and deprecation states, and stills at 1440 / 760 / 480 / 200 %. After that, VERBS.md and CHECKPOINT.md.
