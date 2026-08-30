# P2 durable-publication candidate B: bounded blocking group writer

## Candidate and preserved control

Candidate B changes one axis from Control A only: a `std::sync::mpsc::sync_channel` submission
path feeding one single-owner blocking writer that drains a reusable fixed group buffer. It makes
no lock-free claim. Control A remains the simpler required comparator, but this branch retains no
valid A implementation after `CONTROL_A_FRESH_CLOSURE.md`; therefore B is frozen and rejected
before build, not authorized as a substitute journal.

No shared `Published`/identity/workflow edit, async facade, detached task, database, serde,
network, tracing export, unsafe/SIMD, `Arc<File>`, or lock-free claim is authorized. A future
isolated B implementation may own only a new nested adapter and its top-level tests; it may not
recreate or alter A’s frame grammar, receipt surface, or shared types.

## Literal candidate bounds and required proof

| Resource | Candidate bound / required falsifier |
|---|---|
| submission slots | 4 accepted-but-undurable commands; fifth `try_send` is exact overload rejection |
| queued command bytes | `4 * 92 = 368`; a command owns one fixed frame only after A’s reduce/encode boundary |
| blocked producer waiters | 0: the public candidate uses `try_send`, never blocking `send`; cloned blocking senders would be a rejection |
| pending receipts | at most 4, exactly one per admitted slot; each is terminal exactly once |
| reusable group buffer | one fixed `[u8; 368]`, group cardinality 1..=4, high-water reported; no `Vec<WorkflowEvent>`/`WorkflowRecord` |
| writer ownership | one writer thread owns the journal file and probe; producers never own/share it |
| progress | independently scheduled producer reaches exact full rejection while writer is blocked; release, join, and every primary/cleanup/join error are preserved |

The writer’s only success order is admission -> reduce/encode -> group write -> file sync ->
individual stable receipts -> release effects. A group write/sync failure fans out one exact typed
failure to every receipt in that group and releases none. Shutdown, poisoned writer, dropped
producer, duplicate, checksum corruption, each write length/error, file/directory sync, every
crash prefix, restart, torn tail, recovery, and group-failure fan-out all require deterministic
fault schedules and an independent reducer/recovery oracle.

Required measurements are allocations, copies, logical/physical writes, file/directory sync count
and group size, slot/byte/receipt/group high-water, producer progress, primary+cleanup/join error
trace, latency distribution, retained bytes, release text, normal dependency tree, and recovery
frames scanned. The expected comparison is against a retained valid A control, not metadata alone.

## Rejection before builder authority

**REJECT CANDIDATE B / RETAIN BASELINE.** B has two exact counterexamples at this commit:

1. `CONTROL_A_FRESH_CLOSURE.md` proves no valid A journal, stable receipt, frame-fault corpus, or
   recovery control remains. Implementing B would silently reimplement the missing control rather
   than compare one changed axis.
2. Plain `sync_channel` clones permit arbitrarily many blocked `send` callers. Its 0-waiter bound
   is truthful only for a `try_send` public contract, whose bounded receipt completion and group
   fan-out must be fully designed and calibrated before a build. Neither premise exists here.

The next admissible B card requires an independently retained/calibrated A candidate plus a
complete public receipt/overload/shutdown matrix that makes all five candidate bounds observable.
No B source, manifest, lockfile, test, dependency, or metric is claimed by this card.
