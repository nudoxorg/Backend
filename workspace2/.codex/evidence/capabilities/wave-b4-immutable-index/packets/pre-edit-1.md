# Wave B4 immutable-index rationale-free pre-edit packet

## Identity and source scope

Capability: `wave-b4-immutable-index`.

Frozen manager card: `.codex/evidence/capabilities/wave-b4-immutable-index/packets/phase0-card.md`;
SHA-256 `cc55f7b9150dc4f90d65f422c7bc977a4482a81fbe28dc93723cc12e563dee00`.

Baseline commit/tree: `f2565a9fb33af06053bd19721d4dc2753ec09ed5` /
`8477cb2ab93763c468d5431740cfb1d5e4c4cf82`.

Candidate evidence commit/tree: `7330ff7f31b8253bf48e1c2a24d054539287948b` /
`f3018ac0a581683f67eb695f68f104eeccd1397d`.

Read-only source snapshot contains only current index vocabulary, `nudox-id`, their nested/root
manifests and locks, the frozen governing plan/testing documents, and no Git history or manager
evidence directory. This packet is copied separately into the sidecar build root.

## Frozen contract

One published immutable snapshot exposes separate typed exact lookup, prefix scan, and lexical search
against an explicitly named snapshot and declared segment IDs. D1/D2 prove insert/update/delete/
tombstone behavior. S1 remains pinned after S2/S3. Deterministic lexical rows retain typed score,
tie order, and segment provenance. Missing selected E2 is exact partial data, not zero hits. A stale
rendezvous route may retry another worker; route, node, cache, and tier do not alter semantics or IDs.
Independent compaction verifies S3 equivalent to S2 before atomic replacement. Sealed deltas are
inputs, not compiler work.

No universal request/document DTO, dynamic query, boxed stream, serde, backend posting-loop branch,
consensus truth, default cache, Qdrant, Trustfall, unsafe, SIMD, compiler invocation, or production
write belongs to this phase. Tantivy is permitted later only as a bounded nested lexical differential
adapter; it is never logical schema/truth/identity/terminal authority. Deterministic in-memory owners
may establish semantic parity only; durable file/NVMe/object-store evidence remains unproved until its
own adapter card.

## Required reviewer output

Return ranked findings first using `BLOCKER`, `MAJOR`, `MINOR`, or `QUESTION`, with exact source or
packet location, evidence, violated law, consequence, smallest boundary correction, and falsifier.
Then include the literal tripwire table required by `review-rust-gem`, including zero-hit rows and
exact scanned locations. State the strongest attempted counterexample, most likely hidden allocation/
branch/lifetime, one cleared suspicion, whether the simplest standard-library control was considered,
confidence, unverified platform/tooling gaps, and approval status. Make no edits and do not score.

## Review evidence available

The proof matrix has B4-01 through B4-13 all red. Current consumers are only
`planes/index/crates/nudox-index-vocab`, its vocabulary test, `crates/nudox-id` marker exports, and
the nested index manifest. The only production source candidate is the existing three ID aliases;
there is no candidate manifest/segment/query/publication/adapter implementation to approve.

Existing focused gate commands are the five exact Nix commands in `closure.md`; their result is
evidence only for current I0 vocabulary and not for B4 behavior.
