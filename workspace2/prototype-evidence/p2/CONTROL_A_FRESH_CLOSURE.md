# P2 control A: fresh prototype closure

## Verdict

**RETAIN BASELINE / REJECT PROTOTYPE.** The frozen Control A card passed its final fresh Luna and
Terra pre-edit calibration, but its sole Luna builder checkpoint produced an uncommitted,
incomplete, over-forecast draft. The independent Terra closure review cleared deletion of that
draft and confirmed that no adapter candidate remains. This is prototype-only evidence, not
product closure, a shared merge, or P2 completion.

## Custody, commits, and model proof

| Item | Value |
|---|---|
| source candidate | `be015582d6705d5cf0a059fda4a9bacc1732efd0` |
| manager worktree | `/private/tmp/nudox-prototype-durable-publication-heart-build` |
| manager branch | `codex/prototype-durable-publication-heart-build` |
| card freeze | `1b76e77c` |
| first fresh rejection record | `1f62c563` |
| narrow byte-boundary authority | `ac15cb84` |
| replacement calibration record | `20ce77a2` |
| structural card repair | `e67b6ec1` |
| final calibration record | `7cdfefa2` |
| rejected builder-churn record | `5551dfea` |
| shared branch action | none; `orchestra-shared` was never edited, merged, rebased, or cherry-picked |

| Role | Task ID | Explicit model | Fork | Outcome |
|---|---|---|---|---|
| first fresh reader | `/root/p2_build_manager/control_a_luna_reader` | `gpt-5.6-luna` | `none` | found direct canonical-byte dependency gap |
| first fresh reviewer | `/root/p2_build_manager/control_a_terra_preedit` | `gpt-5.6-terra` | `none` | confirmed that gap |
| replacement reader/reviewer | `/root/p2_build_manager/control_a_luna_reader_r2`; `/root/p2_build_manager/control_a_terra_preedit_r2` | Luna/Terra | `none` | tightened resource/audit/card proof |
| final reader/reviewer | `/root/p2_build_manager/control_a_luna_reader_r3`; `/root/p2_build_manager/control_a_terra_preedit_r3` | Luna/Terra | `none` | zero blockers/majors; builder authorized |
| builder/cleanup | `/root/p2_build_manager/control_a_luna_reader_r3` | `gpt-5.6-luna` | `none` | uncommitted incomplete patch then deletion-first cleanup |
| closure reviewer | `/root/p2_build_manager/control_a_terra_preedit_r3` | `gpt-5.6-terra` | `none` | clean no-candidate closure |

## Evidence and rejected churn

The baseline workflow gate passed: `cargo test -p nudox-workflow` reported 9 unit tests, 1
integration test, and 0 doctests. The frozen A resource/copy control was one retained journal
`File`, a transient creation-only directory `File`, `[u8; 32]`, one `[u8; 92]`, stack BLAKE3,
one 68-byte canonical-record-to-frame copy, and the userspace-to-kernel 92-byte copy. Those were
design budgets only; they were never implementation measurements.

The rejected draft is recorded in `CONTROL_A_FRESH_BUILD_HANDOFF.md`: a 21-line manifest,
504-line generated lockfile, 3-line library, 25 physical/412 normally formatted journal, and
1,043 generated target files. The 436 formatted production lines exceeded the binding 432-line
forecast before tests, fault seam, UI fixtures, or measurements existed. It retained no commit.
The exact untracked adapter directory was then removed; independent closure review observed a
clean tree and no adapter path.

## Strongest counterexamples and remaining uncertainty

The decisive counterexample is an apparently compiling `std::fs` journal with only magic/checksum
validation, a manufactured `WorkflowRecordError`, no feature-gated I/O fault seam, and no external
authority/retained-log/fault corpus. It cannot falsify exact directory/tail source-and-byte-image
behavior, full header/error priority, post-sync effect gating, or zero retained workflow
collections. A tracked/untracked/ignored adapter path would also falsify this closure; none
remains.

UNVERIFIED: all adapter correctness, fault matrix, allocation/copy count, normal/feature surface,
release text, dependency tree, syscall/write/sync count, latency, recovery work, device durability,
power loss, Windows directory sync, NFS/rename/CAS, TOCTOU, hostile writers, group commit,
producer progress, join/cleanup error composition, and compact head safety.

## No merge and next prototype condition

No code is promoted. A future isolated A candidate must begin from a new calibrated card and a
builder checkpoint that stays inside the binding formatted forecast while implementing the entire
literal control/fault/UI corpus. It must preserve the parent-approved direct `nudox-id` trait seam,
opaque private `AppendSuccess` construction, complete retained-log audit, and exact directory/tail
persistent-image falsifiers. It may not edit shared workflow/identity paths, define shared
`Published`, or merge this branch.
