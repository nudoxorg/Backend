# Root steward cold calibration

Status: **CALIBRATED FOR CUSTODY; I0 REMAINS OPEN**

This artifact records the blind forward-test that reopened the earlier Index I0 acceptance. It is
process evidence, not a product score or a replacement for an I0 closure receipt.

## Frozen trials

| Trial | Governance specimen | Fresh role | Result |
|---|---|---|---|
| Initial | shared `7fa853bd`, existing I0 receipt and code | explicit non-inheriting `gpt-5.6-terra` request, read-only task `steward_contract_forward_test` | Rejected I0 custody and numeric scoring |
| Retest | shared `a9e6e370`, same I0 evidence | explicit non-inheriting `gpt-5.6-terra` request, read-only task `steward_contract_retest` | Source/receipt distinction coherent; I0 artifact still rejected |

The orchestration API accepted both explicit model selections and returned the named task identities.
The runtime exposes no second selected-model field, which the current receipt contract now states
instead of treating a worker name or self-report as proof. Both trials ended with clean read-only
status and no artifact edits.

## Defects the first trial caught

- The receipt called a pre-receipt source commit the exact candidate without defining the later
  evidence-only commit.
- Law rows omitted artifact identity and distinct hash-work/`no_std` evidence.
- Resource rows omitted baseline/candidate/delta, allocation alternatives, exports, and churn.
- The final independent review omitted mandatory tripwire rows, so its approval was inadmissible.
- Requested model selection, manager recommendation, and root acceptance were not distinguished
  precisely enough.
- Rubric readiness required two writers while rubric calibration specified only two scorers.

Those failures produced shared commits `22d4f41c`, `a9e6e370`, and `e00f7961`. The resulting rules
require one canonical card digest, exact worktree custody, source-preserving checked arithmetic, a
separate source-candidate and receipt identity, complete fresh calibration after semantic rewrites,
scoped tree equivalence across integrated histories, two independent rubric writers, and no product
score before adoption.

## Retest decision

The fresh retest could no longer approve through the earlier custody ambiguity. It correctly returned:

- I0 `OPEN` until its receipt is rebuilt from the current literal template;
- numeric rubric `NOT ADOPTED`, with zero readiness rows closed;
- explicit successful non-inheriting model selection as transport evidence, while preserving the
  absence of a separate runtime-selected-model field;
- nine concrete missing I0 evidence classes rather than a prose approval.

The retest exposed two remaining generalized gaps. Current governance now invalidates the entire
applicable cold deck after any semantic card rewrite and requires scoped source/test/manifest/lock/
fixture/generated-consumer equivalence when a candidate crosses Git histories.

## Validator status

The official skill validator could not start because its Python environment lacks `yaml`. Ruby's YAML
parser successfully loaded every changed skill frontmatter, placeholder scans were empty, and Git diff
checks were clean. This is an explicit tooling gap, not an official-validator pass.

## Next falsifier

Rebuild the I0 receipt literally, attach a conforming fresh read-only Terra review and two exact-source
clean gate passes, then give the packet to another fresh steward reader. If that reader can accept with
an omitted row, stale digest, evidence-only prose standing in for an artifact, or unproved integrated
tree equivalence, stewardship calibration reopens.
