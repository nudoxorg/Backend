# Durable publication Wave A.2 — proof matrix (Phase 0)

The exact focused test/measurement names, resource constants, physical-fault inventory, and mutant
schedule for every row are frozen in `contracts/resource-and-fault-ledger.md`. A row remains `RED`
until its named test exists, dies against the named weakened behavior, and passes from the candidate.
The authority construction and downstream compile-fail contract for DP-14 is frozen separately in
`contracts/published-generation.md`.

| ID | Law / weakened implementation | Falsifier / required evidence | State | Owner |
| --- | --- | --- | --- | --- |
| DP-01 | Full admission mutates credit or consumes command | Fill exact item/byte/receipt limits; next `try_submit` returns the same command and every high-water/credit is unchanged | RED | Luna service card |
| DP-02 | Producer reaches file/probe or queue is unbounded | Block owner, concurrently submit to capacities 1 and N, assert one file owner and exact slot/byte/waiter/receipt high-water | RED | Luna service card |
| DP-03 | Duplicate facts are appended twice or conflict wins | Repeat identical command and change exactly one fact; assert idempotent receipt/head versus typed conflict/no bytes | RED | Luna service card |
| DP-04 | Cancellation leaks or releases at a boundary | Cancel at admitted, queued, grouped, synced, pre-head, and post-head; assert terminal/counters/bytes/reopen image per boundary | RED | Luna service card |
| DP-05 | Receiver loss / poison loses cause or stranded receipt | Drop receiver and inject group write/sync error; exact receipt fan-out, causal source, and all credits return | RED | Luna service card |
| DP-06 | Shutdown hides join/cleanup failure | Simultaneous owner failure and join/cleanup failure returns typed composed error with both sources/owners; handle joins once | RED | Terra focused test |
| DP-07 | Group only writes one item, duplicates frames, or syncs before write | Group 1..=bound using reused fixed storage; independent journal reducer sees exact ordered frames; one sync outcome fans out exact per-command receipts | RED | Luna journal card |
| DP-08 | Any append length/error or sync failure publishes a receipt/effect | Every partial/complete group write and sync injection returns no failed receipt/effect; reopen derives exactly the physical valid prefix | RED | Luna journal card |
| DP-09 | Publication fact is formed before stable receipt or is mutable | Mutate/reject stale receipt; publication record cannot be emitted before named stable journal bytes; exact immutable record checksum validates | RED | Terra focused test |
| DP-10 | Head can name absent/corrupt/unstable fact | Fault every temp/head write, sync, CAS/rename, directory sync, and crash prefix; visible head always validates and points to existing stable checked fact | RED | Luna head card |
| DP-11 | Head overwrite/conflicting publish is non-atomic | Coherent duplicate retains byte-identical head; conflicting compare fails exact and old head remains byte-identical | RED | Luna head card |
| DP-12 | Reopen trusts head or shadow state | Independently reduce journal + validate immutable publication bytes + head; corruption/torn prefix either rejects exact cause or reconstructs same authority | RED | Terra focused test |
| DP-13 | Pending sender fails to wake/recheck | Only if a pending API exists: pending-before-ready stores wake, receiver/drop/cancel wakes, registration followed by recheck cannot strand work | RED | Terra focused test |
| DP-14 | Published generation is forgeable / marker-only / not tied to verified fact | Downstream compile-fail literal/conversion and mixed-owner attempt; Sol feature journey calls the frozen `PublicationPaths`/`PublicationLimits`/publisher/wait/reopen surface and compares public stable facts | RED | Durable adapter |
| DP-15 | Added structures hide allocation or dependency cost | Terra reproduced `warmed_nonempty_append_has_no_heap_allocation_and_exact_control_receipt`: zero post-warm heap allocation and exact second receipt. Candidate must measure group reuse, retained bytes/copies, and `cargo tree`. | REPRODUCED BY TERRA | Terra measurement |

## Resource controls

Initial controls to be frozen with the code card: configurable but construction-bounded command items,
encoded frame bytes, receipts, and one group; one owner thread; group storage exactly `group_limit *
JOURNAL_FRAME_BYTES`; no retained `WorkflowEvent`/`WorkflowRecord` collection after admission; no
producer `File`, `Probe`, `Arc`, or `Box`. Every bound is tested at zero, one, exact, and plus one.

## Salvage ledger — rejected P2

| Historical mechanism | Kind | Disposition / destination | Falsifier retained |
| --- | --- | --- | --- |
| 32/92 fixed journal grammar and checksums | representation | already accepted in `durable-journal::format` | accepted D0 byte/checksum mutations |
| private stable receipt after file sync | authority | already accepted in `StableReceipt` | D0 sync-fault/reopen tests |
| streaming independent reduction/reopen | algorithm | already accepted in `journal::recover` | torn tail and full-frame rejection |
| strict fault-source/persistent image requirements | oracle | retain as DP-08/DP-10 tests | write/sync/directory image schedules |
| `sync_channel` bounded-control insight | algorithm | evaluate only as simple owner admission, no lock-free claim | DP-01/DP-02 |
| rejected 2,212-line multi-module prototype | surface | rejected; no symbols/API copied | Control A incomplete-feature and P2 C absent-byte counterexamples |
| rejected draft fixed-file implementation | diagnostic | reject compressed/source-erasing implementation | P2 fresh build checkpoint findings |
