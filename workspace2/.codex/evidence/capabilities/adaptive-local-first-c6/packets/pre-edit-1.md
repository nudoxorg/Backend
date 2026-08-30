# INVALIDATED frozen pre-edit review packet: adaptive-local-first-c6

Snapshot: baseline commit `f26fec5388064e85239c3983ba4d900d5146929f`, tree
`51b94c9b39e4a4d635e17525fe546f1abdedc99a`.

The chief invalidated this packet before any review result because it reduced the required base graph
to policy plus identity and left `RemoteFact::pinned: bool` representable. It is retained solely for
the sidecar-attempt record. Do not use it for approval or implementation.

## Contract

- Public terminal: `adaptive_journey.rs::outage_expands_proven_local_capability_and_recovery_contracts_without_identity_drift`.
- The pure, no-std policy receives typed facts and emits one exact bounded action per cursor step.
- Outage or inconsistent remote generation expands demand-proven local capability. Healthy recovery
  contracts only unprotected facts backed by matching remote facts.
- `FactKey` is identical through every action and physical tier. No local cache, reconstructed ID,
  server SDK, runtime, verifier, or shadow mutable store is allowed.
- CPU, battery, RAM/storage credits, pressure, latency, demand, remote health, and signed bundle
  availability must each have a falsifiable policy role.
- Pinned/in-use local facts and non-verified bundles are never displaced or activated/released.
- Restart/replay/cancellation at action boundaries must preserve exact streamed action sequence.
- The release consumer must be a concrete shipping binary and remain under the chief-owned 50-MB
  gate without optional/server dependency leakage.

## Scope

- Proposed production paths: `crates/nudox-local-first/src/lib.rs`, its manifest, and one release
  binary under the same package.
- Proposed tests: only `crates/nudox-local-first/tests/`; public journey remains ordinary top-level
  crate integration evidence.
- Reserved/excluded: `tools/check-local-first-base-budget.sh`, foundational crate APIs, transport
  implementation, runtime implementation, package verifier, bundle store, filesystem/network code.

## Required attacks

Apply every row C6-01 through C6-12 in `proof-matrix.md`; in particular attack constant bodies,
input-order dependence, stale/inconsistent generation eviction, byte-credit overcommit, protected
fact eviction, bad bundle acceptance, cursor replay/terminal duplication, hidden allocation, and a
release target that does not consume policy input.

## Claimed controls before implementation

The retained safe control is one cursor plus one action and scans caller-borrowed slices. There is no
proposal to use allocation, a generic/macro, unsafe, SIMD, atomic state, cache, or new dependency.
The resource skeleton and TESTING mapping are in `proof-matrix.md` and `brief.md`.
