# Proof matrix: go-authority-fidelity

law | weakened implementation | falsifier | required evidence | state | owner

## M1 Wire completeness (image v5)
The v5 image carries every oracle Output fact listed in brief law 2. |
Weakened: producer silently drops module/package/param-name/interface-method-
set planes while bumping the version. | Falsifier: differential test on a
real module — decode oracle JSON `Output` and the produced v5 image; for
every JSON fact assert the corresponding image row exists (module cells,
package rows, every func-row parameter name, every interface's
post-embedding method set). | Round-trip test green on the real fixture
module + golden.json transcript. | RED | W1

## M2 Reader validation strength
The v5 reader rejects every malformed plane before lending rows. | Weakened:
reader accepts a v5 header but skips new-plane tiling/sort/bounds laws.
| Falsifier: hostile mutation battery — for each new plane, mutate one
structural class (count, sort order, tiling, reserved cell, atom range,
digest) and assert the exact typed error variant. | Mutation table green;
zero mutations accepted. | RED | W1

## M3 Parameter-fact projection
Parameter carrier facts carry exact source names. | Weakened: lower keeps
blank `_` carriers ignoring the new plane. | Falsifier: lower an image whose
func row carries named parameters; decode carrier facts; assert exact
names in order for params and results; assert variadic last-param name.
| Unit tests green in lower/go.rs + decoded fragment assertion. | RED | W3

## M4 Recursive + mutual-recursion fidelity
Self-nominals and mutually recursive types project without Unknown folding.
| Weakened: projection folds recursive/mutual references to typed Unknowns.
| Falsifier: image fixture with `type A struct{ b B }; type B struct{ a
*A }` plus self-recursive `type L struct{ next *L }`; decode facts; assert
Nominal locals, no Unknown rows. | Green. | RED | W3

## M5 Multi-package resolution
Local resolution is per-declaration package, not first-package-global.
| Weakened: a file whose package is not the image's first package resolves
same-package targets as foreign or orphans. | Falsifier: image with two
packages (A first, B second); reference inside B to a B function with empty
target_package must resolve Local; a satisfaction target in B resolves
Local. | Green; today this faults OrphanTarget (reproduce first). | RED | W3

## M6 Constant-value + module-metadata hosting
Exact constant values, const-group identity, iota, module metadata, and
package rows are projectable into the fragment lane or explicitly escalated.
| Weakened: silently dropped at projection with no lane cell and no fork.
| Evidence: Sol fork decision recorded in journal + index; projection
tests match the decided host. | AUTHORITY_FORK (open) | Terra

## M7 Struct rendering
Go structs/interfaces/functions render through compiler/ir render displays.
| Weakened: golden test renders flat name-only items and calls it struct
rendering. | Falsifier: golden file pins `type T struct { Field K `tag`
... }`-shaped text with fields, docs, and signatures through
SignatureDisplay/TypeDisplay/DocsDisplay; mutation of a field or doc
changes the golden. | Golden render tests green. | RED | W4

## M8 PURL lifecycle
`golang:module@ver` resolves, fetches, assembles a workspace, runs the
oracle, publishes fragments, reopens, and indexes; old fragments keep
validating. | Weakened: journey stubs the proxy or skips reopen/index.
| Falsifier: end-to-end test on a real module zip (checked-in, version
pinned): PURL -> fragment ids -> publish -> reopen -> index projection;
then reopen a pre-v5 fragment (fixture committed at baseline format) and
assert it still validates. | Green with raw output retained. | RED | W5

## M9 Real-module decoded-output analysis
Real modules (niche + broad) produce decoded facts matching their sources.
| Weakened: only the synthetic fixture module runs. | Falsifier: run the
full pipeline on (a) a small real util module, (b) a real broadly-used
module; decode fragments; assert representative decoded outputs (declarations,
docs, satisfaction edges, references) against known source facts; every
mismatch becomes a Luna card row. | Raw decoded summaries in evidence dir.
| RED | W6

## M10 Baseline repair
`cargo test -p compiler-driver` compiles and passes. | Weakened: deleting
stale foreign tests or weakening assertions instead of mapping moved APIs.
| Falsifier: full crate test target green with no `#[ignore]`, no deleted
test, and every repaired assertion still names its law. | Crate gates green.
| RED | W0
