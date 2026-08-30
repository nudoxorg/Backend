# Source-isolated Terra review custody

This artifact supersedes the earlier direct-outer-review custody interpretation. It binds the controller correction at skill commit `b3473581` without merging that shared skill commit into this capability branch.

## Required future dispatch

Every future material-candidate or closure review is launched by a **dispatch-only** Codex CLI parent in a fresh disposable build root. Its runtime is `gpt-5.6-sol` at `low`; it does no source review, test interpretation, or implementation. It must use the registered `nudox_terra_reviewer` configuration `.codex/agents/nudox-terra-reviewer.toml`, require `gpt-5.6-terra` at `xhigh`, and spawn the reviewer with `fork_turns="none"`. A full-history fork is invalid because it inherits the parent role.

The reviewed source is a separately exported, hash-checked, read-only `git archive` snapshot without `.git` history or manager journal. The snapshot is supplied to the reviewer only as a readable absolute path. It is never the CLI `-C` target, an `--add-dir` path, a writable root, or a compiler-output directory. The separately hashed rationale-free packet is read-only too.

The CLI parent is rooted only at `<build-root>` and is configured with:

```text
TMPDIR=<build-root>/tmp
CARGO_TARGET_DIR=<build-root>/target
model=gpt-5.6-sol
model_reasoning_effort=low
sandbox=workspace-write
sandbox_workspace_write.writable_roots=[]
sandbox_workspace_write.exclude_tmpdir_env_var=true
sandbox_workspace_write.exclude_slash_tmp=true
```

The parent/reviewer may write compiler outputs and temporary experiment output only below `<build-root>`. `$TMPDIR` and `/tmp` must not remain implicit writable roots. The effective runtime receipt must show the reviewer sandbox is `workspace-write` restricted to that sole disposable root; an asserted configuration is not sufficient.

Before any review result is narrated, the raw sidecar return must contain a nonempty runtime reviewer task/receiver ID. A task label, a parent claim that it spawned a reviewer, an empty receiver list, or a wait receipt with no child identity is a custody failure. The closure/index receipt must record: parent sidecar task, reviewer runtime task, registered role/config and config hash, actual model/effort, effective sandbox and root, excluded implicit temp roots, snapshot commit/tree/path, packet path/digest, and identically computed pre/post aggregate source hashes. Source-hash divergence, a writable snapshot, an unverified child role, or a direct child of this writable manager rejects the review.

## Historical interpretation

The two Luna-parent sidecars remain custody failures: attempt 1 failed before child creation because a full-history role override was rejected; attempt 2 failed because its Luna parent exposed no Luna child model. Their raw receipts remain in `closure.md` and are never reclassified as reviews.

`reviews/pre-edit-direct-1.md` and `reviews/pre-edit-direct-2.md` are retained as **attack evidence only**. Direct outer collaboration dispatch did not provide the revised sidecar parent/effective-sandbox custody, so neither review may transition a proof-matrix row, authorize worker dispatch, or serve as independent approval. Their findings remain useful falsifiers already resolved or carried into the frozen Luna card.

## Next admissible review boundary

After the current Luna worker returns a coherent material candidate and its exact source commit is reproduced, export that commit into a fresh archive and create a packet that omits builder rationale. Then run the above sidecar protocol. If the runtime fails, preserve its raw stream and make exactly one bounded alternate attempt that changes one operational variable only; do not weaken model, snapshot immutability, or source-isolation rules.

## Material-candidate custody result

The corrected protocol was exercised against clean candidate `acee01eabae2385337e777d1d2ab6faaec5fdbe0`, tree `49955140ae163b9b4e3526da1945c8a0460dcf73`. The primary Sol/low sidecar failed authentication before spawn. The one-variable alternate emitted only a task label; its wait receipts contained `receiver_thread_ids=[]` and the runtime repeatedly panicked on an invalid environment value. The identically computed read-only source hash remained `86ab1b428d0fe17487a095712db434be6329bb2789f54926332cc297f5f35d75`, with zero writable source files or directories.

The full receipt is `reviews/candidate-custody-blocked.md`. Its state is `EVIDENCE_BLOCKED`: there is no runtime reviewer identity, verified Terra/xhigh custody, finding, or approval. No further or weaker alternate was attempted.
