# Adaptive local-first C.6 brief

## Terminal

`nudox-local-first` must turn one immutable snapshot of typed local facts, pinned remote facts,
demand, health/consistency, latency, battery/memory/storage/CPU credits, and signed-bundle facts
into a replayable, bounded stream of exact placement effects.  The chief-owned terminal is
`crates/nudox-local-first/tests/adaptive_journey.rs::outage_expands_proven_local_capability_and_recovery_contracts_without_identity_drift`.

The public trace requires the following order during an outage: move an eligible cold RAM copy to
NVMe, fetch a demanded pinned remote fact into RAM using the capacity released by that move, then
activate a verified analyzer bundle.  On healthy, generation-consistent recovery it must evict the
eligible cold NVMe copy and release the optional bundle, while retaining the pinned and in-use
facts.  `FactKey` is the only identity used in every action; physical tier never changes it.

## Product laws

1. The policy is pure, total, no-std, deterministic for logically equal snapshots, and emits one
   action per cursor step without allocation or retained mutable truth. Inputs are capped at 64
   local, 64 remote, 64 demand, and 32 bundle facts; the maximum stream is 224 actions.
2. A remote outage or inconsistent generation expands only demand-proven local capability.  A
   healthy response can contract only a fact that has a matching pinned remote fact and is neither
   `Pinned` nor `InUse` locally.
3. Every action carries the unchanged `FactKey` and exact bytes; RAM, NVMe, and object storage are
   location facts, never alternate identity authorities.
4. Every resource class participates: RAM/storage decide residence actions, CPU and battery gate
   optional expansion, pressure selects contraction, and latency/demand select locality.
5. Only `BundleAvailability::Verified` can lead to a bundle action: `Available` may acquire and
   `Active` may release. Missing, signature-rejected, and identity-mismatched facts have no effect.
6. Actions are bounded and restartable. Replaying from `ActionCursor::START` produces the same
   actions; resuming from a returned cursor neither duplicates nor loses an action; discarding a
   cursor is cancellation before an effect boundary.
7. The measured release base graph contains and genuinely exercises current typed owners for roots,
   descriptors, schemas, operations, a minimal local store, the bounded runtime, and a concrete
   runtime-independent transport seam. No server SDK, exporter, database, codec, compiler, model,
   or caching dependency enters its package graph.
8. Validation is permutation-invariant and precedes planning. Duplicate local `(FactKey, tier)`,
   remote `FactKey`, demand `FactKey`, and bundle `(kind, identity)` authorities are exact typed
   errors; the least canonical duplicate wins. Distinct local tiers for one `FactKey` are allowed.
9. Action priority is move, fetch, acquire, evict, release, with `FactKey` tie-breaking and bundle
   `(kind, identity)` tie-breaking. Each cursor call recomputes checked reservations for its prefix
   from the unchanged snapshot; moves charge storage before releasing RAM and fetches charge their
   destination. A changed snapshot restarts at `ActionCursor::START`.

## Explicit negative space

- No cache, asynchronous runtime, transport client, filesystem adapter, serializer, `dyn`,
  `Arc`, `Box`, `Vec`, unsafe code, macro, SIMD path, or new dependency is authorized.
- This slice does not define remote fetch execution, bundle signature verification, durable
  application of an effect, or a second state store. Their adapters provide the typed input facts;
  the policy neither reconstructs those facts nor owns shadow state.
- The existing object/root/schema/operation/store/runtime crates remain their own invariant owners.
  The release base consumer must call their existing typed surfaces, not add unused manifest edges
  or recreate their state. There is no accepted transport crate yet: C.6 owns the smallest concrete
  runtime-independent seam that turns a `PlacementAction::Fetch` into an exact typed range request
  without an SDK, executor, cache, or second decoding path.
- Bundle residence is an externally owned typed fact with `Available` and `Active` states. It is not
  a bundle-use lease and does not authorize releasing a bundle protected by a future use lease.
- `RemoteFact::pinned: bool` is not an acceptable final representation. A remote fact in this policy
  is already a pinned remote proof, so the boolean admits an unpinned state that has no contract.
  The slice must remove it and let `RemoteFact` itself carry that authority.

## Baseline and ownership

- Baseline commit/tree: `f26fec5388064e85239c3983ba4d900d5146929f` /
  `51b94c9b39e4a4d635e17525fe546f1abdedc99a`.
- Manager checkout: `/Users/mileswirht/.config/codex/worktrees/d865/c6-terra` on
  `codex/terra-c6-adaptive-local-first`.
- Production ownership: `crates/nudox-local-first/src/lib.rs` and its package manifest.
- Focused policy/property/fault ownership: `crates/nudox-local-first/tests/`.
- Chief-owned public boundary: `crates/nudox-local-first/tests/adaptive_journey.rs`; it may be
  strengthened but never weakened.
- Reserved chief path: `tools/check-local-first-base-budget.sh` is not edited here. This slice will
  provide a concrete `nudox-local-first` release binary for that script to measure.

## Consumers and dependency direction

The only baseline consumer is the chief journey. `FactKey` imports the closed identity types from
`nudox-id`; no policy type is imported by foundational crates. The shipping
`local-first-base-client` must construct one `ObjectRef<ObjectDomain>` with `SchemaId::Object`, bind
it into a one-entry `GenerationRoot`, derive a `LocalObjectProvider` and run the pinned-object
operation, admit the same bounded work through an `InlineRuntime`, and insert/borrow its canonical
bytes through `InlineMemoryStore`. A `Fetch` action must translate to a crate-owned `RangeRequest`
containing the identical `FactKey`, offset zero, and the action's exact checked byte length. The
binary returns a typed error if any identity, terminal, store, runtime, or range observation differs.

Its only authorized direct normal package edges are `nudox-id`, `nudox-object`, `nudox-root`,
`nudox-schema`, `nudox-operation`, `nudox-store-memory`, and `nudox-runtime`. Each edge must appear in
the executed route above. `tools/check-local-first-base-budget.sh` builds twice in isolated target
directories, compares hashes, requires a successful self-check, enforces the exact direct-edge set,
rejects optional/server packages, and caps the stripped executable below 50 MiB with separate 8 MiB
text and 8 MiB data ceilings measured by the Nix-pinned LLVM tools.

## TESTING.md digest and mapping

`TESTING.md` SHA-256: `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b`.

| TESTING.md clause | Matrix row or evidenced exclusion |
| --- | --- |
| allocation/layout: exact representation and representative allocation path | C6-08, C6-12 |
| universal negative, conservation, exact result | C6-01 through C6-07 |
| identity checked conversion and authority cells | exclusion: `nudox-id` owns conversion; C6-03 proves policy preserves its typed identity |
| foundation frame mutation and borrowed pointers | exclusion: no frame/view code is changed |
| object/root/store/hydration locality identity and first-write rules | C6-03, C6-06; remaining storage admission is owned by those crates |
| operation/runtime phase, cancellation, bounded streams | C6-07, C6-10; no runtime or atomic transition is introduced |
| end-to-end actual typed components and fault boundaries | C6-09 plus chief journey C6-11 |
| hot demand, long outage, inconsistent generation | C6-02, C6-04, C6-05 |
| battery/memory/disk/CPU pressure | C6-05, C6-06 |
| corrupt/mismatched bundles | C6-06 |
| restart, replay, cancellation at every effect boundary | C6-07 |
| release/dependency/text budget | C6-12 |
| base graph and concrete transport seam | C6-13 |
| remote-pin invalid state | C6-14 |

No applicable clause is satisfied by a success-only assertion. Each matrix row names an exact action
trace or accounting observation and the neighboring prohibited action.
