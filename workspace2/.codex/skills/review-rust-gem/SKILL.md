---
name: review-rust-gem
description: Perform a hostile, evidence-backed review of one workspace2 Rust slice before parent approval. Use after an implementation checkpoint or when auditing claimed correctness, simplicity, typing, allocation, concurrency, durability, diagnostics, testing, or performance. This skill reviews; it does not silently implement fixes.
---

# Review a Rust gem

Read `../deliver-reviewed-rust-slice/SKILL.md` completely, then the one applicable domain skill. The
shared craft laws are the review standard. Do not invent a second style guide.

The reviewer is adversarial toward claims and collaborative toward the implementation. Find the
smallest design that actually proves the contract. Green tests, clever types, and low LOC are inputs,
not approval.

## Required inputs

Require: approved contract card, parent decisions, before/after tree or diff, direct consumers,
claimed ledgers, commands/results, and applicable baseline evidence. If any is absent, report the
missing proof; do not infer success.

Freeze scope before reading details: record changed paths, generated/manifest changes, net production
LOC, deleted APIs, and unrelated dirty files. Review is read-only except parent-approved isolated
experiments. Check wildcard workspace members for orphan/incomplete crate directories before running
gates. Never repair code while compiling the findings; that hides causal evidence.

Recompute the remaining forecast with the shared skill's uncertainty reserve. A claim that technically
fits only by consuming the reserve is a scope failure and must split before further implementation.
Compare planned and actual formatted LOC for every file before reviewing semantics. A variance above
20% or 25 lines is a process finding: stop the phase, identify the missing responsibility, and require
a new boundary. Search for public errors, types, dependencies, and reexports that have no consumer and
falsifying test in this checkpoint; delete them rather than calling them preparation for the next one.

## Review passes

Run all applicable passes in this order:

1. **Contract:** map every preserve/prove row to code and a falsifying test. Reject deferred rows or
   substituted capabilities.
2. **Boundary:** identify the invariant owner, dependency direction, leaked adapter policy, duplicate
   representation, compatibility shim, and public surface that can be private/deleted. For each
   witness/view with multiple fields, try a downstream struct literal that combines parts from two
   valid owners. Compilation is a blocker when coherence is required.
3. **Less-is-more:** find wrappers, getters, delegate traits, stateless structs, helper proliferation,
   parallel algorithms, repeated fields, redundant wire metadata, and branches that representation
   can remove. Recount normally formatted logical source; reject `rustfmt::skip`, one-line functions,
   or statement packing used to manufacture a low-LOC claim.
4. **Types:** inspect raw primitives, tuple coordinates, optional correlated state, string stages,
   catch-all variants, generic ledgers, lifetime truth, conversions, field visibility, and invalid
   states. Demand descriptive generic names.
5. **Ownership/layout:** account for owner plus backing, peak live memory, pointer depth, allocation,
   copies, stable-address need, rejection/drop, `Arc` churn, stack/thread-stack cost, fragmentation,
   and text-size monomorphs. Compare the real lifetime alternatives.
6. **Control/errors:** trace hot and failure paths. Find repeated conditions, unpredictable branches,
   hidden scans, oversized functions, lost sources/values, panic paths, hand-written formatting, and
   error priority drift. Trace every validated tag, offset, and length into projection. A second
   raw-to-closed decode, `unreachable!`, `expect`, silent omission, fallback, or unproved narrowing
   conversion means the representation did not retain its proof.
7. **Concurrency/async:** prove receiver-level concurrency, physical bounds, linearization, ordering,
   waker arm/recheck, cancellation, ABA/reuse, poison, shutdown, and progress. Search for locks hidden
   in libraries. Require Loom on production transitions and Miri for unsafe ownership.
8. **Protocol/durability:** recompute every byte and preimage. Mutate every cell. Trace write/sync/
   directory/receipt/crash/replay semantics and partial operations. Reject duplicated metadata and
   undefined authentication.
9. **Diagnostics:** ask the promised operator questions using only emitted typed events. Prove no-op
   laziness, exact chronology/correlation, bounded retention/export, triggered dump, and core/client
   exclusion from server machinery.
10. **Tests/evidence:** try to make tests pass with the implementation broken. Demand exact negative
    assertions, boundary tables, public integration, isolated allocation measurement, raw performance
    evidence, feature/target coverage, and source-bearing fault injection.

## Finding format

Every finding contains:

```text
Severity: BLOCKER | MAJOR | MINOR | QUESTION
Location: exact file and line
Evidence: code/test/measurement observed
Violated law: contract or shared-skill statement
Consequence: correctness, safety, ownership, work, memory, API, or operability
Smallest correction: deletion or redesign boundary, not a patch recipe by default
Falsifier: exact test/measurement that proves the correction
```

`BLOCKER` means violated semantics, unsafe/durability uncertainty, lost errors/owners, unbounded state,
or a missing contract capability. `MAJOR` means unjustified allocation/generic/dependency, material hot
work, abstraction leakage, or weak integration evidence. `MINOR` must still name an actual cost; pure
preference is omitted. Questions never disguise findings.

## Approval rules

Do not approve when:

- any contract row lacks a public falsifying test;
- code calls a weaker behavior by the requested name;
- an allocation/generic/unsafe/SIMD/dependency ledger is incomplete;
- concurrency is modeled in analogous test code rather than production transitions;
- tests assert only success/no panic or use `expect`/`unwrap` as reporting;
- an LOC or simplicity claim depends on suppressed formatting or compressed logical statements;
- diagnostics exist only in an outer demo;
- a coherence-dependent public struct literal can combine unrelated valid fields, or validated input
  still reaches a panic/fallback/redecode during trusted projection;
- retained complexity rises without a measured/deleted cost;
- the implementation crossed its named baseline/cap, hid written-then-deleted churn, or introduced
  future-phase public surface without current behavior and tests;
- full gates are red for an owned finding.

Approval requires zero blockers/majors, reproducible commands, honest remaining caps, and a smaller
next decision. “Approve with comments” is not final approval; it is another checkpoint.

## Reviewer self-check

Before handoff, state:

- the strongest attempted counterexample;
- the most likely hidden allocation/branch/lifetime;
- one thing that looked suspicious but evidence cleared;
- whether a simpler standard-library design was genuinely considered;
- confidence and unverified platform/tooling gaps.

Return findings first, ordered by severity, with a short approval status last. Do not write a score
unless an independently validated rubric was explicitly supplied.
