---
name: build-leased-range-transport
description: Scope and evidence laws for a workspace2 runtime-independent leased range stream and its file, HTTP, or verified-network adapters. Use with deliver-reviewed-rust-slice and manage-rust-swarm when designing, implementing, or reviewing bounded async range delivery, lease conservation, cancellation, partial terminals, reorder, or transport adapters.
---

# Build leased range transport

Read `../deliver-reviewed-rust-slice/SKILL.md`, `../manage-rust-swarm/SKILL.md`,
`../build-operation-runtime/SKILL.md`, and `../build-object-hydration/SKILL.md` completely. Read
`../../../ASYNC_STREAMING.md` and the async sections of `../../../TESTING.md`. Use
`../audit-data-layout/SKILL.md` before choosing a numerous lease, slot, completion, or reorder
representation.

This skill owns one semantic range-delivery capability at a time. It does not authorize an async
runtime, HTTP/QUIC SDK, Bao proof format, cache, object store, unsafe, SIMD, or foundation protocol
change. Terra derives the vertical card and representation; Luna implements or falsifies one frozen
proof boundary.

## Invariant map

Draft this during the first bounded red-test/implementation cycle and finish it before promotion.
It does not authorize edits. Label every owner, borrow, byte/item credit, wake edge, sequence, and
terminal transition:

```text
demand + physical credits
    -> unique buffer lease
    -> pending I/O owner
    -> keyed completion owner
    -> synchronous borrowed validation/hash
    -> store transfer or drop
    -> credits returned exactly once
```

The semantic stream is independent of executor and transport. Local and remote delivery use the same
range keys, owned lease, data event, and terminal vocabulary; they differ only in the concrete source
owner and waiting mechanism. Logical object/content identity never contains provider, URL, runtime,
cache tier, retry, or physical range-placement facts.

## Laws

- A lease is non-cloneable linear authority over one bounded byte region and its credits. Every
  success, rejection, cancellation, failure, stream drop, and consumer transfer returns or transfers
  that authority exactly once.
- Item slots and bytes are reserved before I/O begins. Pending, completed-but-buffered, held-by-
  consumer, and reorder-window owners are all charged. Queue length is not a byte bound.
- A data item carries its typed semantic sequence/range. Out-of-order completion is legal; stable
  presentation uses an explicitly bounded reorder owner and never an unbounded collection.
- `Complete`, `Partial`, `Degraded`, `Cancelled`, and `Failed` are distinct typed terminal facts.
  Terminal appears exactly once. Exhaustion after terminal is fused; `None` never means success.
- A source that can be not-ready cannot expose `Option<Event>` as its poll result: pending, terminal,
  and fused must remain distinguishable even in the runtime-independent semantic control.
- Pending registers its waker before returning and rechecks state after registration. Cancellation
  and readiness have a named linearization order; stale wakes and repeated polls cannot duplicate a
  lease or terminal. Any cancellation source that can resolve pending work owns or reaches the wake
  registration. A flag with no wake path is not cancellation authority.
- Insufficient caller output is decided before consuming a batch. It returns an exact typed capacity
  failure with destination and every lease unchanged, or returns a typed remainder that still owns
  the unconsumed leases. Truncate-and-release is data loss.
- Pure prefix parsing, full validation, hashing, and store admission are synchronous over borrows from
  the same owner. The adapter never invents a second decoder or copies bytes into a protocol DTO.
- Core async types are concrete associated futures/streams or finite static composition. No public
  `dyn`, boxed future/stream, `async-trait`, erased error, detached task, ambient runtime handle, or
  task-per-range policy.
- CPU-heavy validation/decode leaves the I/O executor through a bounded, joined capability. Blocking
  file/database calls never hide inside an async signature.
- Errors retain exact range/sequence, requested and observed extent, source phase, rejected owner when
  recoverable, and causal I/O/protocol source. Retry/degradation is explicit policy, not `Internal`.
- Item and byte accounting are named semantic facts, never `(u8, u64)` or repeated raw fields across
  terminal variants. Checked length conversion/summation either becomes impossible by using the
  native bounded type or has its own source-preserving error with original operands and owners; it
  never saturates, wraps, uses a maximum sentinel, or masquerades as a budget rejection.
- Observation is one lazy coarse typed event at admission, completion/degradation, and terminal. No
  per-byte/per-poll logging, payload identity metric labels, or exporter dependency in the core.

## Do and do not

```rust
// DON'T: collection, erasure, and ambiguous exhaustion.
async fn fetch(ranges: Vec<Range>) -> Vec<Result<Vec<u8>, Box<dyn Error>>>;

// DO: one bounded owner crosses the wait boundary; validation borrows it synchronously.
Data { sequence, range, lease }
Terminal(Partial { delivered, missing })
```

```text
DON'T: allocate one task/buffer per requested range and count only the request queue
DO: reserve item + bytes -> start at most reserved work -> charge completion/reorder/consumer owners

DON'T: cancel flag with no registered wake + best-effort drop
DO: cancel authority reaches the waiter -> exact winning owner -> deterministic credit conservation proof

DON'T: copy until output fills, release the remaining leases, and return a shorter count
DO: preflight exact output capacity or return a remainder that still owns every unconsumed lease

DON'T: HTTP error -> empty stream or zero hits
DO: exact Failed/Degraded/Partial terminal carrying requested range and source
```

Raw bytes use standard `Deref<Target = [u8]>`, `Borrow<[u8]>`, or `AsRef<[u8]>`; a callback that only
forwards `self.buffer.as_ref()` is delegation and must not be called validation. An HRTB callback is
earned only when it lends a real proof-bearing view tied to the lease and prevents a view that would
otherwise outlive its owner. If key and buffer are independently valid facts, expose named fields or
standard borrows instead of trivial `.key()`/`.bytes()` getters. Semantic scalar newtypes have named
facts plus exact `From`/`TryFrom` and `Deref` where ordinary scalar operations are valid; never expose
positional `.0` as the API. Use a self-referential owner only when the view must outlive the callback
and measured removal of a copy or revalidation pays for construction, drop, code size, and dependency.
`Arc` must prove shared lifetime ownership that cannot be expressed by transfer, scope, arena, slab,
or completion-owned buffer.

Do not invent scalar validity merely to justify a newtype. If range key zero is invalid, the product
contract names why and tests the adjacent semantic law; otherwise use the full raw domain. When lease
key and buffer are independently valid, expose named fields rather than a trivial constructor/getter
pair. Private construction is reserved for coherence that a mixed-owner falsifier can actually break.

## First vertical proof

The first slice is the smallest runtime-independent public journey that can be falsified:

1. a caller supplies fixed item/byte capacity and a deterministic completion source;
2. at least two keyed ranges complete out of order through unique owned leases;
3. one consumer observes the declared ordering policy, validates borrows from those same owners, and
   reaches one exact terminal;
4. cancellation is injected at every pending/ready/held boundary;
5. all leases and both credit classes are conserved after success, failure, cancel, and stream drop;
6. repeated polling after terminal is fused and performs no allocation or wake registration.

This slice contains no real filesystem/network adapter, authenticated range grammar, cache, retry
fleet, or runtime SDK. Those are later concrete adapters over the proven semantic owner.

## Evidence rows

Terra turns every applicable row into a literal card with numeric ceilings and an executable
falsifier:

```text
lease uniqueness | compile-fail duplicate/clone/borrow escape + drop counter | exact owner once
validation authority | raw borrow versus proof-bearing view compile/runtime cases | no dishonest validated API
capacity conservation | model snapshot after every scheduled action | items and bytes both exact
wake correctness | deterministic `Wake`/`Waker` schedule including a source that never self-wakes | register-before-pending, cancellation wakes, recheck, no stale duplicate
short output | settled multi-lease batch into limit-1 destination, then exact retry | destination and leases unchanged before retry
terminal law | complete/partial/degraded/cancel/failed plus repeated poll | once then fused
reorder law | reverse/random completions at 0/1/limit/limit+1 | exact bounded work and storage
allocation | isolated nonempty repeated poll/completion | zero per poll after setup
copy/pointer | completion/validation/hash/store pointer ledger | one owner, no payload copy
diagnostics | short read, disconnect, protocol failure, cancel race | exact operands and source
dependency | cargo metadata/tree and client binary | no runtime/transport/OTEL SDK in core/client
codegen | release text per real monomorph | reserve retained; no generic explosion
```

Tests live under an ordinary owning crate's top-level `tests/`; no dedicated test-only crate. The
schedule driver returns structured failure evidence—action index, action, owner/credit snapshot,
terminal state, wake counts, and exact source—without `expect`, panic, or a shadow implementation.
Use concise table cases over that driver. A sequential producer/drain loop is not concurrency proof.

## Adapter research gate

Terra researches a concrete adapter only after the semantic slice closes:

- bounded blocking file worker is the mandatory portability baseline;
- Compio/Monoio must return the exact completion-owned buffer across cancellation and beat the
  baseline on total CPU/latency without changing semantics;
- HTTP range and iroh/Bao are replaceable adapters; verified-range technology never becomes logical
  identity merely because it authenticates physical bytes;
- `futures-concurrency`, slab/pool, or self-reference enters only when an experiment deletes more
  code/state/work than it adds and preserves exact cancellation evidence.

Record upstream trait/lifecycle surface, target support, unsafe/dependency/text cost, failure model,
and rollback before promotion. A fashionable crate or microbenchmark is not evidence.

## Closure

Return the shared handoff plus owner/credit state diagram, linearization points, raw deterministic
schedules, allocation/copy/work/text ledgers, dependency proof, strongest stale-wake/cancel/drop
counterexample, rejected representations, and the next adapter decision. Never claim object-pack,
index, compiler, cache, or distributed transport completion from the semantic stream alone.
