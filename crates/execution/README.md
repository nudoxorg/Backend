# backend-execution

This crate owns bounded execution control around admitted, typed version
identities. `VersionedWorkIdentity<R>` binds independently schema-marked
recipe, input root, read manifest, workspace authority, and output-equivalence
versions. `WorkKey` is derived as a `backend-version::ObjectVersion` with the
relation schema included in its domain. `ResultReceipt<R>` is constructed only
from a matching identity, leased fence, and an `OutputAdmission` minted after
canonical-byte/schema/coverage and engine validation; untrusted worker claims
pass through `UntrustedResultReceipt` and `ResultReceipt::admit_wire` before
acceptance. Transport adapters bound canonical output bytes before creating
the untrusted claim.

Placement is admitted through opaque `LocalCapability`, `RemoteCapability`,
and `CostSnapshot` observations. Each is bound to the exact work key, carries
an observed/expiry window, and is minted only by the corresponding engine or
replication verifier; stale or low-confidence observations conservatively
keep optional work local. A successful authority verifier mints private
`ExecutionAuthorityEvidence` that binds the receipt's authority, fence,
output, and semantic coverage.

`Admission` issues affine reservations across interactive, background,
transfer, and compaction envelopes, including checked multidimensional
`ResourceVector` credits. `WorkInterner` coalesces a bounded number of
followers, retains terminal output until readers release it, and never evicts
live work. `DeltaPlanner` selects no-op, reuse, incremental update, scoped
rebuild, or full rebuild. Placement charges complete critical-path cost,
supports local fallback and pure hedging, and uses `HedgeRace` for first-valid
winner cancellation. Route cancellation tokens can be forwarded to local
processes and remote cancellation protocols. `AttemptManager` enforces one
active writer per key,
monotonic owner epochs and ordinals, opaque fences, expiry/takeover, and one
accepted result. `WaiterTable` bounds cancellable demand, while `Supervisor`
provides lock-free event counters.

Reservations, leases, interner handles, and waiters are affine guards. Dropping
one releases its owned resources or publication demand; terminal APIs consume
the leader guard so callers cannot leave a required release step implicit.

The lower-core seams stay explicit: flow prepared arrangement deltas can be
handed to `DeltaPlanner::plan_flow_output`, store pack sizes can become checked
storage reservations with `AdmissionRequest::for_store_pack`, and a negotiated
replication capability envelope can be reduced to a conservative
`RemoteState`. These adapters carry evidence into execution without making
execution a second owner of flow, store, or transport identities.
