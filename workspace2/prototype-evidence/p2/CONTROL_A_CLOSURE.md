# P2 control A: calibration-only authority fork

## Verdict

**RETAIN BASELINE.** This is neither product closure nor an implementation
success: control A received no builder authority after the final blind review
found two unfalsified fault/resource laws. The source baseline remains exactly
`9a14f069d593c748abd5c036fc2d5415b74d2012`; production and test churn are zero.

## Custody and commits

| Item | Value |
|---|---|
| source candidate | `9a14f069d593c748abd5c036fc2d5415b74d2012` |
| manager worktree | `/private/tmp/nudox-prototype-durable-publication-heart-control` |
| manager branch | `codex/prototype-durable-publication-heart-control` |
| frozen first card | `90eade8436dcaecfd6224a517ad0dbf9d75bf734` |
| first rewrite | `a1a7de44ecc97cf0db661d8ea8f5ac2b223466b1` |
| final card | `d932691e73afa346101d04848a84fd9a7748feb8` |
| historic counterexample | `root-review-durable` `0ff6f988f449274d1ef49434ece249a6f2f709cc`; inspected only for its 2,212-line/12-path scope failure; never copied or cherry-picked |
| shared branch action | none: `orchestra-shared` was never edited, merged, rebased, or cherry-picked |

## Task/model proof

| Boundary | Luna explicit model/fork | Terra explicit model/fork | Result |
|---|---|---|---|
| first card | `/root/p2_control_manager/control_a_luna_calibration`, `gpt-5.6-luna`, none | `/root/p2_control_manager/control_a_terra_preedit`, `gpt-5.6-terra`, none | rejected: success-pair/fault-visibility and evidence/API gaps |
| first rewrite | `/root/p2_control_manager/control_a_luna_calibration_r2`, `gpt-5.6-luna`, none | `/root/p2_control_manager/control_a_terra_preedit_r2`, `gpt-5.6-terra`, none | rejected: recovery/accessor/stop-rule and feature-surface contradictions |
| final card | `/root/p2_control_manager/control_a_luna_calibration_final`, `gpt-5.6-luna`, none | `/root/p2_control_manager/control_a_terra_preedit_final`, `gpt-5.6-terra`, none | rejected: missing directory/tail sync falsifiers and narrow retained-record audit |

All were explicit non-inheriting spawns. Terra reviewers were read-only. No implementation or
mechanical builder was authorized after any failed card.

## Candidate, delta, and measured evidence

| Candidate | State | Decision |
|---|---|---|
| A: exclusive `std::fs::File`, fixed 32/92 control | calibration only; no adapter path created | retain source baseline |
| B: bounded blocking MPSC/group control | not issued; A prerequisite absent | not evaluated |
| C: compact receipt-gated CAS head | not issued; B prerequisite absent | not evaluated |

Changed production LOC: **0**. Changed test LOC: **0**. No adapter manifest, lock, source, or
fixture exists. Only manager evidence/cards changed. The only reproduced executable gate was
`cd workspace2 && cargo test -p nudox-workflow`, passing 9 unit tests, one integration test, and
zero doctests. Therefore adapter feature-off/on gates, Clippy, release, compile-fail, dependency tree,
allocation/copy/syscall/latency metrics, recovery work, and clean candidate transcripts are absent
rather than claimed. No dependency graph changed.

The rejected A control would have used one exclusive file, a 32-byte header, a transient 92-byte
frame/scan buffer, stack BLAKE3 state, and no retained record collection. Its honest copy budget is
one 68-byte canonical-record-to-frame copy plus the userspace-to-kernel I/O copy; it does not claim
zero copies.

## Strongest counterexamples and UNVERIFIED

The surviving counterexamples are: (1) directory-sync failure after header file sync with erased or
misclassified source and a returned journal; (2) tail-repair sync failure after truncation reported as
healthy recovery; (3) a preallocated `Vec<WorkflowEvent>` cache retained through replay. Still
UNVERIFIED: TOCTOU path replacement, hostile cross-process writers, dishonest device caches/physical
power loss, Windows directory sync, NFS/remote rename/CAS behavior, syscall tracing, x86/Linux
performance, MPSC cancellation/drop/join/reclamation, group fan-out, compact-head safety,
head-to-stable-byte proof, and async completion.

## Parent-authorized successor requirements; no merge

The parent accepted the final reviewer findings as literal successor-card requirements: add exact
feature-on directory-sync and tail-repair-sync integration falsifiers asserting step, source, no
owner, and on-disk outcome; and audit every dynamic `WorkflowEvent`/`WorkflowRecord` collection
(`Vec`, `Box`, `Arc`, `Rc`, collect, preallocation). Per calibration policy, this failed family issues
no production authority. A fresh isolated manager card may use those requirements, recalibrate with
fresh explicit Luna and Terra roles, then implement only A if it passes. B/C, shared typestate,
active identity changes, shared-branch writes, roadmap closure, and any merge remain forbidden.
