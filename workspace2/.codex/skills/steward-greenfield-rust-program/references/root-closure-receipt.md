# Root closure receipt

Fill this receipt for the exact candidate commit. It is an evidence index, not a retrospective. Link
raw files or quote compact command facts; never replace them with “all green.” Delete inapplicable
rows only after recording the contract reason.

## Identity and custody

```text
capability:
candidate commit:
candidate tree digest:
review worktree and branch:
manager baseline and commit range:
manager model proof:
Luna worker model proof per writing turn:
independent Terra reviewer model proof:
reviewer no-edit proof:
root-owned changed paths:
unrelated dirty paths:
```

Model proof is runtime spawn/session evidence. A role name or prose assertion is insufficient.

## Law ledger

Use one row for every contract law, including negative space.

```text
law | public terminal | falsifier | pre-fix result | candidate result | evidence state | artifact
```

Allowed evidence states:

- `OBSERVED`: another role supplied raw evidence; root has not reproduced it.
- `REPRODUCED`: root reran or independently inspected it against the exact candidate.
- `FALSIFIED`: the candidate violates the law; closure stops.
- `UNVERIFIED`: unavailable, ambiguous, or missing; it remains an explicit gap and cannot close a law.

For each root repair, the pre-fix result must identify the failing parent commit or an equivalent
mutant. A test added after a fix without witnessing a failure is regression coverage, not proof that
it catches the repaired defect.

## Mechanical tripwires

Record every row even when the count is zero. Locations are exact paths and lines; disposition is
`required`, `cold-only`, `false-positive`, or `finding`.

```text
tripwire | count | locations | disposition | law/measurement
panic/unwrap/expect/unreachable
source-dropping conversion or map_err
dyn/Box/Vec/Arc/Rc
public tuple fields or positional semantic tuples
unit/stateless namespace structs
public local traits and one-implementation delegation
one-letter generic parameters
numeric discriminants, sentinels, offsets, capacities, or loop bounds
test-only Option, discarded result, or success-only assertion
unsafe/SIMD/allocator/dependency additions
public item without a current consumer and falsifier
```

A review that omits this table is incomplete even if its prose says there are no findings.

## Resource and scope ledger

```text
formatted production LOC: baseline / candidate / delta / cap / unused reserve
formatted test LOC: baseline / candidate / delta / cap / unused reserve
allocations: site / count / bytes / lifetime / rejection / compared alternative
copies and materializations:
logical work and branch evidence:
optimized consumer artifact and retained control:
dependencies, features, and public exports added/deleted:
written-then-rejected churn:
```

Measure owner plus backing storage and peak simultaneous ownership. Release text evidence names the
consumer artifact; `.rlib` metadata totals are not accepted.

## Hostile attempts

```text
owner/witness mixing:
input removal or constant-body mutant:
invalid tag/offset/length after validation:
error source and rejected-owner loss:
cancellation/progress/reuse:
torn/corrupt/restart behavior:
disabled diagnostics and bounded export:
simpler std-only or representation-deletion experiment:
strongest surviving counterexample:
```

Every applicable attempt names the mutation or fixture and its exact observed result. “Covered by
tests” is not a hostile attempt.

## Clean closure

```text
focused commands and exact results:
complete commands pass one and immediate status:
complete commands pass two and immediate status:
unverified platform/tooling:
zero blockers/majors confirmed against:
post-gate source/evidence edits: none | closure reopened
root concurrent durable artifact:
roadmap/skill changes caused by observed misses:
verdict: ACCEPTED | REJECTED | OPEN
```

`ACCEPTED` requires the exact committed candidate, every law `REPRODUCED`, zero blocker/major, unused
reserve, two clean full passes, and no later edit. Otherwise choose `REJECTED` or `OPEN`; do not
average missing proof into a score.
