# v6 · Moments: browse, meet, add, hop

The lead, 2026-09-28. Board: `v6/moments/Moments.html?v=browse|package|graph|add`, on this repository's real dependency world (1,350 packages from `Cargo.lock`; surfaces from a source scan; release dates, yanks and newer releases from the sparse-index cache; your use from a scan of your crates; who-uses-what-of-whom between registry crates from their sources). Extractors: `extract_world.py`, `extract_via.py`, `extract_trust.py`, `extract_sym.py`, `extract_more.py`. Their output under `data/` is gitignored because it is regenerated from the local index. The curated page contracts `data/page6/` (`extract_page6.py`) and `data/shapes/` (`extract_shapes.py`) are tracked, because the native lanes build against them. On a fresh clone, run the extractors before opening the board. Research: `.local/lanes/wave6/research/MOMENTS.md`.

## The owner's corrections (2026-09-28, evening). These override anything below.

> "the dependencies and dependents isn't going to scale and shouldn't be the focal point. When you're browsing you just want to know how legit a package is, and the description, and more. That can be a separate page, but not all the way … The list of all the things it depends on is frankly sick but should be progressively disclosed in a cool way, like wikipedia … the symbol words and seeing it all at once is the most important, so on package pages it needs to either come already unfurled or be easy to do so (like docs.rs). Licenses should get special attention so you can hover and it'll give the heads-up like github. transitions between packages are pretty good. contents and versions should be the same thing … graphs would be nice … old security advisories … keep expanding and trying new things."

And earlier: "most of the product pages you're looking at won't be good inspiration, you need to think more abstract … utility while being creative and intelligent and fun and interesting to browse/hover/interact with."

What changed because of it:

- **Legit first.** The package page opens on what it is and whether to trust it. The relations are one section among five, and the relay became its own view (`?v=graph`).
- **Words, all at once.** Contents lists every public name by module, unfurled by default (docs.rs "All items"). Shingles remain the browse overview and the folded state. Opening a package *unfurls* them: each shingle flies to its own word's mark and the word unrolls from it.
- **Contents and versions are one thing.** Contents is always *at a release*. The release comb sits on the list; hovering another release recolours the same words (new mint, changed amber, gone coral struck through). Words never move, and a line says what changes for you. The sidebar's lenses are Contents *at 1.53.1* · Rests on · Used by. There is no Versions lens.
- **Progressive disclosure is a primitive, not a page.** Every package name anywhere (README prose, deps rows, used-by rows, cards) is a preview link. Hovering opens its card. Names inside a card are links too, so the next card opens beside the first, joined by a strand (Gwern's popups, Wikipedia's previews). The tree beneath a package unfolds one step at a time, only along the path you're curious about. The Rests-on list also unfolds inline, a layer at a time.

## The package page, top to bottom

1. **Hero.** Gem, name, the release chip ("1.53.1 · your pin"), what it says it is, who made it, where it lives, categories, edition and MSRV.
2. **Five facts, each a component that opens on hover.**
   - **Releases.** Count and age. The **release comb** shows every release on a time line: tall = major (0.x minors count as major), mid = minor, short = patch, coral = yanked, mint = your pin, amber = newest. It shows how far behind you are. Age, cadence and churn read before any number does. Hover gives the list with dates.
   - **Licence.** The chip is tinted by a verdict for *your* project (MIT OR Apache-2.0). Hover gives GitHub's card: Permits / Asks of you / Won't promise. It parses `OR` (you choose; it picks the lighter option for you and underlines it), `AND` (all apply), `WITH` (exceptions), and the old `/`. It shouts about **no licence** (8 packages in this tree) and copyleft. The world has MPL-2.0, `LGPL-2.1-or-later` as an option, `(MIT OR Apache-2.0) AND Unicode-3.0`, and `WITH LLVM-exception`.
   - **Surface.** Public names, the family bar (types / callables / contracts / values), modules and lines of code, *you use N*, tests and examples.
   - **Weight.** Packages beneath and lines beneath, as a bar: its own code vs direct vs deep ("its own code is 3% of it"). Hover shows where the weight comes from, per direct dependency.
   - **Heads-up.** Read from its own source: build script, proc macro, unsafe count (or *forbids unsafe*), starts programs, network, files, env vars, C calls. Hover shows each with file:line and the line. Advisories go here too, honestly labelled while no feed is configured.
3. **About.** The README's first paragraphs. Every package it names is a preview.
4. **Contents at a release.** Module rows, names flowing, kind marks, mint = your code names it. Hover a word for its signature and first doc sentence. Hover a release for the same list at that release.
5. **Rests on | Used by.** Rests on is sorted by weight, with version, proc-macro/build.rs pills, dep count and lines. ▸ unfolds its own dependencies in place. Used by lists your crates first with how often, then the rest folded. "as a graph" opens the relay.

**Hop.** Clicking any package name goes there. The name you clicked grows into the title (the title ruling), and the package you left flies to its row on the new page, ringed as *from*. The jump bar keeps the trail as a sentence: `toml → toml_edit ← cargo`.

## Browse (the Library as strata)

- **Structure.** Your crates on top, what they rest on beneath (most-used first, labelled), then everything beneath as **true strata**: one layer per step further from your code, each ordered under its parents (barycentre), so strands hang short like roots. Each package is its territory in miniature, mint where your code names it.
- **Hover.** The legit card, the same component as the previews, plus your relation: "you use 5 of 36, 117 times", or the chain that explains why it's here. Strands go to what it rests on (solid) and who uses it (dotted). For anything beneath, a single bright **core** is drawn up through the layers to the crate of yours that pulls it in.
- **The lens.** Over the dense lower layers, a loupe magnifies what's under the pointer, names included, so 1,217 unlabelled blocks are readable without clicking.
- **Type** to light every matching name across the whole library ("41 items in 12 packages"). **V** gives the versions lens (newer releases in amber, used-and-newer items amber).

## Add (play a card)

- A registry search is dealt as a hand. A card's corners are its cost to you: **brings new** and **shares with you**.
- Hover lifts it (the others make room), and what it shares pulses mint in the strata.
- **Play.** Click, or drag it up. The card gathers itself (a short dip), the band makes room (every block glides on a spring, rippling from the front), and the card flies an arc. Words let go in the first third; only the frame and shingles travel.
- **Landing.** It lands "just added" at the front of what you rest on, dashed (unread). What only it brings settles into the layer beneath. What it now shares leaves its old owner's group ("tokio-stream is no longer only one package's"). It develops as the index reads it.

## Graph (the relay, `?v=graph`)

- **Layout.** What it rests on on the left, where it comes from; who uses it on the right, where it goes. Strands carry use counts.
- **Hop.** The chosen chip grows into the hero. The package you left shrinks into the column its relation to the new one puts it in (it *is* a user of the new one), and shared neighbours stay put.
- **Arrival.** You arrive through the relation, as "How toml uses toml_edit": the five items, with the real lines in toml's source that name them, lit on the map.

## Open questions for the owner

1. **Advisories.** The index can ingest RustSec, OSV and GHSA, but no feed is configured on this machine. May we clone `rustsec/advisory-db` into `.local/` for real advisory history (the comb would mark advisory dates and affected ranges)?
2. **"Add" means which?** Add to a project (edits Cargo.toml, the board's current reading) or add to the library for reading only (same motion, into an "also here" band)?
3. **Downloads and popularity** aren't in the offline index. Worth a fetch-on-open from crates.io, or is "used by N in your library" the honest local proxy?

## Discover: the registry, browsable, with Add where you already are (spec, 2026-09-28 night)

The owner: "the add page is good but unpractical, you're almost adding straight off of search or while you're browsing from a crates.io like view (which you still haven't specced out really)."

So Add stops being a place of its own. Add is a verb, and it happens where you already are:

1. **The jump bar.** Typing a name you don't have shows registry answers under your library's, each with its legit strip and a `+`.
2. **A package page's category** ("encoding › parser-implementations") opens Discover on that aisle.
3. **Discover itself.**

**Discover's layout.**
- **The dock.** Your project is pinned at the top as a thin band: the direct dependencies of the crate you're adding to, as blocks sized by weight, with a count.
  - It is the Add-armed consequence lens (research #3, KiCad's ratsnest).
  - Hovering any registry card lights, on the dock, the packages that card would share with you (mint), and shows a dashed ghost slot where it would land, plus "+N new".
  - Nothing else in the dock moves.
- **Aisles.** A search field, then aisles: one horizontal row per category, scrolled sideways.
  - At the head of each aisle, **what you already have in this category** (from `categories.json`'s `yours`). This answers "do I need this at all?" before you browse ("you already have toml; basic-toml is the lighter alternative").
  - Then the registry's crates in that aisle.
  - Search collapses the aisles into one ranked aisle.

**A registry card** carries the same vocabulary as the package page, at card size:
- the shingle fingerprint (surface);
- the release comb (age, cadence);
- the licence seal;
- the heads-up stack;
- the cost to you ("brings 1 new · shares 20");
- a `+`.

Hover opens the card in place, taller, with its README line and what it would pull in. Click the name to open its page, a registry package page in the "not yet yours" dashed state.

**Add.** Press `+`:
- **The flight.** The card gathers itself, then flies to its ghost slot in the dock with the play spring (hand.js's flight, retargeted).
- **Landing.** The dock makes room, the count ticks when the card lands, and what it brings settles as small blocks behind it.
- **Toast.** "Added tonic to desktop · brought 1 · shares 20 · Undo".

It is the same moment as the board's `?v=add`, but it happens mid-browse, one click away.

**Data.** `data/registry.json` holds every crate on disk that isn't in the lock: facts, releases, licence, heads-up counts, brings. `data/categories.json` indexes the aisles and what you already have in each.

## The symbol page, redrawn across languages (2026-09-28, late)

The owner: "the whole where it comes from what uses it is really clever but the way it's presented leaves a lot of blank space and doesn't scale well … prototypes for this across languages, and for highly used symbols, that are used in many different ways. Describing generics and fallibility … works less well in languages like javascript."

**Where it is:**
- `Lab.html`: the instruments, each on real symbols in five languages. Forced states for stills: `&only=plate|ways|crowd&id=<id>&k=<id>:<knob>,…&s=<id>:<sibling>&b=<id>:fail|none`.
- `Moments.html?v=symbol5`: the page composed from them. `toml::Value` uses the same model as `?v=symbol`, so the two compare directly. `&id=rs-serde_json-from_str | py-re-match | js-which | ts-effect-map` shows a function.

**Data.** `extract_shapes.py` writes `data/shapes/<id>.json`, all local:
- **Corpora:**
  - the Rust registry sources plus this repository (yours);
  - the Go module cache;
  - the Python 3.14 stdlib with pip's vendored libraries, meson, PIL and wheel;
  - two node_modules trees.
- **Evidence is checked.** Every fact cites a line, and the extractor refuses to run if the cited text isn't on that line.
- **Call sites** are lexical. They drop:
  - comments and doc examples;
  - mentions inside string literals (59 of this repo's from_str "sites" were fixture strings in the facet galleries);
  - older duplicate versions of a crate or module.

  Each site is tagged code, test, example or bench, and yours or not.

### 1. The plate: one shape for every language (replaces the pipe and the badge row)

**Layout.**
- Ports in on the left, the name plate in the middle, ports out on the right.
- **Exits** hang beneath: *maybe nothing* (hollow), *fails / throws / raises / rejects / fails inside* (coral), *later* (peri, dashed clock), *many*.
- The words are the language's own. Rust *fails*; JS/TS *throws*; Python *raises*; a Promise *rejects*; an Effect *fails inside* (its failure is a channel, not an exit).

**How we know** is drawn on every fact:
- **in its type**: solid.
- **in its docs**: dashed.
- **in its code**: dotted.
- **at call sites**: beaded.

Hover shows the line. This is the answer for JavaScript: it has no types, so `which.sync` shows every fact dotted and names its source lines (`throw getNotFoundError(cmd)` at which.js:121), instead of pretending to a Rust-like signature. Python's stdlib has no hints either, so re.match's None is *in its docs* and its `re.error` is *in its code*. TypeScript can't say "throws", so zod's throw is *in its docs*, and its sibling safeParse moves the failure *into its type*.

**Generics are threads, named by role, never by letter alone.** The pills keep the letters; the words say what they mean:
- **you choose**: from_str's T, drawn as a dial that spins through what callers actually chose (about 250 distinct types; Value in 8 crates, ThemeSet, Metadata, …).
- **must be**: BinarySearch's E is `cmp.Ordered`.
- **passes through**: Effect's E and R, re.match's AnyStr.
- **turns into**: map's A into B, through the function port.
- **the schema decides**: zod's `output<this>`.

A thread runs along a lane under the ports and joins every pill of its name.

**Knobs** are options that change the shape, drawn as switches on the plate.
- `which.sync` **nothrow**: the throw exit becomes a null exit.
- `which.sync` **all**: one path becomes many.
- `subprocess.run` **check=True**: opens the CalledProcessError exit. Each knob says how many real calls pass it ("29 of 127 calls").

**Click an exit** and it unfolds in place into a **bow-tie**. Why it fails sits on the left (serde_json's Syntax / Data / Eof, from its `Category` enum); what callers actually do sits on the right. For from_str outside tests (122 calls): 26% crash (unwrap/expect), 19% pass it up, 19% convert, 18% handle, 4% drop it.

### 2. The family: siblings by what differs

Siblings form a small matrix whose axes are what differs:
- zod: parse / safeParse × now / later;
- from_str / from_slice / from_reader / from_value by input (only from_reader can fail with Io; from_value only with Data);
- re: match / search / fullmatch by where it looks;
- Effect: map / mapError / mapBoth / flatMap by channel.

Hover a cell and the plate becomes that sibling. Only what changed moves (keyed FLIP): the failure exit leaves and the result port arrives.

### 3. The ways: highly used names as a double word tree (replaces "your code and it" at scale)

**The tree.**
- All call sites fold into a word tree (Wattenberg and Viégas; a linguist's concordance).
- Left is what callers write before the call (`let x: T =`, `match`, `if`, `err :=`, `.pipe(`).
- Right is what they do after, grouped by consequence (crashes, passes it up, converts, handles, drops, tests it in a condition, reads it unchecked), then the actual token (`.unwrap()`, `?`, `.map_err(…)`, `.group()`).
- Word size and band width grow with count.
- **in code / in tests / all / yours** switch the corpus, because an unwrap in a test isn't one in a library.

**The concordance.** Hover a branch and the concordance keeps only those lines, aligned on the call, one per crate first so it shows spread rather than one crate's tests.

**Findings on real data:**
- **Python.** In the stdlib's own tests, 165 of 549 re.match calls read `.group()` without checking for None. In code, 26 test it in a condition and 25 check it later.
- **Go.** testify's assert.Equal splits 253 "expected first" to 159 "two values", with 2 possibly reversed.
- **TypeScript.** Effect.map is 180 data-first to 96 data-last, and 16 of its uses are `() => …`, which ignores the value (that's what `Effect.as` is for).
- **Rust.** About 250 distinct types were chosen for from_str's T. 64 of the 830 sites are yours, 30 of them outside tests.

A handful of sites (which: 1) is shown as a list, not a tree.

### 4. The ledger: where it comes from, where it goes, dense (replaces the fan)

**Layout.** The fan's rows of one token each (the blank space) become two columns of wrapped token flows meeting at a spine, like a T-account.
- **Left:** is one of 7, reads one from text, converts from, made by, other types give one.
- **Right:** reads it (17, capped with "+5 more" that unrolls), changes it, consumes it, takes it, held in.

**Weight.** Every member carries a mint bar for how much your code reaches it, so the separate reach bar is gone: the weight sits on the words.

**The reach matrix.** "How it's used" is a matrix of who (your crates, then other packages) × which part, with a square per pair sized by use. Click a square for its lines. Hover a member in the ledger and its column lights.

### 5. The crowd: a trait implemented thousands of times

serde::Serialize in this world:
- 8.3k implementors across 343 crates, 83% by derive;
- yours: 836 across 19 crates, grouped as their own region;
- each crate is a region cut into derived (teal) and hand-written (hatched).

Hover shows its types.

### Still open

- The dense page doesn't yet carry the sibling strip, history ticker or the hop moves from `?v=symbol`. They port unchanged, and the ledger's tokens are the same `tok()`.
- Cross-language data is lexical. The product's producers (tsz/OXC, ruff and pyrefly, go/types) would give the index the same facts with real resolution. The "how we know" grades map directly onto what each producer can prove.
- The lab and page currently run on the board. The native port waits on the owner's pick among these, plus W-Glyph's page scaffolding.

## The symbol page, simple form (2026-09-29): what ships

The owner, after the lab:
> "You're being too ambitious … a clean view with progressive disclosure and smarter layout would be much preferred, rather than going symbol lab mode. Closer to current docs.rs flow with creative visual … fallibility and enums and just representing core data structures should involve more of your attention … using colors and shapes and icons to communicate things like fallibility."

What we took from the lab:
- the rail (as vertical rows, not a schematic);
- fallibility as shape and colour;
- generics as violet pills with a hover card;
- the "how we know" grade, reduced to a dotted underline;
- the family, reduced to "Next to it";
- workspace uses with verb tags.

What we dropped: the crash statistics, the word trees, the ledger, the matrix, the knobs as switches and the crowd.

**Where it is:** `Moments.html?v=symbol6&id=rs-from_str | rs-Value | rs-as_str | rs-AllocationInfo | rs-Serialize | py-re.match | js-which.sync`. Code is `symbol6.js` and `symbol6.css`; data is `extract_page6.py` → `data/page6/*.json`, which is the page model. The native spec and brief is `.local/lanes/wave6/lead/briefs/W-Sym6.md` (lane W-Sym6, Sonnet).

**The grammar, in one place:**
- **Outcomes** are the rail's end:
  - peri arrow: gives;
  - slate dashed ring: or nothing;
  - coral ✕ block: or fails / throws / raises / rejects;
  - peri clock: later;
  - double arrow: gives each.

  The same glyphs mark every method row and sibling.
- **Ports:**
  - filled dot: required; hollow: optional; double dot: variadic;
  - square: the receiver, tinted by what it does.
- **Verbs** tag methods and workspace places alike:
  - makes: teal ＋;
  - reads: ink ring;
  - changes: amber ring with a dot;
  - uses up: peri disc;
  - holds: bracket;
  - matches: fork;
  - calls: arrow into a plate;
  - derives / implements / asks for: violet.
- **Types:** plain word first, then the written type, quieter. Generic parameters are violet pills. A type read from docs or code rather than declared has a dotted underline.
- **Data:**
  - an enum is a fork: "one of N", a teal rail with diamonds; a case holding itself shows a loop mark;
  - a struct is a bracket: "holds N", a thick bar with squares, and "you read it · N" in mint;
  - a trait is "what you write".
- **Workspace:** one package picker (a menu, not a chip row), verb chips, tests off by default, rows grouped by package; click opens the line in your editor.
