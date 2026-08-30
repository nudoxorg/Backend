# P6 Sol cross-cutting review

## Frozen specimen

| fact | value |
| --- | --- |
| source candidate | `44c22154fd5238e4769562590420371979306050` |
| Terra closure candidate | `c50a1d161017ee4d2bb28cbe1640755c57587844` |
| Terra branch | `codex/prototype-protocol-registry-dispatch` |
| Sol review branch | `codex/prototype-protocol-registry-dispatch-sol-review` |
| Sol review worktree | `/private/tmp/nudox-p6-sol-review/workspace2` |
| shared-branch merge | none |

The reviewed terminal is admission or rejection of a registry/static-dispatch experiment. It is not
permission to add a consumer, macro, dependency, public API, or permanent protocol code.

## Findings

No blocker or major finding remains in the Terra closure packet. The rejection is the smallest result
consistent with the frozen call graph: one qualifying foundation conversion path is not two real
production consumers.

The normally formatted manual conversions and matches remain the safe standard-library control. A
macro or delegation crate cannot delete duplicated production proof when the index and compiler
invocations exist only in fixtures.

## Independent reproduction

| attack | Sol evidence | result |
| --- | --- | --- |
| compiler caller inventory | non-test search for `FullRegistry::dispatch` and `.dispatch(` | zero invocations |
| index caller inventory | non-test search for `SegmentFamily::try_from` and family variants | zero invocations |
| foundation caller inventory | `parse_header` forwards `schema_wire` to `SchemaId::try_from` | one qualifying path |
| input-removal mutant | forced `SchemaId::try_from` to receive constant `SchemaId::Frame` | expected failure: 31 passed, 3 failed |
| raw-tag mutation | failed exact schema-provenance and header-mutation assertions | trusted conversion materially consumes wire input |
| simpler representation | retained explicit conversion/matches | no smaller candidate than the manual control |
| raw authority or owner mixing | no P6 witness, owner, authority type, or public field was added | not applicable; no negative API claim introduced |
| phase × method totality | no P6 stateful public type or method was added | not applicable |
| resource controls | no production, test, lab, dependency, allocation, copy, or codegen change | zero delta; candidate measurements remain unclaimed |

The disposable Sol mutant lived at `/private/tmp/nudox-p6-sol-mutant/workspace2`, detached from the
Terra closure commit. It changed only `crates/nudox-view/src/validate/header.rs` and was removed after
the focused failing test completed. Neither prototype branch was mutated by the attack.

## Gates

Two clean full gates passed independently in the Sol review worktree:

```text
RUSTC_WRAPPER= cargo fmt --check
RUSTC_WRAPPER= cargo test --workspace --all-targets
git diff --check
git status --porcelain
```

The nested compiler and index workspaces also passed `cargo test --locked --workspace --all-targets`.
The compiler result is two fixture tests; the index result is four vocabulary tests. Neither changes
the production-caller count.

## Evidence and resource ledger

- Production/test/lab LOC delta from the source candidate: `0/0/0`.
- Public API, manifest, dependency, allocation, copy, dynamic dispatch, panic, unsafe, and SIMD delta:
  zero.
- Invocation/expansion, macro diagnostics, cross-crate IR/assembly, compile time, monomorphized text,
  and candidate layout/allocation evidence: `UNVERIFIED` because no candidate was admitted.
- Terra task/model proof, calibration churn, candidate table, tripwires, and raw mutant summaries are
  retained in `CLOSURE_REVIEW.md`, `REPRODUCTION.md`, and `MUTANT_RESULTS.md`.

## Future integration boundary

First close the typed identity grammar and land two independently shipped non-test callers that
forward real runtime input through the same closed registry or dispatch need. Then repeat the frozen
call-graph inventory and input-removal mutants before comparing manual code, a narrow private
`macro_rules!`, or a maintained crate. Permanent codes, a proc macro, and tagless/GADT machinery remain
outside that card unless their separate admission laws are met.

## Verdict

`REJECT PROTOTYPE`. Retain the manual control. This branch is evidence only and must not be merged into
`orchestra-shared` as an implementation.
