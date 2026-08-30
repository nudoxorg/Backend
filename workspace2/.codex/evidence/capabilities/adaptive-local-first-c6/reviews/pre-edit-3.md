# Pre-edit hostile review 3

Reviewer task: `/root/c6_review_dispatch/nudox_terra_reviewer`  
Dispatch parent: `/root/c6_review_dispatch`  
Requested and reported reviewer: `gpt-5.6-terra` / `xhigh`  
Runtime child/receiver ID: `/root/c6_review_dispatch/nudox_terra_reviewer`  
Snapshot: `/private/tmp/nudox-c6-preedit-source-bundled.MHCNPD/workspace2`  
Source aggregate before/after SHA-256:
`7cd4f6c3cc80b357bb21d8fea5f355ccdcb6ac65c27a31e343d2f9739d17a0dd`  
Verdict: **REJECT / EVIDENCE_BLOCKED before production edits**

The collaboration router returned a nonempty child task and receiver ID. The reviewer reported its
model/effort and an unchanged source digest, but no separate effective sandbox receipt was exposed;
that custody caveat is retained and must not be rewritten as final approval.

## Findings

1. The chief journey required recovery eviction of `cold` without a matching `RemoteFact(cold)`,
   directly contradicting the frozen recovery law. The strongest counterexample is a correct C6-04
   implementation: it must emit no eviction for the snapshot that the journey required to evict.
2. Determinism and boundedness lacked a canonical comparator, duplicate rule, fixed cardinalities,
   action priority, prefix reservation semantics, scan bound, and a maximum cursor/action count.
3. `BundleAvailability::Verified` did not distinguish an available inactive bundle from an active
   resident bundle, so fresh recovery and recovery after acquisition were observationally identical
   while the policy was forbidden to retain shadow truth.
4. C6-13 had no concrete base binary/range-request seam/dependency-edge route, and the claimed chief
   budget script did not exist in the reviewed snapshot. Nominal unused imports remained a live
   mutant.
5. Removing `RemoteFact::pinned` needed compile-fail evidence; a runtime test cannot prove the
   former `pinned: false` state is unrepresentable.

## Tripwires

| Tripwire | Review result |
| --- | --- |
| panic / unwrap / expect / unreachable | none |
| source-dropping `map_err` | none |
| lossy conversion / raw bypass | `RemoteFact::pinned: bool`; scalar `From` conversions are lossless |
| checked/saturating arithmetic | no policy arithmetic yet; cursor/cardinality proof absent |
| `dyn` / `Box` / `Vec` / `Arc` / `Rc` | none |
| public tuple fields | none; private scalar-newtype fields only |
| unit/stateless structs | none |
| public local traits/delegation | standard `Deref` implementations only |
| one-letter generic parameters | none |
| numeric semantic values | cursor start sentinel and journey fixtures only |
| test-only `Option`, discarded result, success-only assertion | none; chief journey exact but ignored |
| unsafe/SIMD/allocator/dependency additions | none; only `nudox-id` normal dependency |
| public item without current consumer/falsifier | vocabulary had only the ignored journey; no shipping consumer |

Retain: typed `FactKey` in every action; caller-borrowed slices; one-action cursor/`PlanStep`; no-std,
unsafe-forbidden safe control; closed bundle verification outcomes.

Reject: boolean remote pinning; the inert body as a candidate; input-order action selection;
policy-only or nominal release binary; SDK/executor/cache/verifier additions; the invalidated packet.

Targeted formatting and Clippy passed. Normal tests were green only because the chief journey was
ignored; explicitly including it failed at ordinal zero against the inert control. No raw Nix,
Dylint, release-budget, or candidate approval was claimed by this review.
