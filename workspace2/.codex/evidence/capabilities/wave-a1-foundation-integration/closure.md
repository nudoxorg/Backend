# Wave A.1 foundation integration — Phase 0 review receipt

Phase 0 remains open; no production source or public test has been edited by
this manager and no Luna worker has started.

## Non-accepting pre-edit review

The direct source-isolated reviewer task
`01a05156-e0f9-7d01-b958-298f51832b45` ran as
`nudox_terra_reviewer`, config `.codex/agents/nudox-terra-reviewer.toml`,
`gpt-5.6-terra`/`xhigh`. Its full JSON receipt is
`packets/pre-edit-1-reviewer.jsonl`. It returned `EVIDENCE_BLOCKED`, not
acceptance.

- Read-only snapshot: `/private/tmp/nudox-wave-a1-preedit4-source.8SEUXP`,
  no `.git`; source aggregate before/after
  `aedfcfa63824c6b58601fef367378202a08a2cc70543541445ec23211a2eab43`;
  `workspace2` aggregate before/after
  `3da9f90f98cea3c0e5e408a53d9fcb280e79fe51ad67d79792b1b6bd514cf8c9`.
- Sole intended build root:
  `/private/tmp/nudox-wave-a1-preedit4-build.TzGnVQ`; explicit `TMPDIR` and
  `CARGO_TARGET_DIR` were below it; `/tmp` probe was non-writable. Runtime
  also reported parent `/private/tmp`, so this review is retained only as a
  non-accepting finding source, not proof of strict sole-root custody.
- Packet and frozen red SHA matched their stated values. The normal Dylint
  runner was unavailable under this sidecar because required Nix toolchain
  variables were absent and the runner has source-local target assumptions;
  this is tooling evidence, not a production finding.

The review invalidated the packet because the snapshot did not provide the
digest-matching brief/matrix/index and contained historical
`crates/nudox-object-pack/MANAGER_EVIDENCE.md`. A next packet must attach the
exact reviewed evidence inputs and the export must exclude that historical
manager-evidence file.

## Chief-owned red-journey repair and clean custody

The Sol chief repaired the public-test gaps in
`7f90b60d8ee3efa27c7d60f6819781fab2327f71`, test SHA-256
`31c92918c4318cf1d5092504a16e34724e2f9c8e772e85e2e0a894ebab9db5df`.
Its assertions cover exact requested missing identity, rejected-owner
bytes/pointer/capacity with exact nested `HashMismatch`, wrong full artifact
for header/index/body, header/index truncation and mutation, stale bind, and
unchanged store/no capability after rejection.

Manager history `015adbd49a6173bd2a1c0c4df89a3a73d6ee0fba` is retained solely
as failed custody evidence: it merged another controller branch, contrary to
scope. The active manager instead starts at
`8007d7702d797b5ed8711050015e9cddd41fb9f5` and carries only ordinary
single-parent chief-test transplant `e121f82365d835e5afa6bc569ed1cded595d2152`.
It must regenerate a clean, path-limited snapshot with exact digest-matching
brief/matrix/index/TESTING inputs and no legacy `MANAGER_EVIDENCE.md` or
builder-rationale artifact before a fresh pre-edit review. No writer may start
until that review closes.

## Invalid launcher records

Attempts `01a0514f-35c8-7fa2-ba58-c883e2ef5c04`,
`01a05150-d187-7103-8754-44f8c5052aa9`, and
`01a05154-c239-7081-9665-97987c45b5b8` were invalid dispatcher launches: the
first lacked referenced role configs and used a full-history spawn; the latter
two emitted `wait` with no receiver. They minted no admissible reviewer receipt
and made no source change. Raw launcher logs remain in their disposable build
roots under `/private/tmp/nudox-wave-a1-preedit{,2,3}-build.*`.

The direct-review attempt `01a05156-e0f9-7d01-b958-298f51832b45` remains a
non-accepting finding source only. The replacement pre-edit procedure is
authoritative: a distinct, dispatch-only `gpt-5.6-sol`/`low` sidecar must spawn
registered `nudox_terra_reviewer` at `gpt-5.6-terra`/`xhigh` with
`fork_turns = "none"`, retain the nonempty runtime child task ID before waiting,
and use only the disposable build root with empty configured writable roots and
implicit `TMPDIR`/`/tmp` writes excluded. A direct manager reviewer, a Luna
parent, full-history fork, missing receiver, or prose-only child claim fails
custody. The immutable source export is passed only as a readable absolute path,
not a working directory or writable root.

Correct-procedure sidecar attempt 1, task
`01a05172-7fcd-7581-8096-cd9833388d55`, is also invalid. Although an agent
message claimed `/root/hostile_review`, the raw JSON has no spawn receipt and
its first collaboration call is `wait` with `receiver_thread_ids: []` and
`agents_states: {}`. It was terminated without a reviewer child. The immutable
snapshot aggregate stayed
`99379d055dbccaad07a4f1138cec18275d1d997d0766a68c96747991cc3220c4`
before and after; the raw sidecar stream is
`/private/tmp/nudox-wave-a1-preedit6-build.sOIcaY/sidecar-parent.jsonl`,
SHA-256 `644409a9481f84df9b44da88a7c6ae69bb552ff016d64aff6198a8579367b7ae`.
The sole permitted alternate changes the dispatch prompt to require an actual
first `spawn_agent` tool call before any prose or wait, retaining this failure.

Correct-procedure sidecar attempt 2, task
`01a05175-5815-7d43-ad85-03391fc29e3e`, changed only that first-action prompt
constraint. It repeated the same failure: a prose `/root/hostile_review` claim,
no runtime spawn receipt, then `wait` with `receiver_thread_ids: []` and
`agents_states: {}`. It was terminated without a reviewer child. The immutable
snapshot aggregate stayed
`aeca4858aa27374ccc8da5ca654419c3459cefbcbedbbcb7d04d184e59a03ad1`
before and after; raw stream
`/private/tmp/nudox-wave-a1-preedit7-build.0yYv3V/sidecar-parent.jsonl` has
SHA-256 `77ec8955358e049c1cd946b4ec2dea318b6e8c1b932f5f8c78784c850f30d944`.

## Phase 0 outcome: EVIDENCE_BLOCKED

Two materially different bounded dispatch prompts under the required
`gpt-5.6-sol`/`low` parent, reviewer-only role configuration, empty explicit
writable roots, excluded implicit temp roots, and immutable source snapshots
could not obtain a runtime reviewer child. No direct review, production edit, or
Luna dispatch is permitted as a substitute. Affected matrix rows A1–A10 remain
`RED`; the missing proof is the independent pre-edit reviewer receipt. The
external owner/action is the Codex runner: make the registered
`nudox_terra_reviewer` spawn return an actual nonempty runtime receiver/task ID
which can be waited, under the declared role/model/effort and sidecar sandbox.

Frozen implementation constraints remain: no shared-worktree merge, no
test-only crate, no transport/publisher/runtime policy, no generic store-to-pack
dependency, and no default `Box`/`Vec`/`Arc`/`dyn` or cache escape hatch.
