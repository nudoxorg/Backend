# Review-dispatch custody correction

This evidence-only correction binds the reviewer procedure to the authoritative skill source
`b34735817b7313dcb1e3224ea89d39a8246434ca`, read in full before this record. Its
`manage-rust-swarm/SKILL.md` SHA-256 is
`3d5155742fd2eba0fcc2184b2512dc1aa4ccb17c287baf58ee7c4b58a6eafa40`; its
`review-rust-gem/SKILL.md` SHA-256 is
`6caaa91f552ea648049bb0a049f6a771b6103c0be88e183112e4c74930044454`.

It does not change the bound source snapshot (`4c60694d`), the frozen packet digest
(`d48564edfbcb3379c53e13f6108d2b91a4b007580f0bed966379db41fcb124da`), the Sol terminal, or any
production path. It replaces the former dispatch interpretation only.

## Mandatory future procedure

The writable Terra manager never parents the reviewer. A fresh disposable sidecar build root receives
only the role-discovery configuration/skills; it reads the separate source export by absolute path and
does not receive that export through `-C` or any writable-root grant. Its only job is to spawn the
registered `nudox_terra_reviewer` with `fork_turns = "none"`, wait for that exact child, and return the
raw receipt. The sidecar is `gpt-5.6-sol` at `low`, invoked as follows, with placeholder paths replaced
only by the fresh build root and read-only export:

```text
TMPDIR=<build>/tmp CARGO_TARGET_DIR=<build>/target \
codex --ask-for-approval never exec --model gpt-5.6-sol \
  -c model_reasoning_effort=low \
  -c 'sandbox_workspace_write.writable_roots=[]' \
  -c sandbox_workspace_write.exclude_tmpdir_env_var=true \
  -c sandbox_workspace_write.exclude_slash_tmp=true \
  --sandbox workspace-write --json --skip-git-repo-check -C <build> <dispatch-only prompt>
```

The raw runtime result must contain a nonempty reviewer task ID and resolve the registered reviewer as
`gpt-5.6-terra`/`xhigh`. A zero-receiver `wait`, narration without that ID, full-history fork,
unrestricted sandbox, writable snapshot, direct-manager child, or changed source digest is a rejected
operational attempt, never a partial review. Compiler output and temporary writes may occur only under
the build root; implicit `$TMPDIR` and `/tmp` roots are excluded. A valid receipt records both task IDs,
role/config/model/effort, effective sandbox and sole writable root, excluded roots, snapshot/packet
digests, and source before/after aggregate digests.

## Superseded non-review attempts

| Attempt | Retained raw material | Failure classification | Why it cannot count as a review |
| --- | --- | --- | --- |
| Luna-parent dispatch 1 | `/tmp/nudox-a2-preedit-build.pWG259/sidecar.jsonl` | rejected operational attempt | Runtime `wait` records `receiver_thread_ids: []`; no child ID exists. |
| Luna-parent dispatch 2 | `/tmp/nudox-a2-preedit2-build.VAwl1x/sidecar.jsonl` | rejected operational attempt | Runtime `wait` records `receiver_thread_ids: []`; no child ID exists. |
| Luna-parent dispatch 3 | `/tmp/nudox-a2-preedit3-build.YaQMRa/sidecar.jsonl` | rejected operational attempt | Parent/model routing did not establish the required dispatch-only Sol parent; no qualifying child receipt exists. |
| Direct Terra pre-edit | `/tmp/nudox-a2-direct-review-build.d6Sh63/reviewer-final.md`; preserved narrative and tripwire table at `reviews/pre-edit-1.md` | superseded non-review | It was a direct reviewer process, not a Sol/low dispatch-only parent spawning a nonempty `fork_turns = "none"` child. Its factual findings remain raw context only. |
| Direct/stale follow-ups 4–8 | `/tmp/nudox-a2-preedit4-build.RPM1Pv/reviewer-final.md`, `/tmp/nudox-a2-preedit5-build.7BgHZa/reviewer-final.md`, `/tmp/nudox-a2-preedit6-build.PB4AG4/reviewer-final.md`, `/tmp/nudox-a2-preedit8-build.NM7JZo/pre-edit-1.md` | superseded or rejected operational attempts | They used the superseded topology or yielded no valid nonempty registered reviewer task receipt. |
| Stale corrected-snapshot reader | terminated task prefix `01a0516b` | rejected operational attempt | It had no valid child task ID and was terminated before verdict. |

The fixed cause exposed by these bounded attempts is the prior sidecar topology/child-router mismatch,
not a source or production defect. They therefore do not meet either reviewer boundary and do not
authorize Luna or production edits. The next review starts fresh under the procedure above.

## Exhausted corrected sidecar alternate

The mandatory Sol/low topology was then tried twice against the unchanged read-only source export and
packet. Neither attempt created a reviewer child, so neither has reviewer source digests, an effective
reviewer sandbox, reviewer findings, or an approval status to record.

| Attempt | Parent identity and sole build root | Raw receipt and SHA-256 | Single operational variable | Result |
| --- | --- | --- | --- | --- |
| Required procedure | Sol/low parent `01a0516f-a8ce-7d92-b746-4da2398880c6`; `/tmp/nudox-a2-sol-dispatch.DTd9WW` | `sidecar.jsonl` `a5cef1a89afd31f3c557be76b16f6f1f5e5a12cda4f4bad2de64e9ad99093c61`; `parent-final.md` `e130ff7904dc58db950e15b82f63a86783a487b56322e20d67246c9e27a7d096` | none | Spawn rejected before a child ID: `Unknown model gpt-5.6-luna`; runtime listed only `gpt-5.6-sol, gpt-5.6-terra`. |
| Bounded alternate | Sol/low parent `01a05171-18a5-7ff0-b29d-da351cefc866`; `/tmp/nudox-a2-sol-dispatch-alt.nFgCRj` | `sidecar.jsonl` `67f0cd498c0f0fd90821e1d13ba50837f0209d55856c82f1fe42bb32ac6043b0` | `agents.default_subagent_model=gpt-5.6-terra` only | Runtime emitted `wait` with `receiver_thread_ids: []`; it supplied no spawn result or child task ID and was interrupted. |

This is the same unavailable child-dispatch capability through two materially different, bounded
configurations. The external owner is the Codex CLI/collaboration-router runtime: it must resolve the
registered `nudox_terra_reviewer` to `gpt-5.6-terra`/`xhigh` and return its nonempty task ID from the
specified Sol/low parent. On that repair, restart a fresh source export and review; do not reuse either
failed attempt or its build root.
