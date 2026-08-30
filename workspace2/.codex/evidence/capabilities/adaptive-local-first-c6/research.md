# Adaptive local-first C.6 research journal

| Decision / rows | Source or experiment | Mechanism and conflicting trade-off | Decision changed / saturation |
| --- | --- | --- | --- |
| C6-01, C6-07, C6-08 | Baseline `crates/nudox-local-first/src/lib.rs` and the chief journey | The existing public API already has a `Copy` cursor and one-action `PlanStep`; a collected plan would add allocation and duplicate cancellation/replay state. | Retain streamed cursor shape; replace only inert body. Needs a non-empty allocation/work control. |
| C6-03, C6-04 | `nudox-id` consumers and `FactKey` in the baseline source | `FactKey` pairs `GenerationId` and `ContentId<ObjectDomain>` before physical policy. Reconstructing either from bytes would introduce a second authority boundary. | Actions will carry the existing key unchanged; policy compares typed keys only. |
| C6-05, C6-06 | Chief journey’s zero RAM plus first move | Effect order itself can make a later fetch fit after the first move. A one-pass stateless policy must derive prior reservations from the cursor/snapshot rather than mutate a hidden cache. | Candidate must use deterministic ordinal derivation, not mutable planning state. Needs a competing safe-control calculation. |
| C6-09, C6-12 | `BundleAvailability` closed enum and package manifest | Verification is adapter-owned and currently encoded as a closed fact. Adding a verifier, bundle store, or optional dependency would widen the client graph without a consumer. | Treat only `Verified` as eligible; record rejected outcomes in tests, not a new adapter. |
| C6-12, C6-13 | Current `nudox-root`, `nudox-object`, `nudox-schema`, `nudox-operation`, `nudox-store-memory`, and `nudox-runtime` public inventories | The base graph already has typed owners, but the baseline local-first crate links only `nudox-id`; a policy-only executable would falsify the required release graph. | Require a binary that performs a small typed path through each current owner. The absent transport owner must be a narrow range-request translation, not a new client/runtime. |
| C6-14 | Baseline `RemoteFact` source and chief correction | `pinned: bool` permits a remote-fact value that contradicts the policy vocabulary of pinned remote facts. | Make remote proof intrinsic by deleting the boolean; no enum/newtype is needed because every admissible remote fact has the same authority. |

## Research status

The first three rows are local-source evidence that fixes representation constraints. Two independent
post-Phase-0 sources/experiments will be added for the policy scan/credit decision before it is marked
saturated: the worker safe-control/property experiment and a Terra release/dependency measurement.
No dependency, unsafe, SIMD, macro, or generic design is being researched because no current consumer
requires one.
