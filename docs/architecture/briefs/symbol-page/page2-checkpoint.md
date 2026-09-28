# D-Page checkpoint: the symbol page, answered by question

- **Prototype.** `Nudox-Design-System/v4/page2/Page2.html?page=value|from_str|serialize|smallvec|error`, served at `http://127.0.0.1:47811/v4/page2/Page2.html`.
- **URL state.** `x=1` (⌥ held), `t=2` (200 % text), `tall=1` (whole page), `rel=a|b|c`, `open=<verb>[:<pkg>],acts,usual,row:<method>,kind:<K>,example`, `hover=<card key>`, `flip=decl|q-fail|…`, `fly=0..1`, `scroll=<section>`, `gatestate=unindexed`.
- **Files.** All new, all under `v4/page2/`:
  - `Page2.html` is the frame, copied from Graph.html.
  - `page2.css` and `page2.js`.
  - `extract2.mjs` is the data script. It reads `world.mjs`, `rustsrc.mjs`, `facts.mjs` and `source.mjs`, and writes `page2.json` (489 KB).
  - `stills.sh` renders the stills; `snap2.sh` renders a single still.
  - `.harvest/*.html` caches Graph.html's own page sections, taken with headless Chrome `--dump-dom`.
- **Rebuild.** `node page2/extract2.mjs && page2/stills.sh`.
- **Also written.** `CONCEPT.md` (approved) and `VERBS.md` (the cross-language verb table).

## Stills: `v4/shots/page2/` (48)

**The octopus replacement comes first.** W-Shell's plain relation list should become this.

| Still | Shows |
|---|---|
| `rel-rest-value.png` | "Who uses it" at rest: a right-aligned verb, then names (mark + name, yours in mint, as many as fit **on one line**), then "and N more". *In* rows come first, then *out* rows. The door is at the right of the heading. |
| `rel-hover-joint-value.png` | resting on a name raises **the joint**. Here, `ManifestPackage.version: Option<toml::Value>` with Value underlined periwinkle, "holds a Value · yours". The row's trailing zone shows the comb and "17 · 13 yours". |
| `rel-hover-verb-value.png` | resting on a verb shows its definition and its spelling in Rust, TypeScript, Python, Go, Java, C# and C++. |
| `rel-500-rest-serialize.png`, `rel-500-open-serialize.png` | **486 relations.** At rest it is one line. Unfolded in place: "486, 17 of yours, in 13 packages". Then bands (yours · 17 in 2 packages / in serde_core, for the standard library's types · 89 grouped by family / elsewhere · 469 in 11 packages), one line per package, "+ N", then fold · filter 486 · see all 486 in the graph. |
| `rel-500-xray-serialize.png` | ⌥: every row's comb, count and exact relation kind (`impl · derive`, `generic bound`, …). |
| `rel-door-hover-value.png` | the door: the item's real fan, drawn small (one strand per group, log length, mint when it holds yours). On hover the strands spread and "see it in the graph" appears. |
| `rel-door-flight-value-0.5.png`, `…-1.png` | the flight. The rows become the prism: names arc out of their rows into the fan under italic-serif group heads, strands draw from the growing gem, and the page recedes. Then `Graph.html?focus=…`. |
| `rel-480-value.png`, `rel-200pct-serialize.png` | narrow: the verb sits above its names; 200 % behaves like a 720 px room. |
| `rel-B-scales-serialize.png`, `rel-C-combs-serialize.png` | the two rejected presentations, kept for the record. |

**Five complete pages at four sizes.**
- The pages are `value`, `from_str`, `serialize`, `smallvec` and `error`.
- The sizes are `-1440`, `-760` (shelf → 42 px spine), `-480` (no shelf) and `-200pct` (zoom 2, 720 px effective).
- Each still is captured at exactly the page's height.

**docs.rs parity states.**

| Still | Shows |
|---|---|
| `decl-bound-hover-from_str.png` | a bound in words: `T: de::Deserialize<'a>` → "T is any type that can be built by a deserializer, borrowing for 'a" |
| `gate-off-smallvec.png` | `const_new`: "Off in your build: new_const, from_const and 1 more aren't there for you", plus the Cargo.toml line to copy |
| `gate-unindexed-smallvec.png` | the engine's honest state: "Behind feature const_new, not indexed in this build … not listed here, not missing" |
| `caps-usual-value.png`, `usual-open-value.png` | "the usual 21": a short card; pressing it lists them in place by arrival |
| `deprecated-smallvec.png` | `ExtendFromSlice` struck through; the card says "the trait is deprecated" |
| `acts-like-open-smallvec.png` | "acts like a slice &[its items], through Deref and DerefMut: 125 more methods …", opened: all 125 from rust-src, bytes-only and arrays-only groups, SmallVec's own shadowing methods struck through, Rust 1.0.0 to 1.94.0 |
| `member-docs-open-smallvec.png` | one-line member docs folding out to their full docs (`drain`: notes, Panics, source line, since; `with_capacity`: its example) |
| `flip-decl-smallvec.png`, `flip-fail-smallvec.png` | a section turned over to its exact source lines, with the declaration lines lit |
| `xray-smallvec.png` | ⌥: trait names beside capability words, exact types beside plain words, and per row: since · file:line · "you promise" · "can panic" · gate |
| `kinds-hover-error.png` | serde_json::Error's four kinds, told apart by `classify()`, each with its real causes (Syntax: 19 messages from `ErrorCode`'s Display) |
| `doing-it-serialize.png` | a trait's "how do I get one": derive it (438 of its 486 types do) / or write the one method / "not yours? check for a serde feature" (the trait's own `on_unimplemented` note), plus what the derive writes |
| `who-tree-serialize.png` | your own derive sites as real code |
| `since-hover-value.png` | the since card |

## Real, unknown, and computed

**Real, read from sources or world data:**
- **Relations.** All from world.js edges. `relationsOf` is ported entry-for-entry in `page2/world.mjs`.
- **Counts:**
  - toml::Value: used in 59, 7 yours; comes from 55; taken by 10; held by 17 (13 yours); called on by 14.
  - Serialize: done by 486, 17 yours, 438 of them derived; asked for by 129.
  - serde_json::Error: comes from 129.
- **Declarations with bounds, docs with sections, impl blocks and their where-clauses.** All parsed from the registry source (`rustsrc.mjs`), including toml's `impl_into_value!` macro, which expands to its 11 `From` impls.
- **Feature gates**, with **why** each is on:
  - `display` and `parse` are toml defaults;
  - smallvec `serde` is on because you turn it on;
  - serde_core `std` is on through serde_json's std. This is computed by feature unification from Cargo.toml.
- **Acts like.** Slice methods and their `#[stable(since)]`, read from rust-src in the nix store.
- **Since.** From releases.json:
  - toml::Value is "≤ 0.5.11", the oldest of the 4 releases read out of 120;
  - its only real change is `deserialize_enum` in 0.8.23, found after folding re-export paths to their declared path and ignoring parameter renames;
  - SmallVec is "≤ 1.15.1", with nothing changed across the 4 releases read.

**Shown as unknown:**
- since and changes for serde_json and serde_core ("No release history read for serde_core yet …");
- 8 serde_core macro invocations that don't expand without repetition support ("and 8 macro-made we did not read").

**Computed by the prototype, which the engine must own:**
- **Auto traits.** A hand-checked table in `source.mjs` `AUTO`. For serde_json::Error: not UnwindSafe or RefUnwindSafe, because of io::Error's boxed inner error. SmallVec's are conditional on A. Value has all six.
- **Panics read from code.** `.expect("index not found")` in Index/IndexMut, labelled "read from its code; its docs don't say".
- **Error kinds.** Parsed from `classify()` and `ErrorCode`'s Display.

## What the Rust side lacks (named gaps)

**Extractor / engine facts that do not exist yet:**
1. **`cfg` predicate text per item and per impl.** The prototype's `extract.mjs` drops it. The engine uses cfg only to include or exclude (`crates/engine/src/driver/lower/rust.rs` `module_cfg_disabled` / `meta_cfg_disabled`). Needed on every `Row`: `gate: Option<Predicate>`, plus which feature set the index was built with. That is the difference between "off for you" and "not indexed in this build".
2. **Feature unification.** Needed: "on because of X" (`page2/extract2.mjs` `enabledFeatures()`, which walks Cargo.toml plus each crate's `[features]`). Cargo metadata's resolved features would answer it exactly.
3. **Impl blocks as first-class rows.** Header, generics, where-clause, trait with its args (`From<&'a str>`, not `From`), associated items (`type Err = …`), line span, and docs on impl members. Today world.json has only `impls: [{trait, line, members}]`, with trait args lost. That is why "becomes" and the grouping of methods by condition can't come from the index.
4. **Type-level bounds and where-clauses.** Functions keep `gen` / `wh`; types don't (PARITY §2). SmallVec's `A: Array` survives only because it is inline.
5. **Full member docs.** Variants, fields and methods keep the first sentence only (`extract.mjs` `firstSentence`; engine `Member.summary`). Needed: a section-aware `DocFragment` list per member, with `# Errors` / `# Panics` / `# Safety` / `# Examples` as a heading kind.
6. **`#[deprecated(since, note)]` payload.** The prototype keeps only a boolean; the engine keeps nothing (PARITY #4).
7. **Required versus provided per trait method.** The engine lacks it (PARITY #2). Dyn-compatibility: a generic method without `where Self: Sized` makes the trait not usable as dyn (`source.mjs` `contract.dyn`).
8. **Arrival for every capability.** derived / written / through (blanket) / from its parts (auto). `Arrival::Blanket` and `Arrival::Auto` exist in `apps/desktop/src/model/pages/common.rs` and are never constructed. Auto traits need the trait solver.
9. **Deref target plus the target's inherent methods, with `#[stable(since)]`.** Needs std/core indexed once, shared by every page.
10. **Macro-made impls.** The token expansion here handles no-repetition `macro_rules!` only. The engine (ra_ap_hir) sees expanded impls natively; they must reach `impl` edges.
11. **Release history for every locked crate.** Only toml and smallvec have it. The declared-path folding and parameter-name rule must live in the diff, not in the UI.
12. **The joint per relation.** The exact span where a relation happens: the field's type, the parameter, the calling line, the derive attribute. Today it is reconstructed by re-reading the file (`extract2.mjs` `jointOf`). The engine has the spans (`ReferenceSite.span`); the relation row needs to carry one.

**Recipe engine bug surfaced by the gates.** Getting one's best route for SmallVec is `new_const`, which sits behind `const_new`, and your build does not turn that on. The recipe producer table must drop producers whose gate is off for you. The page now dims the route with "const_new — not in your build".

## What `anatomy::` can reuse, and what is new

**Reused as is.** The four anatomies (fork / holds / pipe / contract), `can.rs`, the Getting one rails, `in_use.rs`, cousins and the type speller are harvested verbatim from Graph.html, so the Rust ports already draw them. Two exceptions:
- Traits get one more `afoot` line: "nothing is given for free: it has no provided methods · not usable as dyn Serialize, because serialize is generic over S".
- The contract uses the harvested plain-words row.

**Changed:**
- `prism.rs` on the page is replaced by **`relations`**: verb rows plus the comb plus the in-place unfold by band and package, plus the door and the flight. `semantics::relations` already has the groups. It needs:
  - the verb table (`VERBS.md`: *comes from · done by · called by · taken by · held by · called on by · asked for by · used by · calls*);
  - bands (yours / here / elsewhere) and per-package groups;
  - the joint per entry;
  - `asked for by` (bound users).
- `can.rs` gains the dotted glyph (from its parts) and the fold into "the usual N". Only special capabilities stay at rest; derived basics, Display, "through" caps, autos and universal blankets fold. Exceptions such as "not unwind-safe" stand alone, and gated-off impls fold to "N behind features you don't turn on".
- `does.rs` gains:
  - groups for impl-block conditions, titled in words ("when its items copy freely"), with the exact header under ⌥;
  - folding after 6 rows per receiver group;
  - trailing marks (since, "you promise", "can panic", gate, file:line);
  - a dimmed row plus key mark when a gate is off;
  - struck names when deprecated;
  - a press to fold the one-line summary out to the full doc.

**New components:**
- **`sentence`**: one clause per question, each scrolling to its section. It is in the UI face at ink2, verbs at ink0 weight 560. Each clause is a single unbreakable unit.
- **Marks line**: path, alias, and gate or deprecation.
- **Declaration strip**: exact code with bounds that explain themselves on hover, plus ⌥ bound words.
- **`acts_like`**: one line that folds open.
- **`fails`**: one component for Errors / Panics / Safety / `@throws` / `Raises:`, plus error kinds with their causes.
- **`changed`**: the since clause, rows per release read, and the release comb with the pin in mint.
- **Gate card and deprecation card.**
- **Section flip to source.**
- **Doing it** for traits.
- **Your derive sites** as real code.

## Open questions

1. **Section headings.** "Getting one / What it does / When it fails / Who uses it / What changed" answer questions but aren't phrased as questions. Is that right, or should the headings *be* the questions?
2. **Getting one for an error type.** Today it shows `custom(msg)`, which is how *you* make one in a Deserialize impl, and then "comes from 129 calls". Should an error type's "how do I get one" be only the 129 failing calls, grouped by kind?
3. **Unfolded "acts like".** It is a flat index of 125 names. Grouping by word family (`split_*`, `chunks*`, `sort*`) would be calmer; say if you want it.
4. **Relations presentation at 1 relation.** The row still carries its verb column. One lone relation could instead fold into the sentence and draw no row. My view is that the row stays, for consistency.
5. **Contrast.** Every text run is now ≥ ink3 (≈ 5.1:1). Harvested sections are overridden in `page2.css`: `.anat .ah`, `.cgrp`, rails, captions and the ghost routes of the rails, which are now quiet by ink step, not by opacity. The Rust anatomy port should adopt the same overrides.
