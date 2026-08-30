# P5 C0-IR counterexample salvage ledger

This ledger treats `916789c6`, `328f890a`, `788499a5`, and their reverts as rejected
counterexample/design quarry only. No earlier calibration, builder, reviewer, command, or release
output is acceptance evidence for this fresh card.

| history observation | retained C0-IR technique | rejected from C0-IR | later C0-COMPILER queue |
| --- | --- | --- | --- |
| `328f890a` used a shadow type compile fixture. | The negative proof must be an external compiler process with one deliberate mismatch and one-error counting. | Any local/shadow fixture or source inclusion; it proves an unrelated type system. | None. |
| `788499a5` linked a resolved rlib and added a legal mutant. | Resolve and pass the actual exported `nudox_ir_vocab` rlib through `--extern`; mutate only `EntityId` to `TypeId` and require the diagnostic predicate to become false. | Prior test error exits, non-typed cleanup, and compiler-path repair scope. | The same causal-fixture pattern can later test static dispatch subsets. |
| `788499a5` searched a target dependency directory. | Artifact selection must be explicit, recorded, and manager-reproduced with the source digest and raw stderr. | Arbitrary artifact selection, a source compile, or acceptance without retained resolved path. | Add a separately calibrated artifact selector if two crate variants/feature outputs become live. |
| `916789c6` added a combined bool/index capability matrix. | None for this terminal. | Combined bool/index matrix and position-based capability encoding: unrelated compiler scope, no C0-IR consumer, and a representation whose proof is not the branded-ID error. | Reconsider a named three-cell capability proof only in separately calibrated C0-COMPILER. |
| `328f890a` replaced the bool matrix with type-level capability rows. | None for this terminal. | One-implementation delegation and type-level matrix machinery: grows compiler surface without advancing the ID proof. | Consider only after two concrete compiler consumers and input-forwarding/codegen evidence. |
| `916789c6` / `788499a5` added a release consumer and compared broad binary totals. | Distinct nonempty input forwarding is a real compiler insight, but not needed for C0-IR. | Incomparable binary totals and compiler release consumer in this slice. | Queue a named release consumer with per-monomorph retained control and input-removal mutant for C0-COMPILER. |
| `328f890a` used panic-style process handling and `788499a5` improved it. | Preserve command failures and cleanup failures as typed test outcomes; do not convert them into green proof. | `expect`, `unwrap`, `panic!`, `process::exit`, ignored cleanup, or failure/source loss. | Reuse the typed harness pattern only when a C0-COMPILER card has enough reserved test LOC. |
| `aaa71d06` and `ebb81a9f` reverted the broadened implementation. | The restored compiler tree is a protected control; changed-path checks prove C0-IR did not touch it. | Reintroducing compiler paths, manifests, examples, codegen measurement, or C1 surface. | C0-COMPILER must start from a new card, calibration deck, and its own consumer/codegen evidence. |

## Decision

Adopt only the actual-artifact causal fixture, legal-mutant predicate, raw evidence capture, and
non-discarding cleanup discipline. The compiler matrix, shadow fixtures, panic consumer, incomparable
binary totals, and combined C0 scope remain rejected.
