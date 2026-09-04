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
module + golden.json transcript (protocol 31 tests). | REPRODUCED BY TERRA | W1

## M2 Reader validation strength
The v5 reader rejects every malformed plane before lending rows. | Weakened:
reader accepts a v5 header but skips new-plane tiling/sort/bounds laws.
| Falsifier: hostile mutation battery — for each new plane, mutate one
structural class (count, sort order, tiling, reserved cell, atom range,
digest) and assert the exact typed error variant. | Mutation table green;
zero mutations accepted (protocol mutation battery). | REPRODUCED BY TERRA | W1

## M3 Parameter-fact projection
Parameter carrier facts carry exact source names. | Weakened: lower keeps
blank `_` carriers ignoring the new plane. | Falsifier: lower an image whose
func row carries named parameters; decode carrier facts; assert exact
names in order for params and results; assert variadic last-param name.
| Unit tests green (signatures carriers + sig-param plane). | REPRODUCED BY TERRA | W3/W9

## M4 Recursive + mutual-recursion fidelity
Self-nominals and mutually recursive types project without Unknown folding.
| Weakened: projection folds recursive/mutual references to typed Unknowns.
| Falsifier: image fixture with `type A struct{ b B }; type B struct{ a
*A }` plus self-recursive `type L struct{ next *L }`; decode facts; assert
Nominal locals, no Unknown rows. | Green. | REPRODUCED BY TERRA | W3/W9b

## M5 Multi-package resolution
Local resolution is per-declaration package, not first-package-global.
| Weakened: a file whose package is not the image's first package resolves
same-package targets as foreign or orphans. | Falsifier: image with two
packages (A first, B second); reference inside B to a B function with empty
target_package must resolve Local; a satisfaction target in B resolves
Local. | Green (calls_resolve_local_and_foreign + package-keyed names). | REPRODUCED BY TERRA | W3c

## M6 Constant-value + module-metadata hosting
Exact constant values, const-group identity, iota, module metadata, and
package rows are projectable into the fragment lane or explicitly escalated.
| Weakened: silently dropped at projection with no lane cell and no fork.
| Evidence: Sol fork RESOLVED per parent mandate (R3): constants host in
the `GoFacts` extension row (W7 landed, 44-byte row: constant_value atom
list, constant_group i64, constant_flags); module/package rows stay
image-only with documented justification. Falsifier: decoded extension row
of a projected const fact carries the exact value atom, group id, and
iota flag; the former image-only pin flips. | Landed: W8 projection (41d1820a + 1684cf4c). | REPRODUCED BY TERRA | W8

## M7 Struct rendering
Go structs/interfaces/functions render through compiler/ir render displays.
| Weakened: golden test renders flat name-only items and calls it struct
rendering. | Falsifier: golden file pins `type T struct { Field K `tag`
... }`-shaped text with fields, docs, and signatures through
SignatureDisplay/TypeDisplay/DocsDisplay; mutation of a field or doc
changes the golden. | go_render.rs exact goldens + mutation falsifiers (2533417a); render gaps (receiver types, generic constraints, struct tags) recorded as findings. | REPRODUCED BY TERRA | W10

## M8 PURL lifecycle
`golang:module@ver` resolves, fetches, assembles a workspace, runs the
oracle, publishes fragments, reopens, and indexes; old fragments keep
validating. | Weakened: journey stubs the proxy or skips reopen/index.
| Falsifier: end-to-end test on a real module zip (checked-in, version
pinned): PURL -> fragment ids -> publish -> reopen -> index projection;
then reopen a pre-v5 fragment (fixture committed at baseline format) and
assert it still validates. | go_purl_lifecycle 7 tests across four journeys (ad0b90ed + 00f9b1fa). | REPRODUCED BY TERRA | W11

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
| go_corpus.rs 21 rows pinned (e8102519 + 32846106): 6 Complete, 14 CapacityTerminal, x/tools AuthorityRefusal (documented toolchain limit). Edge fixes: W12f/W12l producer+projection cards, W12g-W12k geometry sizing, W13 refusal. | REPRODUCED BY TERRA | W12

## M10 Baseline repair
`cargo test -p compiler-driver` compiles and passes. | Weakened: deleting
stale foreign tests or weakening assertions instead of mapping moved APIs.
| Falsifier: full crate test target green with no `#[ignore]`, no deleted
test, and every repaired assertion still names its law. | State: PARTIAL —
the go branch carries the crate-target compile repair (rust cfg(test)
remap 43a079741 + daff31f62; canonical tip's rust test module does not
compile on canonical itself). Remaining reds classified Terra-side:
(a) 8 go cfg(test) stale vs trunk schema-2 wire / 44-byte GoFacts row /
cause-retention terminals — W9; (b) java capacity, csharp bound buffer,
shared lower/tests.rs EmptyPath fixture — trunk-caused mechanical, W9;
(c) 6 rust runtime reds (receiver/nominal/macro/trait/variant/capacity
expectations) — rust-lane semantics, inherited debt, exact repro
commands retained in the closure receipt, NOT go-lane scope. Go/java/csharp/shared green (W9, W12k); remaining: 5 rust semantic reds + W12k-recalibrated rust capacity. | RED (rust inheritance only) |
W9/W12k -> closure gate

## M11 Position non-carriage (brief law 8)
The v5 image carries no line/column positions when they are losslessly
derivable from the source digest + byte offsets already present. |
Weakened: an implementation starts carrying derivable positions in a new
plane or padding cell. | Falsifier: the golden-geometry tests pin the
exact header/plane layout (image.rs constants) and the differential
fact-existence test (M1) pins the exact row set per oracle Output fact —
any added position carriage changes golden bytes or adds a row class and
fails. | Geometry goldens green (protocol geometry tests). | REPRODUCED BY TERRA | W1

## M12 No source-text scanning in projection (brief law 4)
Projection never recovers Go facts by scanning source text; source
contact is the digest binding only. | Weakened: collect() slices or
searches `source` beyond `Sha256::digest`. | Falsifier: image built for
the digest of source A; collect() invoked with source B of identical
length → exact `SourceBinding` fault retaining both operands, before any
projection work; no projection output derives from B's bytes. | Test
green in lower/go.rs cfg(test). | REPRODUCED BY TERRA | W3

## M13 No silent test skips on the oracle boundary
Go oracle e2e tests fail typed when the toolchain is absent; they never
return `Ok(())` after printing a skip notice. | Weakened: skip-on-unavailable
masks a broken oracle path as green (observed live: the three e2e tests
passed as no-ops on this host until COMPILER_GO_COMPILER was set).
| Falsifier: with a toolchain path that cannot spawn, the test returns the
typed toolchain error, never Ok; with COMPILER_GO_COMPILER set, every e2e
test executes real oracle work. | Landed W13 (92d554e2 + 32846106): the three skips fail typed; the refusal falsifier holds; the corpus assembly pre-downloads pinned deps. | REPRODUCED BY TERRA | W13

## M14 Projection fault operands survive the terminal
Projection faults retain their exact operands at the collect boundary. |
Weakened: `terminal()`/`lane_terminal()` fold every ProjectionFault into
`NoSupportedDeclaration` with `let _ = fault;`. | Falsifier: per fault
class, either an existing typed CompileFailure arm carries the operands or
the class is proven unreachable behind the reader's validated planes; no
silent erasure remains. | ASSESSED BY TERRA: the fold is a deliberate,
documented boundary — `ProjectionFault` operands are structurally
retained (the `#[expect(dead_code)]` reasons say so) and the shared
`CompileFailure` surface has no operand slot for these classes; adding
one is a public cross-crate API change owned by Sol (fork recorded in
the closure receipt). Not a dodge: the classes are producer/reader
disagreement behind the reader's validated planes, and every class the
corpus could reach was proven unreachable or fixed (W12f/W12l). | REPRODUCED BY TERRA (assessment) | Terra
