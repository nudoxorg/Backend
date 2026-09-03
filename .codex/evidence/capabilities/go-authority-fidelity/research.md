# Research journal: go-authority-fidelity

Every row names the matrix decision it informs. Sources: repository facts,
vendored producer/reader source, module-proxy protocol documentation,
local experiments.

## R1 — Wire delta v4 -> v5 (informs M1, M2)

Repository facts: the uncommitted working tree already carries image v4
(producer `oracle/image.go` 1297 LOC, reader `image.rs` 2072 LOC) with
planes: declarations 56B, types 52B, methods 64B, type-params 16B, members
40B, docs 16B, references 48B, constraints 28B, satisfactions 20B, children
8B, atom plane; header 124B with 8 reserved bytes at 116..124.

v4 drops these oracle Output facts (verified by reading
`oracle/serialize.go` definitions against `image.go` marshal):

1. `Module{path, dir, go_version, version}` — nothing carries it.
2. `Package{import_path, name, files}` — only the doc survives
   (DocOwner::Package); name and files are lost.
3. `Param{name, pos}` for every func parameter/result — the type plane
   carries types only; lower fabricates `_` carriers (lower/go.rs UNNAMED).
4. `Type.all_methods` (complete post-embedding interface method set) —
   embeddeds are carried but re-expansion is impossible locally when an
   embedded interface is foreign (named refs fold to name+pkg, no body),
   so the fact is NOT locally derivable and must be carried.

Decision: v5 adds four planes — module (1 row), packages (one row per
package), signature parameters (owner = func type row, ordinal, name,
offset), interface method sets (owner = interface type row, name, sig root,
pkg) — and consumes the 8 reserved header bytes for two extra plane counts,
bumping version 4 -> 5 and the digest domain. Reader laws: new planes tile
canonically (owner-contiguous runs, sorted), reserved cells zero, atom
ranges bound, digest over header+body as today. Line/col is NOT carried:
derivable from source bytes + offsets (source digest binds the source).
Decision status: frozen into card W1.

## R2 — Multi-package projection defect (informs M5)

`Projector::new` (lower/go.rs) takes `image.declaration(0).package` as THE
local package. The producer flattens every package of the module into one
image (`buildAuthorityPlan` loops `output.Packages`). A reference inside a
non-first package with empty `target_package` (same-package call) fails
`lookup` (names map is global across packages) -> OrphanTarget terminal.
Reproduced by reading the code path; M5's falsifier makes it executable.
Decision: per-declaration package resolution; names keyed by (package,
name). Frozen into card W3.

## R3 — Constant/module hosting in the lane (informs M6)

`GoFacts` (compiler/ir/semantic.rs:1292) = {signature, type_parameters,
fields, method_set, build_constraints}. No cell hosts an exact constant
value, const group, iota, module metadata, or package row. The fact lane
has atom lists (build_constraints precedent) but GoFacts has no value
atom cell; adding one is a compiler/ir surface change. Per the parent:
compiler/ir is escalation-owned. AUTHORITY_FORK prepared for Sol with a
precise proposal: extend GoFacts with `constants: AtomListId`-style cell(s)
or approve image-only retention for v5. Interim: image carries all facts
(M1); projection keeps today's honest image-only classification; no
silent drops beyond the already-documented ones.

## R4 — Module proxy protocol (informs M8)

GOPROXY protocol (proxy.golang.org live-checked: HTTP 200 on
`github.com/pkg/errors/@v/list`): GET `$GOPROXY/<module>/@v/list`,
`/@v/<version>.info` (JSON), `/@v/<version>.mod`, `/@v/<version>.zip`
(zip layout `module@version/<path>`). `file://` and `off` are legal
GOPROXY values; a checked-in real module zip with `GOPROXY=off` +
`GONOSUMCHECK`/`GOFLAGS=-mod=mod` + `GONOSUMDB`/`GONOSUMCHECK`-equivalent
(`GONOSUMDB` is legacy; modern is `GOPRIVATE`/`GONOSUMDB` — the exact env
set is `GOPROXY=off GONOSUMDB=* GOSUMDB=off GOPATH=<ws>`) makes the
journey hermetic while using REAL module bytes. Decision: W5 client speaks
the HTTP protocol via ureq (workspace dep) and accepts a caller-supplied
proxy base; the integration test defaults to the real proxy with a
checked-in zip fallback path exercised explicitly. Frozen into card W5.

## R5 — Environment constraints (informs M8, M9, gates)

This host: macOS 15.7.4 arm64, no `go` on PATH, network live,
`~/go/pkg/mod` cache populated by earlier sessions. Terra installed Go
1.27.1 (go.dev darwin-arm64) under ~/nudox-tools for evidence; oracle
needs go >= 1.23 (go.mod) + golang.org/x/tools v0.30.0 (network fetch on
first build). Oracle runs via `NUDOX_GO_ORACLE_BIN` override (bounded
subprocess, process group, output limit, deadline — all existing). Tests
that need the oracle fail typed-unavailable without it
(`missing_oracle_is_typed_unavailable_without_fallback` precedent).

## R6 — Rendering path facts (informs M7)

render.rs displays operate on `Ir` items (ItemView: name/kind/visibility/
semantic_type/members/docs). `FactSet::build_ir` (driver/lower.rs:780)
currently emits flat items: visibility Unknown, members &[], docs &[],
source None — so struct rendering through displays cannot show fields or
docs today. Enrichment options: (a) build_ir reads the admitted lanes
(docs lane, GoFacts fields) and attaches members/parents/docs — a shared
lower.rs edit enumerated across all language consumers; (b) renderer reads
the compact fragment directly — new render surface in compiler/ir
(escalation lane). Decision: option (a), Go-driven but lane-generic,
with consumer enumeration in the W4 card; requires W3's facts first.
Status: dependency W4 <- W3 recorded.

## R7 — Saturation

Two independent passes over producer (image.go/serialize.go), reader
(image.rs), lower (go.rs), and lane (compiler/ir GoFacts) produced no new
wire facts after R1-R3; the v5 plane set is closed unless the Sol fork
(M6) or a real-module mismatch (M9) reopens it.

## R8 — Stale-artifact hazard on this host (informs all gates)

After cherry-picked commits landed (the three v5 wire commits appear twice
in the reflog — original plus cherry-pick), `cargo` did not invalidate
`compiler-languages-go`: the linked rlib still rejected version 5
(`Header(Version { found: 5 })`) while the tree said `VERSION: u16 = 5`.
One `touch` of the source rebuilt the rlib and the gate went green. Every
gate sequence in this capability therefore begins with the affected
package sources touched or `cargo clean -p <pkg>` when a freshly landed
commit misbehaves against a green reading. Recorded as an environment
fact, not a code law.

## R9 — Mandate delta from the parent (revises M8, M9)

The parent mandate (this session) fixes the corpus at 20 mostly-random
REAL modules (broadly-known + stdlib-adjacent + 17+ niche including
multi-module repositories), requires perf profiles, deep generated-IR
review vs source truth, and integration tests codifying the pipeline. It
also restates "project EVERYTHING ... constant values" (M6) and adds
rendering via compiler/ir render.rs with golden tests (M7). M9 rewritten;
M6 stays the only open fork with a precise proposal pending (GoFacts wire
WIDTH 28 -> 32 hosting a constant-value atom cell, or an equivalent
additive revision decided by Sol). Package rows are projectable within
driver authority as Module entities (namespace boundary kind) if W3's
design supports it; module-row metadata (go directive, version) has no
lane cell and stays image-only unless M6's fork resolves otherwise.

## R10 — W1 outcome and gate state at re-freeze

W1 landed as commits a358683ce -> 4201c2744 -> 98711b363 -> 3178de64c
(v5 wire + reader + protocol tests + fixture). Gates this session:
`cargo test -p compiler-languages-go` 28/28 green; `cargo test
-p compiler-driver --test go_image` green (the v1-fixture inherited red
was already repaired by W1's fixture update; the test's error-erasure
`Err(_) => Compile` was replaced with cause retention during diagnosis).
`cargo test -p compiler-driver --lib` still blocked by stale `#[cfg(test)]`
modules in lower/{clang,csharp,rust,typescript,python}.rs (40 errors,
E0277/E0422/E0308/E0004/E0106/E0599 families) — exactly M10/W0. go.rs's
own cfg(test) module compiles (warnings only).
