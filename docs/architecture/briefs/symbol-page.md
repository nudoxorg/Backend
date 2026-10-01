# The symbol page: beyond docs.rs

Handoff brief. Written by W-Parity (Sonnet), 2026-09-27, at HEAD `92f974b370660b8273147886893703d3305e99c4`
on branch `canonical`. The next agent on this component has no access to the lead's scratch
folder or this conversation — everything it needs to act is either in this file, in the
copied source documents under `docs/architecture/briefs/symbol-page/`, or cited by exact
`path:line` in the repository itself. Where a claim about code could not be verified at HEAD
in this pass, it says "unverified" rather than guessing.

---

## 1. Purpose

The owner's words, verbatim, collected across sessions:

- "The symbol page should shatter docs.rs."
- "most symbol pages don't feel as comprehensive as docs.rs, this needs a lot of work."
- "on the symbol view that octopus thing just isn't flexible enough, could be improved to fit
  better into the interface, and to work more consistently across languages."
- "A specialized GUI format instead of code views."
- "Facts are components, not text."

**What "beyond docs.rs" means concretely**, synthesized from `page2/CONCEPT.md`,
`Nudox-Design-System/v4/DIRECTIONS.md` and `docs/architecture/gui-plan.md` §8.3:

1. **Carry everything docs.rs carries**, not as a code-shaped dump but as a purpose-built
   anatomy: a fork for an enum, a bracket for a struct, a pipe for a function, a contract for
   a trait — the same four shapes across all seven supported languages, with generics, bounds
   and where-clauses spelled in plain words as well as exact source (⌥ reveals the exact
   spelling beside the plain one).
2. **Then add what docs.rs structurally cannot**: real call sites from your own tree ("In
   use"), a chain engine that answers "how do I turn an X into a Y" across packages
   ("cousins"), capability arrival in plain words ("copies freely", "sorts", not a raw trait
   list), and a real "when it fails" section built from tracing which callables actually
   construct which error variant — not just the return type.
3. **Organise the page by the question a reader arrives with**, not by docs.rs's declaration
   order: what is it → how do I get one → what can I do with it → what can go wrong → who
   uses it → what changed. Each section's heading is a plain statement, not a question (the
   sentence already asks); see §4.1.
4. **Replace the relations "octopus"** (a bezier-curve fan, `apps/facet/src/anatomy/prism.rs`)
   with a **sentence and verb rows** on the page itself, and move the fan itself into the
   graph view, reached through a small door drawing in the "Who uses it" heading. The octopus
   was rejected because its size is set by geometry (one relation still costs a 300px fan; 500
   relations show only "and 477 more"), its words depend on the kind of item being viewed, it
   reads sideways, and it sits beside the prose rather than in it. See `page2/CONCEPT.md` §1.
5. **Facts are components, not text.** Each fact — deprecation, a feature gate, a license, a
   version — is its own small component with a rest state (a glyph plus at most one word), a
   hover card, and (for some) a press action. A line of fields joined by `·` is the rejected
   pattern (the package hero's old facts line, "0.1.0 · cargo · MIT OR Apache-2.0 · 1278
   declarations", was rejected outright — `DIRECTIONS.md` §1). On the symbol page this shows
   up as: the marks line (path · alias · since · gate · deprecated, each its own mark), the
   gate card, the deprecation card, and each relation's "joint" hover card.
6. **Work the same way for all seven supported ecosystems** (Rust, TypeScript/JS, Python, Go,
   Java, C#, C++), not just Rust. The verb grammar, the anatomy shapes, and the doc-section
   convention detector are all designed language-neutral from the start (`page2/VERBS.md`);
   what varies per language is only which rows exist and how a fact is spelled, never the
   grammar itself.

---

## 2. Where it stands at HEAD

### 2.1 File map (symbol-page-relevant, `path:line` checked at HEAD)

**Desktop, production render path:**
- `apps/desktop/src/shell/bodies/symbol.rs` (966 lines) — the page body. `body()` (`:27-86`)
  picks between the fixture-backed anatomy (`anatomy_reference`, `:307-353`) and the plain
  fallback (`declaration` + `docs` + `relations` + `ledgers`) depending on whether
  `runtime::fixture_world::anatomy(...)` resolves the declaration.
- `apps/desktop/src/runtime/fixture_world.rs` — loads `Nudox-Design-System/v4/graph/world.json`
  (16 MB) **once per process** and joins it to indexed declarations by source identity
  (`IdentityAdapter`). This is a documented stand-in (its own module doc, `:1-14`): "until the
  runtime projects one [a `World`] from the index's own data, this snapshot is that world...
  the anatomy is an enhancement, never a gate." **This is the single most important
  architectural fact for whoever picks this up**: the anatomy (fork/holds/pipe/contract/can/
  does/Getting one/In use) is driven by a frozen prototype extraction of a handful of crates
  (serde, serde_json, toml, smallvec, csv, plus this workspace) via
  `Nudox-Design-System/v4/graph/extract.mjs`, joined to real indexed rows only when a
  declaration's source identity matches. It is **not** the real engine's facts pipeline.
- `apps/desktop/src/runtime/page_mapping.rs` — the real read-model mapper (`rose()`,
  `references()`, `member_of()`, `derive_impl_blocks()`, `client_gap()` etc). This is where
  Phase 1's facts (`DeclFacts`) actually reach `DeclRef`/`Member`, independent of the fixture
  world.
- `apps/desktop/src/model/pages/symbol.rs` (`SymbolPage`, `Member`, `DocSections`,
  `DocSection`, `Relation`, `Arrival`) and `apps/desktop/src/model/pages/common.rs`
  (`DeclRef`, `DeclFacts`, `Deprecation` at `:375-430`, `KindFamily`, `GapReason`).

**Facet (design-system crate), built but only partly wired:**
- `apps/facet/src/anatomy.rs` (196 lines) + `apps/facet/src/anatomy/{fork,holds,pipe,contract,
  can,does,in_use,prism,rail,text,gallery,tests}.rs` (2,379 lines total) — the GPUI elements.
  `prism.rs` (`pub fn prism`, `:47`) still exists and compiles, but **is not called by
  `bodies/symbol.rs`** (see below).
- `apps/facet/src/semantics.rs` + `apps/facet/src/semantics/{bounds,caps,members,model,names,
  page,recipes,tour,fails,types,usage}.rs` plus `recipes/chain.rs`, `tests/`. This is the pure
  logic layer, all unit-tested, all operating on `facet::graph::model::World` (the fixture
  world's own type), not on the real engine's `Row`.
  - `semantics/relations.rs`: `relations_of` (`:31`), `page_shows` (`:132` — the groups a
    page's own anatomy already covers, so the relations list says each thing once),
    `except` (`:143`), `prism` (`:154`, the old octopus's column layout — still present,
    still compiled, **not called from the desktop** any more, see 2.2).
  - `semantics/recipes.rs` (1,578 lines) + `recipes/chain.rs`: the "Getting one / Calling it"
    engine (Knuth least-cost derivation) **and** the hand's chain finder — `Connector`
    (`chain.rs:207`), `convert` (`:393`), `join` (`:556`), `arrange` (`:573`). This is the
    "one chain authority" `GRAPH-HANDOFF.md` asked for; it is landed (commit `d2b0a9589`,
    "feat(facet): semantic recipe chains").
  - `semantics/fails.rs` (508 lines) — "How it fails" (gui-plan §8.3): kinds an error type
    can be, and which callables construct each kind, with call-graph memoization. Landed
    (commit `4ff1c26b8`), unit-tested, **but not referenced anywhere under `apps/desktop`**
    (`rg -n "semantics::fails" apps/desktop` = zero hits, checked this pass). This is a
    built, tested component waiting to be wired in — not a gap in the engine, a gap in the
    page.
  - `semantics/tour.rs` (568 lines) — the package "Start here" reading-path algorithm
    (gui-plan §8.2). **This one is wired**: `apps/desktop/src/runtime/fixture_world.rs:26`
    imports it (`use facet::semantics::tour::{self, Tour}`).
  - `semantics/page.rs`: `Page { node, facts, shape, caps, prism, does }` (`:24-38`) and
    `page()` (`:47-60`) — **still builds and carries the old `prism` field**
    (`super::relations::prism(...)`, `:57`). The `Page` struct has not yet dropped it.

### 2.2 What renders today (verified by reading `bodies/symbol.rs`, not by a fresh capture — see caveat)

For a declaration the fixture world resolves (broadly: items in serde/serde_json/toml/
smallvec/csv and this workspace), the reference lens renders, in order: the shape
(fork/holds/pipe/contract, or the raw declaration for an alias/constant/shapeless item), the
`can` line, the recipe rail (Getting one / Calling it) when there is one, the docs, **then a
plain verb-row relations list** (`unsaid_relations`, `bodies/symbol.rs:359-410`), then Does.

`unsaid_relations`'s own doc comment (`:361`) already states the design intent precisely:
*"the prism belongs to the graph"* — i.e. **the old bezier-fan octopus is already gone from
the production render path** for anatomy-backed pages. What replaced it is a first cut of the
page2 verb-row idea: a right-aligned verb column (`verb()`, `:414-423`) next to wrapped,
clickable, kind-marked names (`:374-410`) — but **without** the "and N more" fold, without
banding by yours/here/elsewhere, without the per-package sub-groups, without the hover "joint"
card, without the verb-definition hover, and without the door drawing + flight into the graph.
For a declaration the fixture world does *not* resolve, the plain fallback still uses the
older `relations()` function (`:640-`, not read line-by-line this pass) for the same purpose,
and that path has not been touched at all — it is where the "octopus" reference in the old
audit and in the owner's complaint originated, and it may still be the actual old
rose/relations styling; **the successor agent should diff `relations()` against
`unsaid_relations()` before assuming they already match.**

Nothing under `bodies/symbol.rs` renders `SectionKind`/`DocSections`'s Errors/Panics/Safety
groupings distinctly yet in the anatomy path (F4's data exists on `Member`/`SymbolPage` per
Phase 1, but `docs()` at `:551-` was not re-read this pass to confirm styling) — mark this
**unverified** until read.

**Capture caveat.** The task asked for a harness capture where possible
(`.local/target/debug/backend-desktop-gui-harness capture --scene desktop-package --out …
--size 1440x900`). That binary exists from a build at 16:30 today. A fresh capture was not
attempted in this pass: `backend-facet` — a crate `backend-desktop` depends on — was mid-edit
by a concurrent lane for the entire second half of this session (`apps/facet/src/overlay/
float/model.rs:980,989`, "missing field `linear`"), and capturing against a binary built
before that edit, or attempting a fresh build against broken code, would both be misleading.
Re-run the command above once `cargo check -p backend-desktop --tests` is green again.

### 2.3 The tests, and exactly how to run them

- `apps/desktop/tests/facts_truth.rs` — `cargo test -p backend-desktop --test facts_truth --
  --nocapture`. Real embedded `backend-locald`, two small fixture packages
  (`apps/desktop/tests/fixtures/facts_{standalone,unmanifested}`), asserts F1-F4 on the real
  `SymbolPage`/`Member` read model, once per lane (compiler-backed and structural).
- `apps/desktop/tests/content_truth.rs` — `cargo test -p backend-desktop --test content_truth
  -- --nocapture`. Real content assertions across `crates/present` (workspace member,
  structural-only) and `frontends/rust/fixtures/rich_project` (standalone, compiler-backed).
  **Currently fails at HEAD**, not from Phase 1 work — see §7 D9.
- `cargo test -p backend-compile facts` — the carrier's own unit tests
  (`crates/compile/src/facts.rs`'s `mod tests`).
- `cargo test -p backend-compatibility-tests --test structural_facts` — one test per
  language (Rust/Java/C#/Python/TypeScript/Go/C++), real source through
  `SyntaxFrontend::analyze`, asserting rendered deprecation and obligation text.
- `cargo test -p backend-facet --lib -- semantics anatomy` — the facet-side unit suite
  (semantics logic + anatomy element tests); last known-good count (W-Anatomy CHECKPOINT-2,
  2026-09-27): **72 passed, 1 ignored** (the ignored test, `semantics::tour::tests::
  whole_world`, belongs to `tour.rs`, not to this agent's scope).
- `cargo test -p backend-present --lib` — includes `sections.rs`'s own doc-section-detector
  tests (`sections::tests::*`), 50+ tests total in that crate.

### 2.4 The last green runs, quoted (this session, at HEAD `92f974b37`)

```
test declaration_facts_reach_the_page_on_both_lanes ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 32.06s
```
(second consecutive run: `finished in 28.28s`, identical)

```
test facts::tests::an_oversized_note_is_cut_visibly_and_refused_on_admission ... ok
test facts::tests::a_semantic_observation_wins_only_when_it_looked ... ok
test facts::tests::documentation_conventions_read_their_words ... ok
test facts::tests::every_attribute_spelling_reads_its_since_and_note ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 53 filtered out; finished in 0.16s
```
(second consecutive run: `finished in 0.13s`, identical)

```
test go_reads_the_deprecated_paragraph_and_interface_methods ... ok
test java_reads_deprecated_annotations_and_interface_defaults ... ok
test rust_reads_deprecated_attributes_and_trait_obligations ... ok
test python_reads_deprecated_decorators_and_abstract_methods ... ok
test typescript_reads_jsdoc_deprecation_and_optional_members ... ok
test csharp_reads_obsolete_and_default_interface_members ... ok
test cpp_reads_deprecated_attributes_and_pure_virtuals ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
```
(second consecutive run: `finished in 0.12s`, identical)

```
thread 'every_board_reads_real_content_through_the_desktop_runtime' panicked at
apps/desktop/tests/content_truth.rs:303:50:
references gap
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 54.40s
```
Full analysis in §7 (D9) and in `docs/architecture/briefs/symbol-page/facts-checkpoint-1.md`.

---

## 3. The design

The prototype at `Nudox-Design-System/v4/page2/Page2.html` is the design target. It is a live,
data-driven HTML/JS page (not a mockup): its data comes from
`Nudox-Design-System/v4/page2/extract2.mjs`, which reads the world, the registry sources,
`releases.json`, rust-src and each crate's `Cargo.toml`.

**How to serve and view it.** From `Nudox-Design-System/v4` (or anywhere; the scripts
`cd` themselves): `python3 -m http.server 47811 --bind 127.0.0.1` from the
`Nudox-Design-System` folder (one level above `v4/`), then open
`http://127.0.0.1:47811/v4/page2/Page2.html?page=<name>` where `<name>` is one of `value`
(toml::Value), `from_str` (toml::from_str), `serialize` (serde::Serialize),
`smallvec` (smallvec::SmallVec) or `error` (serde_json::Error). `gsnap.sh`/`snap2.sh`/
`stills.sh` in the same folders will start that same server automatically if it isn't
running, and take headless-Chrome screenshots.

**URL parameters** (from `page2/CHECKPOINT.md`): `page=<name>`, `x=1` (⌥ held / x-ray), `t=2`
(200% text), `tall=1` (whole page, no viewport crop), `rel=a|b|c` (force one of the three
relation-presentation prototypes — verb rows / scales / combs, see below), `open=<verb>[:<pkg>]
,acts,usual,row:<method>,kind:<K>,example` (force a fold open), `hover=<card key>` (force a
hover card open, for stills), `flip=decl|q-fail|…` (force a section's source-flip state),
`fly=0..1` (freeze the door flight at a fraction), `scroll=<section>` (jump to a section),
`gatestate=unindexed` (force the "not indexed in this build" gate state).

**Regenerating the data**: `node page2/extract2.mjs && page2/stills.sh` from `v4/`.

**Every still in `Nudox-Design-System/v4/shots/page2/` (48 files, all present at HEAD),
one line each** (grouped as `page2/CHECKPOINT.md` groups them):

- `rel-rest-value.png` — "Who uses it" at rest: verb, then names (yours in mint), then "and N more"; *in* rows before *out* rows.
- `rel-hover-joint-value.png` — resting on a name raises the joint card (`ManifestPackage.version: Option<toml::Value>`, underlined periwinkle).
- `rel-hover-verb-value.png` — resting on a verb shows its definition and its spelling in all seven languages.
- `rel-500-rest-serialize.png` — 486 relations, one line at rest.
- `rel-500-open-serialize.png` — the same, unfolded: bands (yours / serde_core / elsewhere), one line per package.
- `rel-500-xray-serialize.png` — ⌥: every row's comb, count and exact relation kind.
- `rel-door-hover-value.png` — the door: the item's real fan drawn small (one strand per group, log length, mint when it holds yours), spreading on hover.
- `rel-door-flight-value-0.5.png` / `-1.png` — the flight mid-way and at the end: rows become the prism, strands draw, the page recedes.
- `rel-480-value.png` — narrow: verb above its names.
- `rel-200pct-serialize.png` — 200% text, behaves like a 720px room.
- `rel-B-scales-serialize.png` — rejected presentation B (scales), kept for the record.
- `rel-C-combs-serialize.png` — rejected-at-rest presentation C (combs), kept as the row's hover/⌥ detail.
- `decl-bound-hover-from_str.png` — a bound in words on hover ("T is any type that can be built by a deserializer...").
- `gate-off-smallvec.png` — a feature-gated route dimmed, "Off in your build", plus the Cargo.toml line to copy.
- `gate-unindexed-smallvec.png` — the engine's honest state: "not indexed in this build ... not missing".
- `caps-usual-value.png` / `usual-open-value.png` — "the usual 21" folded, then opened by arrival.
- `deprecated-smallvec.png` — `ExtendFromSlice` struck through, card: "the trait is deprecated".
- `acts-like-open-smallvec.png` — "acts like a slice", opened to all 125 Deref-target methods with their Rust `since`.
- `member-docs-open-smallvec.png` — a one-line member doc folding out to its full text (Panics, source line, since).
- `flip-decl-smallvec.png` / `flip-fail-smallvec.png` — a section turned over to its exact source lines.
- `xray-smallvec.png` — ⌥: trait names beside capability words, exact types beside plain words, since/file:line/gate per row.
- `kinds-hover-error.png` — serde_json::Error's four kinds from `classify()`, with real causes.
- `doing-it-serialize.png` — a trait's "how do I get one" (derive it / write the one method / "check for a feature").
- `who-tree-serialize.png` — your own derive sites, as real code.
- `since-hover-value.png` — the since card.
- `{value,from_str,serialize,smallvec,error}-{1440,760,480,200pct}.png` (20 files) — five complete pages at four sizes each.

---

## 4. Decisions, numbered

Sourced from `page2/RULINGS.md` (the lead's rulings on D-Page's checkpoint — these are
**authoritative over `page2/CONCEPT.md`'s proposals** where they differ), `facts/PLAN.md`
(the facts rulings, mostly the plan's own recommendations, approved by the coordinator
resuming the agent with "Phase 1 ... as approved" rather than a line-by-line reply — see the
note at the end of this section), the docsrs `PARITY.md` conclusions, and
`Nudox-Design-System/v4/motion/MOTION.md`'s rulings on the D-Page motion proposals.

1. **Headings stay statements** ("Getting one", "What it does", "When it fails", "Who uses
   it", "What changed"), not questions. Reason: "questions as headings read cute and add
   noise; the sentence already asks." (`RULINGS.md` §Open questions, item 1.)
2. **An error type's "Getting one" is what produces it.** List the failing calls grouped by
   kind (the kinds `classify()` tells apart), with one quiet line below for "making your own"
   (`Error::custom(msg)` inside a Deserialize impl). (`RULINGS.md` item 2.)
3. **"Acts like", unfolded, groups by word family** (`split_*`, `chunks*`, `sort*`, `iter*`,
   …), one line per family; a family of one goes into "others". (`RULINGS.md` item 3.)
4. **A single relation still gets its row**, for consistency, rather than folding into the
   sentence. (`RULINGS.md` item 4.)
5. **Every run of text is ≥ ink3.** W-Shell applies ink3 to `anatomy::heading` in Rust; ports
   adopt `page2.css`'s contrast overrides. (`RULINGS.md` item 5.) This is **not yet true** of
   the `does` line under the Glacier theme — see §7.
6. **Verb rows replace `anatomy/prism.rs` on the page.** The relations presentation is verb
   rows: yours first, "and N more" unfolding by band then by package, comb on hover/⌥.
   (`RULINGS.md` opening paragraph.) **Status at HEAD: partially done** — see §2.2.
7. **The prism/octopus lives in the graph only, reached by the door.** `unsaid_relations`'s
   own doc comment already states this design intent (`bodies/symbol.rs:361`).
8. **Counts name their tier** — e.g. "486, 17 of yours, in 13 packages", never a bare "486".
   (Implicit throughout `page2/CONCEPT.md` §2-3 and the stills; stated most directly in the
   `rel-500-open-serialize.png` description.)
9. **DTO 7→8, with a reopen proof required.** `facts/PLAN.md` §1: "confirm... I will prove
   that a workspace written before the change reopens, with the journal replayed and the PSR
   records re-admitted byte-identically." This landed: `crates/library/wire/mod.rs:65` is
   `DTO_VERSION = 8`; the version gate itself is tested
   (`crates/library/wire/tests.rs::dto_versions_and_outer_fields_are_strict`, and confirmed
   by a fresh mutation this pass — see `facts-checkpoint-1.md` §5.3). A dedicated
   before/after-reopen proof for a real on-disk workspace was captured by the prior agent as
   scratch evidence (`reopen-show-before-readd.json`, `reopen-view.journal.before` — not
   preserved in the repo); the PSR round-trip tests in
   `crates/engine/src/builtin/relation.rs` (`declaration_facts_round_trip_in_the_facts_format_only`,
   `a_record_stating_no_containment_encodes_exactly_as_it_did_before`) are the repo-preserved
   equivalent and are green.
10. **Re-parent impl methods under their impl row** (Phase 2, R1) — the plan's own
    recommendation ("I recommend re-parenting"), not yet built; still open for the lead's
    call per the plan's §7 risk list, item 0. Treat as **proposed, not decided**, unless the
    Phase 2 agent finds a more recent ruling.
11. **Empty document (not a fabricated placeholder) for undocumented structural rows.** F3's
    "producer honesty fix": landed, `crates/local-service/src/builtin/view_build/
    semantic.rs:362-384` (verified reading the code this pass, see
    `facts-checkpoint-1.md` §1).
12. **`ItemKind::Variant → DeclarationKind::Constant`** is a known, deliberately-deferred
    defect (a Variant should get its own kind), fixed **in Phase 2**, not Phase 1. Confirmed
    still present at HEAD: `crates/local-service/src/builtin/view_build/identity.rs:69`,
    `ItemKind::Constant | ItemKind::Variant => DeclarationKind::Constant`.
13. **The recipe engine must drop producers whose feature gate is off for the reader's own
    build** (the SmallVec `new_const` case, behind `const_new`), once W-Facts Phase 2 (R3)
    delivers gate facts; until then the page dims such a route with "not in your build".
    (`RULINGS.md`, "Recipe engine" section.) **Not built at HEAD**: no `gate`/`const_new`
    logic exists in `apps/facet/src/semantics/recipes.rs` (checked, zero hits this pass).

**The lead's rulings on `facts/PLAN.md` (verbatim, 2026-09-27; added by the lead at review).**
The finisher couldn't find these in the W-Facts transcript because they were delivered as an
agent message. They are binding and settle the plan's open questions, including §7 item 0
(re-parenting):

1. **F2 carrier:** yes. The structural-pairing obligation covers Phase 1, and the IR flags
   column (F2b) is deferred. Record which languages' semantic rows are covered only through
   pairing.
2. **DTO 7 → 8:** confirmed. Proof that a pre-change workspace reopens is required.
3. **R1 containment:** re-parent impl methods under their impl rows so that the two lanes
   agree. Land the page's gather-by-impl in the same change, so the type page's `does` never
   goes empty between commits.
4. **content_truth:** add a second small workspace-member fixture. Never edit
   `crates/present`'s real code for tests.
5. **Producer honesty:** approved. Undocumented rows get an empty document, not
   "{kind} in {path}:{line}".
6. **`ItemKind::Variant → DeclarationKind::Constant`:** in scope for Phase 2 as its own item,
   with a test showing that an enum's variants arrive as variants on the page. Anatomy's "one of"
   depends on it. If the fix moves any pinned value, stop and report it.

Phase-2 cadence, same as Phase 1: implement a phase, then write a checkpoint that includes:
- each fact's text arriving on the page, quoted from a test;
- the byte-identity golden;
- one quoted mutation panic per fact;
- the crates built and tested, with two identical runs each.

Then stop for review.

---

## 5. The docs.rs parity ledger

One line per docs.rs capability, drawn from `docsrs/PARITY.md`'s five worked examples and its
"15 biggest gaps" ranking. Status is have / partial / missing / beat (we already do it better
than docs.rs). "Needs" names the fact or engine piece; "Phase" says when it lands relative to
this work (Phase 1 = W-Facts F1-F4, done; Phase 2 = W-Facts R1-R5, not started; Page = the
page-rendering work in this brief's §6, independent of the facts phases; Beyond = not
scheduled in either phase, a future lane).

- Declaration with generics, bounds, where-clause (functions) — have. Needs: nothing new (§0 of `facts/PLAN.md` — the structural lane's written signature already includes it). Phase: none.
- Declaration with generics, bounds, where-clause (types) — partial. Needs: R1's pairing fix (impl-block collision currently erases a semantic type page's written signature whenever the type has an impl in the same file). Phase: 2.
- Variant/field/member one-line docs — have (was: docs.rs shows full docs; we had first-sentence only). Phase: 1, done.
- Full member docs beyond the first paragraph — have. Needs: F3 (`Member.docs: Arc<[DocFragment]>`). Phase: 1, done.
- `# Errors` / `# Panics` / `# Safety` as distinct sections — have (data model). Needs: F4 (`DocSections`, `SectionKind`). Phase: 1, done for the data; Page for the render style (see §2.2 caveat, unverified whether `docs()` in `bodies/symbol.rs` already styles them distinctly).
- Doc examples (fenced code blocks) — have, mechanism exists (`DocFragment::Code`); whether the Rust doc-comment lowering actually extracts fenced examples was flagged "wired but unconfirmed" in PARITY.md and not re-verified this pass. Phase: none (if it works) or Beyond.
- Required vs. provided trait methods — have. Needs: F2 (`DeclFacts.obligation`, structural pairing). Phase: 1, done.
- Trait implementors ("Implementations on Foreign Types") — partial. Needs: R1 (a direct `Implements` edge, or a hardened self-type match; today's `is_impl_block` heuristic needs a signature starting `nominal(`, missing blanket impls). Phase: 2.
- Blanket impls (`&'a T`, `Cow<'a,T>`) — missing. Needs: R1's self-type peeling (`&`/`Cow`/`Box`/`Rc`/`Arc`) plus `Arrival::Blanket` construction (R2). Phase: 2.
- Auto trait impls (Send/Sync/Unpin/...) — missing. Needs: `Arrival::Auto` construction from the trait solver (`ra_ap_hir`), out of scope for R1/R2 as written (flagged "deferred" in `facts/PLAN.md` §6). Phase: Beyond.
- Item-level deprecation — have. Needs: F1. Phase: 1, done.
- cfg/feature gates, verbatim predicate text — missing. Needs: R3 (`DeclarationFacts.gates`). Phase: 2.
- Which feature set the index used ("off for you" vs "not indexed") — missing. Needs: R4 (report the fixed `all_features: true` policy on a capability row). Phase: 2.
- Methods from `Deref<Target=[T]>` ("Methods from Deref") — missing. Needs: std/core indexed plus a synthesis pass; PARITY.md's own "highest-effort item on this whole audit". A cheap partial (one summary line, no enumeration) is proposed but not built. Phase: Beyond, with a Page-level cheap partial available now.
- "Not dyn compatible" note — missing, low priority (niche fact, no object-safety computation anywhere). Phase: Beyond.
- Macro signatures (pattern/arms, not just the bare name) — missing/unconfirmed. Phase: Beyond (cheap, low value).
- Source links per declaration/impl block — have. Phase: none.
- Real callers in your own tree, syntax-highlighted ("In use") — beat. `apps/facet/src/anatomy/in_use.rs` is a complete GPUI port; wired into the fixture-anatomy path (`bodies/symbol.rs` Lens::Usage) but not into the plain-fallback Usage lens, which still lists bare file:line with no code snippet. Phase: Page (wiring only; the byte spans already exist on `ReferenceSite`).
- Since / changed-in with real diffs across two arbitrary versions — beat (when data exists). `releases.json`/`releases-ui.js` diff toml and smallvec pairwise; docs.rs shows one version only, never a diff. Desktop's `Lens::History` is a literal placeholder string (`bodies/symbol.rs:59-62`, per PARITY.md; not re-read this pass to confirm it is unchanged). Phase: Beyond for production wiring; the algorithm exists in the prototype.
- Cousins / cross-package conversion roads ("turn a toml Value into a serde_json Value") — beat, prototype-only. `recipes/chain.rs`'s `convert`/`join`/`arrange` are landed in `apps/facet` (see §2.1) but not rendered as a page section yet. Phase: Page.
- Capability arrival in plain words ("copies freely", "sorts") — beat. `apps/facet/src/anatomy/can.rs` + `semantics/caps.rs`, wired for fixture-backed pages. Phase: none (already better than docs.rs where it renders).
- "How it fails" with real makers per kind, not just the return type — beat, built, **not wired**. `semantics/fails.rs` (508 lines, tested) exists and is unreferenced by `apps/desktop`. Phase: Page.
- Package "Start here" reading path — beat, wired. `semantics/tour.rs`, used by `fixture_world.rs`. Phase: none (already landed for the fixture path; real-index projection is separate future work per `fixture_world.rs`'s own module doc).

---

## 6. The plan in order, with an acceptance test for each step

### 6.1 Page rendering order (from `page2/RULINGS.md` "Implementation order")

Each step should render "unknown" (never a guess, never a blank space that looks broken) until
its underlying fact exists — this is the same `Known<T>`/`GapReason` discipline the rest of
the desktop model already uses.

1. **Relations verb rows** in `semantics::relations` + `apps/facet/src/anatomy` (or their
   successor modules): the verb table from `page2/VERBS.md` §1, banding (yours / here /
   elsewhere) and per-package groups within a band, the "and N more" unfold, the hover joint
   card. *Acceptance*: a golden test comparing the verb-row output against the prototype's
   `relationsOf` output on a real fixture (as `apps/facet/src/semantics/tests/relations.rs`
   already does for the underlying groups, `relations.golden`), extended to check the folded
   "and N more" text and the per-package sub-lines. The joint card itself waits on the engine
   carrying spans per relation (§6.3's "the joint per relation", deferred past Phase 2).
2. **The sentence** — one unbreakable clause per question, each scrolling to its section.
   *Acceptance*: a test that the sentence's clause count matches the number of non-empty
   sections, and that clicking (or a synthetic scroll-to) each clause lands on the right
   section anchor.
3. **`can` fold** ("the usual N", keeping only special capabilities at rest) plus the dotted
   auto-trait glyph. *Acceptance*: a fixture with ≥ 4 universal/derived caps and one
   exception (e.g. "not unwind-safe") asserts the fold hides the universal ones and keeps the
   exception at rest.
4. **`fails`** — one component, starting from what `semantics/fails.rs` already computes
   (kinds + makers). *Acceptance*: wire `fails.rs`'s existing, tested output into a new
   anatomy element and a `bodies/symbol.rs` render branch; the existing `fails.rs` unit tests
   (against `tests/engines.golden`) are the ported logic's acceptance test, and a new render
   test (content, not count) confirms the section appears with the right kind names.
5. **`acts_like`** (Deref target: one line, then the fold) — per Ruling 3, unfolds grouped by
   word family. *Acceptance*: a fixture type with a Deref target whose inherent methods share
   ≥ 2 families (e.g. `split_*`, `chunks*`) asserts the folded view groups them and a family
   of one lands in "others".
6. **`does`**: impl-condition groups (titled in words, e.g. "when its items copy freely"),
   folding after 6 rows per receiver group, trailing marks (since, "you promise", "can panic",
   gate, file:line). *Acceptance*: a fixture with > 6 methods on one receiver group asserts
   the fold; a fixture with a conditional impl block asserts the group title text.
7. **Gates, deprecation, member docs, since** — each lands as W-Facts Phase 2 delivers the
   fact (R3 for gates; F1/Phase 1 already delivers deprecation and member docs — confirm
   they're actually *rendered* with the "unknown until known" discipline, since Phase 1 only
   guaranteed the *data* reaches `Member`/`DeclRef`, not that `bodies/symbol.rs` styles it).
   *Acceptance*: a fixture with a deprecated member asserts a struck name and a hover card
   with the note text; a fixture with a `#[cfg(feature = "x")]` item, once R3 lands, asserts
   the dimmed row and key mark.

### 6.2 W-Facts Phase 2, in the order `facts/PLAN.md` §4 ranks them

- **R1: impl rows + the pairing fix** (rank 5, "largest Rust win per line changed").
  `DeclarationKind::Implementation = 19`; `view_build/identity.rs:73` gets its own pairing
  family; the structural lane gains `(impl_item type: (_) @name) @definition.implementation`
  (`frontends/rust/lib.rs:20-30`); `ImplementationFacts { self_type, contract, blanket }` from
  tree-sitter fields. **The blanket-impl pin (confirmed by the lead):**
  `apps/facet/src/semantics/recipes/chain/tests.rs:140`
  `any_deserialize_owned_needs_a_blanket_impl_the_world_does_not_record`.
  - It runs on a synthetic `traits_world()`, never on the live `world.json`.
  - It pins today's behaviour: a generic `from_str` returns "any DeserializeOwned", and
    `Value`, which implements only `Deserialize`, does not join, because the world records
    no blanket impls. A type that implements `DeserializeOwned` itself does join (`JoinHow::As`).
  - When R1 lands, the world model gains blanket impls. Then keep this test for a world
    *without* the blanket impl, and add a sibling where the synthetic world records serde's
    `impl<T: for<'de> Deserialize<'de>> DeserializeOwned for T`. In that sibling,
    `c.join(&generic, &c.piece(value))` must return `Some(JoinHow::As)`.
  - `Connector::traits_of` (`recipes/chain.rs:268`) is where the blanket is applied.

  *Acceptance*: `content_truth.rs`'s
  Boxed-type assertion (`GapReason::Encoded`, currently pinned at content_truth.rs's Boxed
  fixture, per `facts/PLAN.md` §1 goldens table) flips to a real written signature, plus a new
  test asserting a blanket impl (`impl<'a, T> Serialize for &'a T`) is captured as a row named
  by its self type.
- **Variant → Constant** fix (adjacent to R1's kind work): change
  `view_build/identity.rs:69` to give `ItemKind::Variant` its own `DeclarationKind` (or
  confirm the desktop model already treats it correctly downstream before changing the wire
  tag — a `DeclarationKind` change affects canonical row bytes, so treat it with the same
  content-addressing care as any other Phase 1/2 change).
- **Impl rows on the page**: `page_mapping.rs`'s `is_impl_block`/`derive_impl_blocks`
  rewritten to use `kind == Implementation` and `ImplementationFacts` instead of the leaf-name
  sniff; the `does` ledger groups methods by impl block.
- **R2: Arrival** (Direct/Blanket/Derived), built on R1's impl rows and `derives` facts.
- **R3: cfg gates** — `DeclarationFacts.gates`, verbatim predicate text; page-side
  `SymbolPage.gates: Known<Arc<[Gate]>>`.
- **R4: which feature set the index used** — report the fixed `all_features: true` policy
  (`crates/engine/src/application/host/authority.rs:196`) on the local compiler capability
  row.
- **R5: type-level bounds/where-clauses** — mostly free once R1's pairing fix lands (written
  signatures already carry them; the fix is what makes them reach the page for a Rust type
  that also has an impl in its file).

### 6.3 Two defects to fix regardless of phase order

- **The recipe engine drops producers whose feature gate is off for the reader's build**
  (Ruling 13 above, SmallVec's `new_const` behind `const_new`) once R3 exists; until then, dim
  with "not in your build" (already the UI intent per `RULINGS.md`, not yet built — no
  `gate`/`const_new` logic exists in `recipes.rs` at HEAD, confirmed by grep this pass).
- **The missing "made by" test.** `apps/facet/src/semantics/relations.rs:132`'s `page_shows`
  drops `Word::MadeBy` from the relations list on type pages specifically because "Getting
  one says it" (the recipe rail already shows makers). This exclusion is exercised by
  `page.rs:53`'s call into `except(relations_of(...), &page_shows(...))`, but — per the task
  brief that produced this pass — a mutation of `page_shows` was previously caught only by an
  unrelated `fails`-family test, not by a test that specifically asserts "a type page's
  relations list omits Made By, and Getting one covers it instead." **This could not be
  re-verified by a fresh mutation in this session**: `backend-facet` (which `page_shows` lives
  in) was mid-edit by another lane for the remainder of this session (see §2.2's capture
  caveat), so the mutation-and-revert discipline this repo requires could not be safely run
  against it. The fix is straightforward regardless: add a test in
  `apps/facet/src/semantics/tests/relations.rs` (which already has a `MadeBy` assertion at
  line 77, for a different function) that specifically covers `page_shows`'s type-only
  `MadeBy` exclusion and fails if it's removed.
- **Flight-label defects** (`page2/RULINGS.md` "Defects to fix in the implementation"): names
  running together with no gap ("index_mutvalue", "indexvalue" in
  `rel-door-flight-value-0.5.png`), and labels colliding mid-arc (inherits / requirement /
  inherited_string, package_facts / parse_rustsec). Fix: order the stagger by distance to the
  landing slot (nearest first), choose arcs that don't cross since landing slots are known
  before takeoff, order the stagger by landing y, and never let two legible labels overlap in
  any frame (`Nudox-Design-System/v4/motion/MOTION.md`'s binding no-overlap law: "two legible
  texts never overlap in any frame" — RULES.md quotes this verbatim as one of the design-law
  bindings for every wave-4 lane). `MOTION.md` §"Neighbouring lanes" → D-Page also rules: order
  the stagger by distance nearest-first, replace the page's `opacity 1 → 0.18` recede with a
  clip-based close ("the page must not dim to nothing"), and use the glide curve for the
  cubic-out.

---

## 7. Known gaps and risks

- **D9: workspace members get no compiler publication for typed relations, but *do* now get
  a structural references answer** (this is more precise than the original framing —
  investigated this pass, not merely restated). `content_truth.rs:9-13` documents the
  intended design: `crates/present` (a workspace member) gets only the structural
  projection, no compiler ("semantic") publication, so typed relations (`rose.up/down/left/
  right/implemented_by`) and references should all report `GapReason::NoSemanticPublication`.
  At HEAD, `rose.up` still correctly gaps (the test's assertion at line 300 passes), but
  `references` does not: `crates/local-service/src/builtin/commands/semantic_query.rs`'s
  `execute_references` (`:900`) falls back to `execute_structural_references` (`:1003`)
  whenever no complete semantic publication is found for the package, returning a real
  (structural) `Ok(SurfaceReply::References { .. })` instead of an error. `git blame` dates
  this fallback to commit `64c36433b2`, "Merge the product command pipeline split (PR #40)",
  **2026-09-25 23:36 UTC — before any wave-4 lane started and before the prior W-Facts
  agent's own recorded HEAD.** So this is not a Phase-1 regression; it is a real engine
  improvement that `content_truth.rs`'s assertion (line 303-305) was never updated for. This
  blocks: nothing for the *reader*, who now gets bare use-sites for their own code where
  before they got nothing — but it blocks **typed relations, and the anatomy's relations
  list, and the "Who uses it" verb rows' `done_by`/`called_by` grouping** for a reader's own
  workspace code, since those still depend on the semantic publication that workspace members
  don't get. Whoever picks this up should: (a) fix `content_truth.rs`'s stale assertion (it's
  a one-line change, not a design change), and (b) decide whether workspace members should
  also get a structural fallback for *typed* relations (not just bare references), which would
  make the anatomy actually useful on the owner's own code — today it is not (see next gap).
- **The page doesn't say how a reference was matched (added by the lead, from W-Green).**
  Structural references carry `SemanticConfidence::Syntactic` (`structural.rs:1637`). The
  desktop model threads it through as `ReferenceSite.confidence`
  (`apps/desktop/src/model/pages/symbol.rs:364`). But nothing under `apps/desktop/src/shell/**`
  or `apps/facet/src/**` reads it, so the page can't say "matched by path" the way every
  count must name its tier. A related hazard: structural `FileSpan.bytes` are relative to the
  caller's excerpt, not to the file. The semantic lane's spans are file-absolute, and both use
  the same `FileSpan { file, bytes }` shape. Until that is fixed, a jump to source from a
  structural reference lands in the wrong place.
  `apps/desktop/tests/content_truth.rs` documents it inline.
- **Blanket impls are missing from `world.json` and the index** — confirmed independently by
  `docsrs/PARITY.md` (the extractor's `paths()` only resolves nominal self-types; `&T`/
  `Cow<T>` yield no self-type node) and by `facts/PLAN.md` (the real engine's `derive_impl_blocks`
  has the identical loss via its leaf-name heuristic). Fixed by R1 (Phase 2).
- **Glacier theme contrast**: `page2/RULINGS.md` states every run should be ≥ ink3 (≈5.1:1);
  the anatomy port's own evidence log (`wave2/shell/anatomy-lint-glacier.log`, cited in
  `QUEUE.md`) records ink3 measured at **3.68-4.38**, and the `does` line specifically at
  **1.03:1** — a near-invisible line. This needs a calibration pass before the page can be
  called done; it is flagged in `QUEUE.md` as a queued Sonnet task ("Theme calibration"), not
  yet started as far as this pass could tell.
- **Cross-language consistency**: the page must work for all seven supported ecosystems. What
  the facts carrier gives today, per language, for the two Phase-1 facts that generalize best:
  - Rust: deprecation from `#[deprecated]` (structural: sees and drops before Phase 1, now
    kept; semantic: staged via `FactSet::attach_item_attributes`); obligation from
    `function_signature_item`/`function_item` in a trait.
  - Java: deprecation from `@Deprecated` (kept raw in `JavaFacts.annotations` since before
    Phase 1, only the *interpretation* was missing — now interpreted); obligation from
    `default`/`abstract` modifiers.
  - C#: deprecation from `[Obsolete(...)]` (kept with arguments already); obligation from
    interface member bodies / `abstract`/`virtual`.
  - Python: deprecation from `@deprecated`/`@typing_extensions.deprecated`/
    `warnings.deprecated` decorators (kept raw with arguments already); obligation from
    `@abstractmethod` inside an ABC/Protocol base.
  - TypeScript: deprecation from JSDoc `@deprecated` (kept as doc text already); obligation
    from interface `method_signature`/`?`-optional members, abstract class members.
  - Go: deprecation from a `// Deprecated:` doc paragraph (kept verbatim, no flag before
    Phase 1); obligation: interface methods are always Required (Go has no default-method
    concept — this axis collapses to nothing for Go, by design, not a gap).
  - C++: deprecation from `[[deprecated("...")]]`/`__attribute__((deprecated))` (structural
    only; the semantic/libclang lane does not model attributes at all, `frontends/clang/src/
    legacy/facts.rs:234-261` — this is a real, standing gap, not deferred by choice); obligation
    from `= 0` pure-virtual vs a virtual with a body.
  All seven are exercised end-to-end by `tests/compatibility/tests/structural_facts.rs` (§2.3),
  which is the practical proof that the grammar generalizes — but only at the structural
  (tree-sitter) layer. Whether each language's *semantic* (compiler-backed) lowering also
  surfaces these facts for a real toolchain-indexed package was not verified in this pass for
  Java/C#/Python/TS/Go/C++ (no non-Rust toolchain was exercised); the Rust semantic lane was
  proven by `facts_truth.rs`'s `facts_standalone` fixture.

---

## 8. Seams with other lanes

- **W-Shell**: owns the hand/jump bar and the graph door. Per its own resume brief
  (`QUEUE.md`, and the W-Shell transcript filtered for "chain"/"anatomy"/"made by"): it is
  building §15, "the plan for graph integration", listing every seam that makes the graph
  first-class, including **the symbol page's door into the graph: a focus request carrying
  origin rects** (from `GRAPH-HANDOFF.md`), and **cross-package navigation from anatomy
  links** (anatomy links today only peek, per `GRAPH-HANDOFF.md`; they need the graph's
  search-and-resolve exposed as a service). Whoever builds the page2 door drawing and flight
  (§3, §4 ruling 7) should coordinate the exact focus-request shape with W-Shell rather than
  invent a second one — `apps/facet/src/semantics/recipes/chain.rs`'s `Connector` is already
  the shared authority for chains between the hand and (eventually) the graph's
  `discovery.rs::chains`, per `GRAPH-HANDOFF.md`'s "one chain authority" item.
- **W-Marks**: builds `ecosystem_mark`, `license_mark`, `version` (on `controls::comb`) and
  `dep_link`, wired into the package hero's marks line — not the symbol page directly, but the
  same "facts are components" pattern this brief's §1 describes should be followed for the
  symbol page's own marks line (path · alias · since · gate · deprecated) once W-Marks's
  components are available to reuse. Do not build parallel mark components; ask W-Marks (or
  its successor) for its finished API first.
- **The motion grammar** (`Nudox-Design-System/v4/motion/MOTION.md`, binding for every lane):
  the door's flight is `Gather` (spread's cousin — things go *to* their new places, not away),
  480ms with a `(k mod 8) × 17ms` stagger, ordered nearest-first; the page's recede is a close
  (clip), never a fade to near-zero opacity; curves are `glide` for the cubic-out. Every other
  transition on the page (fold/unfold, peek unfurl, section flip) should be picked from
  `MOTION.md`'s vocabulary (§"The vocabulary") rather than invented ad hoc — the whole point
  of the grammar is that a motion's meaning is legible without narration.

---

### 8.x Lead's rulings from W-Shell's graph plan (2026-09-27, CHECKPOINT-2 §15)

- **The door, receiving side.** W-Shell builds `GraphView::enter_from(node, origins)`. At G, or when
  the door is pressed, the page hands `(SymbolRef, Bounds)` for the relation rows in view (from
  `Targets::bounds_of`). In the graph, each row's label flies from its origin to its prism slot on
  an arc and lands at about 480 ms. **This page builds the door button and its small drawing**
  ("the prism drawn small and true", `Nudox-Design-System/v4/page2/page2.js:223`), and calls the
  same `Map::show(…, door, …)` path. `apps/facet/src/anatomy/prism.rs` is being **retired**
  (it is the rejected octopus), so build the door drawing as a new, small component.
- **Links.** A link with no place is drawn as text, one rule in `kit`. A cross-package anatomy
  link resolves through the new shared `runtime/node_pages.rs` service. When that fails, the
  foot's single visit-scoped `Notice` says why. Build page links on both.
- **The History lens is a dead end today.** It always says "No release history is recorded…"
  (`apps/desktop/src/shell/bodies/symbol.rs:87-90`). `runtime/fixture_releases.rs` already has
  toml and smallvec releases, and the upgrade lens reads them. The page's "What changed" section
  should read the same source until the index serves release history.
- **Blanket impls are missing from `world.json` and the index** — confirmed independently by
  `docsrs/PARITY.md` (the extractor's `paths()` only resolves nominal self-types; `&T`/
  `Cow<T>` yield no self-type node) and by `facts/PLAN.md` (the real engine's `derive_impl_blocks`
  has the identical loss via its leaf-name heuristic). Fixed by R1 (Phase 2).
- **Glacier theme contrast**: `page2/RULINGS.md` states every run should be ≥ ink3 (≈5.1:1);
  the anatomy port's own evidence log (`wave2/shell/anatomy-lint-glacier.log`, cited in
  `QUEUE.md`) records ink3 measured at **3.68-4.38**, and the `does` line specifically at
  **1.03:1** — a near-invisible line. This needs a calibration pass before the page can be
  called done; it is flagged in `QUEUE.md` as a queued Sonnet task ("Theme calibration"), not
  yet started as far as this pass could tell.
- **Cross-language consistency**: the page must work for all seven supported ecosystems. What
  the facts carrier gives today, per language, for the two Phase-1 facts that generalize best:
  - Rust: deprecation from `#[deprecated]` (structural: sees and drops before Phase 1, now
    kept; semantic: staged via `FactSet::attach_item_attributes`); obligation from
    `function_signature_item`/`function_item` in a trait.
  - Java: deprecation from `@Deprecated` (kept raw in `JavaFacts.annotations` since before
    Phase 1, only the *interpretation* was missing — now interpreted); obligation from
    `default`/`abstract` modifiers.
  - C#: deprecation from `[Obsolete(...)]` (kept with arguments already); obligation from
    interface member bodies / `abstract`/`virtual`.
  - Python: deprecation from `@deprecated`/`@typing_extensions.deprecated`/
    `warnings.deprecated` decorators (kept raw with arguments already); obligation from
    `@abstractmethod` inside an ABC/Protocol base.
  - TypeScript: deprecation from JSDoc `@deprecated` (kept as doc text already); obligation
    from interface `method_signature`/`?`-optional members, abstract class members.
  - Go: deprecation from a `// Deprecated:` doc paragraph (kept verbatim, no flag before
    Phase 1); obligation: interface methods are always Required (Go has no default-method
    concept — this axis collapses to nothing for Go, by design, not a gap).
  - C++: deprecation from `[[deprecated("...")]]`/`__attribute__((deprecated))` (structural
    only; the semantic/libclang lane does not model attributes at all, `frontends/clang/src/
    legacy/facts.rs:234-261` — this is a real, standing gap, not deferred by choice); obligation
    from `= 0` pure-virtual vs a virtual with a body.
  All seven are exercised end-to-end by `tests/compatibility/tests/structural_facts.rs` (§2.3),
  which is the practical proof that the grammar generalizes — but only at the structural
  (tree-sitter) layer. Whether each language's *semantic* (compiler-backed) lowering also
  surfaces these facts for a real toolchain-indexed package was not verified in this pass for
  Java/C#/Python/TS/Go/C++ (no non-Rust toolchain was exercised); the Rust semantic lane was
  proven by `facts_truth.rs`'s `facts_standalone` fixture.

---

## 8. Seams with other lanes

- **W-Shell**: owns the hand/jump bar and the graph door. Per its own resume brief
  (`QUEUE.md`, and the W-Shell transcript filtered for "chain"/"anatomy"/"made by"): it is
  building §15, "the plan for graph integration", listing every seam that makes the graph
  first-class, including **the symbol page's door into the graph: a focus request carrying
  origin rects** (from `GRAPH-HANDOFF.md`), and **cross-package navigation from anatomy
  links** (anatomy links today only peek, per `GRAPH-HANDOFF.md`; they need the graph's
  search-and-resolve exposed as a service). Whoever builds the page2 door drawing and flight
  (§3, §4 ruling 7) should coordinate the exact focus-request shape with W-Shell rather than
  invent a second one — `apps/facet/src/semantics/recipes/chain.rs`'s `Connector` is already
  the shared authority for chains between the hand and (eventually) the graph's
  `discovery.rs::chains`, per `GRAPH-HANDOFF.md`'s "one chain authority" item.
- **W-Marks**: builds `ecosystem_mark`, `license_mark`, `version` (on `controls::comb`) and
  `dep_link`, wired into the package hero's marks line — not the symbol page directly, but the
  same "facts are components" pattern this brief's §1 describes should be followed for the
  symbol page's own marks line (path · alias · since · gate · deprecated) once W-Marks's
  components are available to reuse. Do not build parallel mark components; ask W-Marks (or
  its successor) for its finished API first.
- **The motion grammar** (`Nudox-Design-System/v4/motion/MOTION.md`, binding for every lane):
  the door's flight is `Gather` (spread's cousin — things go *to* their new places, not away),
  480ms with a `(k mod 8) × 17ms` stagger, ordered nearest-first; the page's recede is a close
  (clip), never a fade to near-zero opacity; curves are `glide` for the cubic-out. Every other
  transition on the page (fold/unfold, peek unfurl, section flip) should be picked from
  `MOTION.md`'s vocabulary (§"The vocabulary") rather than invented ad hoc — the whole point
  of the grammar is that a motion's meaning is legible without narration.

---

## 9. Working rules and commands

- **Build**: `.local/devenv/cargo <cmd> -p <crate>` only, from the repo root. Never
  `--workspace`, never `cargo clean`. Relevant crates for this component:
  `backend-compile`, `backend-engine`, `backend-library`, `backend-local-service`,
  `backend-present`, `backend-facet`, `backend-desktop`, `backend-compatibility-tests`.
- **Test**: trust only two identical consecutive runs. A mutation must be applied, tested and
  reverted inside one Bash command with a `trap` that restores the file and `touch`es it
  afterward; quote the real panic line, never a narrated "would fail". Assert on rendered
  content (the exact text a reader would see), never on a count alone.
- **Harness capture** (once `backend-desktop` compiles cleanly):
  `.local/target/debug/backend-desktop-gui-harness capture --scene desktop-package --out
  <path> --size 1440x900` (and `--scene desktop-symbol` for this page specifically, per
  `QUEUE.md`'s scene names). Rebuild the harness binary first if the crate has changed since
  16:30 today (`ls -la .local/target/debug/backend-desktop-gui-harness`).
- **Git**: read-only except `git status`, `git diff`, `git log`, `git show`, and `git add
  <a new file you created>`. Never stash, checkout, restore, reset, clean, commit, rebase or
  filter-repo. The owner commits between sessions; a clean `git diff` at the start of a
  session means the previous session's work is already in HEAD — reorient with `git log
  --stat -15` and `git show <sha> -- <file>` rather than assuming uncommitted work was lost.
- **Shared crates**: `backend-facet` is edited by multiple lanes concurrently. If it fails to
  compile because of someone else's in-flight edit, wait and retry — do not fix their code
  (this happened during this very session: `apps/facet/src/overlay/float/model.rs` was
  missing a field for the entire second half of this pass).
- **Scope discipline**: stay inside this component's files. `apps/desktop/src/shell/bodies/
  symbol.rs`, `apps/facet/src/anatomy*`, `apps/facet/src/semantics*`,
  `apps/desktop/src/model/pages/{symbol,common}.rs`, `apps/desktop/src/runtime/{page_mapping,
  fixture_world}.rs`, `crates/compile/src/{facts,syntax_facts}.rs`,
  `crates/present/sections.rs`. If a change is needed in another lane's file (the graph body,
  W-Marks's components, W-Shell's shell chrome), write the request into your own checkpoint
  rather than editing it.

---

## 10. Appendix index

Copied into `docs/architecture/briefs/symbol-page/`, each `$S/...` scratch path rewritten to a
repo path or marked as scratch-only evidence not preserved in the repo:

- `page2-concept.md` — D-Page's design rationale: why the octopus fails, the three relation
  presentations tried against real data, the question-ordered page layout, the graph door, the
  docs.rs parity fold-in table.
- `page2-checkpoint.md` — the 48 stills inventory, what's real/unknown/computed today, what
  the Rust side lacks (12 named engine gaps), what `anatomy::` can reuse vs. what's new, and
  D-Page's own open questions.
- `page2-rulings.md` — the lead's binding rulings on D-Page's checkpoint (§4 of this brief is
  built from it).
- `page2-verbs.md` — the full cross-language verb grammar (relation verbs, qualifiers, arrival
  glyphs, and what the prototype's world can and can't supply today).
- `docsrs-parity.md` — the full gap matrix: five worked Rust examples (toml::Value,
  toml::from_str, serde::Serialize, serde_json::Value, toml::de::Error, smallvec::SmallVec,
  serde_json::json!, RawValue::NULL, smallvec's serde feature, ExtendFromSlice's deprecation),
  the "what docs.rs cannot show and we can" section, the ranked 15 gaps, and the cross-language
  generalization notes.
- `facts-plan.md` — W-Facts's own implementation plan (F1-F4 Phase 1, R1-R5 Phase 2), with
  every file:line it read, its serialization/hash-impact table, its test plan, and its ranked
  summary of what it deferred and why.
- `facts-checkpoint-1.md` — this pass's own checkpoint: what of Phase 1 was already in HEAD,
  what was missing (two dead-code warnings, `content_truth.rs`'s Phase-1 test additions,
  `facts_polyglot.rs`) and the reasoning for deferring the latter two, quoted test results
  (two consecutive runs each), the D9 investigation, and five fresh mutations with quoted
  panics.
