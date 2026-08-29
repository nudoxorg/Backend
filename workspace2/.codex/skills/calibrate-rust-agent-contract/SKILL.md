---
name: calibrate-rust-agent-contract
description: Adversarially forward-test a workspace2 Rust contract, domain skill, reviewer, or rubric with fresh agents before production edits. Use when ambiguity, repeated churn, false completion, or plausible misinterpretation must be eliminated; this skill changes instructions and evidence contracts, never production code.
---

# Calibrate a Rust agent contract

Read `../deliver-reviewed-rust-slice/SKILL.md`, `../manage-rust-swarm/SKILL.md`,
`../review-rust-gem/SKILL.md`, and the applicable domain skill and plan completely. Read
`../write-evidence-rubric/SKILL.md` when a score or cap is in scope.

This is a cold behavioral test of instructions. Do not edit production code, manifests, fixtures, or
roadmaps. Do not tell evaluators the intended design, known defect, desired answer, or prior agent
failure. Preserve the exact requested model topology and verify model selection from runtime evidence;
task names are not proof.

## Freeze the specimen

Record exact skill/plan/card paths and content digests. Extract the literal parent decisions without
repairing them. The trial output schema is:

```text
first observable capability and terminal
exact allowed paths and named baseline
preserved facts and prohibited adjacent behavior
expected public surface and explicitly forbidden surface
evidence row | falsifier | hard cap | stop trigger
budgets and reserve
questions requiring parent authority
```

Reject append-only amendment chains. An active card must be understandable from one canonical body;
terms such as “clarifies,” “supersedes,” or “all earlier declarations remain” are a structural
failure when an earlier declaration still exists. The manager may retain old cards in Git, but every
cold reader receives exactly one current digest. A post-trial semantic edit makes the trial stale.

## Cold trials

Run all three against the same frozen specimen:

1. **Independent reader:** restates the schema above and the first checkpoint without proposing code.
2. **Plausible misreader:** chooses the cheapest interpretation that could still look compliant and
   describes the patch it would attempt. Seed no answer; reward finding permission gaps.
3. **Reviewer calibration:** a separate explicitly selected Terra receives the frozen contract plus
   small candidate artifacts and must reject each seeded defect with the exact cap and falsifier. It
   remains read-only and is not shown the intended rejection.

For high-risk binary, durability, concurrency, unsafe, or cross-crate contracts, use two independent
readers. A manager may use Luna for reader/misreader trials, but reviewer calibration uses a separate
real Terra. Every role must be explicitly selected; inherited manager models and role-like task names
invalidate the run.

## Required adversarial deck

Use only cases applicable to the specimen, but do not omit an applicable case:

- a public multi-field witness assembled from unrelated valid owners;
- a checked arithmetic failure collapsed into a sentinel, saturation, generic budget error, or value
  that cannot retain the original operands/owners;
- validation followed by raw-tag redecoding, `unreachable!`, fallback, omission, or unchecked cast;
- a green happy-path test that never falsifies exact error/source/owner behavior;
- a synthetic adapter/frontend whose constants let optimized code ignore the supplied input while all
  public tests stay green;
- a claimed clean/reproducible gate that creates an untracked lockfile or deletes generated state only
  after status inspection;
- an unapproved manifest, reexport, future-phase type, backend enum, or compatibility shim;
- a generic, macro, unsafe block, SIMD kernel, allocation, `Arc`, or dependency justified by imagined
  future users instead of two current consumers and measured/deleted cost;
- blocking work hidden behind an async signature, collection hidden behind a stream, or analogous test
  state used as concurrency proof;
- legacy behavior smuggled into a greenfield phase;
- an `8` claim with a missing public integration, hostile boundary, restart/fault, or raw measurement;
- superficial LOC/test-count/benchmark changes that should not improve a score.

Candidate artifacts may be tiny isolated compile fixtures or precise diff excerpts. They must be
executable when compilation or a gate is the claimed evidence; prose-only strawmen do not calibrate a
reviewer.

## Rewrite rule

Compare normalized fields, not writing style. Rewrite only the narrow instruction whose ambiguity was
observed. Add a do/don't or stop rule when the wrong behavior was plausible; delete duplicate prose
when the issue was authority conflict or overload. Never encode a task-specific type or patch as a
universal law.

After any semantic rewrite, rerun the complete applicable cold deck with fresh roles and no prior
transcript; every result for the earlier digest is stale. For cross-crate work this means both
independent readers, the plausible misreader, and the separate explicit non-inheriting Terra reviewer,
not one convenient reader. Stop after two failed rewrite rounds and return the unresolved decision to
the parent; repeated prompting is not calibration.

Classify every missing field before escalation. Baseline SHA/digests, exact repository paths,
formatted LOC forecasts, numeric reserve, runnable commands, dependency facts, and the smallest
existing public consumer are discoverable manager work. The manager fills them and repeats the cold
trial. Only two materially different observable terminals, permanent wire semantics, or authority
outside the named capability are parent decisions.

Reject a specimen that ends mid-sentence/list/table or names phases absent from its closure matrix.
Every architecture plan ends with a literal `Plan closure` section; every executable card ends with
one exact next decision. Structural validity is tested before an evaluator spends a turn.

## Passing result

The contract passes only when:

- independent readers agree exactly on capability, terminal, paths, negative space, evidence, caps,
  reserve, and authority questions;
- the plausible misreader finds no interpretation that broadens or substitutes the capability;
- the reviewer rejects every seeded defect at the intended severity/cap and does not invent style
  findings;
- official skill validation, frontmatter checks, and diff checks pass; and
- the manager reports raw evaluator outputs, divergences, rewrites, and remaining uncertainty.

A passing calibration authorizes a builder checkpoint; it does not score or approve implementation.
