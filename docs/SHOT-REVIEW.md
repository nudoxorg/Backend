# docs/SHOT-REVIEW.md — review findings against the first captured frames

Reviewed by the lead, against `tests/shots/fixtures/*.png` (all four opened and looked
at). These are defects in the *evidence*, not in the app, and they must be fixed
before any frame is cited as proof of anything.

## R1 — `expect_change > 0` is far too lenient (harness defect)

`Stage::shoot` currently asserts only `changed > 0` when the caller claims the
previous step altered the screen. A blinking text caret changes ~12 000 pixels of
a 5 184 000-pixel frame — 0.24% — and passes. So the guard fires only on a frame
that is *pixel-perfect* identical, which is almost never true once anything
animates.

**Fix:** make the claim quantitative. `expect_change` should take a minimum
fraction, and the common case should demand a *material* repaint (suggest ≥1% of
pixels for a state change like opening an overlay or switching tabs). Where a
step legitimately changes only a small region, the call site should say so
explicitly with a smaller threshold and a comment — that way the threshold
documents the expectation instead of hiding it.

## R2 — frames 01 and 02 are byte-identical (scenario defect)

`01-shell-boot` and `02-corpus-loaded` have the same MD5. The caption "Corpus
loaded and answering queries" is not supported by the frame: the fixture corpus
is resident before the first frame paints, so there is no "before" state to
contrast with.

**Fix:** either delete frame 02, or make it show something the boot frame cannot
— e.g. capture it in *package* mode, where the real producer genuinely takes tens
of seconds and the boot frame really does show an empty corpus. The second option
is much better: the loading state is real and worth showing, it just isn't real
for fixtures.

## R3 — frames 03 and 04 are near-identical (scenario defect)

`03-omni-search-open` is captioned "cmd-K opens the omni-search overlay", but the
frame already shows the query `Point` typed and both results rendered. The cause
is in the scenario, not the app: the `wait_until` that polls for corpus readiness
issues `set_input(query)` on every poll, so by the time the overlay is opened the
store already holds the query and its hits.

**Fix:** clear the search input after the readiness probe and before opening the
overlay, so the "just opened, empty" state is real. Better still, make the
readiness probe use a throwaway query distinct from the one being demonstrated,
then reset it.

## R4 — panel titles are illegible in the dark theme (product defect)

"Project" and "Editor" render at near-background contrast in every frame. No
overlay is open in `01-shell-boot`, so this is not a modal scrim. Every other
label in the same chrome (`PACKAGES`, the package name, `23 symbols`, `Jobs`,
`Logs`, `1 package`, `idle`) is legible.

Being tracked by the contrast track; recorded here because it is visible in
*every* frame and therefore contaminates every screenshot until fixed.

## R5 — no real crate appears in any frame yet (coverage gap)

All four frames are against `nudox-fixture-rich`, a 23-symbol synthetic fixture.
The goal is explicitly "on a real generic package end to end". `memchr-2.8.3`
lowers to 11 329 IR entries in ~22 s and is staged at `result/memchr-2.8.3`,
along with `memchr-2.7.6` and `memchr-2.8.0` for a real three-generation lineage.

Until a frame shows real `memchr` API, the suite demonstrates that the shell
works, not that the product works.

---

**Standing rule this implies:** a caption is a claim. If the pixels do not show
what the caption says, the caption is a lie and the frame is worse than no frame
— it will be believed once, and then nothing else in the report will be.
