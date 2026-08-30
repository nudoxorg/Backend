# Production Rust design notes

These are techniques to adapt, not dependencies to copy wholesale.

## uv

uv recommends `cargo nextest` and uses reviewed `insta` snapshots plus tooling to apply CI-generated
snapshots. We should use nextest for fast partitioned execution and snapshots only for stable complex
diagnostics/golden plans; exact typed assertions remain the authority for invariants. Snapshot updates
must be diff-reviewed, never blindly accepted.

Source: https://github.com/astral-sh/uv/blob/main/CONTRIBUTING.md

## iroh and number 0

Iroh's async post documents concrete failures from detached Tokio tasks, swallowed panics, blocking
database work on executors, and cancel-unsafe channels. The applicable rules are structured task
ownership, explicit runtime capabilities, observed join results, cancel-safe queue operations, and a
separate path for blocking work.

Source: https://www.iroh.computer/blog/async-rust-challenges-in-iroh

Iroh's 1.0 retrospective is also a warning against compatibility-driven abstraction fatigue. It moved
away from broad IPFS compatibility toward a small QUIC plus BLAKE3-verified-streaming core that could
fit real mobile deployments. Our content/root grammar should remain ours; IPFS/IPLD is an adapter or
interop format only when it pays measured product rent.

Source: https://www.iroh.computer/blog/the-road-to-iroh-1-0

`n0-future` centralizes cross-platform async choices. Its portability/cancellation curation is useful,
but its boxed future/stream aliases conflict with our monomorphized hot core. We can adopt the idea of
one adapter facade without adopting erasure.

Source: https://github.com/n0-computer/n0-future

### Blob bytes, verification sidecars, and zero-overhead locality

Current `iroh-blobs` keeps a blob's bytes opaque and unaltered, while BLAKE3 range-verification
outboards remain separate metadata. Its collection grammar is simply concatenated 32-byte links, so
known-width indexing needs no per-element serializer, delimiter, or owner. That reinforces two Nudox
rules: the typed `ObjectRef`/request selects the grammar outside canonical bytes, and partial-fetch
proofs are sidecars rather than fields copied into every root/locality record. A locality artifact can
therefore omit self-description already carried by its verified descriptor unless an independently
measured standalone use case earns it.

The 0.90 rewrite also reports that its previous friendly in-process RPC path added allocations, and
replaced the low-level/friendly split with an interface designed to add no overhead beyond the async
channels an in-process boundary already needs. Apply the same standard here: local object, compiler,
and index calls use the same semantic request/result grammar as remote calls, but they do not encode,
allocate a transport envelope, or erase types merely to imitate a network hop.

Sources:

- https://docs.iroh.computer/protocols/blobs
- https://www.iroh.computer/blog/iroh-blobs-0-90-changes

## Foyer and redb: projections are policy, immutable coordinates are truth

`foyer` is a useful remote/node cache reference because it deliberately separates replaceable cache
algorithms, memory and disk engines, zero-copy in-memory access, and opt-in observation. Do not import
its whole cache into the portable client core; steal the boundary: an object's typed immutable
coordinate does not change when a policy promotes it from object storage to NVMe to RAM.

`redb` is a useful local persistence reference rather than the sovereign object format. It exposes a
zero-copy typed read API over copy-on-write B+trees, supports concurrent nonblocking readers, and can
run `no_std` over a caller-supplied `StorageBackend`. That makes a good later adapter seam for local
mutable catalogs. Canonical objects, roots, and locality artifacts must remain directly streamable
and range-verifiable outside any database page format.

Sources:

- https://github.com/foyer-rs/foyer
- https://github.com/cberner/redb

## s2n-quic

s2n-quic layers ordinary unit/integration tests with snapshots, Bolero properties, replayable fuzz
corpora, Loom concurrency permutations, Kani proofs, Monte Carlo network simulations, DHAT heap
profiles, Miri, LLVM coverage, benchmarks, and interoperability. This is the closest testing model for
our protocol and lock-free boundaries.

Source: https://aws.github.io/s2n-quic/dev-guide/ci.html

Adaptation:

- one property harness should run deterministically under normal tests, become a fuzz target, and be
  eligible for Kani on bounded representations;
- every discovered failure becomes a small permanent corpus case;
- Loom calls the production atomic transition core through an atomic shim;
- simulated loss/reorder/cancel runs use deterministic seeds and virtual time;
- memory and work counters are CI artifacts, not prose claims.

## Materialize

Materialize separates fast Rust tests, data-driven rewriteable cases, and long-running stability/load
tests. Its data-driven DSL keeps large input/output matrices out of monolithic source files, and its
persist design uses a simple in-memory implementation behind the same semantics for testing.

Sources:

- https://github.com/MaterializeInc/materialize/blob/main/doc/developer/guide-testing.md
- https://github.com/MaterializeInc/materialize/blob/main/doc/developer/design/20220330_persist.md

Adaptation: create compact declarative case files for wire mutations, workflow transition matrices,
and hydration coverage. The parser for a test DSL must be test-only and must not recreate production
logic. Keep PR gates fast; run large concurrency/fuzz/endurance suites separately with fixed seeds and
persist failures as replay cases.

## Deterministic async simulation

Madsim and Tokio Turmoil demonstrate that failures which are nearly impossible to reproduce on a
wall-clock executor become ordinary test inputs when time, scheduling, network, and storage effects are
seeded capabilities. RisingWave's experience reinforces the product rule: a simulator is valuable only
when it drives the production state machines and I/O boundaries, not a second mock implementation.

Sources:

- https://github.com/madsim-rs/madsim
- https://github.com/tokio-rs/turmoil
- https://www.risingwave.com/blog/applying-deterministic-simulation-the-risingwave-story-part-2-of-2/

Adaptation:

- keep protocols, reducers, stream phases, leases, credits, and errors executor-neutral;
- put virtual clock/network/filesystem capabilities in a nested harness workspace so the lean client
  never acquires simulator or server-runtime dependencies;
- model delay, reorder, duplicate, disconnect, process restart, cancellation at a poll boundary,
  short I/O, and torn durable append as explicit schedule values;
- report the seed, schedule position, typed causal error, resource-conservation snapshot, and flight
  recorder tail together, making every failure a one-command replay artifact;
- compare public observable state after every scheduled action, not only at scenario completion.

Do not put Madsim's broad dependency substitutions into the core graph. Its approach is useful
research, but replacement versions can constrain unrelated libraries. Turmoil is the first adapter to
evaluate for a later nested network/filesystem harness because it is narrower; Wave 1 keeps a tiny
manual-waker and explicit-fault driver until a measured harness deletes more code than it introduces.

## Deliberate non-adoptions

- No general IPFS/multihash/multiaddr stack in the semantic core.
- No boxed future/stream facade in hot public contracts.
- No snapshots as substitutes for exact error/state assertions.
- No mock state machine that duplicates the implementation. Model tests use a visibly simpler
  specification and compare after every command.
- No property generator that mostly produces invalid garbage. Generate valid structures through
  public constructors, then mutate exactly one invariant for negative cases.
- No simulator-specific executor, clock, socket, or filesystem type in a core public signature.
- No random fault without its seed, exact schedule position, and permanent replay representation.

## Closed enum mechanics

Strum's derive-only, `no_std`-compatible surface is available for mechanical enum iteration, count,
discriminants, and static diagnostic names. Use `EnumCount`/`VariantArray` to derive exhaustive test
domains and fixed inline capacity, `EnumDiscriminants` to avoid hand-mirrored event-name enums, and
`IntoStaticStr` only at the reporter boundary. Do not use string parsing/display as the protocol wire
format, and do not add a derive when an ordinary exhaustive match carries real semantic policy.

Source: https://docs.rs/strum/latest/strum/

## Const programming and closed mutation grammars

qti3e's const-friendly `BitFlags` experiment is a useful technique study: it seals primitive
storage choices, derives a finite bit universe from enum positions, keeps operations const, and
contains representation conversion in one audited place. Its public unsafe flag traits and union
conversion are not a dependency or a default design for Nudox.

Source: https://gist.github.com/qti3e/24a8d8a1b970fcf3e11e842855ef177c
(revision `f99ee962911319cf341150e1c06c8b741c7944a7`, 2026-07-30)

Apply the narrower principle:

- represent finite protocol geometry, registries, mutation classes, and legal flag sets as closed
  associated constants or const tables; consume them with compact runners rather than rediscovering
  static facts with runtime offset ladders;
- prove const-table coverage, record widths, offsets, and static capacities at compile time when
  stable Rust can express the proof; a run-time test still checks the public constants against the
  declared wire record;
- treat each const generic or static policy as a code-size budget. It must remove measured state or
  work, and its monomorphized text cost belongs in the layout evidence;
- use a sealed primitive and a single private representation proof only if a concrete safe design
  loses under measurement. Exact layout assertions and Miri remain required before any unsafe
  conversion is accepted.

## Const evaluation as a policy plane

qti3e's July 2026 `BitFlags` experiment demonstrates several useful techniques in one small design:
enum discriminants are bit positions; a macro derives the required bit width; storage is statically
selected from sealed primitive integer representations; and union-based zero-extension/truncation
makes construction, union, insert, remove, retain, contains, and iteration setup usable in `const fn`.
Its compile-time division checks reject a flag/storage mismatch before runtime:
https://gist.github.com/qti3e/24a8d8a1b970fcf3e11e842855ef177c

The adaptation is the compile-time authority, not automatic adoption of those public unsafe traits.
Use const/associated-const tables for protocol field geometry, mutation corpora, closed flags, rank
stride, and storage capacity; use inline const assertions for static monomorph validity. The Rust
Reference confirms inline const blocks can capture generic parameters and are evaluated at compile
time when reached: https://doc.rust-lang.org/reference/expressions/block-expr.html#const-blocks

Stay conservative about type-level arithmetic. Stable const parameters remain restricted inside type
expressions, while `generic_const_exprs` and its newer successor machinery are explicitly incomplete:
https://doc.rust-lang.org/reference/items/generics.html#const-generics and
https://github.com/rust-lang/rust/issues/76560. Prefer stable associated constants and inline checks;
use nightly generic-const expressions only inside an isolated measured crate when they delete runtime
state or invalid instantiations enough to justify compiler coupling. Every const-policy candidate also
reports monomorphized text size—runtime bytes saved by emitting many near-identical functions is not a
lean-client win.

## Compile-time protocol registry, not distributed runtime registration

Independent planes adding identity domains create merge pressure on the sealed `nudox-id` table, but
the obvious distributed-registry mechanisms weaken the actual invariant. `linkme` gathers const
elements into linker sections and exposes a runtime slice; it does not make duplicate durable labels
a type error, depends on platform linker behavior, and has documented dead-section edge cases:
https://github.com/dtolnay/linkme and https://github.com/dtolnay/linkme/issues/36. `inventory` owns an
atomic linked registry behind erased nodes and unsafe initialization machinery, which is the opposite
of a zero-state canonical identity vocabulary: https://github.com/dtolnay/inventory/blob/master/src/lib.rs.

Do not turn linker symbol collisions into a uniqueness proof. Rust 2024 marks `export_name`,
`no_mangle`, and `link_section` unsafe because collisions can cause undefined behavior, not a portable
diagnostic: https://doc.rust-lang.org/reference/abi.html#the-no_mangle-attribute. Stable const-generic
parameters also cannot directly use `[u8; 16]`; packing a readable durable label into `u128` would move
the same registry burden into less auditable literals:
https://doc.rust-lang.org/reference/items/generics.html#const-generics.

Keep protocol-domain labels in one declarative compile-time table until an extension requirement
falsifies that choice. Plane managers may propose one row, but the root integrates registry changes
serially and reruns pairwise domain/encoding uniqueness. Revisit only with a cross-platform specimen
where two isolated crates declaring the same 16-byte label fail deterministically before execution,
without a runtime registry, erased node, unsafe collision, or shipping dependency. Merge contention
alone does not justify runtime state or weaker canonical identity.

## Packed borrowed ownership

`zerocopy` 0.8's `KnownLayout`, `FromBytes`/`TryFromBytes`, `Immutable`, endian cells, and typed
references are the first choice for roots, locality maps, and manifests. They let one canonical byte
owner expose checked borrowed records without a parallel object graph or application `unsafe`:
https://docs.rs/zerocopy/latest/zerocopy/

An owned validated artifact should first offer an HRTB callback which lends a view from its concrete
owner. If a dependent view truly must be retained inside a movable owner, `self_cell` is the smallest
candidate to measure: it is `no_std`, stable, macro-rules based, Miri-tested, and explicitly charges a
heap allocation for movability. `ouroboros` is more flexible but proc-macro heavier. Neither is a way
to avoid the byte owner itself, and neither enters the core merely to make lifetimes look convenient:

- https://docs.rs/self_cell/latest/self_cell/
- https://docs.rs/ouroboros/latest/ouroboros/attr.self_referencing.html

`slice-dst` can fallibly allocate a header plus one homogeneous trailing slice with unwind-safe
initialization. That is useful for one header/array object, but a locality artifact has multiple typed
regions and already needs a stable canonical binary grammar. Prefer one byte artifact plus borrowed
typed sections; adopt a custom DST only if it measurably removes more metadata/branches than it adds
construction and unsafe surface: https://docs.rs/slice-dst/latest/slice_dst/

### Succinct monotone row sets

- `sux` provides pure-Rust bit vectors, rank/select indexes, Elias–Fano monotone dictionaries, typed
  backend composition, and unaligned conversion. It is a serious layout-lab reference, not an
  automatic production dependency. Its backend composition can target borrowed/unaligned storage,
  while its usual examples retain several boxed index inventories: https://github.com/vigna/sux-rs
- `vers-vecs` is another pure-Rust rank/select and Elias–Fano reference. Its own guidance reports
  large gains from BMI2/popcnt on x86, which makes it valuable for measuring static CPU-policy
  monomorphs but unsuitable as an unqualified portable-client promise:
  https://github.com/Cydhra/vers
- Quickwit's `bitpacking` implements scalar plus SSE/AVX block codecs for ordinary and increasing
  integers. Its SIMD-specific formats differ, so a durable grammar cannot silently depend on host
  feature selection: https://github.com/quickwit-oss/bitpacking
- Roaring has a cross-language portable serialization and rank/select APIs, but its container
  directory and FFI/native ownership are likely excessive for one root-bounded `u32` set. Retain it as
  a density-adaptive reference: https://github.com/RoaringBitmap/RoaringFormatSpec
- `epserde` can replace large owned zero-copy sequences with borrowed slices and mmap-backed views,
  which is relevant to server-local projections. Its documentation explicitly says it performs no
  validation or padding cleaning and relies on unsafe/native-layout contracts, so it is not the
  untrusted network artifact boundary. Measure it only as a machine-local projection adapter over a
  separately authenticated artifact: https://github.com/vigna/epserde-rs

For locality, benchmark membership-with-rank rather than membership alone. Rank removes explicit
payload indexes: exception rank selects a class bit, class rank selects a provider/overlay lane, and
overlay-present rank selects the descriptor lane. A cold validated header may choose among wire
grammars, but each hot path must enter one concrete monomorph before iterating.

Two additional layouts sharpen the rank-directory experiment:

- Rank9 interleaves each 512 raw bits with an absolute rank word and one packed relative-rank word.
  That spends 25% over the raw bits but normally fetches the directory with the data and answers rank
  with one final-word popcount. `sbits` documents this exact 10-word block and also provides
  partitioned Elias–Fano; use it as an executable reference, not as an owned/serde artifact type:
  https://docs.rs/sbits/latest/sbits/bitvec/index.html
- `bitm::RankSelect101111` reports a 3.125% rank overhead. It and BSuccinct's reproducible benchmark
  workspace are the lower-overhead comparison to a simple u32-per-64 directory:
  https://docs.rs/bitm/latest/bitm/ and https://github.com/beling/bsuccinct-rs

The locality lab must charge the whole artifact and distinguish cache misses from arithmetic. Compare
plain prefix-per-256 (up to four scalar popcounts), prefix-per-64 (one popcount, 6.25% raw-bit
overhead), interleaved Rank9 (one nearby header, 25%), and a lower-overhead two-level directory. The
sequential cursor is a separate baseline: it carries semantic ranks and performs no rank query at all.
SIMD is a static CPU-policy monomorph over the same canonical bytes, never a host-dependent format.

## Cursor Continuity, secure index reuse, and Git partial clones

Cursor's Continuity design makes an object-store write-ahead log the durable source of truth while
ordinary repositories on local NVMe are disposable materializations. Atomic compare-and-swap on a
small WAL index linearizes publication; rendezvous hashing places warm copies but is not required for
correctness; compatible updates are batched; and expensive compaction is performed once before its
result is distributed to replicas. The important adaptation is separating durable immutable facts
from horizontally scalable hot projections, not adopting Git's packfile format.

Source: https://cursor.com/blog/git-at-any-scale

The same article is a sharp warning against naive object-level distribution: graph traversal reveals
the next key only after reading the current object, so a remote key-value fetch per edge serializes
latency. Our generation/root formats therefore need compact closure/range manifests that let a client
batch the missing frontier. Content addressing establishes identity; it does not by itself establish
an efficient access plan.

Cursor's secure-indexing design adds two reuse mechanisms: walk only divergent Merkle branches, and
choose a close existing index as a starting projection before applying exact cryptographic diffs.
Reuse is never trusted merely because it is similar—access and exact content checks remain separate.
For our later index plane, a similarity sketch may select a seed generation, while typed root diff and
content verification decide which projection fragments actually survive.

Source: https://cursor.com/blog/secure-codebase-indexing

Git partial clone demonstrates the client product shape: retain graph metadata needed for common
operations, omit cold payload blobs, and batch missing-object requests when an operation actually
needs bytes. It also shows why an overly thin metadata clone can backfire: treeless history queries
cause repeated tree faults and redundant transfers. Our client tiers must be chosen per operation
profile, with roots/descriptors kept when they prevent serialized remote discovery.

Source: https://github.blog/open-source/git/get-up-to-speed-with-partial-clone-and-shallow-clone/
