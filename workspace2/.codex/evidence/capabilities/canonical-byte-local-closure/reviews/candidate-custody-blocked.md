# Candidate review custody: blocked

State: `EVIDENCE_BLOCKED`. This is a custody receipt and attack artifact, not a review, finding set, or approval.

## Candidate and immutable source

- Candidate commit: `acee01eabae2385337e777d1d2ab6faaec5fdbe0`.
- Candidate tree: `49955140ae163b9b4e3526da1945c8a0460dcf73`.
- Candidate branch: `codex/performance-data-structure-closure`.
- Checkout porcelain was clean before and after both attempts.
- Read-only archive: `/Users/mileswirht/.config/codex/visualizations/2026/08/30/01a05132-8a81-71b1-9463-8423e36bc349/custody-kIhzuU/source-acee01e`.
- Aggregate source SHA-256 before and after: `86ab1b428d0fe17487a095712db434be6329bb2789f54926332cc297f5f35d75`.
- Writable files/directories below the source archive: `0 / 0`.

The dispatch-only build root was `/Users/mileswirht/.config/codex/visualizations/2026/08/30/01a05132-8a81-71b1-9463-8423e36bc349/custody-kIhzuU/build`. `TMPDIR` was its `tmp` child and `CARGO_TARGET_DIR` its `target` child. The source was supplied only by readable absolute path, never as the CLI `-C` target, an `--add-dir` path, or a writable root.

Both launches requested `gpt-5.6-sol` at `low`, `workspace-write`, `sandbox_workspace_write.writable_roots=[]`, `sandbox_workspace_write.exclude_tmpdir_env_var=true`, and `sandbox_workspace_write.exclude_slash_tmp=true`. The parent's sole role was to spawn the registered `nudox_terra_reviewer` with `fork_turns="none"`, wait for that child, and return its raw receipt.

## Bounded attempts

1. Sidecar task `01a051d5-08a3-7403-a66a-3c5c8e4891c6` set `CODEX_HOME` to the disposable configuration. WebSocket and HTTPS dispatch repeatedly returned HTTP 401 (`Missing bearer/basic authentication`) before a reviewer spawn. The parent turn failed. There is no reviewer task ID.
2. Sidecar task `01a051d5-9cf1-7d42-961c-7aae624d4c55` changed exactly one operational variable: it omitted the isolated `CODEX_HOME` while retaining the model, effort, sandbox, source immutability, and temp-root constraints. It emitted the task label `/root/nudox_terra_reviewer`, but every subsequent wait receipt returned `receiver_thread_ids=[]`. The runtime also repeatedly panicked at `std::env.rs:162:83` on invalid environment value `â\x88\x99`. The process was interrupted after repeated empty waits.

The task label is not a runtime child identity. There is no nonempty reviewer task/receiver ID, no verified `gpt-5.6-terra`/`xhigh` child, no effective reviewer sandbox receipt, no reviewer-run gate, and no finding or approval. The two streams are retained only as attack evidence. Per the corrected `b3473581` procedure, no third route or weakened-custody substitute is attempted.

## Consequence

The controller may report its own implementation, tests, resource evidence, and manager reproduction, but it must not transition any proof row on the basis of independent review. Formatting commit `622cdbe5` was created after this blocked attempt, so even a hypothetical result against `acee01ea` would not cover the terminal source identity.
