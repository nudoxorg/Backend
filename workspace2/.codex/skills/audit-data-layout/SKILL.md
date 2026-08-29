---
name: audit-data-layout
description: Scope rules for experimentally comparing workspace2 representations, allocation mechanisms, ownership lifetimes, cache/branch behavior, text size, unsafe, and SIMD. Use with deliver-reviewed-rust-slice before changing a numerous/hot type or making a zero-cost claim.
---

# Data-layout audit scope

Read `../deliver-reviewed-rust-slice/SKILL.md` completely first. It is authoritative for idioms,
review, allocation candidates, generics, SIMD, unsafe, evidence, and tests. This skill adds only lab
scope.

## Routing

Experiments live in `layout-lab/`; production stays unchanged until a candidate wins and its domain
owner approves adoption. Read only the affected type/owners/consumers, matching `LAYOUT_AUDIT.md`
baseline/hypothesis, and the relevant `TEST_INFRA.md` evidence law.

Under prototype orchestration, commit every control/candidate and raw result on the isolated
prototype branch. A winning lab row is only `PROMOTE FOR FUTURE INTEGRATION REVIEW`; it does not
authorize production edits or a shared merge.

## Experiment card

```text
Question: one falsifiable representation claim
Safe baseline: simplest correct shape
Candidates: at most three, one changed axis each
Profiles: portable client and remote server
Workloads: zero/one/boundary/+1/large; sequential/random; success/reject/drop
Measures: owner+backing, peak, allocations, copies, work, branch/cache, time, text
Adoption threshold: stated before measurement
Production paths: exact files only if one wins
```

Wait for parent approval before adding candidates.

## Audit invariants

- Store raw machine-readable target/compiler/flags/workload results. Isolate candidates and measure
  construction/destruction plus simultaneous owners.
- Keep losing results. Recommend exactly adopt, retain baseline, specialize by static policy, or
  defer; adjectives are not evidence.
- Generic experiments report real monomorphs and release text. GADT/tagless trials compare a closed
  enum and inspect optimized cross-crate output.
- Unsafe trials begin from a safe reference and include exact layout, differential tests, Miri,
  sanitizer when available, Loom for real concurrency, ZST/over-aligned/panicking-drop cases, and
  rollback.
- SIMD trials start only after an end-to-end scalar profile and report every alignment/tail,
  crossover, ARM/x86, first-error equivalence, and binary delta.
- Tagged/content-addressed layouts measure entropy at the actual routing/partition consumer. A smaller
  digest with fixed prefix cells may preserve cryptographic strength while still destroying low-bit
  bucket distribution; size/collision claims alone are incomplete.
- Conversion experiments preserve standard trait semantics. A candidate that wins LOC by making
  `From` mutate/normalize the supplied representation is rejected before measurement.
- Production instrumentation is not benchmark instrumentation. Counters stay lab-only unless the
  product independently requires the typed aggregate event.
- Test one ownership axis at a time before chaining mechanisms. Instantiate dependencies rather than
  dismissing or adopting them from docs; retain the raw losing row, then remove a losing dependency.
  A chained unsafe candidate gets one normally formatted proof module and stops at the first product
  compatibility, capacity/slack, panic/drop, provenance, Miri, or budget failure.

## Closure

Return the shared handoff plus raw evidence, uncertainty, adoption decision, safety proof, production
delta, and remaining criticism.
