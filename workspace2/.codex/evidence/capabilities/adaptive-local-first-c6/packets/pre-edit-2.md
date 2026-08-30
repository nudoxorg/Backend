# Frozen pre-edit review packet: adaptive-local-first-c6

Snapshot: Phase-0 revision candidate, no production implementation yet. This packet intentionally
contains no builder rationale or proposed internal algorithm.

## Contract

- The public terminal remains the ordinary crate integration journey named in `brief.md`.
- Typed local facts, pinned remote facts, demand, remote health/consistency, latency, all four
  resource conditions, and signed-bundle outcomes drive one pure, replayable stream of exact actions.
- Outage/inconsistency expands only demand-proven local facts; healthy recovery contracts only
  eligible local copies with matching pinned remote evidence. `FactKey` stays identical across every
  storage tier and action.
- `RemoteFact` is itself pinned proof; an `unpinned` remote fact must be unrepresentable.
- The shipping base client genuinely exercises current root, object-descriptor, schema, operation,
  minimal-store, and bounded-runtime owners. It also owns only a small runtime-independent typed
  fetch-to-range-request seam; it introduces no network SDK, executor, cache, verifier, or second
  decoder.
- Optional compilers, analyzers, codecs, models, exporters, database adapters, and server SDKs are
  only hash-pinned bundle facts and do not enter the release base dependency graph.
- Effects are bounded/streamed and must retain exact replay, prefix cancellation, and fused-terminal
  behavior. No mutable shadow truth is allowed.

## Scope

- Proposed production paths: `crates/nudox-local-first/src/lib.rs`, its manifest, and one shipping
  binary under that package.
- Proposed tests: `crates/nudox-local-first/tests/`; the chief journey remains an ordinary public
  integration test.
- Excluded: `tools/check-local-first-base-budget.sh`, foundational implementation changes,
  filesystem/network/runtime adapters, signature verifier, bundle store, and optional/server graph.

## Required attacks

Attack C6-01 through C6-14, especially a policy-only/release-only binary, unused dependency edges,
an invented SDK transport, the `pinned: false` representation, input-order action drift, stale remote
eviction, byte-credit overcommit, protected local eviction, bad-bundle activation, cursor duplication,
hidden allocation, and an input-ignoring release consumer.

## Required evidence

The final reviewer needs the complete literal tripwire table, raw Nix gates, a genuine release
consumer trace, an allocation/work control for non-empty policy input, and the chief base-budget
result or explicit external-owner receipt. No new generic, macro, unsafe, SIMD, atomics, cache, or
dependency is pre-approved.
