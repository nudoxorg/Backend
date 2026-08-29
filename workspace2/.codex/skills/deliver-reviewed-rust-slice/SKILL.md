---
name: deliver-reviewed-rust-slice
description: The mandatory workspace2 Rust craft, performance, structure, testing, and parent-review protocol. Use before designing, implementing, reviewing, or benchmarking any performance-sensitive slice, then apply exactly one short domain skill for scope-specific laws.
---

# Deliver a reviewed Rust gem

This is the single authority for workspace2 Rust idioms, patterns, structure, evidence, and review.
Read it completely before a domain skill. Domain skills add scope; they do not redefine these laws.

Deliver one capability, not an interpretation of the roadmap. The parent owns architecture and the
eventual rubric. The agent owns one proof, reports early, and waits before broadening.

## Contract before code

No production edit is allowed until the parent supplies or approves every row:

```text
Capability: one externally observable behavior
Allowed paths: exact crates/directories
Must preserve: public semantics, bytes, ownership, errors, and state laws
Must prove: exact tests, traces, and measurements
Out of scope: explicit adjacent work
Budget: retained bytes, allocations, copies, work, latency, text, and net LOC
Checkpoint: the next parent decision
```

If a row is missing, inspect the direct path and propose it. Never fill ambiguity with a framework,
compatibility layer, copied type, backend enum, or broad refactor.

## Mandatory parent loop

### Checkpoint 1 — evidence and design; no production edits

Return:

1. A coverage table quoting every `Must preserve` and `Must prove` row, with its design element and
   falsifying test. `Deferred`, `unchanged`, and an adjacent component's behavior are not coverage.
2. Existing call path and invariant owner with file/line evidence.
3. The simplest safe/std baseline and at most two serious alternatives.
4. Ownership/allocation, generic, diagnostic, and work ledgers.
5. Exact tests that can disprove the design.
6. Expected files and net production LOC.
7. Decisions that require parent authority.

Timebox inspection to one routed-file pass plus eight additional read/search/experiment calls. Put
unknowns under parent decisions. If the contract does not fit the budget, request a scope change;
never silently substitute a smaller capability.

Before requesting edit authority, attach this normally formatted skeleton ledger:

```text
Approved baseline: checkpoint name + exact existing production LOC by file
file | public/private items | methods | error variants/fields | docs | estimated formatted LOC
reserve | max(10% of phase, 25 lines)
phase delta | sum excluding the untouched baseline and including cross-crate support edits

test file | laws/cases | shared fixture representation | estimated formatted LOC
test reserve | max(10% of test phase, 15 lines)
```

Estimate from a formatted prototype or a comparable existing module, never from the desired cap.
Count public field docs, error operands, reexports, lint reasons, and cross-crate support changes.
For protocol, unsafe, or concurrency work, approve at most one new proof-bearing production module
per checkpoint. A separately approved cleanup may accompany it; future-phase types and errors may not.
Test estimates are independently binding. Recount test support before test bodies, then after each
test file; apply the same 20%/25-line and unplanned-item stop rules instead of presenting an oversized
suite after production approval.

When a test refactor depends on a new fixture representation, format the fixture and one complete
representative assertion family in scratch before proposing file caps. Extrapolate from the exact
remaining variants and platform cases. Do not infer the new total only by subtracting old helper LOC;
typed setup and exact error operands often move rather than disappear.

Reserve is capacity left unused, not the difference between a near-cap estimate and the cap. For a
260-line test ceiling, the implementation estimate is at most 234 and at least 26 lines remain
untouched. A 253-line estimate plus seven nominal lines is over budget and must be simplified before
editing.

### Checkpoint 2 — smallest vertical proof

Implement the minimum end-to-end behavior, then stop before a second policy/backend/format case or
optimization. Return:

- public API diff and deletion ledger;
- exact tests and commands;
- allocations, copies, retained owners, logical work, and text-size delta;
- largest control-flow offender before/after;
- exact typed diagnostic trace;
- remaining flaws and next parent decision.

More than roughly 250 net production lines or an unbriefed crate is presumed mis-scoped. A larger
coherent plane must be decomposed into reviewed vertical proofs, not hidden under a higher estimate.
The LOC gate is preemptive: record the baseline before editing, recount after each production file,
and stop to re-scope when the current diff plus remaining contract forecast reaches 60% of budget.
Crossing the limit and then reporting it is checkpoint failure; delete the unapproved draft before
continuing.

Forecasts include an uncertainty reserve of at least the larger of 10% or 25 normally formatted
production lines. A design estimated at 295 lines against a 300-line cap is over budget, not a narrow
success. Split at an observable capability boundary before editing; never plan to recover the margin
through shorter names, missing docs/errors, or compressed source.

The budget is not a minification contest. Run `cargo fmt` before counting. One-line functions,
multiple logical statements per line, compressed match arms, omitted whitespace, and module/file-wide
`rustfmt::skip` count at their normally formatted logical size and fail the readability gate.
`rustfmt::skip` is reserved for a small table or golden byte fixture whose visual shape is itself the
evidence. The same rule applies to tests, labs, benchmarks, generated reporters, and unsafe proofs.

After each production file, compare actual to the skeleton before opening the next file. Stop and ask
for a rebase when any file exceeds its estimate by 20% or 25 lines, when one file consumes 60% of the
phase, or when a new public item was absent from the skeleton. Do not finish a coherent over-budget
draft because it is already written. Revert the unapproved phase to the named approved baseline, then
split it. The handoff records written-then-deleted LOC and the estimation error; hiding churn fails.

File entries are forecasts governed by that variance rule, not brittle exact caps. Use a hard
per-file cap only for a real external bound such as stack, protocol, binary-text, or parent-declared
review size. A one-to-five-line formatting or borrow-scope difference that remains inside the phase
reserve is reported, not repeatedly written and reverted. The protected phase ceiling and unplanned
surface rules remain hard.

```text
DON'T: “index.rs ~97” then add parsing, validation, lookup, read containment, six error variants,
       public facts, docs, and reexports in one pass.
DO:    approve header geometry; then directory witness; then lookup; then body verification.
       Each phase owns only errors and tests reachable through that phase's public behavior.
```

### Checkpoint 3 — integration and closure

Only after approval, integrate adjacent paths and run full gates. The agent scores its slice only if
asked. The parent owns product completion and any future rubric.

## Abstraction and boundary laws

- One type owns each invariant. New cases trigger a boundary review; never weaken an invariant with
  `Option`, bool, string, catch-all variant, or caller-coherence requirement.
- Harden crate boundaries around semantic capabilities and canonical bytes. Adapters own filesystem,
  network, runtime, tracing/OTEL, cache, and platform machinery.
- Prefer deletion and the standard library. A wrapper/helper must remove an invalid state, branch,
  copy, repeated algorithm, or unstable dependency—not rename a field or delegate one method.
- Split files by invariant and ownership boundary. Keep functions under the configured complexity and
  length thresholds by extracting named semantic phases, not one helper per branch.
- Source must remain normally formatted and locally readable. Never use formatting suppression or
  statement packing to satisfy an LOC budget; split the capability at a real ownership or proof
  boundary instead.
- In a glob-member workspace, never leave a new crate directory without a valid manifest and target.
  Stage experiments outside the glob or create/remove the complete scaffold atomically so unrelated
  Cargo gates remain runnable.
- Public fields are correct only when every field is an independently valid fact and every public
  struct literal is coherent. If two borrows, coordinates, identities, lengths, or witnesses must
  come from the same validation event, keep their pairing private and expose the independent facts
  through `Deref` or a fact record. Review by deliberately mixing fields from two valid owners; if
  that compiles, the boundary leaks its invariant. Use `AsRef`, `Borrow`, `From`, and `TryFrom` where
  their standard meaning is exact. Do not add `.get()`, `.wire()`, `.content()`, `from_bytes()`, or
  namespace/stateless structs for discoverability.
- Magic numbers include unexplained tuple positions, loop bounds, capacities, offsets, sentinels, and
  arithmetic constants. Replace them with typed records, named constants, semantic newtypes, enums,
  or a derived `size_of`/`offset_of` fact.

## Types and generics

Plain primitives are for loop coordinates and malformed raw observations. Persisted/cross-crate
quantities use transparent semantic types with direct fields or standard conversions. Do not wrap a
local cursor merely to satisfy a style rule.

Every type/const parameter must name:

- two real implementations or consumers in this slice;
- the branch, copy, invalid state, or duplicate algorithm it removes;
- monomorphized text-size cost.

Use standard traits before local traits. A trait with one implementation or line-for-line delegation
fails. Descriptive generic names are mandatory; single-letter type parameters are forbidden in
shipping APIs. A long but meaningful generic list is acceptable.

Use GATs for lending items and concrete futures. Tagless-final/GADT encoding is a lab candidate only
for a real typed DSL with multiple interpreters or compile-time variant elimination. Adopt it only if
illegal variants are uninhabited, optimized cross-crate output erases tags/calls, and call sites get
simpler. An ordinary closed enum wins by default.

Typestate owns one real authority axis over one unchanged runtime representation. Put common facts in
one generic record, make construction private, and use uninhabited state markers. Prefer
`PhantomData<fn() -> State>` when the marker must not impose `State`'s ownership or auto-trait
semantics. Prove illegal transitions with compile-fail tests and prove zero runtime cost with exact
size/alignment/drop evidence. A transition must consume a proof, transfer ownership, or perform/name
the external effect that grants the next authority. Two duplicated structs whose only difference is
a private `Seal`, or a public `.publish()` that only swaps zero-sized markers, fail the abstraction
test; consolidate the representation and bind the transition to the real effect or delete the phase.

```rust
// DON'T: duplicate facts behind nominal seals.
struct Verified { root: GenerationId, dependencies: DepSetId, seal: VerifiedSeal }
struct Ready { root: GenerationId, dependencies: DepSetId, seal: ReadySeal }

// DO: one representation; construction and state-changing operations remain private/checked.
struct Generation<State> {
    root: GenerationId,
    dependencies: DepSetId,
    state: core::marker::PhantomData<fn() -> State>,
}
```

A macro needs two existing production consumers before implementation. Count consumers, not imagined
future variants or tests written only for the macro. Conventional enum-to-trait delegation is not
tagless dispatch: it retains a runtime discriminant and `match`. Before writing a macro, compare the
manual expansion and maintained crates; report which branch, invalid state, or repeated proof the
macro removes. A procedural macro additionally requires source-spanned compile-fail tests and an
auditable expansion. A declarative macro wins when a deliberately narrow internal grammar suffices.

```rust
// DON'T: one user and pure delegation.
trait BytesBackend { fn bytes(&self) -> &[u8]; }

// DO: use the standard capability.
fn consume(bytes: impl AsRef<[u8]>) { /* ... */ }
```

## Ownership, allocation, and memory reuse

Allocation is permitted; habitual `Box`/`Vec`/`Arc` is not. For every retained allocation record:

```text
site | mechanism | exact bound | allocation time | owner lifetime | rejection | alternatives
```

Evaluate mechanisms according to the real lifetime:

1. borrow/reborrow canonical bytes;
2. caller output or reusable scratch;
3. array/`ArrayVec`/fixed inline storage with a stated stack budget;
4. caller arena, bump region, slab, pool, or lease for grouped lifetimes;
5. mmap/range-backed or ref-counted region when ownership escapes;
6. exact-reserved `Vec`, boxed slice, segmented buffer, or persistent structure when it wins.

Charge headers, fragmentation, spare capacity, construction peak, destruction, and simultaneously
live owners. An allocation earns its place by eliminating a copy, extending a required lifetime,
providing stable addresses, bounding fragmentation, or reducing dominant traversal. “Variable
length” and “must be owned” are not evidence. Return rejected owners unchanged and retain allocation
sources.

```rust
// DON'T: make convenience ownership semantic.
pub struct Object { pub bytes: Box<[u8]> }

// DO first: lend the already-owned canonical storage.
pub struct ObjectView<'bytes> { pub bytes: &'bytes [u8] }
```

An `Arc` must prove an escaping shared lifetime. Scoped work borrows. One owner-lifetime `Arc` may
earn a worker/file lifetime; per-operation `Arc` allocation/refcounting does not pass by default.

## Concurrency, async, and streams

- Audit the receiver first: `&mut self` is exclusive admission. A concurrent queue behind it is
  decorative. MPSC capabilities need `&self` plus a proven `Sync` owner or an explicit split handle.
- “Lock-free” excludes `Mutex`, `RwLock`, `Condvar`, and queues that hide them. Name the progress
  guarantee, linearization points, atomic orderings, ABA/reclamation story, and false-sharing plan.
- Preallocate bounded slots. Represent phases as exhaustive enums/atomic states owning exactly valid
  fields. Prove admission, publication, cancellation, terminal observation, reuse, poison, drop, and
  shutdown using production transitions under Loom; use Miri for initialization/provenance/drop.
- Local-ready paths lend borrows without task or serialization. Remote/file I/O returns concrete
  futures/streams with register-before-`Pending`, recheck-after-register, fused terminal behavior,
  bounded in-flight leases, and cancellation-safe ownership return.
- A `Ready` future around blocking I/O is dishonest. Put blocking startup/maintenance in named
  blocking APIs and real async work in a bounded adapter worker/completion runtime.
- Streams do not collect merely to call the next layer. Preserve provider-start, item, partial,
  degradation, cancellation, and terminal errors as distinct typed states.

```rust
// DON'T: optional correlated state.
struct Pending<Work> { ticket: Option<Ticket>, work: Option<Work>, ready: bool }

// DO: each phase owns only its valid facts.
enum Pending<Work> { New(Work), Queued { ticket: Ticket, work: Work }, Done }
```

## Durable and binary data

- Reduce/validate before append; release an external effect only after the declared stable receipt.
- Define write, flush, file sync, directory sync, remote quorum, and explicitly excluded durability.
- Specify ordering, fencing, duplicates, checksum, torn tails, corruption, retry, poison, compaction,
  crash points, shutdown, and replay. Replay streams and retains no log.
- Under demand, drain into a bounded reusable group-commit batch. Map one write/sync to individual
  receipts and define exact failure fan-out without releasing effects.
- Put file-global magic/version/geometry once, not in every fixed record. Derive widths and offsets
  from typed records. Do not ship a bit-at-a-time checksum loop.
- Declare fixed endian wire records once with `repr(C)` plus zerocopy where safe. Hash structured
  identity through typed records; arbitrary chunks are for physical artifact identity.
- Distinguish semantic, artifact, pack, and range-proof identities. Version grammar with a closed
  encoding marker; do not repeat request-authenticated metadata without a measured recovery need.
- Validate once into a compact borrowed witness whose closed tags and coordinates are typed. Trusted
  projection may not repeat raw-to-closed conversion or use `unreachable!`, `expect`, fallback values,
  silent omission, or an unchecked narrowing cast to recover facts validation supposedly proved.
  If projection remains fallible, validation has not produced the right representation. Write exact
  caller output only after total preflight; insufficient output leaves every byte unchanged.
- A packed collection is binary-searchable/range-addressable and states whether it is complete or
  sparse. A one-object envelope or a subrange of one payload is not a pack.
- Do not add nom/binrw/winnow for fixed records. Consider one only when a genuinely variable grammar
  loses substantial manual state/error code while keeping borrows and provenance.

## SIMD and unsafe

Scalar is authoritative. Do not SIMD construction, BLAKE3 hashing, sparse lookup, pointer chasing,
small fixed records, or journaling by default. A proposal requires a profile showing one dominant
long independent contiguous kernel, dispatch/crossover, identical first error/output, every short
length/alignment/tail, ARM+x86 plans, end-to-end gain, and release text delta. Use `fearless_simd`
only after parent approval.

Do not wrap `fearless_simd::dispatch!` for one kernel. Cache `Level` once, keep dispatch cold and
literal body to one generic call, and keep the `#[inline(always)]` kernel explicit. A shared SIMD
macro requires a second measured kernel with the same scalar/crossover/error/tail policy; otherwise
it hides the most important performance decision and multiplies codegen without deleting complexity.

Unsafe starts from a safe reference and must produce a material result safe Rust cannot express as
clearly. Keep one reviewed module. State initialization, alignment, bounds, provenance, aliasing,
lifetime, concurrency, unwind, and drop laws. Require differential tests, exact layout, Miri,
sanitizer where available, Loom for the actual transition, and rollback. No production custom
allocator, self-reference, tagged pointer, intrusive collection, or reclamation scheme first.

## Diagnostics and tracing

Clear names explain static intent; typed events explain one execution. Logging never compensates for
unclear code and never replaces durable audit/security facts.

Emit one compact typed event at coarse decisions: accepted/rejected transition, reservation/release
summary, durable/group-commit boundary, retry/degradation, and stream terminal. No function
entry/exit, item/row/chunk logs, string stages, or high-cardinality metric labels.

`()` must not construct fields. A bounded flight recorder retains chronological events and dumps on
a typed failure/target trigger. Server adapters perform interest checks, correlation, sampling,
batching, tracing, and OTEL mapping. Tests assert exact event order and span/correlation fields.

For a single-writer journal, move the mutable probe into the writer: producers publish typed work and
atomic overload totals; the writer emits ordered events; explicit close returns the recorder.

```rust
// DON'T
tracing::trace!("enter append");

// DO
probe.record_with(|| FileJournalEvent::BatchCommitted { first, count, durable_end });
```

## Errors and control flow

- Preserve the original source and rejected value. No `map_err(|_| ...)`, catch-all `Internal`,
  `expect`, `unwrap`, panic fixture, string step, or erased dynamic error in typed core paths.
- Use `thiserror`/derive macros for declarative formatting. Do not hand-write repetitive `Display` or
  `Debug`; derive or reshape the data.
- Validation error priority is part of the contract. Tests mutate every structural class and assert
  the first exact variant plus operands/source.
- Avoid branchless theater. In hot loops, predictable branches can beat extra allocations or scans.
  Reshape invalid states and split cold validation from trusted traversal; then profile.
- Early returns are fine for rare failures when they clarify the happy path. Repeated near-identical
  limit branches should be a declarative table or typed common operation, not copy/paste.
- Tuple positions like `.0`/`.1`, literal loop counts, `1 + (n - 1) / q`, and repeated binary-search
  ladders demand named facts or a stronger representation.

## Code and test structure

- `src/` contains shipping behavior. Public cross-crate scenarios live in top-level `tests/` or a
  nested test-support crate. Shared fixtures use `tests/support`; never ship `src/scenario.rs`.
- Tests name one law and show input, action, exact result/error, and exact diagnostic trace. Use
  tables/rstest for homogeneous cases; avoid 700-line runners and helpers that hide the assertion.
- Test coordination returns exact setup, timeout, release, join, and observed failures. Do not erase
  cleanup errors with `let _ = ...`; if several failures can coexist, use a small typed aggregate or
  choose and document a deterministic source-preserving priority.
- Test plumbing may not restate the wire grammar once per integer width or mutable/immutable access.
  Use one typed test record/view or one checked const-width cell primitive. If fixture and mutation
  support exceeds the code containing laws and exact assertions, redesign the fixture before adding
  cases; test LOC is not exempt from less-is-more review.
- Required boundary cases are zero, one, exact limit, limit+1, every truncation, hostile mutation,
  collision/reorder/duplicate, cancellation at every pending point, drop/unwind, and restart.
- Allocation assertions run in an isolated process/serial harness with warm-up and compiler/target/
  flags metadata. Measure the representative non-empty hot path so validation/traversal actually
  executes; an empty boundary proves only the empty path unless that is the stated claim. A global
  counter in a normal parallel test is not deterministic evidence.
- Performance evidence stores raw machine-readable results and measures whole operations: owner plus
  backing bytes, peak live, allocation/copy, logical work, latency, text, and construction/drop.
- Compile-fail lifetime tests keep the forbidden owner live after the shorter scope. Property/fuzz
  tests assert exact outcomes; they do not accept “did not panic.”
- For every private coherence witness, add a compile-fail construction test or an equivalent
  downstream-consumer proof that unrelated valid parts cannot be assembled. Also mutate the raw
  discriminant/coordinate bytes so the public validator reports the exact semantic error before a
  trusted view exists.

## Dependencies and negative space

A dependency must delete a stable, solved, edge-case-heavy problem, stay out of the portable client
graph when adapter-only, and include version/security/text-size evidence. Novelty is not a reason.

Stop and ask the parent before:

- any unbriefed public cross-crate API or manifest change;
- a generic without two users;
- unsafe, self-reference, allocator policy, SIMD, or new dependency;
- the same blocker twice;
- diff growth without a deleted cost/invalid state;
- a new case that fits only through optional/stringly/catch-all state.

## Handoff

Return only contract status, changed files, behavior proved, allocation/generic/diagnostic/work
ledgers, exact commands/results, deletion and net-LOC delta, open criticism, and next decision. Never
claim the global plan or rubric complete.

## Sources behind these laws

- Inferara's GADT experiment: https://inferara.com/blog/rust-tagless-final-gadt/
- Adrian Price's software-craftsmanship essays: https://adrianprice.us/category/development/software-craftsmanship/
- “Logging or Commenting?”: https://www.javacodegeeks.com/2014/07/logging-or-commenting.html
- Terse Systems on diagnostic logging: https://tersesystems.com/blog/2019/10/05/diagnostic-logging-citations-and-sources/
