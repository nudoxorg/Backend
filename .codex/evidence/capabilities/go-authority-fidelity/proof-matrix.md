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

## M9 Real-module corpus proof
Twenty mostly-random REAL modules (broadly-known + stdlib-adjacent + 17+
niche, including multi-module repositories) produce decoded fragments whose
facts match their sources. | Weakened: only one or two curated modules run,
or assertions check only that the pipeline did not fail. | Falsifier: for
every corpus module, run the full lifecycle (proxy locate/fetch, workspace
assembly, oracle, admission, publish, reopen, index); decode each fragment
and assert representative declarations, signatures with parameter names,
docs, references, and satisfaction edges against manually read source
truth; every mismatch becomes a Luna card row; per-module wall/oracle
timings retained. | Corpus table + raw decoded summaries + timing profiles
in evidence dir; integration test codifies the pipeline on a pinned subset.
| RED | W6

## M10 Baseline repair
`cargo test -p compiler-driver` compiles and passes. | Weakened: deleting
stale foreign tests or weakening assertions instead of mapping moved APIs.
| Falsifier: full crate test target green with no `#[ignore]`, no deleted
test, and every repaired assertion still names its law. | State: PARTIAL —
W0 checkpoint 6ecb63ae7 (branch go-fidelity/w0-repair) resolved all
owned-region compile errors and migrated the go fixture to v5 (9/19 go
tests green); 31 sibling runtime reds and 10 go reds (re-owned as W3 R0)
remain; the full-crate gate is required green at closure on the trunk.
| RED | W0 -> W3 R0 -> closure gate

## M11 Position non-carriage (brief law 8)
The v5 image carries no line/column positions when they are losslessly
derivable from the source digest + byte offsets already present. |
Weakened: an implementation starts carrying derivable positions in a new
plane or padding cell. | Falsifier: the golden-geometry tests pin the
exact header/plane layout (image.rs constants) and the differential
fact-existence test (M1) pins the exact row set per oracle Output fact —
any added position carriage changes golden bytes or adds a row class and
fails. | Geometry goldens green. | RED | W1 (landed; Terra reproduces at
closure)

## M12 No source-text scanning in projection (brief law 4)
Projection never recovers Go facts by scanning source text; source
contact is the digest binding only. | Weakened: collect() slices or
searches `source` beyond `Sha256::digest`. | Falsifier: image built for
the digest of source A; collect() invoked with source B of identical
length → exact `SourceBinding` fault retaining both operands, before any
projection work; no projection output derives from B's bytes. | Test
green in lower/go.rs cfg(test). | RED | W3
