# Linux exiting task with unavailable executable

This source slice is based on canonical `e4a877f139d8204daae1dd6ff3b2adb6db93afb3`, including PR310's additive issue diagnostics. Initial witness commit `d7b6145d5e1b6db16fcff17b3b6a29283c7c8a49` is immutable; an atomic follow-up adds the exact audited release gate described below. It does not change fleet capacity, Cargo job limits, collection completeness, terminal/exit proofs, or signal any process. The independent GUI publication change `612630a976d4117b39fc203542b184762e38c241` is unchanged.

## Existing evidence and limits

The saved diagnosis at `/Users/mileswirht/.cache/nudox/mcp-current-triage/current-canonical-lock-repair-20261014/ilo-census-issue-diagnosis01/report.json` recorded PID2284254 (`nix`), start ticks138598638, stateD, flags0x40010c, executable ENOENT and a non-ready pidfd. The existing collector correctly refused `proc-path-missing-without-pidfd-exit-proof`. A separately captured single-thread observation is not a witness for that earlier payload. This change makes no claim that this PID is dead, currently unchanged, or newly admissible.

The saved report's `raw_followup` records uname release6.18.38 and booted modules `/nix/store/yv68w9a7yr31f9iia46kyhw7jvgnzyy4-linux-6.18.38-modules`; `pinned_headers_sched_h_path` is null. This review used the exact upstream stable `v6.18.38` primary source and makes no claim of local header verification. It did not run another ILO process/header probe, collector, admission guard, native job, or build. A future fresh collector must obtain every new witness field together. Missing evidence continues through the existing strict refusal/diagnostic path.

The coordinator subsequently reported that a fresh read-only candidate census was complete but PID2284254 was absent. Its packet is `/Users/mileswirht/.cache/nudox/mcp-current-triage/current-canonical-lock-repair-20261014/linux-exiting-witness-root01/report.json`. The new witness path was **not exercised**; this provides no live-path validation credit. The author did not rerun that collection.

## Why the stat-only proposal is insufficient

The stat reader obtains `num_threads` under the sighand lock, but prints task flags later. A possible interleaving reads one thread while a leader is live; it creates a sibling and exits before flags are printed. A non-leader exec can inherit both leader PID and start time. Repeating stable PID/start/flags/thread-count rows therefore does not itself establish one immutable task or exclude a live sibling. This is a source-level counterexample, not an assertion about the observed PID. See [stat capture order](https://github.com/gregkh/linux/blob/v6.18.38/fs/proc/array.c#L486) and [non-leader exec identity transfer](https://github.com/gregkh/linux/blob/v6.18.38/fs/exec.c#L915).

## Stronger witness and soundness boundary

The typed `_LinuxExitingWithoutExecutable` is available only for ENOENT from the executable read. First, pinned and final stat identities must agree in start ticks, parent, process group, session, state, flags and thread count; they must show `PF_EXITING`, one thread and no `PF_KTHREAD`. Two bounded, complete, duplicate-free status payloads, before and after the unavailable executable read, must agree on all four UIDs and match PID/TGID/parent/state, `Kthread:0`, and `Threads:1`. Required tail counters distinguish a complete payload from a truncated prefix. Any memory field, even zero, defeats the witness.

The follow-up additionally requires trusted `os.uname().release` to be a plain string exactly equal to `6.18.38`, the only audited release. No suffix normalization, version ordering, environment override, or CLI bypass exists. Other, unavailable, and malformed releases retain the original strict executable-absence refusal. Extending this gate requires another kernel source audit; equivalent-looking suffixes and newer releases do not inherit this proof.

In the reviewed kernel, status retains one task reference, acquires its mm before reading Threads under the sighand lock, and prints memory fields whenever mm exists. Thus the same payload supplies the no-mm and sole-thread relationship. An old leader being replaced by exec still counts with its sibling until release; release decrements the count and clears its sighand under the same lock, after which its status reports zero threads. It cannot supply a false sole-thread payload. See [status ordering and locked count](https://github.com/gregkh/linux/blob/v6.18.38/fs/proc/array.c#L248) and [release locking](https://github.com/gregkh/linux/blob/v6.18.38/kernel/exit.c#L151).

No-mm is inferred from absence of MMU and NOMMU memory output, never from executable absence alone: [MMU output](https://github.com/gregkh/linux/blob/v6.18.38/fs/proc/task_mmu.c#L60), [NOMMU output](https://github.com/gregkh/linux/blob/v6.18.38/fs/proc/task_nommu.c#L69). Executable lookup can fail for other reasons: [get_task_exe_file](https://github.com/gregkh/linux/blob/v6.18.38/kernel/fork.c#L1224).

A sole task already in the non-returning exit path with its address space detached cannot resume user Cargo scheduling, fork or exec. It may remain blocked in kernel cleanup and consume resources. `PF_EXITING` means shutdown in progress, not death; [exit_mm](https://github.com/gregkh/linux/blob/v6.18.38/kernel/exit.c#L521) precedes later teardown. A non-ready [pidfd](https://github.com/gregkh/linux/blob/v6.18.38/fs/pidfs.c#L232) does not certify death. The witness therefore produces an identity-validated **occupied unknown** process record, preserving state and process group; it never produces terminal or exit evidence.

The Darwin path already retains validated unknown-executable groups. Linux does not gain that broad fallback: a generic inaccessible live executable could hide Cargo `-j64`. This path instead requires the narrower exit/no-mm/sole-thread proof. Cargo, rustc and runtime-owner command hints remain their existing candidate kinds. The existing unknown-group residual slot and limitations remain unchanged; no job limit becomes known. Existing children and all other process groups still need ordinary fresh complete collection. An accessible Cargo `-j64` still trips the unchanged per-Cargo cap.

## Lightweight validation

Tests use scripted proc adapters and temporary fixture boot IDs; none reads real `/proc`, opens a pidfd, runs `ps`, launches Cargo or invokes the collector. The new suite covers valid occupied accounting, candidate hints, stable identity fences, missing thread proof, the stat-only counterexample, before/after UID/task/thread/state disagreement, duplicate/missing/truncated/oversized status, MMU/NOMMU memory presence, unavailable/ready pidfds, genuine permission/I/O failures, and the unchanged Cargo64 rejection.

Executed with existing `/nix/store/0788rbnl2p558jnyjlm2s2f9r5sxpq3h-python3-3.14.6/bin/python3.14 -B <file> -q` (no Nix evaluation):

- `tests/test_linux_exiting_witness.py`:14 passed (including exact release, unsupported/suffix/newer/malformed release, and unavailable uname controls).
- `.config/scripts/test_build_census_issue_details.py`:3 passed.
- `tests/test_build_census.py`:25 passed.
- `tests/test_linux_proc_race.py`:33 passed.
- `.config/scripts/test_fleet_capacity_census.py`:16 passed.
- `tests/test_fleet_capacity_census.py`:20 passed.
- `git diff --check`:passed.

All111 controls passed after the release gate. Native behavior and future ILO admission remain unvalidated. The coordinator must independently review the follow-up source and run its Python controls; any live-path credit would require a fresh witness actually exercising this path. Canonical integration and launcher pin updates remain coordinator work, separate from currently admitted workloads.
