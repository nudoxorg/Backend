---
name: manage-rust-swarm
description: Autonomously manage one workspace2 Rust capability with a Terra orchestrator, parallel Luna proof workers, and an independent Terra reviewer. Use for proposal research, evolving proof matrices, worker integration, hostile verification, and returning one closed candidate to the Sol chief.
---

# Manage a Rust capability swarm

Read `../../../ORCHESTRATION.md`, `../deliver-reviewed-rust-slice/SKILL.md`,
`../review-rust-gem/SKILL.md`, `references/capability-evidence.md`, `../../../TESTING.md`, and exactly
one applicable domain skill completely. Those documents own role custody, Rust craft, evidence
binding, and the adversarial test catalog; this skill defines the Terra control loop.

The primary Terra owns one capability from brief to candidate. It does not wait for the Sol chief to
supply repository facts, worker prompts, repairs, or the next action. It may return only:

- `CLOSED`: a candidate with exact evidence and zero unresolved blocker/major findings;
- `AUTHORITY_FORK`: one decision between materially different public behavior, permanent protocol
  meaning, new dependency/unsafe/SIMD authority, destructive external state, or scope outside the
  named capability; or
- `EVIDENCE_BLOCKED`: a non-acceptance receipt after two materially different, bounded diagnostic
  attempts encounter the same unavailable tool/platform/source, corrupt baseline, or irreproducible
  branch. It retains raw failures, affected matrix rows, attempted alternatives, and the external
  owner/action required. It is never an architecture escalation or permission to weaken a law.

## Phase 0: close pre-edit proof custody

Before any production-writing Luna starts:

1. create the canonical capability evidence directory and `index.toml`;
2. commit the brief, coupling skeleton, initial proof matrix, safe-control design/measurement plan
   (no production code), red falsifiers, and resource controls;
3. record the exact `TESTING.md` digest and map every applicable clause to a matrix row or a precise
   evidenced exclusion;
4. freeze a rationale-free pre-edit packet and pass it through the registered source-isolated Terra
   reviewer using the distinct sidecar environment defined below;
5. repair and recommit any contract finding, invalidating the earlier packet.

This phase is sequential because workers cannot implement a card that does not yet exist. Read-only
repository mapping may run concurrently only when it cannot create or prejudge production edits.

## Start parallel after Phase 0

Inspect live capacity, reserve one slot for independent review, and immediately dispatch registered
`nudox_luna_implementer` workers for independent rows. Each dispatch expects `gpt-5.6-luna` at `max`
and records the resolved role/config/model/effort/sandbox/baseline/checkout receipt. Useful first cards
include the safe/std control, one counterexample representation, or one resource/codegen measurement.

Do not dispatch essay tasks, architecture selection, product scoring, broad plans, or overlapping
writers. If only one coherent edit exists, use one Luna and do the other work yourself; agent count is
not progress.

## Establish the four durable artifacts

Use `.codex/evidence/capabilities/<capability-id>/` and its schema-bound `index.toml`. Commit one
canonical version of each artifact on the manager branch:

1. **Capability brief**

   ```text
   public terminal and chief-owned red journey
   non-negotiable laws and explicit negative space
   baseline commit/tree and concurrent path ownership
   affected consumers and dependency direction
   TESTING.md digest and applicable clause -> matrix row / evidenced exclusion
   product-authority questions only
   ```

2. **Proof matrix**

   ```text
   law | weakened implementation | falsifier | required evidence | state | owner
   ```

   States are `RED`, `PROVED BY WORKER`, `REPRODUCED BY TERRA`, `FALSIFIED`, or `UNVERIFIED`.
   Only Terra may move a row to `REPRODUCED BY TERRA`.

3. **Research journal**

   ```text
   source/experiment | mechanism | conflicting trade-off | decision or falsifier changed
   ```

   Every row names the unresolved matrix/representation decision it informs. Use primary
   documentation, upstream production source, and local experiments. Record only
   research that changes the candidate set, proof matrix, resource bound, or rollback decision.
   Stop that decision's research when two independent relevant sources/experiments add none of those
   facts; retain uncertainty. Nearby or irrelevant sources do not count.

4. **Closure receipt** containing candidate identity, exact commands and raw artifacts, role/model
   custody, reviewer output, strongest surviving counterexample, and remaining uncertainty.

Update canonical artifacts in place. A semantic change alters the digest and invalidates prior worker
or reviewer evidence for that version. Preserve history in commits, not appended “clarifications.”

## Freeze Luna cards

Each Luna receives one locally decidable card:

```text
registered role: nudox_luna_implementer; expected gpt-5.6-luna/max
baseline and owned paths; no overlapping writer
one public or internal terminal
proof-matrix rows and already-red falsifiers
semantic and resource bounds
forbidden adjacent surface and stop decisions
exact focused commands
commit protocol and return schema
```

The card selects behavior and evidence, not implementation taste. Luna may redesign within the card
when a stronger representation proves the same laws. It must not change a matrix row, invent a future
API, weaken a failure, or silently expand paths. A worker commits each coherent checkpoint after its
focused falsifiers and formatting pass, then reports the commit and the smallest remaining red row.

Put concurrent writing workers in isolated worktrees or rigorously disjoint paths. `index.toml`
records task ID, resolved role config, actual model/effort/sandbox, card digest, baseline, checkout,
and returned commit. A role name, task name, or model string alone is inadmissible evidence.

## Terra's continuous lane

While Luna works, Terra must perform work that cannot be delegated mechanically:

- trace every current consumer and invariant owner;
- write concise unit/fault oracles and prove they fail a plausible weakened implementation;
- study relevant algorithms, formats, allocation lifetimes, concurrency transitions, and production
  implementations;
- compare the safe control with serious alternatives using raw measurements;
- reduce the proof matrix as laws become construction-time invariants;
- prepare the independent reviewer packet without builder rationale.

Terra does not rewrite Luna's code in parallel. It authors tests in manager-owned paths or an isolated
branch and integrates deliberately.

## Ingest every return

For every Luna result:

1. inspect the exact commit and path ownership;
2. reproduce its falsifiers and measurements;
3. attack a nearby legal case and a weakened mutant;
4. compare the mechanism with the safe control and other probes;
5. choose exactly one transition:

   ```text
   accept checkpoint -> integrate and issue the next smallest red row
   reject checkpoint -> preserve evidence and issue one falsifier-bound repair card
   reject representation -> write salvage ledger, retain branch, commission replacement
   learn new law -> update/commit matrix, invalidate stale evidence, issue new cards
   close all rows -> begin independent closure review
   authority fork -> return one precise decision to Sol
   ```

Do not keep a patch because it consumed effort. Do not restart a broad swarm because one reviewer
found a local issue. Translate the accepted finding into one red row and give Luna that bounded repair.

## Independent Terra review

Request the registered `nudox_terra_reviewer` role, expecting `gpt-5.6-terra`/`xhigh`, at three boundaries:

1. frozen brief, proof matrix, and typed skeleton before broad editing;
2. first material end-to-end candidate, before its representation hardens;
3. exact closure candidate and raw evidence after repairs.

Create a fresh `git archive`/export of the exact candidate without `.git` history or the manager
journal. Add only the hashed rationale-free review packet. The writable Terra manager must not spawn
the reviewer as its direct child: live parent permissions can override the custom role's sandbox
default. Instead invoke a distinct Codex parent process rooted at a disposable build directory. Copy
only the project agent configuration/skills needed for role discovery into that directory; pass the
separate source snapshot as a readable absolute path and never as `-C` or `--add-dir`.

Create `<disposable-build-root>/tmp` and `<disposable-build-root>/target`, then use this verified CLI
shape. The explicit empty `writable_roots` override prevents user-level configuration from widening
the sandbox:

```text
TMPDIR=<disposable-build-root>/tmp \
CARGO_TARGET_DIR=<disposable-build-root>/target \
codex --ask-for-approval never exec --model gpt-5.6-luna \
  -c model_reasoning_effort=low \
  -c 'sandbox_workspace_write.writable_roots=[]' \
  -c sandbox_workspace_write.exclude_tmpdir_env_var=true \
  -c sandbox_workspace_write.exclude_slash_tmp=true \
  --sandbox workspace-write --json --skip-git-repo-check \
  -C <disposable-build-root> <sidecar-prompt>
```

Its only prompt is to spawn the registered
`nudox_terra_reviewer`, wait for it, and return the raw receipt. The sidecar parent and reviewer write
compiler output and temporary files only under that build root; commands name manifests inside the
separate snapshot and inherit the explicit `TMPDIR` and `CARGO_TARGET_DIR` beneath the build root.

Record the sidecar task, reviewer task, snapshot tree, packet digest, resolved role/config/model/
effort, runtime-reported effective sandbox and sole writable root, excluded implicit temp roots, and
before/after aggregate source hashes in `index.toml`. Reject the review when the runtime does not
report effective `workspace-write` restricted to the disposable build root, `$TMPDIR` or `/tmp`
remains an implicit writable root, the snapshot is writable or passed as a writable root, the
reviewer is a direct child of the manager, source hashes differ, or the child role cannot be verified. Return
`EVIDENCE_BLOCKED` after the bounded alternate attempt; never replace custody with an index assertion.

Give the reviewer the frozen contract, candidate, consumers, controls, and evidence—never the
builder's rationale, your suspected defect, or intended correction. It applies
`../review-rust-gem/SKILL.md`, returns ranked findings and cleared suspicions, and makes no edits.

Terra reproduces every accepted finding, adds the falsifier to the matrix, and commissions a Luna
repair. If the reviewer cannot reproduce or locate a claim, the evidence remains `UNVERIFIED`; Terra
does not convert uncertainty into approval. The reviewer never owns acceptance.

## Large-block salvage

Before rejecting or replacing a substantial worker implementation, partition it by exact symbol into:

- representation;
- invariant and authority;
- algorithm/hot-path mechanism;
- diagnostics and source preservation;
- test oracle or generator;
- measurement and control.

For every useful row name the destination owner, replay the old falsifier against the replacement,
and add a mutant that dies only if the mechanism is real. Whole-block removal is exceptional and
requires every row to be absorbed or rejected by an executable counterexample. Do not preserve an
accidental public API merely to retain its internal idea.

## Candidate closure

Before returning to Sol:

- every matrix row is `REPRODUCED BY TERRA` or explicitly out of contract with establishing evidence;
- every applicable `TESTING.md` clause is bound to such a row or evidenced exclusion;
- the final independent reviewer reports zero blockers/majors;
- worker and reviewer model selections and edit custody are retained;
- every allocation, generic dimension, unsafe/SIMD path, dependency, and public item has a current
  consumer plus measured or type-level benefit;
- focused and affected workspace gates pass from a clean candidate and status remains unchanged;
- losing prototypes have complete salvage ledgers;
- the journal states its saturation evidence and remaining unknowns.

Return `CLOSED`, `AUTHORITY_FORK`, or `EVIDENCE_BLOCKED` plus the canonical evidence-index path. For
`CLOSED`, return the exact candidate commit/tree, changed paths, public terminal, strongest attempted
counterexample, proof matrix, research decisions, measurements, reviewer findings, raw gates, salvage
destinations, and genuine uncertainty. Do not issue a product score or edit the shared roadmap.
