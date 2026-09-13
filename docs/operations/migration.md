# Package structure and the replacement sequence

This is the selected v2 end state. The earlier 53-crate audit remains evidence, but its folder proposal is superseded. The implemented structure has **twelve shared crates, seven language frontends, four optional integration leaves, and five application binaries: 28 product packages**. Test and development packages remain outside that count. The replaced state machines and their source trees have been removed.

## 1. The folder tree follows ownership

```text
backend/
  Cargo.toml                         # workspace policy, profiles, dependency versions
  crates/
    version/                         # canonical meaning; no I/O or product services
      src/{identity,schema,value,relation,delta,workspace,batch,coverage}.rs
    store/                           # durable objects and atomic head selection
      src/tree/{node,cuts,update,diff,proof}.rs
      src/pack/{columns,encode,decode,location}.rs
      src/commit/{prepare,journal,publish,recover}.rs
      src/{pins,gc,residency,checkpoint}.rs
    control/                         # versioned agent work, evidence, leases, and review
      src/{spec,context,ledger,lease,receipt,durable,wire}.rs
    flow/                            # incremental computation and retained indexes
      src/trace/{spine,cursor,merge,frontier}.rs
      src/operators/{map,filter,join,reduce,distinct,topk,fixedpoint}.rs
      src/graph/{ir,intern,dependencies,demand,ready}.rs
      src/kernels/{selection,consolidate,strings,vectors}.rs
    replication/                     # object/root exchange and versioned transports
      src/{reconcile,missing,transfer,protocol,receipt,local_transport}.rs
    execution/                       # physical resource and placement decisions
      src/{admission,planner,cost,local,remote,attempt,cancel,supervisor}.rs
    semantic/                        # language-independent meaning and fact schemas
      src/{entity,facets,types,edges,source,provenance,validation}.rs
      src/recipes/{names,lexical,graph,embeddings}.rs
      src/compat/{owned_ir,wire_ir,export}.rs
    compile/                         # authority contract and shared extraction pipeline
      src/{authority,manifest,discovery,session,extract,changes,coverage}.rs
      src/source/{acquire,files,generated,environment}.rs
      src/process/{supervise,limits,protocol}.rs
    library/                         # product intents and versioned query/view recipes
      src/{commands,intents,identity,queries,documents,views,protocol}.rs
    client/                          # one typed local protocol client for every surface
      src/{session,transport}.rs
    runtime/                         # zero-setup project and endpoint discovery
      src/lib.rs
    engine/                          # the only composition root
      src/{workspace,registry,services,effects,observability,recovery}.rs
  frontends/
    {clang,csharp,go,java,python,rust,typescript}/
      src/{authority,session,extract,lower,diagnostics}.rs
      # Language-specific files and foreign oracle fixtures remain where meaningful.
  extensions/
    tantivy/                         # specialized full-text execution/materializations
    trustfall/                       # query-language integration
    qdrant/                          # optional remote vector provider
    turso/                           # transactional derived projection of selected roots
  apps/
    desktop/                         # rendering + local engine client
    cli/                             # command parsing + local engine client
    mcp/                             # MCP protocol + local engine client
    locald/                          # local workspace service/bootstrap
    worker/                          # authenticated permitted-recipe remote worker
  tests/
    {laws,compatibility,journeys,crash,concurrency,performance}/
  tools/
    {workspace,dylint,fixtures,bench}/
  docs/
    {architecture,schemas,protocols,decisions,operations}/
```

These are ownership examples, not a mandate to create every listed one-function file. Modules split when they contain a coherent algorithm or invariant. Keep tightly coupled layouts and validation next to each other; avoid generic `utils`, `common`, `manager`, `vocabulary`, `core`, and `service` crates that become dependency escape hatches. A `services.rs` in the sole composition crate wires concrete services; it does not define a new universal service abstraction.

## 2. The allowed core DAG

| Crate | May depend on these lower core crates | Owns | Must not own |
|---|---|---|---|
| `version` | None | Typed IDs, canonical schema/value contracts, relation/batch descriptors, exact deltas, WorkspaceRoot and coverage vocabulary | Filesystems, schedulers, native language enums, UI command routing |
| `store` | `version` | Canonical trees, packs, location index, durable transaction/head, pins, GC | Semantic policy, query operators, compiler ownership |
| `control` | `version`, `store` | Versioned agent cells, context deltas, leases/fences, evidence, review receipts, durable work head | Product workspace publication, code execution, self-certification |
| `flow` | `version`, `store` | Ordered traces, operators, dynamic graph, demand/frontiers, incremental state | Threads/processes/remote placement, product commands |
| `replication` | `version`, `store` | Version negotiation, root reconciliation, bounded transfers, generic job/result envelopes and local transport | Executing compiler recipes, product merge decisions |
| `execution` | `version`, `store`, `flow`, `replication` | Admission, work selection, placement, attempts, cancellation and resource accounting | Semantic authority, user commands, native language matching |
| `semantic` | `version`, `flow` | Entity/type/facet schemas, semantic graph/query kernels, IR compatibility | Native compiler dependencies, durable head mutation, GUI state |
| `compile` | `version`, `flow`, `semantic` | Input discovery/manifest, authority traits, shared lowering/extraction, complete scoped fact changes, process mechanics | Importing concrete language crates, selecting active product workspace |
| `library` | `version`, `flow`, `semantic` | Product identity, durable intents, query/document/view recipes, portable command/reply schemas | Native process ownership, transport sockets, scheduler handles |
| `client` | `library`, `replication` | Typed local session, request framing, daemon bootstrap, and shared retry semantics | Product state, frontend selection, or surface-specific rendering |
| `runtime` | None | Zero-setup project-root and local endpoint discovery | Semantic identity, durable state, protocol policy, or global mutable configuration |
| `engine` | All lower core crates | Composition, registration, concrete command/effect execution, workspace lifecycle | New canonical identity/delta/index formats |

`flow` exposes ready recipe descriptors and checkpoints. `execution` drives them; flow never imports execution to spawn work. `replication` moves a permitted recipe envelope; execution decides to run it. `library` builds typed graph requests and durable intents; engine executes them through concrete compile/store/execution services. Do not introduce a generic repository trait merely to reverse a dependency.

Native leaves implement contracts from `compile` and consume `semantic`/`version`. `compile` never depends on the seven native leaves. The engine registry matches the language once per authority/session boundary and installs specialized kernels. This removes the present driver's fixed all-language dependency coupling. Keep source acquisition/toolchain identity contracts below concrete authority implementations.

Optional extensions depend on the narrow shared contracts they project. Engine registers them. Tantivy, Trustfall, Qdrant, and Turso materializations carry the same input roots, coverage and recipe IDs; none defines another canonical index generation or writes the selected workspace head independently.

Desktop/CLI/MCP share `client`, `runtime`, portable `library` schemas, and the local `replication` transport. They need no native frontend dependency or manual socket setup. `locald` and `worker` instantiate engine with explicit capability sets. Engine's native/provider Cargo features control only leaf inclusion, not different identity or delta semantics. Different deployments cannot silently select different canonical hashing through a feature flag.

Enforce this DAG mechanically in Cargo metadata. A newly needed upward import means responsibility is misplaced; do not cure it with reexports or `pub use` loops. Crate count is not the optimization target, but these twelve shared boundaries have distinct effects, invariants and change rates, so they earn independent compilation and review.

## 3. Contracts that cross packages

There are five boundary artifacts, all concretely versioned:

1. **Canonical object/batch:** schema and dictionary-bound values with validated immutable ownership.
2. **Workspace transition:** exact base/target root-of-roots plus complete relation changes and coverage.
3. **Recipe input/result:** declared output equivalence, validated read manifest, roots and provenance.
4. **View snapshot/delta:** stable row identity, exact source basis, coverage, ordering and bounded cursor.
5. **Effect intent/receipt:** idempotency, expected authority/fence, recovery and external result.

These replace independent root/image/index/library/transport generations. Logical CommitId and execution time remain distinct as defined in ARCHITECTURE; this list does not collapse them. Packs/materializations implement physical storage of canonical objects and batches. View, trace and transport cursors belong to their respective observation/progress/protocol contracts; none is a new semantic authority.

Errors remain typed at the package that can recover: decode/layout fault, stale-base conflict, incomplete coverage, authority failure, admission backpressure, or unavailable capability. Engine maps them once to the portable product reply. Native diagnostic fidelity survives inside provenance/evidence and structured error payloads. Do not flatten everything to String and reconstruct policy by matching text.

The complete current-to-target crate map follows below. A split row means responsibilities are split deliberately; it is not permission for both destinations to keep the old mechanism.

| Current workspace crate | Final owner(s) | Responsibility/deletion rule |
|---|---|---|
| `compiler-application` | `engine` | Workspace/compiler service composition; remove independent one-slot owner after shared scheduler cutover. |
| `compiler-driver` | `compile + frontends/*` | Shared extraction/coverage/manifest contracts in compile; authority-specific projections in leaves; remove all-native imports. |
| `compiler-ir` | `semantic` | Keep semantic/type kernels and owned/wire compatibility readers; replace mandatory whole-image ownership with pinned relations. |
| `compiler-ir-vocabulary` | `semantic` | Merge canonical semantic schema next to generated lanes/codecs; do not create another vocabulary crate. |
| `compiler-languages` | `compile` | Delete umbrella crate; retain only genuinely common authority metadata/contracts as modules. |
| `compiler-languages-clang` | `frontends/clang` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-languages-csharp` | `frontends/csharp` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-languages-go` | `frontends/go` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-languages-java` | `frontends/java` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-languages-python` | `frontends/python` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-languages-rust` | `frontends/rust` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-languages-typescript` | `frontends/typescript` | Preserve native authority/language semantics and oracle fixtures; implement shared manifest/session/scoped-delta contracts. |
| `compiler-publication` | `semantic + store + engine` | Produce complete semantic fact transition; generic atomic publication in store; composition/fencing in engine. Remove driver dependency. |
| `compiler-registry` | `compile + engine` | Pure authority descriptors/contracts below leaves; active provider registration/composition only in engine. |
| `compiler-vocabulary` | `semantic + compile` | Portable provenance/language/schema facts in semantic; authority/discovery policy in compile. |
| `heart-adaptive` | `execution` | Retain pure resource/locality policy strengths; extend to joint refresh/placement costs. |
| `heart-frame` | `version + replication` | Generic validated frame contract in version; actual transport framing/negotiation in replication. |
| `heart-hydration` | `store + replication` | Local decode/residency in store; missing immutable object transfer in replication; no duplicated hydration authority. |
| `heart-identity` | `version` | Domain-separated logical/object/state/workspace identities, preserving distinction from packs and attempts. |
| `heart-memory` | `store + flow` | Immutable owner/pin and pool mechanics in store; mutable operator scratch/batch leasing in flow. |
| `heart-object` | `version + store` | Logical object descriptors in version; admission/residency/pin implementation in store. |
| `heart-object-pack` | `store` | One typed columnar pack codec/location/admission lifecycle. |
| `heart-observe` | `version + execution` | Small observation value contracts in version; resource/work instrumentation in execution; no standalone crate. |
| `heart-root` | `store` | Persistent ordered map, exact root diff and atomic WorkspaceRoot; retain linear diff as oracle/import fallback. |
| `heart-schema` | `version` | Canonical value/batch schema and complete encoding contracts. |
| `heart-telemetry` | `engine` | Subscriber/export wiring and product observability; lower crates emit structured observations without upward imports. |
| `heart-view` | `store` | Pinned physical object view; product ViewRoot belongs separately to library/flow. |
| `interface-cli` | `apps/cli` | CLI syntax/output and local engine client; no native compile owner. |
| `interface-core` | `library + engine` | One portable product command/reply contract in library; concrete service composition in engine; remove competing facade. |
| `interface-documents` | `library` | Versioned document fragment recipes and borrowed projections. |
| `interface-gui` | `apps/desktop` | UI rendering and bounded ViewRoot subscriptions; no library-wide refresh authority. |
| `interface-identity` | `library + semantic` | Product address/selection identity in library; semantic logical declaration keys in semantic. |
| `interface-library` | `library + engine` | Implement durable intent/query recipes in library and their concrete engine execution; preserve intended command model. |
| `interface-mcp` | `apps/mcp` | MCP protocol and local engine client; share query/command semantics. |
| `interface-protocol` | `library + replication` | Portable product DTOs in library; transport/session/wire mechanics in replication. |
| `interface-search` | `library + semantic` | Product query/freshness and semantic ranking/operator recipes. |
| `server-index-acquire` | `compile + replication` | Package/source acquisition and input manifests in compile; immutable transfer in replication; effects executed by engine. |
| `server-index-build` | `flow + semantic` | Consolidation/arrangement construction plus semantic projection kernels; remove private build lifecycle. |
| `server-index-catalog` | `store + library` | Durable catalog relations in store; selection/product intent schema in library. |
| `server-index-core` | `flow + semantic` | Generic retained arrangements in flow; semantic relation/recipe definitions in semantic. |
| `server-index-graph-vector` | `flow + semantic` | Shared graph arrangements, vector facts and versioned ANN overlay/barrier. |
| `server-index-ingest` | `compile + semantic + flow` | Complete fact delta producer, semantic validation and shared batch admission; no downstream repeated whole-image diff. |
| `server-index-publish` | `store + flow` | Shared atomic result admission and input-bound view checkpoint; remove independent snapshot authority. |
| `server-index-qdrant` | `extensions/qdrant` | Optional authenticated remote vector provider; no independent canonical identity. |
| `server-index-retrieval` | `semantic + library` | Semantic query operators and product query contracts/ranking/freshness. |
| `server-index-routing` | `execution + replication` | Cost-based placement/attempt assignment in execution; version-bound request/reply wire and transfers in replication. |
| `server-index-tantivy` | `extensions/tantivy` | Optional specialized lexical materialization/query backend under shared roots and coverage. |
| `server-index-trustfall` | `extensions/trustfall` | Query-language adapter over semantic/flow contracts. |
| `server-index-vocabulary` | `semantic` | Index/query fact schemas colocated with semantic meaning; common root IDs come from version. |
| `server-journal` | `store` | Durable commit/effect receipt journal and recovery sequence. |
| `server-operation` | `flow` | Lending batch/trace cursors and typed operator state; eliminate separate stream lifecycle. |
| `server-runtime` | `execution` | Admission, cancellation, fenced attempts, workers, resource accounting and waiter laws. |
| `server-workflow` | `store + execution + engine` | Generic durable reducer/receipt in store, physical retry/cancel in execution, domain effect wiring in engine. |

## 4. A migration organized around replacing the engine

The replacement was built as an end-to-end substrate rather than a rename. External reference checkouts and independent full-recompute models supplied migration oracles; they have no runtime adapter, workspace membership, or write authority in the completed tree.

| Slice | Concrete result | Depends on | Acceptance gate | What becomes deletable |
|---|---|---|---|---|
| K0 — Contract ledger and baseline | Every canonical facet/authority scope inventoried; representative cold/edit/query/offline workloads captured; dirty source preserved | Existing audit | Coverage ledger and actual performance baseline, no unsupported speed claims | Duplicate prose/types that conflict with the adopted canonical contracts |
| K1 — Version/store kernel | Typed roots and canonical ordered tree; complete-value objects; exact WorkspaceDelta; packs/location/pins; atomic local commit and recovery | K0 | History-independent roots, random apply/diff oracle, compaction invariance, crash matrix, bounds/overflow tests | New writes through flat full-root-only machinery for pilot scope |
| K2 — First shared flow | Column batches, arrangements, delta map/join/distinct, epochs/frontiers, dependency manifests, ephemeral demand | K1 | Signed algebra parity, simultaneous changes, support deletion, retained-byte limits | Pilot custom staging/sort/dedup/cache lifecycle |
| K3 — Vertical local product | One package's docs/name/outline facts → local commit → query → stable row delta; locald shared by CLI/MCP/desktop | K1–K2 | Edit visible through exact ViewRoot; restart and second client reuse same state; local offline operation | Pilot library refresh loop and per-client state owner |
| K4 — Complete authority bridge | All seven language adapters emit complete scoped facet changes, initially from legacy snapshots where needed | K0–K3 | Native semantic/evidence parity; partial coverage never deletes outside authority; manifest invalidation corpus | Whole-image diff at each downstream consumer; driver all-native dependency |
| K5 — Shared semantic arrangements | Exact/name/lexical, both graph directions, source/doc joins and top-k support in flow | K2–K4 | Current-vs-new results under edits/deletes/global-score changes; exact scope/freshness tests | Bespoke common index builders, shadows, routing-specific generation owners |
| K6 — Delta-aware work planner | Work interning, candidate validation, reuse/update/rebuild plans, bounded admission and hot-key partitioning | K2–K5 | No-op work suppression, skew/broad-edit plan switches, cancellation and accounting | Independent compiler/index/render cache and admission orchestration |
| K7 — Replication and remote memo | Root reconcile, missing objects, shared pack wire, recipe receipts, remote cached outputs | K1, K4, K6 | Offline branches, stale-base repair, revoked authority, interrupted transfer, exact input/output basis | Independent snapshot/bundle sync and remote cache identity paths |
| K8 — Remote warm compute | Placement over retained remote traces, background precompute, bounded hedge and fallback policies | K6–K7 | Local baseline/SLO experiments with queue/network/validation cost and interference; stale worker fencing | Separate query-segment routing policy when its functions are covered |
| K9 — Vector and heavy recursion | Versioned vector facts/ANN overlay and explicit fixed-point/SCC plans | K4–K8 | Recall/freshness+deletion tests, exact rerank, SCC split/merge/DRed parity, bounded incomplete states | Duplicate vector authority and graph-wide invalidation paths covered by engine |
| K10 — Native session reuse | Safe persistent authority sessions, input change capture and shared discovery/compile outputs | K4, K6 | Per-language cold/session differential corpus, toolchain reset, memory/cancellation/crash isolation | Redundant authority invocations and duplicated extraction passes proven equivalent |
| K11 — Layout and kernel specialization | Column COW, dictionary reuse, bounded compaction, SIMD kernels, locality-aware batching | K2, K5, measured baselines | Allocations/retained bytes/write amplification/branch counters improve without output drift | Legacy conversion and copies within converted scopes |
| K12 — Cutover and contraction | All surfaces and required providers use shared engine; new package DAG; legacy persistence compatibility reader only | K3–K11 | Full product journeys, durable compatibility/rollback drill, documented unsupported scopes, no dual head writer | Old crate wrappers, secondary lifecycle systems, legacy writers and obsolete test scaffolds |

Work can proceed in parallel after the early contracts: storage/flow, native adapters, portable product views, and remote protocol research have independent implementation lanes with a shared schema ledger. K3 intentionally lands before completing every language/performance feature so the architecture is proven as a real local product. K7 need not wait for every specialized ANN/recursive operator. Do not expose a partial pilot as complete language/product coverage.

## 5. Per-slice replacement mechanics

**Root migration:** export a current coherent generation through validated readers; assign stable authority-derived logical keys; produce complete facet values and the first WorkspaceRoot; keep a reversible mapping from legacy generation IDs to new root/basis metadata. Build canonical trees from sorted bulk input once. New commits use producer mutation batches and path updates. Compare root diff changes with the legacy merge oracle, including docs/evidence that need stronger new coverage.

**IR migration:** keep owned/wire IR readers as compatibility projections over one pinned semantic view. Existing columnar/CSR kernels can first operate on hydrated scope batches. Gradually replace whole-image construction in the hot path with direct relation producers and borrowed arrangements. Preserve IR format/version/endian/alignment/validation contracts for exported artifacts until explicitly retired. A module move must not accidentally change wire layout or typed-ID scope.

**Native migration:** put existing language-specific authority semantics behind `compile` contracts; unify input manifests, sessions, process supervision, batch ownership and scoped change capture. Do not write a new generic parser to replace seven mature compiler authorities. Each adapter declares package/file/project invalidation scope, supported positive/negative reads, tool revision, environment closure and uncertainty. Unknown scope falls back to complete authority scope recompute with honest coverage.

**Index migration:** feed the same semantic transition into old and new projections, compare exact relations/results where contracts match, and classify deliberate ranking/approximation differences. Migrate core exact/name/edge/doc paths first; keep optional specialized engines through root-bound adapters. Per-relation input/coverage metadata prevents one successful index lane from falsely publishing an entire new search generation.

**Product migration:** portable commands and replies live in `library`, the concrete client session lives in `client`, and engine executes durable operations. Stable library/address identity laws survive. Observation is centralized as ViewRoot+cursor; GUI view models receive row/fragment deltas and reset to a coherent snapshot on lag. CLI/MCP share the same query recipes and error semantics.

**Remote migration:** initially use remote as exact immutable memo/object transfer; then add pure scope execution; then warm-trace delta advancement. This order isolates identity/trust/transfer defects before scheduling complexity. A remote worker cannot advance a local workspace head directly. A remotely built physical pack or ANN base is admitted like any other untrusted materialization and selected only under matching input/coverage contracts.

**Folder migration:** move a responsibility when its shared contract is in place, with Cargo path compatibility only temporarily. Track each old crate's remaining public imports. Remove transitional reexports once callers are converted. The final target is the DAG above; aliases should have deletion owners rather than live indefinitely as architectural barnacles.

## 6. Durability, compatibility, and rollback

Version canonical schema, key ordering, hash domain, tree cut policy, recipe ABI, wire protocol, and physical pack format independently. A physical codec migration can preserve logical IDs; a canonical value encoding change requires an explicit version namespace/mapping and invalidates keys where semantics changed. Persist reader capability requirements. Unknown canonical fields cannot be silently dropped while claiming the same complete object version.

During shadow comparison, exactly one selected head/writer is authoritative. Side effects execute through one intent/receipt owner. New derived caches may be discarded on rollback; durable user intents/source commits need a tested compatibility export or a supported old reader. A one-way new schema cutover is an explicit release boundary with backup/recovery planning, not something hidden in a background compaction.

Keep old snapshots readable for the declared compatibility window. A read bridge must preserve docs, visibility, language extensions, occurrences and unavailable states, not only the old partial core payload. Before removing the old writer, test restart at every prepare/flush/head-selection/notification phase and replay duplicate delivery. Retire old physical materializations only after retention and reader pins permit it.

## 7. Engineering quality and performance gates

The existing detailed language, IR and tooling audit remains the checklist for current hazards. New kernel tests add algebraic/property tests, not a duplicate test per forwarding method. Require:

- Canonical encoding determinism; complete-facet coverage; strict bounds/alignment/overflow checks and fuzzed decode admission.
- Random edit/history/merge sequences compared with full reconstruction, plus exact workspace atomicity and crash-recovery laws.
- Trace frontier and pin/cancel races, including old/new pack coexistence; Miri for unsafe/lifetime-sensitive kernels and targeted concurrency model tests where applicable.
- Every language's authoritative corpus, negative dependency additions, generated sources, feature/toolchain changes, recursive types, evidence availability and session/subprocess parity.
- Product journeys across CLI/MCP/desktop: add, resolve, browse, search, graph, edit/delete, restart, two clients, offline, reconnect, lag/reset, stale remote and incompatible capability.
- Performance distributions for cold load, docs-only edit, body edit with unchanged export, rename, broad dependency change, large SCC edit, lexical/global-stat change, vector churn and compaction pressure.

Record useful work and overhead separately: authority work, changed logical rows/nodes, output fan-out, support rows, copied/hashed/serialized bytes, allocations, retained RAM, disk writes/read amplification, remote bytes, wasted speculation, planner time, and p50/p95/p99 interaction latency. Report build time and binary size as costs of generic specialization. Compare connected results against the same local-only workloads and resource envelopes.

Numerical acceptance thresholds follow measured baseline and product SLOs in K0; they are not invented here. Structural gates are already definite: no unchanged full-root traversal on the optimized value-edit path, no global semantic rebuild downstream of equal subscribed facets, no unbounded subscriber/delta chain, no duplicate native analysis for the same valid shared work, and no remote dependency for locally promised operations. Cases with real global dependencies are measured as such rather than hidden from the benchmark.

The implementation backlog is generated from K0–K12 and the 53-row ownership map, with a deletion check for each converted responsibility. Completion means the old mechanisms disappear and the 28-package engine carries the product.

The executable status for the current cutover slice is maintained in
[cutover-evidence.md](cutover-evidence.md). That index records the independent
full-recompute/delta, ordering, payload-budget, join, compaction, crash, remote
fallback, fencing, cancellation, and seven-process-journey checks. It
distinguishes structural evidence from production promotion; passing a
low-level law cannot be used as evidence that a production responsibility has
been deleted.
