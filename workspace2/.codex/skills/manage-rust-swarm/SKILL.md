---
name: manage-rust-swarm
description: Manage fresh scoped Rust workers through evidence, implementation, hostile review, rework, and closure for one workspace2 capability. Use when a Terra manager must decompose a capability, issue worker rubrics, verify every claim, limit churn, progressively strengthen shared skills, and return one integrated candidate to the parent.
---

# Manage a Rust swarm

Read `../deliver-reviewed-rust-slice/SKILL.md`, the applicable domain skill, and
`../review-rust-gem/SKILL.md` completely. The parent supplies one capability boundary. Manage it;
do not reinterpret the roadmap or broaden adjacent public APIs.

The manager owns decomposition, worker prompts, review, integration, gates, and the final candidate.
Workers own narrow evidence or patches. The parent reviews only the manager's closed candidate.

## Freeze the trial

Before spawning a worker, write a manager card:

```text
Capability and observable consumer
Allowed paths and named baseline
Preserved semantics and dependency boundary
Resource budgets: retained/live bytes, allocations, copies, work, queue, latency, text, LOC
Required public/fault/integration evidence
Explicit negative space
Final parent decision
```

Freeze the named baseline independently of version control. Record every writable file's relative
path, formatted LOC, and content digest; record absent paths explicitly. A dirty, untracked, nested,
or from-scratch tree makes `git diff` an unreliable baseline. Reviewers compare the candidate to this
ledger and may use Git only as corroboration. A claim that all pre-existing untracked source is new
churn is a reviewer failure, not an implementation finding.

Reject a capability whose proof cannot fit one review cycle. Split at an observable boundary, not by
file. Record unrelated dirty files and assign disjoint write ownership. Only the manager edits shared
manifests, public reexports, plans, and skills unless explicitly delegated.

The parent and sibling managers freeze the target's transitive production dependency paths for the
duration of the build/break cycle. If an unavoidable parent edit changes one, stop the worker,
recompute the digest ledger, rerun its focused baseline, and issue a named rebase. Never let a worker
silently verify against moving types.

## Worker commit protocol

Put each writing worker in an isolated worktree/branch at the frozen base when the repository state
allows it. The worker commits each proof-bearing checkpoint after its focused falsifiers, formatting,
and owned tests pass; it then reports the commit ID and exact remaining failure. The manager reviews
commit ranges and cherry-picks only accepted checkpoints. Rejected prototypes remain inspectable on
the worker branch and count as written-then-rejected churn; they never become anonymous edits in the
manager tree.

Each worker commit stages only its explicit owned paths with `git add -- <paths>`, checks the staged
diff, and records the baseline/candidate digests in its handoff. Never use `git add -A`, `git commit
-a`, amend another worker's commit, stage unrelated dirty files, or commit a red gate. A repair is a
new focused commit so the manager can see the causal delta. The manager may squash only after closure
and only when the parent asks; review evidence retains the original checkpoint IDs.

If an untracked from-scratch tree has no commit containing the frozen baseline, the parent must first
create or authorize a path-scoped baseline commit. Do not fake commit isolation in a shared dirty
worktree. When the requested low-cost worker model is unavailable, use the available worker model but
preserve this protocol and state the substitution explicitly.

## Commission workers

Use fresh workers. Give each only its raw inputs, exact allowed paths, an evidence rubric, and a stop
condition. Do not leak the manager's suspected answer into an independent audit.

Prefer this sequence:

1. **Scout, read-only:** find current call graph, invariants, measurements, simplest baseline, and
   counterexamples. It may run isolated experiments but cannot touch production.
2. **Builder:** implement the manager-approved smallest vertical proof. It stops on an unplanned
   public item, dependency, unsafe, SIMD, allocation policy, or budget breach.
3. **Breaker, read-only:** receive the frozen contract and resulting artifact, not the builder's
   rationale. Try to falsify semantics, resource laws, diagnostics, and tests.
4. **Repair:** receive only accepted findings with exact falsifiers. Delete/redesign before adding
   machinery. A second failure of the same law returns to decomposition rather than another patch.

With two worker slots, scout and test-breaker may run concurrently only when paths are disjoint and
both are read-only. Never have two builders edit the same abstraction.

Every worker rubric is pass/fail evidence, not an aspirational score:

```text
law | artifact/command | exact expected evidence | cap if absent | stop trigger
```

Require a falsifier for every row. Green compilation alone proves no row. Do not award points for
LOC, genericity, SIMD, unsafe, dependencies, or allocation count; they are charged unless a measured
contract property improves.

Before promising a wrapper around a foreign trait or SDK lifecycle, the scout must compile or fully
enumerate the current trait surface—including resource/configuration forwarding, enablement,
flush/shutdown, error conversion, and shared state ownership—and format a skeleton LOC forecast.
If that complete surface misses the phase budget, split the capability before a builder writes the
wrapper. A test-private counter is not a substitute for an application-visible health capability.

## Review without churn

At every handoff:

1. Re-read the patch from the named baseline; never review prose in place of code.
2. Recompute formatted production/test LOC and unused reserve.
3. Run the hostile review passes in `review-rust-gem`.
4. Rank findings by invalid state or user-visible cost. Reject speculative cleanup.
5. Send one coherent finding packet. Do not drip style comments across repeated turns.
6. After repair, rerun exact falsifiers first, then owned gates, then integration gates.

Concurrency/liveness evidence must observe the producer independently. For a bounded nonblocking
claim, require producer completion under its own deadline while the consumer/exporter remains
blocked, then release and join every participant. A sequential loop followed by release, or a
timeout inside the blocked consumer, cannot prove producer progress. Cleanup paths preserve both the
primary and release/join failures; do not discard errors merely because the test is already failing.

The manager may directly fix only a mechanical integration defect smaller than the cost of another
worker turn. Any representation, ownership, error, concurrency, protocol, or API correction returns
to a worker with a narrower contract.

## Progressive learning

Read [cycle-stages.md](references/cycle-stages.md) at the indicated phase. Early workers receive only
baseline laws and task-local sources. Load niche allocation, codegen, concurrency, protocol, or OTEL
sources only after the scout identifies a concrete decision they can falsify.

After closure, propose at most three reusable skill changes. Each must name:

- the repeated failure or wasted turn;
- the exact new do/don't or stop rule;
- why existing text failed to prevent it;
- a forward-test prompt that would catch regression.

Update a skill only for a generalized lesson, never to memorialize a task-specific patch. Validate
every changed skill with the official validator.

## Closure packet

Return findings first and approval last:

- contract matrix with exact evidence;
- changed/deleted surface and normally formatted net LOC;
- allocation/generic/work/diagnostic ledgers;
- strongest counterexample attempted and result;
- exact test/gate output, including failures encountered;
- worker turns, written-then-deleted LOC, and why each rework occurred;
- unverified platforms and honest roadmap gaps;
- proposed skill changes and the next smallest parent decision.

Never claim the product roadmap complete. The manager closes one capability or returns a precise
blocker.
