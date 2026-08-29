# Workspace2 architecture charter

Workspace2 is a clean implementation of a local-first, immutable, typed data fabric. Local and
remote deployments share object bytes, roots, schemas, and operation semantics. They do not share
heavy service implementations.

## Non-negotiable laws

1. Foundation data is `no_std` and allocation-free. `alloc` and `std` enter through explicit crates.
2. Hot data is validated once and borrowed thereafter. There is no Serde on the data plane.
3. Public identifiers encode their domain in the type. Raw `[u8; 32]`, integers, booleans, and
   strings do not cross semantic APIs without a newtype or enum.
4. No public trait objects. Finite heterogeneity uses generated or handwritten closed enums;
   reusable algorithms use generics and associated types.
5. Every queue and buffer has a declared bound. Admission owns the corresponding physical
   resource; accepted work cannot discover later that memory is unavailable.
6. Immutable objects are first-write-wins. Publication is a separate atomic root transition.
7. Local completeness requires a non-forgeable `VerifiedGeneration` witness. Published authority is
   distinct and may exist only after a durable adapter consumes verification with a stable receipt.
   Incomplete local answers are explicitly partial and enumerate what is absent.
8. Overlay state propagates to every ancestor root that would otherwise hide the local edit.
9. Hot reads are lock-free or owner-threaded. Locks are allowed for cold control paths when they are
   clearer than atomics. Durable state remains transactional.
10. Handwritten `unsafe` is denied by default and isolated to an explicitly reviewed module. A Wave 1
    exception requires a safe reference implementation, a written initialization/alignment/bounds/
    provenance/aliasing/lifetime/threading/drop/unwind proof, differential tests, applicable Miri,
    sanitizer and Loom coverage, and an end-to-end measurement showing the safe implementation misses
    the budget. Unsafe never crosses a public API or becomes a convenience for unchecked indexing.
11. No library panics for malformed input, capacity exhaustion, cancellation, or I/O failure.
12. Complexity must buy a measured property. Speculative generality is a defect.
13. Every delivered stream ends with an explicit complete, partial, cancelled, degraded, or failed
    fact. A truncated stream and an empty successful answer are never interchangeable.
14. Plane-independent merge/dedup keys derive only from canonical facts every plane can compute;
    device, server, database, and provider identity never salt a federated semantic key.
15. Work is change-driven. Sealed generation deltas and wanted/have closure differences drive
    compilation, indexing, transfer, and publication; accumulated corpus size is not a work queue.
16. `lib.rs` files are maps, not implementations. Cohesive invariants live in narrow modules; file
    boundaries make replacement and review cheaper without multiplying public concepts.
17. Transparent newtypes dereference to their representation when that is the intended everyday
    operation. Fully validated aggregate fields are public unless an accessor establishes a real
    invariant; ceremony is not encapsulation.
18. Genericity and arity are not defects; meaningless names are. Use as many generic parameters and
    inputs as static composition requires, naming each semantic role (`ObjectDomain`, `Generation`,
    `Work`) rather than collapsing the contract into `K`, `G`, and `W`.
19. Errors retain their causal value. Allocation, conversion, validation, and state failures are
    never collapsed by `map_err(|_| ...)`, fake domain errors, or catch-all strings.
20. Hot parallel admission is lock-free end to end, not merely built around a lock-free container.
    `Arc`, atomics, and reclamation each require a lifetime/performance proof. Prefer scoped borrowed
    concurrency when ownership need not escape the runtime.
21. Push branching to construction boundaries. Packed common states, sparse exceptional sidecars,
    typestate witnesses, physical credits, and prevalidated layouts make hot execution straight-line.
    Early returns identify genuine boundary failure; they do not replace a coherent state model.
22. A zero-sized type must be a compile-time brand, sealed capability, or typestate witness. Never
    instantiate a unit `Planner`, `Manager`, `Factory`, or similar namespace: use a free function, or
    give the value actual invariant-bearing state such as reusable caller scratch.
23. Wire and memory layouts have one typed authority. Sizes derive from `size_of::<Field>()` or named
    format-field widths, offsets derive from preceding fields, and exact layout assertions guard them.
    Arithmetic like `32 + 8 + 4 + 2` outside that authority is an unexplained second specification.
24. Ownership follows a cost ladder: borrow existing storage, lend caller scratch, store small proven
    bounds inline, then allocate fallibly. `Box`, `Vec`, and reference counting must earn their retained
    bytes and lifetime. Self-referential ownership may use a reviewed library such as `ouroboros` only
    when it materially enables a safe zero-copy view that a simpler borrowed API cannot express.
25. Closed wire vocabularies are types, not integers with helper methods. Use standard checked
    `TryFrom<Raw>` decoding and lossless `From<Closed>` encoding. Derive ordinary error formatting and
    `source` plumbing with `thiserror`; handwritten error code is reserved for a measured constraint.
26. Repeated validation and limit branches indicate a missing construction. Typed limit descriptors,
    validated layouts, transition tables, and typestate should compress the common mechanism while
    preserving the precise field, requested value, maximum, source error, and failure offset.
27. Extension pressure tests an abstraction; it never licenses weakening one. A new case either obeys
    the existing semantic law, composes as a separate closed typed axis, or triggers a boundary
    redesign. Do not widen with catch-all raw values, semantic booleans, optional escape hatches,
    leaked backend variants, or caller-maintained coherence.
28. A validated boundary consumes its internal impossibilities. Public `Internal*` errors, silent
    `None`, `filter_map` omission, and release-only `debug_assert` recovery mean the proof leaks. Move
    validation earlier or change the representation until ordinary consumers see only real domain
    absence/failure and infallible proven operations.
29. Fixed binary grammar is declared once as typed wire records used by both read and write. Endian
    integer cells, checked borrowed casts, exact layout derives, and closed conversions replace offset
    forests and mirrored `read_u32`/`write_u32` code. Parser combinators are reserved for genuinely
    variable grammar where measurement shows they simplify borrowing and diagnostics.
30. Standard conversions own representation boundaries: `From<[u8; N]>` for all-bit-pattern-valid
    identities and `TryFrom<&[u8]>` with the standard slice error. Custom constructors/errors exist
    only for additional invariants or caller actions. Protocol domain labels are fixed typed constants
    emitted from one declarative registry with uniqueness evidence.
31. Canonical generation semantics and device locality are orthogonal owners. A semantic root contains
    only key, parent, and selected immutable object facts; its bytes/ID are identical on local and
    remote planes. A generation-bound locality map contains sparse promises, providers, tiers, and
    remote overlay bases. Changing placement never rebuilds or re-identifies semantic rows.
32. Latency boundaries are asynchronous and streaming; compute boundaries are borrowed and direct.
    Disk, network, and remote object adapters expose statically typed, bounded, backpressured streams
    whose items own physical buffer/credit leases. Validation, hashing, planning, and local-memory
    reads operate synchronously over those borrows. No payload is serialized into an intermediate
    message merely to cross from async I/O into pure compute.
33. Runtime equality checks become proof-bearing transitions. An untrusted request may name a
    generation, but one checked bind produces a demand borrowing its coherent `GenerationView`.
    Planning and execution accept the bound demand and cannot compare or mix unrelated roots again.
34. Identity personalization is typed infrastructure. Application crates never call a hasher with a
    naked protocol literal. A central declarative registry owns domain/protocol constants; a stateful
    typed preimage writer owns canonical field order and produces the branded identity.
35. Observability is a statically composed adapter. The unit probe `()` compiles away; an optional
    bounded local flight recorder stores typed events without formatting; a server-only adapter batches
    correlated OpenTelemetry traces, metrics, and logs. Telemetry never owns data-plane credits, blocks
    publication, exports synchronously, or adds unbounded-cardinality metric labels.
36. Fallible work returns evidence; it never aborts for convenience. Production and scenario code do
    not use `expect`, `unwrap`, or manual `panic!`. Test/scenario functions propagate typed errors with
    step, expected/observed transition, rejected value, causal source, and flight-recorder context.
    Reporters render or export failures; the operation that discovered them does not erase them into a
    panic string. Pure equality assertions remain test assertions, not error-handling substitutes.
37. Hot traversal is shaped for branch prediction before minimizing one-time construction allocation.
    Common-state scans use one forward cursor and strongly biased comparisons; they do not repeat
    binary searches, decode tags twice, or rebuild optional heads per row. A compact routing/index
    allocation is earned when measured retained bytes buy materially fewer unpredictable branches.
    Random lookup and sequential scan may consume the same representation through different cursors.
38. Durable remote truth is an immutable publication log plus compact atomic head/index, not a fleet
    of mutually authoritative warm copies. NVMe materializations are disposable, demand-placed
    projections; rendezvous placement is an efficiency hint, never correctness state. A write is
    acknowledged only after its object/change bytes are durable and its typed head transition is
    linearized. Busy heads batch compatible commits without weakening per-publication identity.
39. Content addressing must not create a remote round trip per graph edge. Roots carry enough compact
    closure/range metadata to discover and batch the next missing objects; clients retain roots and
    current demanded payloads, then fault immutable payloads by typed projection. Server compaction is
    performed once, published as another immutable artifact, and downloaded by replicas instead of
    burning equivalent CPU on every replica.
40. Canonical immutable bytes are the primary collection owner. Roots, locality maps, manifests, and
    indexes validate once into borrowed typed views over caller, store, mmap, or leased backing; they do
    not deserialize into parallel forests of `Vec`/`Box` owners. Builders preflight an exact typed
    layout and stream into caller-provided output. When ownership must escape, an optional
    monomorphized owner wrapper is generic over the concrete byte owner; self-reference machinery is
    admitted only when an HRTB callback cannot express the lifetime and measurement proves a copy was
    removed.
41. Static facts execute at compile time. Protocol geometry, closed registries/flags, const-inline
    capacities, storage/rank policy, and mutation matrices are associated constants or const tables;
    invalid static configurations fail to instantiate where Rust can express the proof. Dynamic
    branches and errors remain only for dynamic input. Const genericity is charged for monomorphized
    text size and may not become decorative type arithmetic.
42. Control flow is a measured resource. Each handoff ranks production and test functions by decision
    count and length, then manually reviews the worst paths. Declarative tables, homogeneous data
    lanes, and linear ownership transitions must reduce the actual branch graph; comment-heavy indexed
    delegation and splitting one conditional ladder into single-use helpers are abstraction debt.
    Production and test code do not silence the workspace complexity/length gates; crossing one is a
    design review that must remove decisions or expose a smaller independently testable invariant.
43. Typestate changes authority, not representation. Unchanged runtime facts live once in a generic
    record; uninhabited state markers select only legal consuming transitions and contribute no tag,
    allocation, drop behavior, or accidental auto-trait bound. Compile-fail tests prove illegal
    transitions and layout tests prove the marker is free. Do not duplicate whole records behind a
    succession of private `Seal` fields. Conversely, do not add a state parameter when there is only
    one consumer or when the alleged transition performs no proof, ownership transfer, or external
    effect: delete that phase or bind it to the real effect.

## Layer direction

```text
nudox-id <- nudox-schema <- nudox-frame <- nudox-view
    ^            ^              ^
    +----- nudox-object <- nudox-root <- nudox-hydration <- nudox-store-memory
                          ^
nudox-operation <- nudox-runtime <- nudox-workflow
```

An arrow points from a consumer to a dependency. Cycles are forbidden. Implementation crates may
depend on vocabulary crates; vocabulary crates never depend on a runtime, store, transport, or app.

## Data identities

- `ContentId<D>`: BLAKE3 identity of canonical logical bytes in domain `D`.
- `ArtifactId<E, D>`: identity of encoded bytes under encoding `E`; never confused with content.
- `SchemaId`: compact, collision-checked closed protocol tag; not a content hash.
- `OperationId`: compact, collision-checked operation tag; not a content hash.
- `GenerationId`: immutable published root identity.
- `ObjectRef<DomainTag>`: copyable descriptor containing kind, schema, content identity, and raw length.
- `SlotId<T>`: process-local generational handle; never serialized as object identity.

Do not give every semantic ID cryptographic width. Width follows the identity's threat and
coordination model: content/artifact/generation identities are hashes, while closed schema and
operation registries use compact tags with build-time uniqueness checks.

`RootEntry<ObjectDomain>` exposes semantic `key`, `parent`, and `object` facts directly. It does not
contain `Residence`. A checked borrowed composition of `GenerationRoot` and its matching `ValidatedLocality`
answers whether that semantic object is resident, promised, or a local overlay over an exact remote
base. Provider/storage-tier extensions therefore cannot leak into root hashing, diff, or closure.

The durable root and locality artifacts are packed canonical bytes. Their ordinary APIs are borrowed
views which reconstruct semantic entries by value while retaining compact parent/route coordinates in
the backing. A local builder and a remote compactor emit the same representation into different
concrete owners; neither side pays to materialize three Rust collections before it can scan.

## Validation boundary

Untrusted bytes become `ValidatedBytes<S>` only after structural validation: magic, version,
declared length, offset arithmetic, alignment, counts, discriminants, section non-overlap, and
configured maxima. Typed views borrow `ValidatedBytes`; they do not repeat whole-object validation.
Validation must be linear in header/TOC size, not payload size, unless content hashing is requested.
Header and descriptor bytes borrow as typed immutable wire records after a checked cast. Semantic
validation then produces a witness over those records; access never reparses scalar fields or repeats
fallible slice arithmetic.

## Resource model

Wave 1 separates immutable bytes, bounded owned slabs, request scratch, and publication roots.
Credits are backed by actual slots. Memory pools retain capacity and return it on `Drop`; pool
exhaustion is typed. Queue messages carry compact handles or `ObjectRef`, never bulk object graphs.

I/O adapters reserve byte and item credits before issuing reads. Each yielded chunk/frame carries the
non-cloneable lease that backs its bytes until validation and ownership transfer complete. Dropping a
future, stream, item, or consumer returns credits exactly once. Async fan-out is bounded by physical
credits and remote policy; completion order may vary, but semantic output order is reconstructed from
typed sequence/range keys rather than head-of-line blocking.

Transparent numeric/byte identities implement `Deref` to their scalar/array representation plus
the relevant `AsRef`/`Borrow` traits. `ObjectRef`-style aggregates expose their already-valid typed
fields directly. Constructors remain only where they establish cross-field coherence.

Inline storage is for small statically proven bounds, not demand-sized payloads. Demand-shaped data
is preferably a borrowed slice, iterator, or view over caller-owned reusable scratch. A surviving heap
owner documents why ownership must escape, its exact capacity bound, allocation point, rejection
behavior, and how it avoids a second materialization.

Allocation count alone is not a performance objective. Reviews record setup allocations and hot-loop
loads, comparisons, search steps, and branch distributions together. Prefer one compact immutable
index built once when it turns two logarithmic searches per row into one random lookup or a linear
merge cursor whose resident path is predictably biased.

## Source topology

Each crate root declares policy and reexports a small public vocabulary. Encoding, validation,
layout, state transitions, indexes, admission, metrics, and tests live in separate files. A module
split must correspond to an invariant or replacement boundary; it is not license to mirror every
type into a file or inflate the public API.

## Compatibility

Unknown required fields or schemas fail closed. Unknown optional sections may be skipped by declared
length. Encoders are canonical. Decoders accept only explicitly supported versions. Tests include
golden byte vectors, mutation/property tests, and truncation at every boundary.

Generated state-machine policy is defined in [`STATE_MACHINE_CODEGEN.md`](STATE_MACHINE_CODEGEN.md).
Ragel may own a measured regular control projection; it cannot replace Rust ownership/typestate or
the memory-order proof for lock-free transitions, and the Colm runtime never enters the lean client.

The shared borrowed artifact/region contract is defined in
[`PACKED_COLLECTIONS.md`](PACKED_COLLECTIONS.md). It is the replacement boundary for owned root,
locality, manifest, and immutable-index collection forests.

## Superseded legacy choices

The deleted `docs/INDEX-PLAN.md` revision 3 is authoritative where it explicitly superseded the
older librarification master. Workspace2 therefore has no TerminusDB, Doltgres, Ladybug, Postgres,
public IPFS, client-side graph engine, or registry archive as a source of truth. In particular,
Terminus is not a hot tier or fallback: graphs and search structures are disposable projections of
typed immutable objects and generations. Any future accelerator is an adapter behind derived-view
contracts and can disappear without changing object, root, operation, or workflow types.

Bulk transport is not frozen in Wave 1. The object/range contracts are compatible with iroh/Bao or
another verified range transport, but transport technology never becomes logical identity. HTTP or
another control plane carries small typed control messages; bulk objects remain content addressed.

The remote storage direction follows the useful parts of Cursor Continuity: an object-store WAL as
durable truth, atomic compare-and-swap publication, ordinary local NVMe materializations, advisory
rendezvous placement, batched durable updates, and one compaction result reused by all replicas. It
does not copy Git packfiles or repository semantics. The local-client direction follows partial-clone
semantics: keep current roots/descriptors and fetch missing immutable payloads in batches on demand,
without allowing remote absence to make already-proven local facts unusable.
