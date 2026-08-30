---
name: manage-rust-swarm
description: Drive one workspace2 capability from a concrete red falsifier through committed implementation, hostile review, repair, and integration while research runs concurrently and must continuously change code or tests.
---

# Manage a Rust swarm

Read `../deliver-reviewed-rust-slice/SKILL.md`, the applicable domain skill, and
`../review-rust-gem/SKILL.md` completely. The parent supplies one capability boundary. Deliver that
capability; do not reinterpret the roadmap or broaden adjacent public APIs.

The manager owns decomposition, implementation velocity, research, review, integration, gates, and
the final candidate. Workers own narrow experiments, falsifiers, or patches. A manager is not a
packet router: it remains able and expected to write, delete, repair, and integrate code itself.

## Progress is the clock

Begin with the smallest executable vertical falsifier and production path. Pre-edit reviewer custody,
rubric refinement, research completeness, model routing, and evidence formatting are never authority
gates for writing reversible code in an isolated branch.

Research is mandatory and concurrent. Each research tranche must name:

```text
question | source/control | decision changed | code/test affected | next falsifier
```

Before opening another research tranche, produce at least one concrete progress receipt:

- a reproduced red test or fault injection;
- a coherent production checkpoint committed on an isolated branch;
- a deleted or simplified mechanism with its former falsifier still passing; or
- an integrated checkpoint with its affected gates.

Research that repeats known ideas, cannot name a changed decision, or delays the next falsifier stops.
Archive the useful citation and return to implementation. Deep reading may continue in a parallel
read-only lane while builders advance; it may not serialize the whole capability. It should feel
wrong to produce a second design packet without an intervening code or test receipt.

An unavailable preferred model, reviewer, sidecar, or receipt format is an orchestration defect, not
a product blocker. Record it once, use an available role or review locally, and keep implementing.
`EVIDENCE_BLOCKED` is reserved for a real external dependency or authority that prevents the code or
its required proof from being produced after two materially different technical attempts.

## Freeze only what protects work

Before parallel writes, record a compact manager card:

```text
Capability and observable consumer
Allowed paths and named baseline
Invariant owner and dependency boundary
Resource bounds: retained/live bytes, allocations, copies, work, queue, latency, text
First red falsifier and first shippable vertical checkpoint
Explicit negative space
```

For a dirty or from-scratch tree, record every writable path and content digest plus absent paths.
Git is corroboration, not proof that pre-existing untracked work belongs to the agent. Assign disjoint
write ownership and let only the manager edit shared manifests, reexports, plans, and skills unless
explicitly delegated.

Split work at observable capabilities and invariant ownership, not arbitrary file or source-volume
targets. Complexity, state space, dependency direction, retained resources, and reviewability are
real scope signals. Line count, word count, and parameter count are not gates.

## Commit protocol

Use isolated worktrees or branches whenever repository state permits. A worker commits each coherent
proof-bearing checkpoint after formatting, focused falsifiers, and owned tests pass, then reports the
commit and exact remaining failure. Red experiments may remain on a named spike branch; they do not
enter the integration branch.

Each commit stages only explicit owned paths with `git add -- <paths>` and inspects the staged diff.
Never use `git add -A`, `git commit -a`, amend another worker's commit, or absorb unrelated dirt.
Repairs are focused follow-up commits until integration; squash only when explicitly requested.

When the merge center is dirty, first inventory and commit its existing changes in coherent,
path-scoped slices without rewriting or dropping them. Then integrate candidates mechanism by
mechanism. Never use a blind merge to settle overlapping public APIs; choose one invariant owner,
transplant the strongest implementation, run the old falsifiers, and preserve rejected work on its
source branch.

## Commission action lanes

Start the builder immediately on the first vertical falsifier. Run at most one read-only scout in
parallel for a concrete unresolved decision. Commission a hostile reviewer only after a candidate
checkpoint exists.

1. **Builder:** reproduce the red, implement the smallest end-to-end behavior, commit, and report the
   exact next red. It stops only for a material unbriefed boundary or external authority.
2. **Scout, concurrent and read-only:** compare production precedents or experiments for one named
   decision. Its answer must change an active implementation/test choice.
3. **Breaker, post-checkpoint and read-only:** receive the contract, candidate diff, and public
   consumer—not the builder's rationale. Try to falsify semantics, resources, diagnostics, and tests.
4. **Repair/integration:** the manager or a narrow builder fixes accepted findings, preferring
   deletion and stronger representations, then commits and integrates the accepted mechanism.

Do not drip style comments through repeated turns. Send one ranked finding packet with exact
falsifiers. A second failure of the same invariant triggers a boundary redesign, performed in code,
not another prose cycle.

Every rubric row is executable:

```text
law | command/artifact | exact expected evidence | technical stop trigger
```

Compilation alone proves no row. Do not reward genericity, SIMD, unsafe, dependencies, novelty, or
allocation slogans; charge them unless a measured contract property improves.

## Review and integration

At each checkpoint:

1. Read the actual diff from the named baseline.
2. Run the focused falsifier and strongest nearby regression first.
3. Review invariant ownership, invalid states, allocation/lifetime, errors, concurrency, and public
   integration using `review-rust-gem`.
4. Delete or reshape before adding helpers, adapters, states, or compatibility layers.
5. Commit the repair and run owned gates, then cross-crate gates.
6. Integrate immediately when the checkpoint is sound; do not hold completed code for more research.

Concurrency/liveness evidence observes producers independently and joins every participant. Cleanup
preserves both primary and release/join failures. Protocol and durability evidence exercises exact
partial operations and replay. Performance claims retain raw controls for the exact representation.

## Learning without bureaucracy

Update a shared skill only for a repeated, generalized failure. State the old ambiguity, the new
decision rule, and a realistic forward test. Keep the update short and validate it with the official
skill validator.

Research notes are durable inputs, not completion artifacts. A capability closes only with integrated
code and executable proof, or a real technical/external blocker that prevents both. Reviewer absence,
agent limits, elapsed time, and a polished evidence directory never close product work.

## Handoff

Return findings first and approval last:

- integrated commit and changed/deleted surface;
- contract matrix with exact executable evidence;
- allocation/generic/work/diagnostic ledgers;
- strongest counterexample attempted and result;
- exact focused and integration gate outputs;
- useful rejected mechanisms and where they remain inspectable;
- unverified platforms and honest roadmap gaps;
- the next smallest executable red.

Never call an evidence-only terminal product completion.
