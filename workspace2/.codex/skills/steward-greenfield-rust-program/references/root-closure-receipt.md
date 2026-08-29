# Root closure receipt

Fill this receipt for the exact source-candidate commit and tree. The receipt is a later evidence-only
artifact and cannot self-name its own commit. Link raw files or quote compact command facts; never
replace them with “all green.” Keep every row. An inapplicable row states the contract reason and the
exact source search or executable experiment establishing inapplicability; otherwise it is
`UNVERIFIED`.

## Identity and custody

```text
capability:
canonical contract path and digest:
calibration artifact digests and post-calibration contract edits:
source-candidate commit:
source-candidate tree digest:
receipt-containing commit: SELF (record exact commit in acceptance/roadmap after commit)
paths changed from source candidate through receipt:
source/test/manifest/lock/fixture/generated-consumer equivalence proof:
integrated shared commit and scoped tree-equivalence proof:
review worktree and branch:
manager baseline and commit range:
manager model proof:
Luna worker model proof per writing turn:
independent Terra reviewer model proof:
reviewer no-edit proof:
root-owned changed paths:
unrelated dirty paths:
```

Model proof is the retained successful orchestration call with an explicit non-inheriting `model`
argument plus its returned task/session identity; a role name or worker self-report is insufficient.
If the runtime exposes a selected-model field, retain and compare it. If it does not, state that
limitation rather than inventing a second attestation; a successful explicit selection remains
transport evidence under the orchestration API contract.

## Law ledger

Use one row for every contract law, including negative space.

```text
law | public terminal | falsifier | pre-fix result | candidate result | evidence state | artifact
```

The artifact cell identifies the source-candidate commit/tree, immutable command-log digest, and
exact path/line or test name. Prose summaries are not artifacts.

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
checked-arithmetic sentinel/saturation or operand loss
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

A review that omits this table is incomplete even if its prose says there are no findings. Exact
locations are complete repository-relative paths and lines; ellipses, bare filenames, and “all
changed files” are not evidence. Zero rows name the complete scanned path set and literal search.

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
tests” is not a hostile attempt. `N/A` without the establishing search/experiment is `UNVERIFIED`.

## Clean closure

```text
focused commands and exact results:
complete commands pass one and immediate status:
complete commands pass two and immediate status:
unverified platform/tooling:
zero blockers/majors confirmed against:
post-gate source edits: none | closure reopened
post-gate evidence edits and current receipt identity:
root concurrent durable artifact:
roadmap/skill changes caused by observed misses:
verdict: ACCEPTED | REJECTED | OPEN
```

`ACCEPTED` requires the exact committed source candidate, every law `REPRODUCED`, zero blocker/major,
unused reserve, two clean full passes, a later receipt whose intervening diff is evidence-only, and no
later source edit. Otherwise choose `REJECTED` or `OPEN`; do not average missing proof into a score.

When a candidate crosses histories through cherry-pick or patch integration, name the integrated
commit and compare every scoped source, test, manifest, lock, fixture, and generated consumer path.
Commit-message or patch-id similarity is corroboration only. Without scoped tree equivalence, the
integrated candidate is a new unreviewed source candidate.
