# GUI work order 2 — "all information is preserved and displayed"

Read `AGENTS-DOCTRINE.md` first. This order is scoped to `workspace/gui/` only.
Do not start it while another agent is building the GUI (doctrine §8: one
`target/` — concurrent cargo corrupts the build-script cache).

Every finding below was read off real captured pixels in `.shots/memchr/`, on a
real `memchr-2.8.3` lowering (11,329 symbols). Re-open those PNGs; do not work
from this description alone.

---

## The standard being applied

The program's goal is that lindsey is *visibly better than docs.rs*. Three of
the five findings below are places where we are currently **worse**, and all
five are places where information we already have in hand is not shown.

Note what is already right, and do not regress it: real doc prose, working
intra-doc hyperlinks, `Implementations 6`, live result counts, a real version
chip. The bones are good. This is about what the frame does with its space.

---

## F1 — Search results are undifferentiated (was L20). Highest priority.

**Evidence:** `.shots/memchr/04-search-hits.png`. Seven consecutive rows each
render exactly `mod memchr` over `mod memchr`, with an identical `mod` chip and
an identical `local` chip. There is no way for a user to choose between them.

**Cause, and why it is not a rendering bug:** those are seven genuinely
different modules — `src/memchr.rs`, `src/arch/all/memchr.rs`,
`src/arch/x86_64/avx2/memchr.rs`, `src/arch/wasm32/simd128/memchr.rs`, and so
on. The engine already knows their distinct paths; the breadcrumb in
`08-symbol-opened.png` proves the data reaches the GUI. The row template simply
does not show the one field that distinguishes them.

**Re-measure before quoting "seven".** Those frames were captured while
dependencies were silently unresolved and all Cargo features evaluated false
(`LIMITATIONS.md` L28), so more arch-gated modules were live than should be.
With features resolving, fewer duplicates may appear. **The defect is
unchanged** — whatever rows do appear are still undifferentiated — but do not
put the number seven in a test assertion.

**Required:** a search row must be uniquely identifying. Show the module path.
Where paths share a prefix, the disambiguating segment is the valuable one —
prefer eliding the common head (`…/arch/x86_64/avx2/memchr`) over truncating
the tail.

**Per doctrine §2, fix the type, not the template.** A row that *can* be built
without its disambiguator is the defect. Make the search-result view model
carry the identifying path as a non-optional field, so a future row cannot be
constructed ambiguously. A test that only asserts "7 results returned" passes
today — assert instead that rendered rows are pairwise distinct.

## F2 — The reader wastes roughly 60% of its viewport

**Evidence:** `08-symbol-opened.png`. Doc prose ends around y=400 of 1800. Below
it is ~600 px of empty background, and `Implementations 6` / `References` /
`Source` sit collapsed and pinned at the very bottom, separated from the content
they belong to by a void.

`Implementations 6` is real, populated, and one row tall. docs.rs would have
those six impls on screen. We have the data and choose not to show it.

**Required:** content should occupy the space it has. Decide deliberately
between expanding the disclosures by default and letting content flow up to
them — and say in your report which you chose and why. Do not add a
scroll-to-fill hack.

## F3 — "ON THIS PAGE" lists exactly one entry

**Evidence:** `08-symbol-opened.png`, right rail: a single `Documentation` item.

The page demonstrably has Documentation, Implementations (6), References, and
Source sections. A table of contents with one entry is worse than none — it
occupies a full rail to say nothing. Populate it from the real section plan,
including the impl methods, and make entries navigate.

## F4 — Breadcrumb reads `memchr › memchr › memchr` (was L18)

Package, module, and symbol collapse to one repeated token. Suppress adjacent
duplicate segments, or qualify them so each segment carries distinct
information. Whichever you choose, `memchr › memchr › memchr` must not survive.

## F5 — Result count contradicts what is drawn

The header says `21`; eight rows are drawn; there is no scrollbar, no fade, and
no "showing 8 of 21". Either show them all or show the affordance. A count the
user cannot reconcile with the list reads as a bug in *our* search.

## F6 — No per-item source jumping

The brief asked explicitly for "full hyperlink support and **source jumping**".
docs.rs links every item to an exact line range
(`src/memchr/memchr.rs.html#288-291`). We have one collapsed whole-symbol
`Source` section, and what it does is **unverified** — `10-source-tab.png` is
byte-identical to `08`, so the disclosure never opened in any captured frame.

**Establish what it already does by mouse before designing anything.** It may
work and simply be unreachable from the keyboard, in which case this collapses
into the same focus defect as everything else in the 08–15 run.

## F8 — Package provenance is thin

docs.rs shows owner, dependency list, repository and homepage links, license,
release date, and a doc-coverage badge. We show name, version, symbol count.

Before promising any of these, **check which are actually in the IR**. Do not
add a field the engine cannot fill — report what is missing instead. An empty
provenance rail is F3 all over again.

## F9 — A `TODO` is rendered as user-facing UI text

**Evidence:** `.shots/memchr/17-bottom-dock-open.png`. The Jobs panel body reads,
centred and in the product's own type:

```
Jobs
TODO(views): crate::views::jobs_panel
```

Developer scaffolding is shipping in a user-visible surface. Check the `Logs`
tab for the same pattern, and grep the crate for other `TODO(`/`FIXME` strings
that reach a rendered node.

Either implement the panel or render an honest empty state. If it stays a stub,
the placeholder must not name a Rust module path at the user — an empty state
says what *the user* would see here when it is populated.

Doctrine §8 applies: a placeholder is a claim too. Add a test that asserts no
rendered text node in any panel contains `TODO(` or `FIXME` — that is a
one-line guard that permanently closes this class.

## F7 — closed, do not work on it

`Implementations 6` was suspected of dropping trait impls. It is **correct and
complete**: the six are `Clone`, `Debug` (from `#[derive]`), the inherent
`impl<'h> Memchr<'h>`, `Iterator`, `DoubleEndedIterator`, and `FusedIterator`,
counted directly from `src/memchr.rs`. See `DOCSRS-COMPARISON.md` §3. Preserve
this behaviour — resolving derives into impls is the valuable part.

---

## Verification — this is the part that matters

The screenshot suite must **prove** these, not assert them loosely. Existing
machinery in `workspace/gui/tests/screenshots.rs`: `FrameCheck`,
`differing_pixels`, and the `Change` enum (`First` / `Major` / `Minor` /
`KnownNoOp`, the weak variants requiring a `&'static str` reason).

- F1 needs an assertion that no two rendered search rows are textually
  identical. This is the test that would have caught it and did not exist.
- Any frame you claim changed must show a real pixel delta. Doctrine §8: two
  byte-identical frames with different captions is the exact failure this
  catches — and `.shots/memchr/` currently contains eleven such frames across
  two groups (08–15 and 17–19), all from unreachable action handlers.
- `Change::KnownNoOp` is not a way to make a red frame green. If a step still
  no-ops after your change, that is a finding to report, not a reason to relabel.

Re-run the suite and re-attach the manifest. State in your report, per finding,
whether the *pixels* now show it fixed — not whether the code looks right.
