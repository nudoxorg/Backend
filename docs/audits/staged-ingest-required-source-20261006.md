# Required staged ingest source and remaining gates

Frozen production/test source is `eab055240866e3ea278885aace37afab0464c275` in the private Sol6.1 worktree. Dependency join is Root `749b1c4218` through private merge `e2141340b79e5cf42e9a131a7f69d57fccd11f80`; reviewed fd1/4a370 shape dependencies are cc7afc381/df96d07. Root is sole integrator.

The exact contiguous private history below includes atomic implementation, fixture, dependencies and evidence. Applying only early staged commits is unsafe: later corrections remove rejected global caps, release global GC barriers into exact per-closure leases, include every non-relation control in recovery packs, yield both direct and manifest GC work at existing byte caps, and reject missing members before publish/recovery. Inspect the final actual diff against the dependency base and preserve these gates.

```text
5bcbed3f16186feb9dc0c4300306a88eb31714aa Introduce explicit pinned workspace closure membership and bounded controls
bfc9dbba9f64ca0622db484c07463c70e43e86e1 Admit exhaustive typed anonymous callable compiler failure facts
fcc876ec5c12161461eb3b8fb8e7db0272a6c7f3 Connect large product intent queue and cold replay to exact staged CAS evidence
1b993828d9938d8f1355776cf6cc93f3929f45b5 Retain exact stored closure reader roots without blocking unrelated GC
8bd5c5361dfd9d20ed5f704e421975752e1db1e7 Adapt workspace closure fixture assertions to explicit control membership
e2141340b79e5cf42e9a131a7f69d57fccd11f80 Join current Root dependencies for staged ingest validation
ddb7aa704f13842b25cddb135b8cfaab80db9e4a Exercise large parser facts through staged queue identity and strict cold replay
0654a73f6de7e12b40c2d2d06a28eb0ec8aa401f Charge physical CAS index and frontier allocation through staged pre-HEAD admission
3e1c3807fca09cc9c746853028cfadf50d659872 Check staged physical quota in the actual publication store
ea70a748747f5a3f18d919e5a1103dd2c2f3f597 Derive staged budgets from configured source policy and transfer selected pending pages
cc7afc381a035580649ce371050126f296e5df29 Preserve anonymous callable member anchors in semantic shapes
df96d07a2abdfc0fce1994a7d1b4be3070515458 Add anonymous anchors to local service fixtures
36a22d07d07e3624a624214203f8ee13d0d4ccb5 Match staged crash oracle to actual library module and clarify quota correction
199d02d2a96094ddeae0fab2ce62227a6846f9dd Correct staged quota audit after removing rejected workspace-wide cap
6a64059b9b33c81d8ad00694b882c8b7d329c9c9 Prove selected pending catalog transfer through public authenticated claim API
8e3fe57278d0f435ea019e0f8d78313024c6d205 Repair source-coordinate call join test fixture imports and typed identities
a5ef240418e92baf706fa95108a904ca32a25149 Measure actual large queue bytes and shared immutable stage allocation
d5f75c7ed4ce3226a429b5e4aa127d225bb2de82 Replace stored evidence scope for small followup commits and verify cold replay
7c7dc4535d6c32a2bcda0945adaa04495e321e89 Record fresh Mac store gates and honest prior staged compile failure
04f57f8822282bd8ec76e5894209f04f574b27f6 Retain complete raw compiler and store-test output for receipts
53af86cfd1d23b495e9104170edd96dbc56f8d28 Persist the exact bounded stored control frontier in workspace recovery packs
2ff4d8139eefa757773c8aaa9f11a7652c66330e Transfer production stage GC protection into exact per-closure leases
f99b7a1a3b1e91c273e607da357f047c21bc5120 Verify live staged requests allow orphan GC and retain exact evidence pages
42d83eb186fbc5900867d481d0e11dfe231c436d Keep relation roots behind typed CAS pointers in stored recovery packs
9a5563e5591e82fbe41f13d7a6144ecd96199486 Add actual acquired archive producer queue and cold facts acceptance gate
7a5154bbde12f21915beb12648d7079987580946 Apply current validated source policy limits without resetting live stage accounting
bd1fc4d50c2fc3be7565507aa5d2f8423edff4fb Continue direct GC mark records within the existing page byte limit
9a380f7c73eca04ccc93c197ab1079ecc089fdd6 Retain authentic 596-file Zod acquisition and cold replay progress evidence
1c6271a6bf4fe75e1ddc47d6397771e22e67d944 Refuse missing stored pages before published frames and interrupted HEAD repair
f7ced0cae34c85a1623b0548c6c9de783b0ab331 Begin manifest GC continuation records with a fresh bounded mark allowance
eab055240866e3ea278885aace37afab0464c275 Assert missing published evidence refusal at the eager cold store open gate
```

Changed files against the joined Root base:

```text
crates/engine/src/builtin/relation.rs
crates/engine/src/builtin/semantic_relation.rs
crates/engine/src/workspace/catalog/publication.rs
crates/engine/src/workspace/head.rs
crates/engine/src/workspace/owner/gc.rs
crates/engine/src/workspace/owner/lifecycle.rs
crates/engine/src/workspace/owner/publication.rs
crates/engine/src/workspace/owner/recovery.rs
crates/engine/src/workspace/owner/transaction.rs
crates/engine/src/workspace/pack.rs
crates/engine/src/workspace/recovery.rs
crates/engine/src/workspace/transition/mod.rs
crates/engine/src/workspace/transition/payload.rs
crates/library/lib.rs
crates/library/semantic_shape.rs
crates/library/wire/reply_semantic_shape.rs
crates/local-service/src/builtin.rs
crates/local-service/src/builtin/commands/adapter.rs
crates/local-service/src/builtin/commands/semantic_query.rs
crates/local-service/src/builtin/commands/semantic_shapes.rs
crates/local-service/src/builtin/profile.rs
crates/local-service/src/builtin/profile_source_facts_tests.rs
crates/local-service/src/builtin/staged_intent.rs
crates/local-service/src/builtin/staged_transport.rs
crates/local-service/src/builtin/view_build/call_join.rs
crates/local-service/src/builtin/view_build/csharp_field_namespace_join.rs
crates/local-service/src/builtin/view_build/go_field_join.rs
crates/local-service/src/builtin/view_build/go_type_mention_join.rs
crates/local-service/src/builtin/view_build/image_rows.rs
crates/local-service/src/builtin/view_build/java_enum_value_join.rs
crates/local-service/src/builtin/view_build/mod.rs
crates/local-service/src/builtin/view_build/py_function_field_join.rs
crates/local-service/src/builtin/view_build/py_static_field_join.rs
crates/store/src/closure/manifest/read.rs
crates/store/src/closure/membership.rs
crates/store/src/closure/mod.rs
crates/store/src/closure/workspace.rs
crates/store/src/durable/allocation.rs
crates/store/src/durable/closure_composer.rs
crates/store/src/durable/gc/mark.rs
crates/store/src/durable/gc/sweep.rs
crates/store/src/durable/gc/tests.rs
crates/store/src/durable/membership_lease.rs
crates/store/src/durable/mod.rs
crates/store/src/durable/publication.rs
crates/store/src/durable/publication_api.rs
crates/store/src/durable/recovery.rs
crates/store/src/durable/store_api.rs
crates/store/src/lib.rs
crates/store/src/tests.rs
docs/audits/staged-ingest-execution-20261006/raw/staged-a5ef2404-fleet.json
docs/audits/staged-ingest-execution-20261006/raw/staged-compile-36a22d07.log
docs/audits/staged-ingest-execution-20261006/raw/store-composer-6a64059b-fleet.json
docs/audits/staged-ingest-execution-20261006/raw/store-composer-6a64059b.log
docs/audits/staged-ingest-execution-20261006/staged-compile-receipt.json
docs/audits/staged-ingest-execution-20261006/store-composer-receipt.json
docs/audits/staged-ingest-execution-20261006/zod-3.25.76-acquisition.json
docs/audits/staged-ingest-membership-api-20261006.md
docs/audits/staged-ingest-physical-allocation-20261006.md
docs/audits/staged-ingest-queue-checkpoint-20261006.md
docs/audits/staged-ingest-reader-leases-20261006.md
```

Current evidence: actual authenticated Zod3.25.76 archive (596 regular files) scanned through production unproven-authority source discovery; queue8,091,684 bytes exceeds unchanged4MiB and refuses inline, staged queue357/pointer180 publishes and cold-reopens with every expected facts record exact on9a380f7c73. No native/compiler completeness credit or identical original8,092,105-byte driver claim. See staged-ingest-execution-20261006/zod-parser-queue-cold-receipt.json.

Store source eab0552408 passes new GC direct/manifest byte continuation and both missing-page pre-publish/pre-HEAD cold repair tests. App source eab0552408 is still being validated; its earlier9a large GC integration was RED before publication/cold and cannot be credited.

Pending allocation is explicitly open: persistent actual-block accounting currently covers staged payload/manifest CAS objects, not all changed frontier/index nodes/descriptors. A future scoped immutable-install admission must catalogue these even if composition is interrupted. No whole-CAS staging cap is attached in production; optional PhysicalAllocationBudget is only an explicit total-store primitive/test helper. Source limits derive from validated existing SourceAdmissionPolicy, and changing configuration preserves the affine live reservation ledger.
