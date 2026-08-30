# Coupling skeleton — Wave C.7

| Module | Invariant owner | Terminal | Dependencies | State/control boundary |
|---|---|---|---|---|
| command/query vocabulary | application service | accepted/rejected reply | nudox-id + accepted compiler/index facts | closed typed command enum |
| local fact projection | application service | Complete/Partial/Degraded | borrowed typed snapshot/root/locality facts | no adapter policy |
| progress/replay | application service | terminal exactly once | bounded operation facts only | stable key + bounded cursor |
| diagnostics | application service | Failed/Cancelled with source | existing typed errors | correlation and chronology |
| probe emission | nudox-observe seam | no-op or typed event | existing Probe | builder laziness at disabled interest |
| public integration | Sol-owned tests | golden journey | all adapters | cross-crate equality |

Test | weakened implementation killed | exact oracle
--- | --- | ---
golden corpus | constant body/input ignoring | every typed reply and order differs
limit+1 | unbounded/late validation | exact rejected operand and unchanged state
outage | remote error mapped to empty success | Degraded/Failed terminal and local facts retained
replay | per-client shadow collector | two cursors equal stable keyed updates
terminal | collector drops terminal | one exact terminal then fused exhaustion

Resource | baseline | bound | measurement | rollback
--- | --- | --- | --- | ---
reply storage | none | fixed command/result bound | size/capacity + allocation counter | remove owner/raise explicit bound
progress | none | fixed event and cursor bound | high-water accounting | reject admission beyond bound
probe | lazy Probe | zero builder work when disabled | real lazy seam test | delete emission path
