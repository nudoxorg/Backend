# Corrected frozen pre-edit packet: adaptive-local-first-c6

Snapshot: chief correction after the rejected `pre-edit-2.md`; no production policy or shipping
consumer implementation exists. This packet supplies only public contract and falsifiers.

## Corrected contract

- Recovery may evict `cold` only because the chief journey now supplies the matching generation-
  pinned `RemoteFact(cold)`. `RemoteFact` itself is the proof and has no boolean pin field; its
  rustdoc compile-fail specimen must reject the former `pinned: false` construction.
- Caller slices are capped at 64 local, 64 remote, 64 demand, and 32 bundle facts. The plan is
  capped at 224 effects and represented by a `u16` cursor.
- Validation precedes planning. Duplicate local `(FactKey, StorageTier)`, remote `FactKey`, demand
  `FactKey`, and bundle `(CapabilityKind, ContentId)` authority is rejected; if several duplicates
  exist, the least canonical duplicate is reported. The same `FactKey` at different local tiers is
  valid and does not create a second identity.
- Action priority is move, fetch, acquire, evict, release. Fact actions tie-break by `FactKey` and
  bundle actions by `(CapabilityKind, ContentId)`, independent of slice order.
- Every cursor call uses the same immutable snapshot and recomputes checked reservations for all
  prior effects. A move charges storage before releasing RAM; a fetch charges its local target.
  Overflow is an exact typed rejection. A changed snapshot restarts at `ActionCursor::START`.
- A verified bundle carries externally observed `Available` or `Active` residence. Only `Available`
  may acquire and only `Active` may release; the policy retains no bundle shadow state.
- The shipping `local-first-base-client` executes the exact `ObjectRef<ObjectDomain>` /
  `SchemaId::Object` → one-entry `GenerationRoot` → `LocalObjectProvider` pinned operation →
  `InlineRuntime` → `InlineMemoryStore` route. It also translates a real `Fetch` into an offset-zero
  `RangeRequest` with the identical `FactKey` and exact action bytes, returning typed errors on every
  mismatch.
- The only direct normal edges authorized for that package are `nudox-id`, `nudox-object`,
  `nudox-root`, `nudox-schema`, `nudox-operation`, `nudox-store-memory`, and `nudox-runtime`.
  `tools/check-local-first-base-budget.sh` compares two isolated release builds, exact direct edges,
  forbidden graph entries, self-check result, and SHA-256; ceilings are below 50 MiB binary, 8 MiB
  text, and 8 MiB data.

## Retained boundary

The policy remains no-std, safe, caller-borrowed, pure, one-effect-at-a-time, and identity-preserving.
No cache, async/network SDK, executor, verifier, filesystem adapter, serializer, `dyn`, `Arc`, `Box`,
unsafe, SIMD, macro, atomic, generic abstraction, or new production dependency is pre-approved.
The inert `next_action` remains only the red safe control.

## Required attacks

Attack every C6-01 through C6-14 row and the corrected details above: permutation and duplicate
selection, limit and limit-plus-one, arbitrary/out-of-range cursor, checked prefix overcommit,
multi-tier same-key identity, inconsistent generations, missing remote recovery proof, every
retention state, every bundle outcome/residence, long outage, replay/restart/cancellation, nominal
imports, unused direct edges, fake range translation, nondeterministic release output, and binary,
text, data, or dependency budget bypass.

The reviewer must return APPROVE or REJECT with the strongest live counterexample, a complete
tripwire table, retained/rejected mechanisms, the immutable snapshot before/after digest, and its
canonical task/model/effort receipt. Approval authorizes only bounded Luna implementation dispatch;
it does not close any proof row or release gate.
