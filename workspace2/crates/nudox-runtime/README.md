# `nudox-runtime` bounded-storage review

Each issued `WorkPermit` owns one `PayloadSlot<Generation, Work>` until the owner restores it or
retires its exact coordinate. `ReadyBitmap` holds only one bit per physical coordinate; it has no
`Work` storage and therefore cannot displace a live owner. Publication writes the payload, then
release-publishes its coordinate; the sole consumer acquires and clears that bit before reading.
Queued/cancelled packed slot state is the sole payload-lifecycle authority. A transition fault moves
the payload into a named quarantine path, terminalizes it as `QueueTransition { source }`, retires
no unrelated state, and restores every held credit before returning the same packed-state error.
Ordinary callback failure is `TerminalOutcome::Failed { failure }`, retaining the caller's concrete
failure value rather than reducing it to a closed tag. Callback panic is an executor defect: the
unwind guard records `ExecutorUnwound` after resource commit, then the original panic continues. A
second `Work::drop` panic during that unwind is Rust's abort boundary.

## Structural state and opt-in accounting

`Runtime::metrics()` is always available and derives only true current state from the bounded
permit pools: active/available/checked-out work slots, byte reservation, retired coordinates, and
terminal occupancy. It does not manufacture historical zeroes for the default runtime.
`Runtime<_, _, _, ()>` carries a zero-sized policy and its monomorphized admission and
completion paths omit accounting instructions. Applications that need history select
`Runtime<_, _, _, AtomicAccounting>`; only that policy exposes `history()` with relaxed high-water,
rejection, and terminal-class totals. The public accounting traits are sealed, so these two static
policies retain their representation and instruction guarantees without a runtime branch.

## Safe-library control

`thingbuf` 0.1.6 was evaluated as the safe baseline. Its fixed MPMC `ThingBuf` provides `push_ref`
and `pop_ref`, and its `static` feature can avoid heap allocation. Its public `Ref` is a transient
queue borrow, however—not a stable `{runtime identity, slot coordinate, epoch}` handle. Adding
cancel-after-publication, stale-handle rejection, and bit-level epoch retirement would require a
second indexed state table beside ThingBuf. That duplicates the payload/coordination machinery and
does not satisfy one-permit/one-payload conservation, so it is deliberately not a production
dependency.

## Waiter layout and scan control

`WaiterSlot` is 40 bytes: packed state (8), requested byte credits (8), and `AtomicWaker` (24).
Thus 64 slots occupy 2,560 bytes / 40 64-byte cache lines. The former wake path read all 64 slots
regardless of demand. The current registry keeps `free` and `armed` `u64` permit bitmaps.
Registration CAS-claims one free coordinate in O(1), then advances its slot epoch. Release clears
the unversioned armed bit and takes its old waker before it publishes the free permit; a WAKING
release is published only by the wake completer. This prevents an old epoch from clearing a newly
armed bit at the same physical coordinate. The completion path reads `armed` first; zero armed
waiters return before loading work, terminal, or byte counters. A single armed waiter touches one
40-byte record.

| Design | 0 armed | 1 armed | 64 armed | Retained work storage |
| --- | ---: | ---: | ---: | --- |
| Former linear slot scan | 64 records / 40 lines | 64 / 40 | 64 / 40 | one spare payload queue cell |
| Current free/armed bitmap | 0 / 0 | 1 / 1 | 64 / 40 | exactly `capacity` payload slots + two 8-byte permit bitmaps |

The fixed stable domain avoids general intrusive waiter lists: those pay unlink/reclamation costs
with a mutex or delicate unsafe lifetime machinery. The runtime instead has no node reclamation and
models generation/status reuse separately under Loom.
