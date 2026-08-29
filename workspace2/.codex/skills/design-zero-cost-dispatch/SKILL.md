---
name: design-zero-cost-dispatch
description: Research, admit, prototype, and review static enum delegation, tagless-final/GADT encodings, and fearless_simd dispatch without dyn, hidden branches, or speculative macro frameworks. Use with deliver-reviewed-rust-slice when two real consumers may justify a dispatch abstraction.
---

# Zero-cost dispatch scope

Read `../deliver-reviewed-rust-slice/SKILL.md` and
`../audit-data-layout/SKILL.md` completely first. They are authoritative for generics, budgets,
experiments, SIMD, unsafe, and evidence. This skill adds the admission and review procedure for
compile-time dispatch abstractions.

## Start from the call graph

Name the exact current callers, concrete implementations, branch location, value cardinality, and
measured cost. Search the whole affected workspace before proposing a framework. Classify the need:

- A closed enum with all variants inhabitable is ordinary runtime dispatch. Prefer an explicit
  `match`; a delegation macro may remove repetition but does not erase the tag.
- A sealed policy trait with a concrete associated representation is already tagless. Do not wrap it
  merely to make generic code look novel.
- A tagless-final/GADT encoding is eligible only when at least two real interpreters make inactive
  cases statically uninhabited and the typed program removes illegal runtime states.
- SIMD dispatch is eligible only for a measured long contiguous kernel. It is independent of enum
  delegation unless two real kernels share the full dispatch law.

Do not edit production at the research checkpoint.

## Compare before inventing

Compare the normally formatted manual implementation with no more than three applicable choices:

1. narrow private `macro_rules!`;
2. a maintained crate such as `delegation`, `enum_dispatch`, or `static_dispatch`;
3. a workspace proc macro.

Inspect current official documentation/source, maintenance, MSRV, transitive graph, no-std support,
cross-crate behavior, generic/lifetime/const support, associated types, hygiene, diagnostics, and
generated conversions/helpers. A crate that solves trait delegation does not count as a GADT.

Return invocation and representative expansion side by side. The invocation must expose the runtime
or compile-time choice; do not conceal it behind an attribute whose generated surface is surprising.

```rust
// DON'T: invent a dispatch framework for one normal runtime enum.
#[delegate_everything]
enum Locality<Domain> { Resident, Promised(Providers), Overlaid(Base<Domain>) }

// DO: keep three genuinely inhabitable semantic states explicit.
match locality {
    Locality::Resident => resident(),
    Locality::Promised(providers) => promised(providers),
    Locality::Overlaid(base) => overlaid(base),
}
```

## Tagless admission proof

Before a tagless macro, provide this table:

```text
interpreter | active cases | inactive representation | production caller | state/branch removed
```

There must be at least two interpreters and two already-shipping production callers. Two
implementations, aliases, tests, or instrumentation modes around one algorithm count as one
consumer. Inactive cases use an uninhabited type such as `Infallible` in the payload position of the
program actually constructed; mentioning it only in an associated type is not an erasure proof.
`Option`, a sentinel, and a panic arm fail. State exactly why each generic and associated type is
needed. Compare against one closed enum and the existing sealed policy-trait form.

For every interpreter name the exact artifact it erases: an enum discriminant, indirect call,
invalid state, duplicated algorithm, branch, or copy. “Static,” “zero-cost,” and “monomorphized” are
not benefits by themselves.

The optimized cross-crate caller must contain no vtable, indirect delegate call, residual tag test,
or panic path for the statically selected case. Compare one purpose-built cross-crate
`#[inline(never)]` static selection against the manual baseline; a broad IR grep for `switch` or
`panic` is too noisy to prove erasure. Record LLVM IR/assembly and release text by monomorph. If the
optimizer proof is fragile or the call site is less readable, retain the manual form.

```rust
// DON'T: call a tagged enum "tagless" because a macro generated its match.
enum Backend { Scalar(Scalar), Vector(Vector) }

// Eligible shape only when real interpreters select different uninhabited cases.
trait RowInterpreter {
    type ScalarCase<Payload>;
    type VectorCase<Payload>;
}
```

## Macro implementation law

A declarative macro wins for a narrow private grammar whose limitations are intentional. A proc
macro must earn arbitrary Rust item parsing or precise semantic diagnostics. Do not combine the two
merely to obtain attribute syntax.

For a proc macro:

- preserve visibility, documentation, attributes, input spans, lifetimes, descriptive type and
  const parameters, and `where` clauses;
- reject duplicate/missing cases and inhabitable disabled cases at their source spans;
- generate no undocumented public helper type, conversion, allocation, `dyn`, or panic arm;
- keep expansion deterministic, small, and reviewed;
- use compile-pass fixtures for complex generics and `trybuild` compile-fail fixtures for each law;
- snapshot one expansion for hygiene auditing, not as the semantic test suite.

The skeleton ledger includes the macro crate, parser, expansion, UI fixtures, cross-crate fixture,
codegen harness, docs, and reserve before approval. “A few attributes” is not an estimate.

## SIMD dispatch law

The default `fearless_simd` shape is explicit:

```rust
#[inline(always)]
fn kernel<SimdBackend: fearless_simd::Simd>(simd: SimdBackend, input: &[u8]) -> Output {
    // one measured contiguous loop; scalar tail retains exact semantics
}

fearless_simd::dispatch!(cached_level, simd => kernel(simd, input))
```

Detect/cache `Level` outside the hot call. Keep the dispatch body to one generic call because it is
repeated and type-checked for each backend. Account for its closure semantics: early `return` and `?`
inside the body do not mean what they mean in the caller; apply `?` to the macro result or use typed
`ControlFlow` deliberately.

Do not build a SIMD wrapper until a second production kernel independently passes the shared SIMD
evidence law. Then prove the wrapper preserves:

- scalar authority and measured crossover;
- identical first error and ordinal;
- every length below and across all vector widths, every tail, and relevant alignments;
- detected, baseline, fallback, ARM, and x86 behavior;
- unchanged untouched output on failure;
- explicit release text and end-to-end wins.

Reject SIMD for construction, sparse/binary lookup, pointer chasing, short fixed records, hashing
already delegated to an optimized crate, and journaling unless a new profile overturns the default.

## Closure

Return one decision: retain manual, adopt maintained crate, prototype in `layout-lab`, or approve a
bounded macro crate. Include rejected alternatives, raw evidence, invocation/expansion, dependency
ledger, compile-time and release-text costs, remaining criticism, and the exact skill amendment
learned from any churn. Production adoption is a separate parent-approved phase.
