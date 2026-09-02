---
name: write-evidence-rubric
description: Design and calibrate a future evidence-based 0–10 rubric for a stable workspace2 architecture or implementation plan. Use only after contracts, reviewer behavior, baselines, and terminal integration evidence exist. This skill writes rubrics; it never scores the product it helped define.
---

# Write an evidence rubric

Read `../../../ORCHESTRATION.md`, `../deliver-reviewed-rust-slice/SKILL.md` and
`../review-rust-gem/SKILL.md` completely. A rubric
is downstream of stable contracts and a trustworthy reviewer. It must make gaming harder, not turn
aspirations into points.

## Readiness gate

Do not write the rubric until all exist:

- stable capability graph and explicit non-goals;
- approved contract cards for each coherent plane;
- reproducible client and remote baselines;
- named public integration/fault/performance evidence;
- a separate read-only Terra reviewer and reviewer skill forward-tested on deficient, complete, and
  stretch implementations without receiving the intended answer;
- definitions for “plan complete” and “same-direction extra mile.”

If any is absent, return a readiness gap list and the smallest calibration work. Never invent weights
to create a feeling of completeness.

Prototype branches are rubric inputs, not scoreable products. They return retain/reject/promote
evidence under `orchestrate-greenfield-rust-prototype`; they cannot be labeled 8/10, stretch, complete,
or production-ready until rebuilt on current shared state and the readiness gate above closes.

## Criterion schema

Every criterion is independently observable:

```text
ID and capability
Why it matters
Evidence source and exact command/artifact
0/2/4/6/8/9/10 anchors
Weight and justification
Hard caps triggered by missing prerequisites
Dependencies and non-overlap with other criteria
Same-direction stretch goal
Known measurement uncertainty
```

No criterion uses “elegant,” “fast,” “robust,” “well tested,” or “production ready” without a numeric
or executable definition. Do not score both the mechanism and its consequence twice.

## Scale semantics

- **0:** absent, inverted, or unverifiable.
- **2:** local demo/happy path; boundary or failure laws missing.
- **4:** coherent component proof; integration, hostile cases, or measurements incomplete.
- **6:** integrated primary path with meaningful negative evidence; material declared gaps remain.
- **8:** the complete approved plan, all mandatory public/fault/performance evidence reproducible.
- **9:** a measured same-direction stretch across another workload/profile/platform without weakening 8.
- **10:** the robust extra-mile form of the same capability: independently reproduced, breakage-resistant,
  and materially better on predeclared measures. Ten is never “more features” or more abstraction.

Full completion is exactly eight. A score above eight cannot compensate for a missing mandatory law.

## Caps and anti-gaming

Apply explicit maximums before weighted aggregation:

- unproved correctness, source loss, unsafe invariant, or durability claim: product cap below 4;
- missing end-to-end public integration or restart/fault proof: affected plane cap below 6;
- unbounded memory/work/export or server dependency in portable client: affected plane cap below 6;
- missing raw performance evidence for a performance claim: that criterion cap 4;
- green tests with weak assertions: testing criterion cap 2;
- unresolved reviewer blocker/major: no final score.
- missing or role-mixed proof chain—builder self-review, Luna substituted for the required Terra
  reviewer, reviewer edits, or primary Terra delegating acceptance: affected evidence is inadmissible
  and no final score is issued.

Source compression, genericity, SIMD, unsafe, dependency totals, allocation totals, test totals, and
agent activity are never standalone points. They matter only through the contract's invalid-state,
memory, work, coupling, correctness, and operability evidence.

## Weighting and calibration

Weights follow user harm and architectural dependency, not implementation effort. Correctness,
ownership, error fidelity, durability, and boundedness are usually caps; performance and ergonomics
score only after them. Keep criteria orthogonal and sum weights exactly once.

Calibrate before adoption:

1. Two independent rubric writers receive the same frozen contracts/baselines and produce criterion,
   anchor, cap, and non-overlap tables without seeing each other's work. Reconcile only when each row
   agrees within one anchor step and names the same hard caps; otherwise rewrite the input contract.
2. Select three frozen artifacts: deliberately deficient, complete-to-plan, and legitimate stretch.
   The deficient artifact must compile and pass plausible weak tests while containing at least one
   seeded invariant leak; prose descriptions are not calibration artifacts.
3. Blind the artifact labels. Two reviewers independently apply the rubric without discussing scores.
4. Any criterion divergence greater than one point or any total that places the artifacts outside
   `<8`, `=8`, and `>8` respectively requires rewriting anchors/evidence.
5. Try gaming patches: superficial source compression, duplicated easy tests, parameter-bag
   coupling, and a benchmark-only optimization. The score must not rise without capability evidence.
6. Version the rubric when contracts or baselines change; never silently edit anchors mid-review.

Before adoption, run the contract itself through `../calibrate-rust-agent-contract/SKILL.md`. If fresh
readers disagree on the first slice, allowed paths, terminal evidence, or a hard cap, the rubric is not
ready even when its arithmetic is internally consistent.

## Output

Return readiness decision, criterion table, caps, aggregation formula, calibration artifacts/results,
gaming tests, version/change policy, and open ambiguity. The rubric writer must not score the current
implementation; hand the validated rubric to an independent reviewer.

The active rubric artifact begins with `NOT ADOPTED`, `CALIBRATING`, or `ADOPTED`, plus the contract
digests, calibration artifact commits, reviewer identities/model proof, and adoption date. Only
`ADOPTED` permits a numeric product score. Replacing a stale score with a readiness ledger is required,
not loss of history: Git retains the old opinion without letting agents cite it as current evidence.
