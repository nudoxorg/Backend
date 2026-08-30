# Review packet: durable-publication-wave-a2 / pre-edit-1

Review the exported source snapshot and frozen facts below. Do not edit. Do not infer a preferred
implementation from the builder; report only concrete contract gaps and counterexamples.

## Scope

Allowed future implementation paths: `adapters/durable-journal/**`, excluding
`tests/wave_a2_red.rs`. Allowed evidence path:
`.codex/evidence/capabilities/durable-publication-wave-a2/**`. Hydration shared paths are read-only.

## Required public outcome

Starting with a real `VerifiedGeneration`, a bounded multi-producer service must preserve the
accepted single file owner, obtain exact stable per-command receipts from a reusable bounded group
commit, append an immutable publication fact, and make a compact validated head visible only after
the named fact is stable. Reopen independently validates journal, publication facts, and head.

## Contract rows

| ID | Required falsifier |
| --- | --- |
| DP-01 | Full item/byte/receipt capacity returns original input and leaves accounting unchanged. |
| DP-02 | Concurrent producer/owner test proves one file owner and bounded item/byte/waiter/receipt totals. |
| DP-03 | Exact duplicate is idempotent; changing one fact rejects with source-bearing conflict/no bytes. |
| DP-04 | Cancellation at admitted/queued/grouped/synced/pre-head/post-head returns credits and releases no premature authority. |
| DP-05 | Receiver loss and group write/sync poison fan out exact source-bearing failure and return credits. |
| DP-06 | Explicit shutdown/join composes simultaneous primary and cleanup failures without source loss. |
| DP-07 | Reused bounded group buffer writes ordered exact frames and maps one sync result to individual receipts. |
| DP-08 | Every group write length/error and sync failure yields no failed receipt/effect; independent reopen derives the valid prefix. |
| DP-09 | An immutable publication fact cannot precede its named stable receipt and rejects stale/forged input. |
| DP-10 | Every temp/head write/sync/CAS-or-rename/directory-sync and crash prefix leaves any visible head validated and backed by stable bytes. |
| DP-11 | Coherent duplicate retains the head byte-identically; conflict leaves the old head byte-identically. |
| DP-12 | Independent journal/fact/head reopen validates or returns an exact cause; it has no shadow recovery state. |
| DP-13 | If a public pending API exists, registration precedes `Pending` and immediately rechecks; loss/cancel wakes. |
| DP-14 | A published authority is non-forgeable, consumes an actual verified fact and stable receipt, and is reconstructed after reopen. |
| DP-15 | Isolated measurement accounts for allocations, retained bytes/copies, group reuse, and dependencies. |

## Existing owners and exclusions

`FileJournal` owns journal frame bytes, append sync, poison, and replay. The chief authorizes one
existing workspace normal path dependency on `nudox-hydration`: durable-journal owns concrete
`PublishedGeneration`, which privately consumes `VerifiedGeneration` and exposes only the verified
plus immutable-publication/head facts used by the current journey. No hydration edit, receipt trait,
shared typestate, future backend abstraction, unsafe, SIMD, async facade, HTTP/transport/object store,
serde, dynamic dispatch, `Box`, `Arc`, workflow or roadmap edit is allowed. The Sol public red journey
must remain unchanged.

## Required reviewer output

Return ranked findings with exact locations and falsifiers, a complete tripwire table, strongest
counterexample, simplest standard-library control considered, and approval status. State a blocker or
major when a law cannot be implemented/tested within this scope.
