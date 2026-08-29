---
name: build-greenfield-compiler-ir
description: Scope and evidence rules for one greenfield workspace2 compiler or compact semantic-IR capability. Use with deliver-reviewed-rust-slice and manage-rust-swarm for typed recipes, compile-time frontend dispatch, arena lowering, packed IR fragments, incremental stages, vertical scheduling, sandbox adapters, or compiler bundles.
---

# Build the greenfield compiler and IR

Read `../deliver-reviewed-rust-slice/SKILL.md` completely, then
`../../../COMPILER_IR_GREENFIELD_PLAN.md`. A manager also reads
`../manage-rust-swarm/SKILL.md`, `../write-evidence-rubric/SKILL.md`, and
`../review-rust-gem/SKILL.md`. Read `../../../PACKED_COLLECTIONS.md` and
`../../../TESTING.md` for IR formats/views.
Before the first production edit in a new phase, the manager applies
`../calibrate-rust-agent-contract/SKILL.md` to the exact phase card.

The design is greenfield. Legacy compiler, IR model, and IR VCS are an anti-pattern/candidate corpus,
not a contract. Do not port their traits, behavior, serde types, indices, source transformations,
sandbox protocol, repositories, or tests without a new first-principles justification.

## One phase at a time

The root names one `C0`–`C6` capability. The Terra manager autonomously selects its smallest
observable child slice and freezes allowed paths, registry edits, dependencies, compile-time subset,
arena/retained/live bytes, allocations/copies, stage/hash work, binary text, cancellation/failure
terminal, and integration consumer before production edits.

For `C0`, the default surface is complete nested workspaces under `workspace2/domains/ir/**` and
`workspace2/planes/compiler/**`, plus separately authorized central registry additions. Use two tiny
synthetic concrete frontends to prove generated dispatch; do not import a real SDK, sandbox, runtime,
object store, serde, or server.

Tests and deterministic drivers stay in ordinary owning crates' top-level `tests/` trees. Do not
create `*-testkit` crates or expose shipping fixture APIs; a genuinely reusable simulator is an
adapter capability and must earn a production consumer independently.

Each C0 synthetic frontend must contribute a distinct checked registry row: its own closed language
tag and a capability or supported-stage difference exercised by a compile-time subset/failure case.
Two nominal types with identical no-op/delegating behavior, or one concrete frontend routed under two
tags, do not prove two consumers or generated dispatch.

The public C0 journey must make source forwarding observable. A constant result that lets optimized
code erase the input pointer/length is routing theater even when two tags return different constants.
Prefer lending the exact caller input or another semantic zero-copy result, then prove pointer/region
identity and exact unsupported-stage operands externally. If a registry/subset is represented by a
zero-sized type, consume or borrow it as a first-class capability value; a struct used only as an
associated-function namespace is not an earned abstraction.

The wildcards above are architecture, never edit authority. The manager enumerates each writable
manifest/source/test/registry file, compiles a normally formatted manual expansion/skeleton, names the
public cross-crate journey and literal commands, and derives numeric LOC/resource/text reserves. It
does not return discoverable card fields to root. Only a product-semantic fork, permanent wire choice,
or new dependency/unsafe/SIMD authority is escalated.

## Required design packet

Return before code:

```text
source/toolchain/dependency/recipe identity matrix
stage input/output and authority transition table
manual closed dispatch expansion and macro value proof
IR fragment lanes, typed coordinates, and one wire authority
frontend arena -> caller scratch -> canonical output lifetime diagram
invalidation cases and exact re-executed stage set
physical admission and cancellation conservation model
binary/dependency/monomorphization forecast
explicit legacy ideas rejected
```

## Compiler/IR-specific laws

- Complete recipe facts determine output; host, retry, priority, path, worker, and deadline do not.
- Acquisition and sealed compute are different real capabilities. Typestate must name the actual effect.
- Language dispatch happens once through a compile-time closed registry into concrete generic code.
- Codegen evidence uses a named release consumer plus retained control and confirms the input reaches
  the concrete path; `.rlib` metadata or a symbol name alone is insufficient.
- A macro requires two real concrete consumers, an auditable manual expansion, compile-fail diagnostics,
  and code-size evidence. Declarative wins unless source-spanned parsing truly needs a proc macro.
- Frontends borrow native parser arenas. They lower directly into caller-owned typed builders without a
  universal oracle document or owned intermediate tree.
- IR is packed fragments with typed dense IDs, kind lanes, type DAG, atom bytes, and pooled lists.
- Local build indices never encode unresolved/import/wire phases in reserved bits. Cross-fragment refs
  are separate typed semantic values.
- Portable artifact incrementality is truth. Salsa/red-green/parser caches are disposable frontend-local
  accelerators.
- Workers own mutable arenas; scoped parallelism borrows disjoint work. `Arc` must prove escaping life.
- Vertical scheduling reserves physical CPU/memory/scratch/input/output/toolchain resources up front.
- Diagnostics and probes are bounded typed outputs. stdout/stderr and log prose are not semantic results.
- Compiler success publishes complete validated fragments/manifests atomically or publishes nothing.

## Do / don't

```text
DON'T: trait Producer { type Oracle; fn invoke JSON; fn lower(&Oracle, ...) }
DO:    one generated family dispatch -> drive<ConcreteFrontend> over its borrowed arena

DON'T: Entry { Symbol<String, PathBuf, Box<_>>, Node<Vec<_>>, Kind }
DO:    typed dense entity lane + kind payload lanes + atom/list/type pools

DON'T: one EntryIndex with runtime reserved bits for local/export/import/resolved
DO:    EntityId<Kind> for local dense space; ExternalRef<ExpectedKind> for cross-fragment facts

DON'T: Box<dyn Error> because generic bounds are inconvenient
DO:    concrete associated error through the generic driver; one closed boundary error at dispatch

DON'T: cache the whole package because compile was expensive
DO:    reuse the exact matching source/interface/stage artifact; recompute only missing keys

DON'T: put process JSON lines, VM config, and worker pool in the job vocabulary
DO:    sealed typed job + replaceable execution adapter + leased artifact/diagnostic streams
```

## Stop triggers

Stop with exact evidence before:

- adding serde, async-trait, public `dyn`, erased errors, string stages/languages, or a universal AST;
- adding Salsa, a parser SDK, VM/process runtime, unsafe, SIMD, self-reference, or reference counting;
- adding a generic or const parameter without two consumers and a branch/copy/invalid-state win;
- retaining per-entity `Vec`/`Box`/`String` owners or allocating after canonical layout preparation;
- adding a typestate that performs no proof/effect/ownership transition;
- changing foundation/index/workflow APIs without a separate named checkpoint;
- preserving a legacy compiler/IR behavior by inertia;
- claiming incrementality, determinism, vertical scale, isolation, or lock-freedom without a falsifier.

## Proof gates

Every applicable slice proves registry uniqueness/exhaustiveness, compile-time subsets, compile-fail
illegal stage/kind/language cases, exact recipe sensitivity/insensitivity, golden IR bytes, truncation
and structural mutation, pointer containment, arena/allocator/copy/retained-byte high water, canonical
output across input/scheduling permutations, exact invalidation sets, causal errors, typed diagnostics,
and a top-level real public journey.

Scheduler work additionally crashes/cancels/kills at every stage and publication prefix, asserts
physical-credit conservation after every action, exercises heterogeneous memory classes under load,
and proves duplicate jobs do not need a distributed mutex. Bundles additionally prove signatures,
transitive dependency closure, target mismatch, partial download, eviction, and client binary budget.

## Research routing

Read only for a concrete choice and record what is rejected:

- rust-analyzer crate boundaries, generated AST, stable summaries, and incremental invariants:
  <https://rust-analyzer.github.io/book/contributing/architecture.html>
- Salsa algorithm/durability and its real allocation/cancellation tradeoffs:
  <https://salsa-rs.github.io/salsa/reference/algorithm.html>
- OXC arena AST and compact-layout practice:
  <https://oxc.rs/docs/learn/architecture/parser.html>
- Cranelift typed dense maps and pooled entity lists:
  <https://docs.rs/cranelift-entity/latest/cranelift_entity/>
- Tagless-final/GADT experiment reference; default remains a closed enum plus generics:
  <https://inferara.com/blog/rust-tagless-final-gadt/>

A source is evidence for a mechanism, never permission to import its entire ownership model.

## Closure

Return the shared handoff plus recipe/stage/IR layout diagrams, exact invalidation and physical-credit
ledgers, arena/copy/high-water and monomorphized text evidence, strongest illegal-stage/kind/owner
counterexample, rejected legacy/framework ideas, and the next smallest parent decision. In prototype
mode return retain/reject/promote only; never claim another frontend, scheduler, sandbox, bundle,
publication, or index integration capability from vocabulary/format evidence.
