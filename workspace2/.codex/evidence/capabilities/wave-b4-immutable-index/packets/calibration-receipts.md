# Wave B4 immutable-index calibration receipts

## Disposition

The final frozen card is
`cc55f7b9150dc4f90d65f422c7bc977a4482a81fbe28dc93723cc12e563dee00`.
The earlier cold-reader and plausible-misreader pair read card
`b278…` before the stable-toolchain and anti-self-certification correction, so those
receipts are retained as stale context and do not certify this card. The final reader and
misreader were read-only and were not allowed to alter matrix state. A second final reader
was interrupted without a final receipt. This calibration is therefore `INCOMPLETE`, not
a passing gate.

## Final cold reader A — retained raw receipt

```text
RUNTIME_ID=/root/wave_b4_terra/b4_reader_final_a
role=nudox_luna_implementer
config=.codex/config.toml
role_config=.codex/agents/nudox-luna-implementer.toml
requested_model=gpt-5.6-luna
requested_effort=max
sandbox=effective danger-full-access host profile; task constrained read-only
baseline_commit=f2565a9fb33af06053bd19721d4dc2753ec09ed5
baseline_tree=8477cb2ab93763c468d5431740cfb1d5e4c4cf82
branch=codex/wave-b4-immutable-index
checkout=/Users/mileswirht/.config/codex/worktrees/f7b8/backend
observed_head=77cde42752851787482dcc1cd8892f3ed92622a3
phase0_card_sha256=cc55f7b9150dc4f90d65f422c7bc977a4482a81fbe28dc93723cc12e563dee00

Restated contract: sealed published deltas feed typed exact/lexical immutable segments;
each query names its pinned snapshot and typed segment IDs; exact, prefix, and lexical
results are borrowed and bounded; deterministic merge preserves score/order/provenance;
compaction independently replaces equivalent segments while prior pins survive. Routes are
advisory. A stale route/lost declared segment is exact absence or Partial, never a silent
zero. Placement across RAM/NVMe/object storage cannot change IDs. Tantivy is at most a
nested lexical differential adapter and cannot own schema or truth.

Control finding: the host sandbox is danger-full-access rather than a technically enforced
read-only sandbox. The task's read-only constraint was honored, but that is a custody gap.
The five only admissible existing-vocabulary commands are the frozen Nix commands using
NUDOX_STABLE_TOOLCHAIN's rustc and cargo with RUSTC_WRAPPER cleared. No execution receipt
was produced. No production candidate or B4 matrix row is proved.
```

## Final plausible misreader — raw receipt

```text
runtime_id=/root/wave_b4_terra/b4_misreader_final
codex_thread_id=01a0514d-37e4-7fb0-99c3-81da6952939d
codex_session_id=01a05132-88cd-7991-a45e-62365ae740e5
role=nudox_luna_implementer; config=.codex/config.toml;
role_config=.codex/agents/nudox-luna-implementer.toml;
requested_model=gpt-5.6-luna; requested_effort=max;
actual model/effort field not exposed independently;
sandbox=effective danger-full-access host profile, task constrained read-only;
no mutations performed
baseline_commit=f2565a9fb33af06053bd19721d4dc2753ec09ed5;
baseline_tree=8477cb2ab93763c468d5431740cfb1d5e4c4cf82;
branch=codex/wave-b4-immutable-index;
checkout=/Users/mileswirht/.config/codex/worktrees/f7b8/backend;
current status clean; current HEAD=77cde42752851787482dcc1cd8892f3ed92622a3 evidence-only commit
phase0_card_sha256=cc55f7b9150dc4f90d65f422c7bc977a4482a81fbe28dc93723cc12e563dee00

plausible_misreader_action_not_executed=Treat the spelled-out public seam as permission to
add a constant-body exact_at stub to the existing vocabulary production crate and a
success-only test. It appears typed and minimal, but violates production_writes=false,
vocabulary ownership, input-sensitive semantics, and Phase 0’s no-implementation boundary.

no_self_certification=PASS as control only; Luna may interpret/report, never move rows to
PROVED BY WORKER, REPRODUCED BY TERRA, EVIDENCE_BLOCKED, or complete/approve the capability.
Overall index remains phase0; calibration_status and pre_edit_review_status are not-yet-run;
no worker/reviewer records are bound yet. Independent Terra review must be a distinct
registered child under the source-isolated sidecar.

stable_toolchain_custody=PASS as read interpretation, not execution; only the five exact Nix
commands using NUDOX_STABLE_TOOLCHAIN rustc/cargo with RUSTC_WRAPPER cleared are admissible.
Ambient cargo is forbidden. No stable-toolchain raw output, exit status, or exact receipt was
produced in this read-only trial; vocabulary gate remains not-yet-run.

named_snapshot_segment_ids=PASS as contract reading; operations must explicitly name one
IndexSnapshotId and declared typed E1/E2/L1/L2 IDs, with S1/S2/S3 symbolic fixtures. No
latest-head, route, node, tier, cache, or untyped-ID substitution is permitted. No actual
snapshot/segment implementation exists.

partial_zero_hit=PASS as contract reading; beta tombstone is Complete with zero rows, while
absent declared E2 is Partial naming exactly E2. No terminal implementation or falsifier
exists; B4-07 remains RED.

tier_parity=PASS as contract reading; RAM/NVMe/object placement cannot alter IDs or results,
and deterministic local/remote parity may establish semantics only. Durable file/NVMe/object-
store range, outage, and durability proof is absent and cannot be claimed.

independent_compaction=PASS as contract reading; S3 must independently consume explicit
E1/E2/L1/L2, verify exact and lexical equivalence to S2, then atomically publish while S1/S2
pins remain valid. No compactor, provenance, equivalence trace, or publication exists;
B4-10 remains RED.

tantivy_control=PASS as contract reading; Tantivy may only be a bounded nested lexical
differential comparing typed document set/order/tie/provenance. It cannot own schema, head,
identity, or terminal truth. No adapter or two-attempt block exists.
```

## Final cold reader B — interruption receipt

```text
runtime_id=/root/wave_b4_terra/b4_reader_final_b
requested_model=gpt-5.6-luna
requested_effort=max
task_scope=read-only independent cold reading of the final frozen card
observed_status=running without a delivered final receipt
interrupt_result={"previous_status":"running"}
final_receipt=absent
```

## Stale calibration receipts

```text
runtime_id=/root/wave_b4_terra/b4_cold_reader_one
requested_model=gpt-5.6-luna; requested_effort=max; task_scope=read-only
card_generation=pre-77cde427 hardening; disposition=STALE

runtime_id=/root/wave_b4_terra/b4_plausible_misreader
requested_model=gpt-5.6-luna; requested_effort=max; task_scope=read-only
card_generation=pre-77cde427 hardening; disposition=STALE
```

No reader or misreader may self-certify the card, alter a matrix row, or substitute for the
required registered Terra reviewer. The raw source-isolated reviewer dispatch logs are separate
in `sidecar-review-attempts.md`.
