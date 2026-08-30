# Wave C.7 unified application closure receipt

Verdict: `EVIDENCE_BLOCKED`

The public terminal, accepted input vocabulary, negative space, adapter boundary, chief proof matrix,
and bounded capability split were frozen. No production implementation, manifest, lockfile, test,
CLI, MCP adapter, GPUI package, dependency edge, or generated artifact was created. No Luna worker
received production-write authority.

## Blocking capability

The first required managed slice, `wave-c7-application-service`, is canonically recorded as
`evidence-blocked` in
`.codex/evidence/capabilities/wave-c7-application-service/index.toml`. Its contract was frozen at
commit `91c938d4a6afdc5b8dd65e0415766c72725cf9c4`; the blocked receipt was committed at
`c3833ca38f8be482f95866ddcb760fa4724fc2d6` and corrected to the raw runtime custody facts at
`6dca42226e138ddd41d2015067282978fdc320a1`.

Two materially different source-isolated dispatch attempts failed:

1. `01a0513e-951d-7721-83b8-31f5f43dd0bb` used a full-history fork with a reviewer model override.
   The router rejected it and its collaboration receipt has empty `receiver_thread_ids`; no
   registered reviewer existed.
2. `01a05140-753b-7003-b7c9-16616993c57b` used a context-free registered-role spawn from a minimal
   ASCII environment. Its source-free shell control passed, then routing failed with
   `Unknown model gpt-5.6-luna; available gpt-5.6-sol,gpt-5.6-terra`.

The Git-history-free snapshot was
`/private/tmp/c7-preedit-source-v2.JTwgEh`. Its deterministic pre/post digest was identical:
`1199b20def1ff63c010cdd5c12f485e5c62b03fe7a4d7c25d8f1aa311980f878`. The rationale-free packet
digest was `dbf8df6bd4a0255a672fdde18328abe92309c01f188fe76d3373e579af4e47b1`.

Raw receipts remain under `/private/tmp/c7-preedit-review-v2.KhEdon` and
`/private/tmp/c7-preedit-review-v3.dMOiEx`. External resolution owner: sidecar role-routing
configuration must make the registered `gpt-5.6-luna` parent available so it can launch the
registered `gpt-5.6-terra`/`xhigh` reviewer under the required source isolation.

## Evidence disposition

- Public terminal: frozen, not implemented.
- Strongest counterexample: an input-ignoring dispatcher returning one successful empty result;
  frozen, not executed.
- Raw Rust/Nix gates: not run because no Rust source, manifest, lockfile, or dependency changed.
- Resource/binary evidence: unavailable because no candidate exists.
- Reviewer findings: none; neither attempt produced a reviewer.
- Retained mechanisms: accepted compiler/index/operation/observe vocabulary and the public boundary
  only; no code mechanism was promoted.
- Rejected mechanisms: no production candidate existed. A self-review or direct writable child was
  rejected as inadmissible custody, not as product evidence.
- Uncertainty: every implementation row in the chief proof matrix remains `RED`/unverified.

Stopping here is required by the capability-cycle contract. It proves no Wave C.7 product behavior.
