# Greenfield compiler and semantic IR plane

This is a from-scratch design. `workspace/compiler`, `workspace/ir/model`, and `workspace/ir/vcs` are
not compatibility targets. Seven frontends in one crate, serde oracle documents, one enormous owned
`Entry` graph, runtime-tagged local/export/import indices, boxed error erasure, string producer IDs,
process JSON lines, Pijul-shaped IR history, and server sandbox types are anti-pattern evidence. Old
features may inspire product experiments, but no old type, wire byte, or behavior is presumed worthy.

The new capability is: turn immutable source and toolchain facts into compact canonical semantic
fragments through an idempotent, vertically elastic stage graph; publish those fragments as ordinary
typed objects; and let local or remote consumers incrementally request only the stages and fragments
they need. The portable client does not contain compilers. It can acquire a signed compiler capability
bundle on demand and speak the same typed job/artifact protocol to local or remote execution.

## Laws

1. A compile result is a pure function of a complete `CompileRecipe`: source root, dependency
   interfaces, language mode, target, toolchain image/content, feature/config facts, stage recipe, and
   output schema. Ambient paths, environment, clocks, hosts, and worker identity never enter the key.
2. Acquisition and sealed computation are separate capabilities. Acquisition may fetch declared,
   hash-pinned inputs; sealed execution sees an explicit filesystem/env/clock/random/network budget.
3. The unit of recomputation is a changed semantic fragment, not a package-shaped heap graph. Package
   publication is a small manifest over immutable fragments.
4. Language heterogeneity dispatches once through a closed compile-time registry into concrete,
   monomorphized adapters. There is no public `dyn Producer`, `Box<dyn Error>`, JSON oracle protocol,
   or language match inside entity/type traversal.
5. Frontends borrow parser/oracle arenas while lowering. They emit through one-pass typed builders;
   borrowed names, spans, and lists are copied at most once into canonical output regions.
6. The semantic IR is a compact indexed relation, not an object-oriented syntax tree. Dense typed
   entity IDs, interned byte/string tables, list pools, and family-specific payload lanes make invalid
   cross-kind references unrepresentable and scans cache-coherent.
7. Mutable compilation state is worker-owned. No `Arc` is needed merely because work is parallel;
   use scoped parallelism over disjoint modules and ownership transfer between stages.
8. Vertical scaling changes admitted memory/CPU lanes and concrete toolchain workers, never artifact
   identity. Fleet width and machine size are independent scheduler inputs.
9. Stage I/O is a bounded typed stream of leases. Pure parse/lower/hash work is synchronous over
   borrows. No stage serializes to JSON or collects all logs/output merely to cross an async boundary.
10. Reuse is fact-driven. A stage artifact is reused because its complete recipe identity matches, not
    because a mutable cache entry looks fresh. Eviction cannot affect correctness.
11. Diagnostics and tracing are outputs with bounded schemas and stable citations to stage/entity/
    source facts. A warning printed in a worker log is not a machine-readable compiler result.
12. The index consumes published IR deltas; compiler availability never affects already-published
    index correctness. A failed compiler produces no half-generation.

## Source topology

Use two nested workspaces so semantic vocabulary cannot depend on execution or language SDKs:

```text
domains/ir/
  Cargo.toml
  crates/
    nudox-ir-vocab/          no_std entity kinds, typed IDs, source/type vocabulary
    nudox-ir-format/         no_std canonical fragment/manifest wire records
    nudox-ir-view/           borrowed validated fragments and typed cursors
    nudox-ir-build/          alloc caller-arena builders and canonical preparation
    nudox-ir-diff/           borrowed manifest/fragment delta and closure proofs

planes/compiler/
  Cargo.toml
  crates/
    nudox-compile-vocab/     no_std recipe/stage/job/capability vocabulary
    nudox-compile-registry/  declarative frontend registry and closed dispatch generation
    nudox-compile-driver/    sync typed stage graph over concrete frontend families
    nudox-compile-schedule/  std owner scheduler and physical-credit admission
    nudox-compile-publish/   std artifact/publication integration
  frontends/
    rust/
    typescript/
    ...                     one nested crate/workspace member per earned adapter
  adapters/
    process/
    sandbox/
    object-store/
    server/
    bundle-loader/
```

Frontend crates depend inward on compile/IR vocabulary and builder contracts. IR never depends on a
frontend, compiler, registry, sandbox, filesystem, GUI, or index. Sandbox is a replaceable execution
adapter, not the definition of a compile job. Generators, mutation corpora, deterministic schedules,
and independent semantic oracles live in ordinary owning crates' top-level `tests/` trees. There are
no test-only crates or shipping fixture APIs.

## Identity and compile recipe

Extend the central declarative identity registry with typed domains:

```text
SourceTreeId, SourceFragmentId, DependencyInterfaceId
ToolchainId, CompilerBundleId, CompileRecipeId, CompileJobId
StageArtifactId<Stage>, IrFragmentId<Language>, IrManifestId
DiagnosticSetId, CompilePublicationId
```

A `CompileRecipe<LanguageMode>` is one canonical fixed/packed record plus sorted referenced sets. Its
fields are validated semantic types and public after construction. A staged builder is justified only
for required facts whose absence would make hashing invalid; it uses zero-sized state markers over one
unchanged representation and compile-fail tests prove missing fields cannot finalize. Do not produce a
fluent setter for every optional fact.

The recipe includes:

- exact source root and selected source-fragment closure;
- dependency interface roots, not mutable registry coordinates;
- typed language dialect/edition/config and target facts;
- exact compiler/toolchain bundle identities;
- ordered environment allowlist and declared input capabilities;
- stage graph version and requested output projections;
- deterministic resource class only when it changes semantic behavior; ordinary scheduling limits do
  not salt identity.

`CompileJobId` derives from the recipe plus the requested stage/output closure. Retry, host, priority,
tenant, and deadline are scheduling metadata and cannot change the result identity.

## Declarative frontend registry and static dispatch

One macro invocation is the authority for language tag, concrete adapter type, supported dialects,
stage capabilities, bundle feature, and default resource profile. Prefer a declarative macro when the
grammar remains tabular; use a proc macro only if diagnostics or syntax cannot be expressed clearly.
The expansion generates:

- closed `Language`/`FrontendFamily` and wire conversions;
- exhaustive `CompileRequest`/`CompileResult` application-boundary enums;
- a single dispatch match that calls generic `drive<ConcreteFrontend>`;
- const capability tables and uniqueness assertions;
- test enumeration and feature/dependency budget cases.

It must not generate tagless-final/GADT machinery merely for novelty, hide control flow in opaque
macros, erase associated errors, or force every binary to monomorphize every language. A worker target
chooses a compile-time subset, and its generated enum contains exactly that subset. The dispatcher is
measured for code size.

The concrete frontend contract uses associated types with earned bounds:

```text
Frontend
  type SourceView<'source>
  type ParseArena
  type Parsed<'arena>
  type Error: Error + Send + Sync
  const LANGUAGE: Language
  fn parse(...)
  fn lower(..., IrFragmentBuilder<'_, ...>)
```

No trait object crosses the core. An adapter-specific dynamic loader, if ever required, ends at a
validated binary frame boundary and cannot leak erased values into compile/IR crates.

## Semantic IR: fragments, not entries

The first IR records exported semantic interface and navigable source facts, not every parser node.
A package is an `IrManifest` over independently replaceable `IrFragment`s, normally one stable source
module or bounded strongly connected component. The manifest contains fragment IDs, dependency edges,
export summaries, source ranges, and a Merkle/range index sufficient for partial clone and changed-only
diff.

An `IrFragment` uses a structure-of-arrays layout:

```text
header: language, schema, recipe, source fragment, counts
entity lane: kind, name atom, parent/module, visibility, source span, payload ordinal
kind lanes: record/function/trait/impl/alias/... fixed compact records
type lane: interned type DAG nodes addressed by TypeId
edge lanes: typed relation kind + source/target entity/type IDs
atom bytes + offsets
list pool + offsets for params, generics, fields, bounds, attributes
diagnostic/source citation lane
```

`EntityId<Kind>`, `TypeId`, `AtomId`, `ListId<Element>`, `FragmentOrdinal`, `SourceFileId`, and
`SourceSpan` use the smallest measured width with checked construction. A local dense ID never doubles
as unresolved/import/wire state through reserved bits. Cross-fragment references are a separate
`ExternalRef<ExpectedKind>` containing a stable semantic key and optional resolved fragment/entity
target; unresolved is a typed diagnostic fact, not `?`, panic, or a surviving build-local index.

Closed entity kind determines the payload lane. Validation proves every ordinal, lane cardinality,
list range, type edge, and source span once. A typed cursor cannot return the wrong payload or shorten
silently. Unknown optional future lanes can be skipped; unknown required kinds fail closed.

Recursive types form an interned DAG. Builder-side hash-consing is worker-local and replaceable;
canonical emission sorts/deduplicates by typed structural preimage. Repeated small lists use a pooled
`EntityList`-style representation rather than one `Vec`/`Box` per entity. Candidate map/list structures
include `cranelift-entity`-style typed dense maps and pools, bump arenas, compact inline lists, and
specialized interning; each earns its dependency through layout and build-time evidence.

## Builder and borrow-checker strategy

The frontend owns its parser arena and the caller owns reusable IR scratch. Lowering receives both as
borrows, so source strings and parser nodes remain live without `Arc`. The builder has three explicit
regions:

1. dense typed entity/type staging in a worker arena;
2. caller-owned sort/intern scratch reused across fragments;
3. exact prepared canonical layout written directly into caller-provided output or a leased slab.

The builder returns typed IDs immediately, permits forward references through a separate fixup table,
and consumes that table during `prepare`. Validation errors retain source entity, expected kind,
observed target, and causal allocation/conversion errors. A prepared write proves output capacity
before touching bytes and becomes straight-line.

OXC-style arena ASTs are strong evidence for parser-local allocation, while rust-analyzer/Salsa shows
the value of stable summaries and change-aware queries. Neither becomes a universal dependency:
frontends may use their native arena/incremental engine behind the contract, and the canonical stage
artifact boundary prevents its ownership model from infecting IR or remote scheduling.

Self-referential crates (`self_cell`, Ouroboros, Yoke) are adapter candidates only when a dependent view
must escape its owner and an HRTB lending callback cannot express the use. Their allocation and drop
cost must be measured. Arena or `Arc` use is never inferred from “compiler code is complex.”

## Stage graph and incremental computation

The portable stage vocabulary begins closed and coarse enough to prove:

```text
Acquire -> Parse -> Summarize -> ResolveInterfaces -> LowerIr -> ValidateIr -> Publish
```

A concrete language can fuse stages at compile time when it has no reusable boundary. Stages are
typed by input and output artifact domains; illegal order is unrepresentable in the driver. The graph
is a const table used for validation/scheduling, not heap nodes or string step names.

Incrementality has two layers:

- **portable artifact incrementality:** content-addressed source fragments, interface summaries, and
  IR fragments are stable across jobs and machines;
- **frontend-local incrementality:** a native Salsa/red-green database or parser cache may accelerate a
  warm worker, but its memo table is disposable and never the remote source of truth.

A body-only change should not invalidate unrelated interface summaries. Dependency compilation reads
published `DependencyInterfaceId`s rather than complete dependency IR. The scheduler compares wanted
stage keys with durable have-sets and emits only missing work. It does not consult a “last compiled”
timestamp or scan accumulated packages.

## Vertically elastic execution

Admission reserves physical `CpuCredit`, `MemoryCredit`, `ScratchByteCredit`, input/output byte leases,
and one concrete toolchain slot before a job starts. Profiles are typed, measured resource envelopes,
not magic language constants. The owner scheduler may choose:

- a small local worker for parse/summary;
- a large-memory local or remote worker for a heavy semantic stage;
- multiple scoped module tasks inside one worker when the frontend proves disjointness;
- multiple stateless workers for independent jobs.

Job state is owner-threaded. Producer submissions cross one bounded queue carrying compact handles,
not source/IR graphs. The worker process owns arenas and streams output leases. No global mutex guards
all toolchains; no lock-free claim is made for a mutex-backed free list. Cancellation closes input,
reclaims credits exactly once, terminates the concrete worker if necessary, and publishes no result.

Idempotent duplicate jobs either join through a bounded owner-owned waiter set or observe the durable
artifact after publication. They do not execute under a distributed mutex. A crash after artifact
write but before head publication is reconciled by the durable job/publication journal.

## Sandboxing and capability bundles

The sandbox adapter consumes a sealed job and projects its explicit budget into a process, VM, or
other isolate. `NetworkOff` is real only if the selected mechanism omits the capability; typestate
must represent an external effect or ownership transition, never decorate a runtime flag. Process
stdout/stderr are bounded diagnostic streams, not the semantic output protocol.

A compiler capability bundle is a signed/content-addressed manifest of target, frontend subset,
toolchain objects, transitive native dependencies, minimum host capabilities, and compile protocol
version. Local installation downloads only the requested bundle and verifies every object. The lean
client base links only bundle vocabulary/verification, not the compiler or sandbox. Bundle unloading
and eviction never invalidate already-published immutable artifacts.

## IR history without a VCS subsystem

Do not port the legacy Pijul-shaped IR repository. `IrManifest` already names immutable fragments and
its parent generation. `IrDelta` is the sorted difference of two manifests plus changed exported
interface roots. History is the ordinary publication log of manifests. Branch/merge, when the product
needs it, composes generation roots and typed conflict objects; it does not require a second repository,
session, change archive, wire DTO layer, or mutable checkout cache.

Partial clone fetches the manifest, relevant export summaries, and demanded fragment ranges. Diff and
dependency invalidation operate on IDs/Merkle lanes without decoding unaffected fragments. A local
overlay publishes a separate root and retains the exact remote basis under the existing locality law.

## Observability and diagnostics

Typed probes cover admission, stage start/terminal, input/output bytes, arena high-water, reused/missing
stage artifacts, cancellation point, sandbox exit, IR validation, and publication receipt. Events are
lazy, bounded, and monomorphized; `()` disappears. Stage and error enums replace string `step`/`kind`.
OTEL export is batched in the server adapter and cannot hold job credits or block publication.

`Diagnostic` is a compact typed record with code, severity, stage, primary source citation, optional
entity/type target, and bounded related citations. Human prose lives in an atom/payload region or is
rendered from typed facts. Tests assert codes/operands/sources, not log text. Traces document important
decisions by recording exact facts once; comments still explain invariants and safety arguments.

## Phase graph and manager slices

Each phase has one Terra manager and explicitly selected real-Luna implementers. The manager freezes a
digest/LOC ledger, owns the rubric, independently breaks worker output, and cherry-picks only exact-path
green commits. Root performs the final cross-plane fork/review/fix. Eight is full planned completion;
nine/ten are measured same-scope stretch.

### C0 — compile and IR vocabulary

Create both nested workspaces; typed IDs; language/stage/entity/type vocabularies; declarative registry
prototype for two tiny test frontends; const capability matrix; compile-fail illegal dispatch/stage/
kind cases; exact size and code-size evidence. No real language SDK, sandbox, scheduler, serde, or async.

### C1 — compact IR fragment

Implement fragment format, prepared caller-output builder, borrowed validator/view, typed entity/type/
list cursors, string atoms, external refs, golden/mutation/property corpus, and zero-copy pointer/
allocation evidence. Use a synthetic frontend with adversarial recursive types and references.

### C2 — recipe and stage driver

Implement canonical recipe/job keys, static stage graph, concrete monomorphized driver, leased stage
outputs, cancellation, typed diagnostics/probes, and wanted/have skip behavior. Prove same facts produce
identical artifacts across worker scheduling permutations.

### C3 — first real frontend

Choose one bounded language slice after measurement—likely a Rust public-interface subset or an OXC
TypeScript subset. Borrow its native arena, lower directly into the IR builder, and compare source-size,
memory, allocations, and semantic corpus against an independent oracle. Do not import every frontend.

### C4 — vertical scheduler and sandbox

Add physical credit admission, owner-thread job lifecycle, bounded process/VM adapter, streamed
diagnostics/artifacts, crash/cancel/retry/idempotency matrix, and sustained heterogeneous load. No index
dependency.

### C5 — compiler capability bundle and acquisition boundary

Implement the signed/content-addressed compiler bundle manifest, target and protocol compatibility
validation, transitive native-object closure, on-demand acquisition, and explicit installation receipt.
The portable client links only bundle vocabulary and verification: it neither links a compiler nor gains a
sandbox/process/object-store dependency. Prove valid subset installation; wrong target, protocol mismatch,
missing/substituted object, partial download, invalid signature/digest, transitive closure breach, eviction,
and uninstall preserve published artifacts and return exact typed causes. Measure client dependencies and
release text. Do not add dynamic frontend discovery or make acquisition part of a compile recipe.

### C6 — published incremental consumer journey and plan integration

Close the public journey: a local or remote consumer names a complete recipe/output closure, acquires an
eligible bundle when needed, receives only validated complete immutable fragments/manifests plus bounded
typed diagnostics, then requests a changed source/interface closure and observes exactly the portable
artifact invalidation set. Exercise local/remote schedules, restart at every publication prefix, bundle
absence/eviction, heterogeneous profiles, cancellation, and duplicates; matching semantic facts produce
identical bytes and identities. The index consumes only published IR deltas and remains correct when
compiler acquisition/execution fails. This is integration and evidence closure, not a VCS/cache/scheduler
rewrite or new frontend.

## Plan closure

The plan is complete at C6 only when C0–C5 retain their stated negative space and the C6 public journey
reproduces all mandatory evidence from clean state twice. Closure requires frozen baseline/digest/LOC
ledgers; public cross-crate tests and compile-fail witnesses; canonical-byte/golden and mutation corpora;
allocator/copy/retained-live-byte, logical-work, and release-text measurements; scheduler cancellation,
crash, and credit-conservation traces; bundle acquisition/rejection evidence; and the end-to-end
local/remote incremental trace. Every phase must have zero blockers/majors, unused reserve, exact
commands, and a reviewer counterexample that fails a plausible weakened implementation. Green happy paths,
local parser caches, bundle availability, fleet placement, host identity, retry metadata, or eviction never
substitute for proof. Full plan completion is eight; nine and ten require predeclared, measured
same-direction robustness on another workload, platform, or failure profile and never additional scope.
