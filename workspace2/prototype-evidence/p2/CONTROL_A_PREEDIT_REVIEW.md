# P2 control A: final blind pre-edit review (rejected)

## Specimen and topology proof

The reviewed specimen was `CONTROL_A_CARD.md` at
`d932691e73afa346101d04848a84fd9a7748feb8`, SHA-256
`49d3e8f4778aa8097d2a8806ff69785e8286b26f2420caff0fff0174be28443c`.
The read-only reviewer was
`/root/p2_control_manager/control_a_terra_preedit_final`, explicitly
`gpt-5.6-terra` with `fork_turns=none`; it read only the card, named direct
workflow sources/tests, and review contract. It made no edits. The paired final
reader was `/root/p2_control_manager/control_a_luna_calibration_final`,
explicitly `gpt-5.6-luna` with `fork_turns=none`; it found no blocker or major.

## Findings

1. **MAJOR A-1 — fault constructors lack exact feature-on falsifiers.**
   `CONTROL_A_CARD.md:115` exposes `directory_sync_error` and
   `tail_repair_sync_error`, and promises source-bearing `OpenError::Io` for
   both, but the evidence matrix mandates neither test. A builder could erase or
   mislabel directory-sync source, return an owner after failed creation
   durability, or claim healthy recovery after failed tail-repair sync.

   A successor card must inject each fault in a feature-on integration test and
   assert exact `OpenError::Io { step: DirectorySync | TailRepairSync, source }`,
   no returned owner, and the exact persistent-byte outcome. Tail repair may
   already truncate; failed repair sync must not claim successful recovery.

2. **MAJOR A-2 — retained-record audit is too narrow.**
   `CONTROL_A_CARD.md:135` catches only `Vec<WorkflowRecord>` and collect forms.
   `Vec<WorkflowEvent>`, `Box<[WorkflowRecord]>`, `Arc`/`Rc`-backed storage, or
   another preallocated dynamic workflow collection can evade it and the
   post-setup allocation measurement.

   A successor card must reject every dynamic container/collection of
   `WorkflowEvent` or `WorkflowRecord` in production (`Vec`, `Box`, `Arc`, `Rc`,
   collect, and preallocation), and classify all dynamic-container hits by
   owner/lifetime/rejection evidence.

## Literal tripwire inventory

| Tripwire | Count | Exact location | Disposition |
|---|---:|---|---|
| panic/unwrap/expect/unreachable | 0 | whole card | absent |
| source-dropping conversion/map_err | 0 | whole card | absent |
| lossy From/TryFrom/raw authority bypass | 0 | lines 92-115 | receipt construction/conversion prohibited |
| checked-arithmetic saturation/operand loss | 0 | line 71 | sequence and maximum retained |
| dyn/Box/Vec/Arc/Rc | 2 | lines 10, 88 | forbidden text; `Vec` audit bypass is A-2 |
| public tuple fields/positional tuples | 1 | line 110 | `Healthy(Recovery)` is an informational view, cleared |
| unit/stateless namespace types | 0 | lines 92-115 | absent |
| public local traits/delegation | 0 | lines 92-115 | absent |
| one-letter generic parameters | 0 | whole card | absent |
| numeric sentinels/offsets/capacities | 17 | lines 55-60, 63-71, 80, 86, 115, 128, 132 | named protocol cells, cleared |
| test-only Option/discarded/success-only assertion | 0 | lines 123-138 | exact negative outcomes required |
| unsafe/SIMD/allocator/dependency addition | 0 | lines 10, 119 | excluded or enumerated |
| public item without consumer/falsifier | 2 | line 115 | A-1 |

## Cleared suspicions and approval

Cleared: exact arithmetic values; five-event reducer publish effect; 32/92-byte
geometry and preimages; header/frame priority; `0..=92` and `1..=91` recovery;
private receipt pairing; stable-path TOCTOU limitation; feature projection/delta;
independent oracle; and caps. Strongest counterexamples are a terminal directory
sync error with erased source and a preallocated workflow-event replay cache.

**Rejected.** The card's final-rewrite limit prohibits repairing these two majors
in this manager family. No Luna builder was commissioned.
