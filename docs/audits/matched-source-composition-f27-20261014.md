# Matched source composition on canonical f27f30e90a

This is a source-only composition, not a native acceptance receipt. Metadata,
Cargo builds/checks/tests, binaries, owner/index work, and remote workloads
are **UNRUN**. Root alone reviews, publishes, and merges this candidate.

## Exact source inputs and composition

Canonical was fetched as `f27f30e90a776d3e9aacfd212c74cef02c873899`.
Reviewed cumulative inputs, including their ancestors rather than only their
last commits:

| Slice | Reviewed head | Merge base with canonical | Changed paths |
| --- | --- | --- | ---: |
| MCP/CLI/locald paging | `b0399c441717aa5ba2db824ba23756e2b145f471` | `611e96d3c1b3756427b6aa7d4f0ec64e15096f53` | 96 |
| Python installed capture/profile | `62e9af8315a88d0377534c8395b205e707349374` | `041b8c6e6548da20e7d7bbec0f8783c77a635afe` | 38 |
| TypeScript binding/authority lane | `d7d2380f9a99dcb40ff62abb7ac58aa8aa063225` | `a52faf72a90f559111fe1a2299242b6cac2d8098` | 24 |

Each input independently merged cleanly with canonical. Local MCP composition
`1452dc28c4129cfb1a434d1543c99254abd2a3d6` has tree
`860491600c1fe698f92919b98f1a33281343fa31`. Adding Python produced
`f26d4bf5bdec25dbedf5cd95f72324698251e96b`, tree
`9762993ba9b4c6d01dc87282605417a93779a984`, with no manual resolutions.

MCP/Python overlap only in Cargo.lock and local-service commands adapter/index.
MCP and TypeScript have no changed-path intersection. Python/TypeScript overlap
in engine compiler/mod/relation and local-service view_build mod/query/semantic/
structural. Of these, only engine builtin/relation.rs conflicted. All other
paths use Git's three-way composition of the reviewed deltas and current
canonical. No source head was substituted wholesale over current canonical.

## Persistence conflict and authorized resolution

The two unpublished source candidates independently used `PSRF`/version 15:
Python for a literal supported minor before source-identity presence, TypeScript
for exact binding anchors and lexical targets with a binding identity after
every declaration's facts. Their writer/reader contracts were incompatible.
The four conflict hunks concerned tag ownership, minimal tag selection, facts/
binding encoding, and file/membership decoder kinds. Automatically merged
`version >= 15` declaration predicates would also have misread Python records.

Fetched canonical supports only versions 2 through 14 and has no `PSRF` tag;
the TypeScript binding atom is not a canonical ancestor. The reviewed cutover
audit explicitly records native compilation/regression/indexing as UNRUN. No
issued TypeScript-PSRF byte receipt was supplied or found in that reviewed
evidence. This does not claim to have scanned private stores. Root explicitly
authorized one unambiguous composed layout rather than a heuristic decoder:

| Format | File header extension | Declaration extension |
| --- | --- | --- |
| Historical versions through 14 | Existing layout unchanged | Existing layout unchanged |
| `PSRF` / 15 | One literal Python minor, 10 through 14 | Facts and identity presence; no exact binding fields |
| `PSRG` / 16 | One optional profile byte: 0 absent, or 10 through 14 on Python only | Facts and identity presence, plus each binding identity; exact Binding containment admitted |

The private closed `FileRecordExtension` owns selection and the shared writer/
reader interpretation. Encoding selects the format once and derives its file
extension through the shared version mapping without rescanning declarations.
Exact bindings or Binding containment select PSRG even
when a Python profile is also present. Otherwise a literal profile selects PSRF;
without either feature the earlier minimal format stays unchanged. Project and
membership kinds reject both file-only versions; file kinds reject reserved
project versions 12 through 14. No greater-than-or-equal-to-15 test grants
binding authority to PSRF.

Source-present tests retain the original literal PSRF/profile/compaction control
and add: all 48 Python profile/binding/identity/fact combinations; TypeScript
bindings without a Python profile; independently spelled literal PSRF bytes;
exact UTF-8 anchors and lexical parent retention; unknown/foreign profiles;
every truncated prefix and trailing bytes; malformed exact identities and
ranges; missing/self child parentage; PSRF's lack of binding authority; and
historical project/membership/file-kind guards. A deterministic 2,048-declaration
profile-plus-binding/facts control refuses the full oversized row, then checks
final PSRG capacity, exact retained parent/child anchors, profile/source identity,
explicit lost-fact/truncation evidence, and canonical restart bytes through
shedding. Native execution is UNRUN.

## Preserved current canonical repairs

PR 246 and c544ddda's complete index_overlap.rs postimage remain byte-identical
to fetched canonical. The package compiler failure and native program forest
files from 99c609d82c also remain exact. Entire cf2ba693 and 99c609d82c repair
patches remain reverse-applicable to the composed source, including the
presentation files and host/authority.rs that receive independent additions.
Other exact preserved postimages include compile tests, builtin semantic relation,
local-service query/local.rs, and store journal allocation controls.

The clean MCP/Python candidate changes only the reviewed slice path union;
18,235 canonical paths outside that delta were compared as Git blobs and are
unchanged. The final composition repeats that comparison, records every
changed-path overlap, and checks independent TypeScript postimages against the
initial staged merge. Only relation.rs and this audit are manual additions to
the automatic TypeScript merge. No new lint suppression or budget adjustment
is part of the resolution. The main checkout's user Nix edit is untouched.

## Staged native acceptance plan, all UNRUN

1. Obtain fresh owned ILO admission with the required actual disk/CI reserve
   and memory capacity. Record the exact candidate tree, Cargo.lock, managed
   permit, host/context and cache identity. First run
   `cargo metadata --locked --offline --no-deps --format-version 1`; preserve
   a refusal/missing-input result instead of rewriting the lockfile or guessing
   dependency selection. This plan grants no workload admission itself.
2. Under the admitted Cargo capacity, run the library/client/present typed
   codec and projection controls, then engine `builtin::relation::tests` and
   `builtin::file_facts_relation::tests`. Include the original Python literal
   profile test, new extension cross-product/corruption controls, and the
   complete inline/paged exact-binding controls. Capture actual executed,
   ignored and skipped counts; source presence is not a passing test receipt.
3. Run targeted MCP named-call, record-budget, graph-budget, semantic-source
   and catalog/schema controls, plus CLI graph JSON fallback/recovery controls.
   Then run engine/frontend Python captured-profile/environment and TypeScript
   binding/authority-lane controls. Supply genuinely admitted interpreter,
   stdlib, SDK and pinned Node/TypeScript inputs for native fixtures; a fixture
   that exits because these are absent is not native acceptance. Retain each
   full failure and do not replace it with a broad passing count.
4. After targeted suites pass, build CLI, MCP and locald together from this
   same locked candidate (`cargo build --locked --bin backend-cli --bin
   backend-mcp --bin backend-locald`), along with every required compiler/lockd
   companion selected by the runtime. Record source/tree, binary hashes and
   effective build/capacity settings before any matched process run.
5. Separately admit sequential product acceptance: exact MCP wire envelopes,
   complete paging/continuations and catalog counts; mixed CLI/MCP saved index
   overlap and cancellation; stopped-owner cold status; Python installed
   capture/cold reopen; then TypeScript exact repeat, selected-source edit,
   dependency declaration edit and options edit against cold controls. Retain
   publication scope, refusal and freshness evidence. Guava/ASP/GymBroApp runs
   remain separate admitted experiments with original compiler limits, never
   overlapping large compiles or treating a partial/refused result as complete.
