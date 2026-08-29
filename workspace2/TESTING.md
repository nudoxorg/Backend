# Adversarial test contract

## Allocation and layout evidence shared by every swath

- Assert exact `size_of`, alignment, and important field offsets from the actual field types; golden
  protocol bytes are the only independent constants. Tests must not repeat production arithmetic.
- Instrument the allocator around the borrowed/inline central path and prove zero allocations after
  setup. Exercise the explicit bounded spill path separately when one exists.
- For every retained `Box`, `Vec`, or reference-counted owner, hit exact capacity, one beyond capacity,
  rejection/rollback, repeated reuse, and drop. Report peak retained bytes rather than container count.
- Closed discriminants round-trip through `From`/`TryFrom`; every unnamed raw bit pattern rejects with
  the original value preserved.
- Error tests match structured fields and causal `source`, not formatted strings alone.
- Identity conversions test `From<[u8; N]>` and standard `TryFrom<&[u8]>` at N-1/N/N+1; no custom
  fixed-width length error is reimplemented.
- Typed wire record tests prove read/write share the same bytes, exact size/alignment has no padding,
  borrowed descriptor pointers lie inside the input, and semantic access performs no scalar reparse.

Tests are executable attacks on invariants. A test that only constructs one valid value and checks
one accessor is documentation, not acceptance evidence.

## Universal rules

- Assert the negative space: every illegal state, boundary, and transition adjacent to the happy
  path fails with the exact typed cause and leaves all accounting/output unchanged.
- `is_err()` and `any(matches!(...))` are not acceptance evidence by themselves. Assert exact error
  payload, exact ordered output/cardinality, unchanged state/resources, and the adjacent legal case.
- Generate state sequences with a deterministic seed and assert conservation after every step, not
  only at the end.
- Make complexity observable with comparisons, probes, visited rows, bytes touched, or retained
  capacity. Wall-clock thresholds are secondary and never substitute for work counters.
- Test representation explicitly: `size_of`, alignment, field/record count, high-water bytes, and
  whether a returned borrow points inside the original backing storage.
- Test idempotence by repeating successful input and conflict behavior by changing exactly one fact.
- Test cancellation/crash at every legal prefix. A final-state-only workflow test is insufficient.
- Avoid mocks that recreate production state. Exercise actual public types across crate boundaries.
- Add abstraction stress cases: introduce the next legal closed variant through the owning vocabulary
  and prove existing consumers remain exhaustive and coherent. Add compile-fail tests for forbidden
  cross-domain substitution/forged witnesses where a runtime negative test cannot express the law.
- A validated iterator's exact cardinality must hold under every legal extension record. No internal
  conversion may turn an expected item into early `None`, a shorter stream, or an `Internal` result.
- Grep every `map_err` site. A conversion failure test asserts the exact attempted operand and
  traversable source error; a rejected ownership transfer asserts the original value is returned
  unchanged. Do not restrict this audit to lines a human reviewer happened to notice.
- Enumerate the typed identity-domain registry, prove tag uniqueness, and compare incremental and
  one-shot canonical hashing. Application crates contain no raw `b"nudox..."` personalization
  literals. Semantic-root identity is invariant under locality-only changes, while a semantic
  object/parent/key change alters it.

## Foundation fabric

- Truncate every valid frame at every byte boundary.
- Mutate every header/directory byte through boundary and representative adversarial values; include
  length overflow, offset wrap, overlap, misalignment, duplicate/out-of-order kinds, flags, reserved
  bits, versions, schema IDs, row/arena maxima, and nonzero padding.
- Prove preflight errors do not touch caller output; prove bytes after encoded length are untouched.
- Prove validation scans only header/directory/padding, not payload, unless hashing is requested.
- Prove every borrowed section pointer lies within the original input allocation and repeated access
  performs constant work without allocation or revalidation.
- Keep immutable golden vectors for every known schema and identity preimage.

## Object, root, store, hydration

- Build roots from many permutations; canonical bytes/ID/order must agree. Reject duplicate keys,
  missing parents, self cycles, deep cycles, parent sentinel collisions, and entry-count boundaries.
- Diff disjoint, equal, moved-only, changed-only, and alternating roots. Assert comparisons are
  bounded by a small linear function of both input lengths.
- Measure exact common-row and sparse-sidecar retained bytes at 1, 100k, and boundary entries.
- Force store-index collisions and maximum legal load; assert bounded probe counts, correct lookup,
  no payload moves/copies, and unchanged memory/accounting after every rejection.
- Exercise two concurrent first writers, coherent duplicate, same-ID metadata conflict, wrong hash,
  wrong length, byte/slot exact fit, and owned-buffer identity after rejection.
- Enumerate planner coverage combinations and publication transitions. Missing/failed/mixed
  generation input can never produce `ReadyGeneration`; replay of ready input schedules zero work.
- Propagate overlays at every depth and through subsequent diff/merge; no ancestor may lose the mark.
- Prove overlay/tier/provider changes touch only sparse locality storage: no semantic row copy,
  allocation, canonical-byte change, or generation-ID change.

## Operation, runtime, workflow

- Prove cursor fusion and unique ownership; progress sources are not cloneable and cannot emit twice.
- Model custom atomics with Loom: admit/dequeue/cancel, byte/terminal credit rollback, stale ABA
  handles, owner drop, producer exit, and every compare-exchange failure point.
- Sustain producers, owner drain, cancellation, and terminal observation simultaneously. Joining all
  producers before draining is not concurrency evidence.
- For capacities 1 through representative N, generate admission/rejection/complete/fail/cancel/stale
  sequences and assert physical-credit conservation and exact high-water bounds after every action.
- Cross the product of workflow phases and events. Legal transitions produce exact state/effect;
  impossible transitions preserve state and exact cause; conflicting outputs never advance.
- Replay/crash after every durable prefix and derive exactly the uncommitted idempotent command.
  Published/failed/cancelled prefixes derive none.
- Exercise the future async seam with a deterministic wake source: pending-before-ready registers a
  wake, cancellation at every poll boundary returns leases, bounded fan-out never exceeds byte/item
  credits, out-of-order I/O completion reconstructs stable semantic order, and fused termination
  preserves the exact terminal fact.

## End to end

- Use actual canonical bytes, frame validation, root, planner, owned store admission, readiness,
  operation cursor, concurrent runtime, and durable recovery—no integer/shadow domain.
- Corrupt/truncate each exchanged artifact, exhaust each physical bound, cancel at each publication
  boundary, and substitute a stale generation. None may become empty success or visible half-state.
- Demonstrate the same operation over locally resident, promised/degraded, newly hydrated, and
  overlaid input with honest batch-level provenance and identical semantic identity rules.
