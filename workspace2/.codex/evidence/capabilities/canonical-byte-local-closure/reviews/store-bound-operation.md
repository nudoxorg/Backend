# Chief store-bound operation review

State: integrated chief correction. This is a later mechanism review over `orchestra-shared`; it
does not relabel the historical source-isolated capability packet or its `evidence-blocked` state.

## Failure reproduced

The earlier `VerifiedGeneration` could be minted by `verify(|_| true)` and retained only copied
generation/dependency facts. The operation then copied those facts again, dropped the witness, and
started from a second request. Consequently neither physical presence, evidence-owner identity, nor
continued residence was represented by the type.

## Design and implementation custody

- The Terra design lane `/root/store_bound_proof_design` returned a no-edit proposal: verify against
  the exact immutable `MemoryStore`, compare the complete descriptor after content lookup, retain
  the store borrow in the witness, and retain that witness through request-free execution.
- The Luna implementation lane `/root/memory_bound_hydration` was explicitly dispatched as
  `gpt-5.6-luna/max`. It did not return a completed checkpoint; it was interrupted after beginning a
  partial edit. Those visible edits were treated as untrusted candidate material, retained where
  correct, and completed/restructured by the chief.
- The chief separated the verified binding error from the legacy request error because descriptor
  mismatch is impossible after verified binding. No worker gate or approval is claimed.

## Shipping mechanism

`StagedGeneration::verify_store` and `BorrowedStagedGeneration::verify_store` reject partial
projections before lookup, query every required content identity in the exact store, and compare the
retained length, schema, and kind with the required descriptor. `VerifiedGeneration` immutably
borrows that same store and exposes only immutable facts through `Deref` plus the exact owner through
`AsRef`.

`LocalObjectProvider::bind_verified` checks generation equality before row lookup and returns a
`BoundLocalObjectProvider` that retains the witness. Its inherent `start()` takes no request, performs
no second lookup, emits one batch and one complete terminal, then remains fused. Promised locality is
reported as `HydratedPromise`, preserving the historical locality fact while stating why bytes are
now locally usable.

Only failed verification allocates: complete missing/conflicting descriptor reports are boxed once
so all operands survive without making every successful `Result` carry a 96+ byte error layout. The
witness, store, provider, and success path remain borrowed and allocation-free after fixture/setup.

## Falsifiers and gates

- partial projection is rejected before store lookup;
- an empty/incomplete exact store cannot mint a witness;
- equal content under a different schema cannot mint a witness and reports both full descriptors;
- the witness points at the exact checked store;
- compile-fail tests reject forged/relabelled witnesses and dropping the evidence store while the
  witness remains live;
- the integration journey proves resident and promised rows bind from one witness, retains borrowed
  body pointer identity, orders stale-generation before missing-key, and fuses after one terminal;
- `cargo test -p nudox-hydration -p nudox-operation --locked --offline` passed: 15 hydration unit,
  four hydration UI compile-fail, two operation unit, and one integration test;
- strict all-target Clippy passed for both crates with `-D warnings`; no lint allowance was added;
- the updated `build-object-hydration` skill passed the repository skill validator under Nix.

## Remaining edge

The legacy `LocalObjectProvider::start(PinnedObjectRequest)` still exists as a compatibility route and
rechecks copied generation/descriptor facts. It must not be described as store-bound. Durable
publication remains the next authority transition: it must consume the retained verified witness
together with a real stable receipt before issuing published authority.
