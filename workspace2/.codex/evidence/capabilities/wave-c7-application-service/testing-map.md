# TESTING.md mapping — Wave C.7 Phase 0

`workspace2/TESTING.md` SHA-256: `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b`.

| TESTING clause | Matrix row / disposition |
|---|---|
| exact typed errors, source, rejected operand | C7-01, C7-07 |
| deterministic seed/state conservation | C7-05, C7-06, C7-09 |
| boundary zero/one/full/+1 | C7-03, C7-09 |
| cancellation at every legal prefix | C7-07 |
| stable ordering and cardinality | C7-03 |
| input-ignoring/constant-body mutation | C7-02, public Sol journey |
| remote outage never empty success | C7-04 |
| terminal exactly once/fused replay | C7-05, C7-06 |
| disabled Probe builder laziness | C7-08 |
| allocation/retained capacity evidence | C7-09 |
| adapter/dependency exclusion | C7-10, Sol-owned gate |
| cross-crate golden journey | C7-02..C7-07, Sol-owned top-level tests |

Loom/Miri/compile-fail are not applicable to the initial synchronous service unless implementation
introduces atomics, unsafe, or a proof-bearing conversion; any such introduction reopens this map.
