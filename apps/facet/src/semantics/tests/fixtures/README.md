The semantic tests' pinned world. Made from the prototype's world.json at a
fixed git revision by `node apps/facet/src/semantics/tests/fixtures/trim.mjs <rev>`
(run from the repository root): present, serde_core and serde_json whole, plus
everything one relation from the four target pages, with sparse copies of the
sources In use mines (callers' lines only; other lines blank so numbers hold).
Then regenerate the golden: `node apps/facet/src/semantics/tests/golden.mjs
apps/facet/src/semantics/tests/fixtures/world.json apps/facet/src/semantics/tests/relations.golden`.
Pinned at bfff25bd9.
