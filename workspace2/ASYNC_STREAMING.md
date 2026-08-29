# Async streaming contract

This project uses async where work waits, not as a universal coloring of computation. Network, disk,
remote compiler, and durable-log adapters are async. Canonical hashing, frame validation, root diff,
planning, and local-memory lookup are synchronous borrowed kernels.

## One semantic stream, two execution shapes

The semantic vocabulary is shared:

```text
StreamEvent<BatchLease, Terminal>
  Data { sequence, lease, batch }
  Terminal(Complete | Partial | Degraded | Cancelled | Failed)
```

- A local source is a unique lending cursor. It never returns `Pending` because no wake source exists.
- An I/O source has an associated concrete `Stream`/future type. It registers a waker before returning
  `Pending`; it is not boxed or erased in the core contract.
- `Terminal` is observed exactly once before fused exhaustion. `None` means only “the already-terminal
  stream is exhausted,” never success, truncation, or cancellation.
- Sequence/range keys carry semantic position. Independent I/O may complete out of order; consumers
  may process keyed output unordered or use an explicitly bounded reorder window when presentation
  requires stable order.

The stable `futures_core::Stream` contract does not promise safe repeated polls after `None`, so our
adapters implement a closed terminal phase and `FusedStream` behavior explicitly:
https://docs.rs/futures-core/latest/futures_core/stream/trait.Stream.html

## Leases are backpressure

Admission reserves both an item slot and byte capacity before issuing I/O. The resulting
non-cloneable lease travels with the buffer. A consumer validates and hashes a borrow from that owner,
then either transfers the owner into immutable storage or drops it. Every drop/cancellation edge
returns credits exactly once.

`Stream<Item = OwnedLease>` is deliberate. Stable Rust streams cannot yield arbitrary borrows from
their own mutable state as a general lending stream. Yield the owner across the async boundary, then
borrow synchronously inside the consumer:

```text
async I/O -> owned BufferLease -> with_validated(|ValidatedFrame| ...) -> store transfer/drop
```

Use `ouroboros` only if a validated view must outlive that callback while remaining tied to the owned
buffer and the self-reference eliminates a measured copy. A short-lived HRTB closure or ordinary
owner method is preferable.

Completion-based file/network adapters are a serious remote-plane candidate because their natural
operation transfers an owned buffer into I/O and returns that same owner with the completion. This
matches `BufferLease` better than holding a mutable borrow across readiness polling. Compio exposes
this shape across IOCP, io_uring, and polling backends; Monoio is a Linux/server thread-per-core
alternative. Neither enters the semantic stream contract. Promote one adapter only after the same
fault schedule proves cancellation returns the exact owner, portable fallback semantics match, and
end-to-end CPU/latency beats a bounded blocking-worker baseline:

- https://compio.rs/
- https://docs.rs/compio-buf/latest/compio_buf/
- https://docs.rs/monoio/latest/monoio/

## Incremental framing

An async adapter must not invent a second decoder. The foundation exposes a pure checked prefix
transition sufficient to learn the declared bounded frame length, then the adapter fills one leased
buffer and calls the canonical full validator. A partial prefix reports exact additional bytes; an
invalid prefix reports the same typed protocol cause as full validation.

`tokio_util::codec::FramedRead` demonstrates the useful `AsyncRead -> Stream` adapter boundary, but
its `BytesMut` buffering and runtime types belong in an optional I/O crate, not in the protocol core:
https://docs.rs/tokio-util/latest/tokio_util/codec/struct.FramedRead.html

## Concurrency shape

- Remote object ranges and compiler jobs are independent futures admitted under physical credits.
- Structured groups are awaited to completion; no detached task may swallow panic/error/lease return.
- Cancellation owns an explicit token/phase and is tested at every poll boundary.
- CPU-heavy decode/compile work leaves the async executor for scoped parallel workers and rejoins with
  owned leases/results. Async tasks never block on synchronous disk/database calls.
- Fixed arrays/tuples of heterogeneous futures can be statically joined/merged. A dynamic demand set
  uses a bounded preallocated slot/slab, not one heap allocation and task per object.

Iroh's production lessons support explicit runtime capabilities, structured concurrency, handled
join results, and cancel-safe queues:
https://www.iroh.computer/blog/async-rust-challenges-in-iroh

`futures-concurrency` is a candidate adapter library because its array/tuple composition is static and
runtime-independent. It is not adopted until benchmarks and cancellation tests show that it deletes
our machinery:
https://docs.rs/futures-concurrency/latest/futures_concurrency/

## Required proof

- deterministic manual-waker test: every `Pending` registers and is later woken;
- cancellation at every poll boundary returns item and byte credits;
- producer/consumer concurrency never exceeds configured in-flight bytes/items;
- terminal is exactly once and polling is fused thereafter;
- out-of-order completion preserves every sequence exactly once;
- stream drop with ready, pending, and held items preserves conservation;
- no allocation per poll after adapter setup;
- the same leased bytes are observed by I/O completion, validation, hashing, and store admission.

The manual-waker proof is also the first deterministic simulation seam. Each driver action names the
exact source, wake, cancellation, delivery, or disconnect transition; the driver returns a typed
`ScenarioError` on an unexpected event and includes the replay position plus lease/credit snapshot.
There is no `expect` at the poll site and no random scheduler outcome that cannot be replayed.

A later nested Turmoil-style harness may supply virtual sockets, time, and filesystem failure, but it
must run these same stream phases and scenario commands. Simulator executor/runtime types remain
outside the stream, operation, runtime, and workflow public APIs.
