# Wave B4 immutable-index frozen Phase 0 card

## First observable capability and terminal

Freeze one representation-open journey: sealed D1/D2 produce immutable S1/S2; typed exact lookup,
prefix scan, lexical search name pinned snapshot and declared typed segment IDs; terminal separates
complete zero results from partial absence; independent compaction produces S3 equivalent to S2 without
changing S1. The literal test seam is in `brief.md`.

## Exact allowed paths and baseline

Allowed writes are only `workspace2/.codex/evidence/capabilities/wave-b4-immutable-index/**`. Baseline
is `f2565a9fb33af06053bd19721d4dc2753ec09ed5` tree `8477cb2ab93763c468d5431740cfb1d5e4c4cf82` in
`/Users/mileswirht/.config/codex/worktrees/f7b8/backend` on `codex/wave-b4-immutable-index`.
Production source, manifests, locks, fixtures, shipping tests, roadmaps, and Downloads checkout are
prohibited.

## Preserved facts and prohibited adjacent behavior

Preserve current typed IDs, canonical-byte truth, immutable typed segments, pinned snapshots,
deterministic rows/scores/order/provenance, exact partial absence, independent compaction equivalence,
advisory rendezvous, tier-invariant IDs, and disabled-probe laziness.

Prohibit representation/API implementation, universal DTOs, dynamic schema, backend posting-loop
matches, consensus, `dyn`, boxed streams, serde, caches, Tantivy schema/truth, Qdrant/Trustfall,
compiler reliance, dependencies, unsafe/SIMD, Cargo edits, and production-writing Luna work.

## Expected public surface and explicitly forbidden surface

Expected semantic surface: three closed typed operations, `ExactLookup`, `PrefixScan`, and
`LexicalSearch`, each explicitly passed one `IndexSnapshotId`, its declared typed segment IDs, and
validated limits/credits, yielding ordered rows and an exact complete/partial terminal. A route or
tier is never a semantic parameter. A later B4-13 adapter may consume those typed lexical facts for a
bounded differential comparison only.

Forbidden surface: `Query` trait object, `SearchRequest` bag, `Document` DTO, backend enum, location
as truth, implicit latest-head lookup, untyped segment ID, raw score float, cache policy, future
relation/usage/vector type, compiler invocation, or transport/runtime handle in core.

## Evidence rows, falsifiers, hard controls, and stop triggers

| Matrix rows | Falsifier | Hard control | Stop trigger |
| --- | --- | --- | --- |
| B4-01/B4-04 | S1/S2/S3 update-delete-tombstone | 2 snapshots, compacted replacement, 3 result limit | query observes head other than argument |
| B4-02/B4-03 | mixed family/every truncation/mutation | one validator/borrow owner, typed family | untyped/repeated raw decode or split layout authority |
| B4-05/B4-06 | permutation/tie/corpus-scan mutant | 4 segments, 4 ranges, 3 rows, 12 merge candidates | unbounded scan, backend loop branch, raw float truth |
| B4-07/B4-09 | E2 absent/stale route/tier move | request names 4 IDs; one retry; deterministic owners only | zero-hit/fallback/latest snapshot/node truth or claiming durable file/object proof |
| B4-10 | independent replacement | S3 records explicit E1/E2/L1/L2 input set, distinct builder receipt, equivalence trace, then atomic head | local repack, S3 alias, or unverified head |
| B4-11/B4-12 | nonempty allocation/probe/compiler control | raw ledger before performance/dependency claim | heap/dependency/probe/compiler escape hatch |
| B4-13 | Tantivy differential or two-attempt block | nested lexical comparison only; each block attempt records exact Nix argv, lock/metadata identity, raw stderr, and distinct failure class | Tantivy schema/head/document API becomes truth or no independently inspectable block receipt |

## Budgets and reserve

Production budget is zero lines, Cargo changes, fixtures, shipping tests, dependencies, allocations,
and executable representation. The first red control has four segments/four ranges/three rows/twelve
merge candidates; these bound evidence workload only. Reserve one independent reviewer; no
production-writing Luna is dispatched.

## Exact focused commands

```text
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" fmt --manifest-path workspace2/planes/index/Cargo.toml --all -- --check'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets --no-fail-fast'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --doc --no-fail-fast'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" clippy --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets -- -D warnings'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" doc --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --no-deps'
```

Ambient `cargo` is forbidden because it can select a nightly compiler and inherit the shared sccache
wrapper; the commands must choose `NUDOX_STABLE_TOOLCHAIN` and clear `RUSTC_WRAPPER`.

## Phase 0 acceptance and anti-self-certification

Phase 0 may close only with this card/artifacts structurally checked, clean pre/post source ledger,
actual manager/reader/misreader/reviewer receipts, raw outputs, a source-isolated review, and exact
toolchain receipt. It cannot mark any B4 row `EVIDENCE_BLOCKED`, `PROVED BY WORKER`, or
`REPRODUCED BY TERRA`; it cannot call the seam implemented, make a release/dependency claim, or use
the evidence wildcard for a production, Cargo, fixture, test, roadmap, symlink, or sidecar-source
write. A later implementation card must turn each listed falsifier into an executable owner-local or
ordinary top-level test, including typed terminal shape, sealed-byte input, exact missing payload,
independent compaction provenance, and deterministic permutation/work controls.

## Questions requiring parent authority

None. Durable identity grammar, dependency/unsafe/SIMD authority, or backend/transport truth would
be an authority fork.
