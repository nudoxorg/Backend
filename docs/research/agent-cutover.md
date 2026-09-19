# Multi-agent execution and regression-gated cutover plan

This plan operationalizes the selected v2 design under `docs/architecture`,
`docs/schemas`, and `docs/operations`. It assumes those architecture, semantic,
layout, local/remote, and migration documents are the design authority. It
describes how a team can execute the migration without losing provenance,
native-language behavior, durable compatibility, or performance evidence. No
implementation or production source change was made for this report.

“Regression-free” is a gated evidence goal: the cutover may proceed only when required equivalence, crash, trust, and performance checks pass their predeclared thresholds. No finite test campaign proves absence of every regression, and no latency guarantee is implied.

## Current repository facts that shape execution

The source workspace is a Cargo resolver-3 workspace with globs for `compiler/*`, `heart/*`, `interface/*`, and `server/index/*`, plus explicit server journal/operation/runtime/workflow members; `server/index/turso` is excluded (`/Users/mileswirht/Downloads/backend/Cargo.toml:1-18`). The v2 map audited the legacy source crates into 25 target product packages, including the new versioned control dispatcher (`docs/architecture/crate-map.json`). The target DAG has ten core crates, seven native leaves, three optional extensions, and five applications (`docs/architecture/package-dag.json`).

The current code has substantial, useful test surfaces. A source inventory found approximately 52 test functions under `server/runtime`, 33 under `server/workflow`, 130 under `server/journal`, 138 under `server/operation`, 46 under `server/index/routing`, 99 under `server/index/graph-vector`, 62 under `heart/adaptive`, 199 under `heart/root`, 98 under `compiler/application`, and about 1,100 under `compiler/driver`; these counts are inventory signals, not pass results. Native language behavior is spread across seven crates (`compiler/languages/{clang,csharp,go,java,python,rust,typescript}`), with corpus and authority tests in each leaf. Existing runtime and graph code also has Loom/model-oriented tests, journal tests include allocation/fault/reopen coverage, routing has a benchmark, and root has capacity-planning benches.

The v2 sequence is K0–K12: contract/baseline, version/store, flow, vertical local product, authority bridge, shared arrangements, work planner, replication, warm remote compute, vector/recursion, native sessions, layout specialization, and final contraction (`STRUCTURE-AND-MIGRATION.md:159-179`). Agents must report against these IDs rather than inventing a parallel roadmap.

## Ownership topology

Use one integration lead and narrowly owned lanes. Every lane has one implementation owner, one independent reviewer, and one oracle owner. A person may fill two roles on a small slice, but a change that modifies a canonical schema or deletion gate requires an independent reviewer. Never allow an agent to merge its own schema authority change.

| Role | Owns | Must not own |
|---|---|---|
| Integration lead | Dependency DAG, train order, conflict resolution, release/cutover decision | Silent semantic changes to a contract |
| Contract authority | `version` IDs, schema, canonical encoding, coverage, transition envelopes | Concrete filesystem, scheduler, native parser |
| Store lane | Durable tree/pack/commit/recovery, pins and GC | View semantics or placement policy |
| Flow lane | Traces, operators, frontiers, graph, demand, arrangements | Process/thread ownership or wire transport |
| Execution lane | Admission, cost, placement, attempts, cancellation, resource accounting | Canonical data meaning or native language selection |
| Replication lane | Version negotiation, missing objects, transfer, receipts, local transport | Executing recipes or advancing local heads |
| Semantic lane | Complete facets, type/edge/source schemas, compatibility readers | Native compiler process lifecycle |
| Native adapters | One language authority each; manifests, sessions, extraction, diagnostics | Shared canonical schema definitions |
| Product/app lane | Library commands, engine composition, GUI/CLI/MCP journeys | Canonical IDs or durable writer policy |
| Oracle/evidence lane | Legacy differential runners, traces, fixtures, thresholds, evidence ledger | Optimizing implementation under test |
| Operations/recovery lane | Backups, replay drills, feature flags, canaries, rollback | Declaring semantic parity without oracle evidence |

The target DAG is the ownership rule: `version` has no lower dependency; `store` depends on version; `flow` on version/store; `replication` on version/store; `execution` on version/store/flow/replication; `semantic` on version/flow; `compile` on version/flow/semantic; `library` on version/flow/semantic; and `engine` composes all (`STRUCTURE-AND-MIGRATION.md:63-85`). An upward import is an escalation, not a convenience reexport.

## Worktrees and the integration train

Use one clean base checkout and one worktree per lane. Branches use `codex/` prefixes, for example `codex/v2-k1-store`. Keep generated fixtures and benchmark outputs outside source paths or in a clearly named evidence directory. Do not share mutable build output between agents when a toolchain or feature change is under review; use isolated `CARGO_TARGET_DIR` values per worktree if parallel builds contend.

The integration lead maintains a linear train of small, reviewable merge points:

```text
baseline -> K0 contracts -> K1 version/store -> K2 flow
                                      |             |
                                      +-> K3 local product
                                      +-> K4 native bridge -> K5 arrangements
                                                            -> K6 planner
                                      K1 + K4 + K6 -> K7 replication
                                      K6 + K7 -> K8 remote
                                      K4..K8 -> K9 vector/recursion -> K10 sessions
                                      K2 + K5 + measurements -> K11 layout
                                      K3..K11 + compatibility -> K12 cutover
```

The train is not a single long-lived branch. Each slice lands as a compatibility-preserving commit series with a tagged evidence packet. The next slice branches from the latest green train head, while independent lanes may prototype against the K0 contract tag. Rebase or merge only at train boundaries; do not repeatedly rebase unfinished work onto moving schema commits.

Each merge request carries: scope/K ID, contract versions touched, old/new file map, oracle command and result, benchmark command and result, migration flag, rollback flag, known unsupported scope, and links to the evidence ledger. A source move without a responsibility/deletion update is incomplete.

## Agent memory reuse and avoidance of duplicate work

Before dispatch, the integration lead creates a context packet with the exact v2 sections, source paths, current train commit, ownership row, and open evidence questions. Agents first read the packet and the latest ledger, then inspect only files inside their lane plus declared cross-boundaries. They do not redo repository-wide audits already recorded in `SOL-REVIEW.md`, `crate-map.json`, `source-verification.json`, or prior packets.

Use stable artifact names:

```text
evidence/K0/schema-ledger.json
evidence/K0/baseline/<workload>.json
evidence/K1/root-oracle/<seed>.json
evidence/K4/native/<language>/<case>.json
evidence/K7/replication/<fault>.json
evidence/K8/latency/<scenario>.json
evidence/K12/rollback/<drill>.json
```

An artifact is append-only once referenced by a gate. Corrections produce a new revision with a reason and supersession link. Agents hand off compact artifacts: changed contract list, invariant list, commands run, raw result paths, interpretation, and unresolved questions. Paste neither large logs nor entire source files into subsequent prompts; pass paths and digests.

Prompt/context packet template:

```text
Objective: Kx <one sentence>
Authority: v2/<document>#<section>; contract versions: ...
Allowed paths: ...; forbidden paths: ...
Inputs: train commit ..., evidence artifacts ..., oracle fixtures ...
Required outputs: code/patch or research, tests, benchmark, evidence JSON
Invariants: ...
Compatibility: old reader/writer/flag behavior ...
Escalate if: schema ambiguity, upward dependency, oracle mismatch, threshold miss
Do not: broaden scope, delete legacy path, claim performance from one run
```

When a later agent needs information, send the existing artifact and ask for a delta review. Do not assign “audit everything” after K0 unless a gate reveals a specific coverage gap.

## Contract and schema authority

The contract lane freezes the five boundary artifacts before broad implementation: canonical object/batch, workspace transition, recipe input/result, view snapshot/delta, and effect intent/receipt (`STRUCTURE-AND-MIGRATION.md:87-99`). It publishes a machine-readable schema registry and a field-coverage ledger derived from `SEMANTICS-AND-LAWS.md:5-77`. K0 must expand nested type/foreign/native variants mechanically; prose names are insufficient.

Schema changes are classified before review:

* A physical codec/location change may preserve logical IDs if canonical bytes and validation are unchanged.
* A canonical encoding, key order, hash domain, tree cut, recipe ABI, wire protocol, or coverage change increments its independent version and requires compatibility mapping.
* A semantic field addition cannot be silently dropped by an old reader while claiming complete coverage.
* A control/demand/attempt field does not belong in the authoritative workspace log unless recovery requires it; authoritative, derived, and control retention remain separate (`SEMANTICS-AND-LAWS.md:152-158`).

The registry owner signs off on every public type and envelope. Native agents may propose fields but cannot alter shared identity/hash/coverage semantics. The execution lane owns attempt/fence IDs; replication owns transport framing; store owns commit receipts; engine maps typed errors once to product replies.

## Regression strategy: oracle, shadow, dual-read, canary, rollback

### Baseline and oracle

K0 records deterministic input manifests, toolchain versions, feature flags, OS/runtime, corpus digests, memory limits, and output normalization rules. Existing implementations remain the reference oracle during migration, as required by `STRUCTURE-AND-MIGRATION.md:159-179`. For each workload, capture canonical output, structured diagnostics, coverage/availability, ordering, provenance, bytes read/written, allocations, and p50/p95/p99 latency. A baseline without raw traces is not admissible evidence.

The oracle comparator must distinguish exact equivalence from declared differences: approximate ANN ranking, native diagnostic wording, physical layout IDs, and timing distributions can differ under an explicit contract; object IDs, roots, complete facets, missing/partial states, authority, freshness, and durable receipts cannot silently differ.

### Shadow execution

During K1–K8, feed identical immutable input manifests and deltas to old and new readers/operators. The old path remains the sole head writer. New outputs are written to an isolated namespace keyed by `(candidate_schema, input_root, recipe, run_id)` and are never visible to product reads. Compare after each closed frontier. Shadow work has explicit CPU/RAM/remote budgets and is disabled automatically on resource pressure.

For native authorities, shadow extraction must use the same source/toolchain/environment manifest but isolated output directories and process limits. It may not mutate shared caches or publish facts. For remote workers, shadow results are untrusted materializations: validate input root, manifest, recipe, coverage, signature, and output digest before comparison.

### Dual-read

After a shadow slice meets equivalence gates, add a read bridge that can read old and new representations. Select the old result as authoritative while logging new-read comparison. A dual-read mismatch returns the old result, records a typed mismatch with both roots/recipes, and blocks promotion for that scope. Dual-read must cover restart, old snapshots, missing objects, partial coverage, cursor gaps, and stale remote attempts.

### Canary and promotion

Canary at the smallest coherent scope: one local workspace, one package, one language, one query recipe, or one worker pool. Use deterministic cohort assignment by workspace/operation identity so retries do not move a user between old/new paths. Promote only after the canary has enough cold, edit, query, offline, reconnect, cancellation, restart, and concurrent-client samples to make the declared confidence meaningful.

One selected head/writer remains authoritative throughout. New derived caches may be discarded on rollback; durable user intents/source commits require an old-reader-compatible export or a tested forward bridge (`STRUCTURE-AND-MIGRATION.md:197-203`). Never run old and new writers against the same canonical head without a transactional arbitration protocol.

### Rollback

Every slice has a kill switch and a tested rollback target. Rollback means stop new admissions, drain/cancel new attempts, leave the old head writer active, and preserve candidate artifacts for diagnosis. It does not delete candidate objects while leases or reader pins exist. For a failed durable transition, recover from the last validated old head or compatibility journal; for a failed remote worker, fence its attempts and retain no result without matching input/authority.

Rollback drills cut execution after every prepare, encode, flush, journal append, pack seal, head selection, notification, transfer receipt, and native process boundary. The expected result is either the old coherent root or a fully validated new root, never a half-visible root. This is the crash matrix demanded by `STRUCTURE-AND-MIGRATION.md:197-203` and `SEMANTICS-AND-LAWS.md:205-218`.

## Multi-agent slice plan and merge order

1. **K0 contract/evidence lane.** Freeze schema registry, complete facet ledger, baseline workloads, raw trace format, comparator, and threshold policy. Merge this first. No downstream agent may invent an identity or coverage field.
2. **K1 store lane.** Implement version/store prototypes and crash/replay oracle behind a compatibility adapter. Merge only after history-independent roots, random edit/diff, compaction invariance, and fault recovery pass (`STRUCTURE-AND-MIGRATION.md:163-167`).
3. **K2 flow lane.** Add batches, arrangements, deltas, frontiers, and dependency manifests. Merge after simultaneous-change/support-deletion algebra and retained-byte tests. Store and flow may work in parallel after K0 but flow cannot merge without K1 contracts.
4. **K3 product lane.** Build one end-to-end local docs/name/outline journey through locald/CLI/MCP/desktop. This is the first user-visible vertical slice and should precede full language migration (`STRUCTURE-AND-MIGRATION.md:168-169`).
5. **K4 seven native lanes.** Run one coordinator per `clang`, `csharp`, `go`, `java`, `python`, `rust`, and `typescript`; each emits complete scoped facet changes through the shared compile contract. Merge language lanes independently, but merge the shared contract adapter only once. Every language must pass cold/session/subprocess parity and negative/partial coverage cases.
6. **K5 semantic arrangement lane.** Consume K4 deltas into exact/name/lexical, graph, source/doc, and top-k arrangements. Specialized Tantivy/Trustfall/Qdrant remain adapters until differential gates pass.
7. **K6 planner/execution lane.** Add work interning, reuse/update/rebuild plans, local reservations, cancellation, fences, and contention accounting. Merge after no-op suppression, broad-edit fallback, stale attempt rejection, and exact resource conservation.
8. **K7 replication lane.** Implement root negotiation, missing object transfer, receipts, offline branch/reconcile, and trust validation. Start with exact immutable memo/object transfer as specified (`STRUCTURE-AND-MIGRATION.md:193`).
9. **K8 remote lane.** Add warm traces, remote placement, prefetch, and pure hedges only after K6/K7. Promotion requires local baseline preservation, bounded speculative load, stale fencing, and network cost evidence.
10. **K9 vector/recursion lane.** Add versioned ANN overlays and SCC/fixed-point plans with exact fallback oracles. Keep approximate results explicitly recipe/recall bound.
11. **K10 native session lane.** Add persistent sessions one language at a time; retain subprocess fallback. Promotion requires cancellation/crash/memory isolation and cold/session equality.
12. **K11 physical optimization lane.** Apply column COW, dictionary reuse, compaction, SIMD/locality tuning only against frozen contracts and measured baselines. Never mix layout tuning with semantic schema changes in one merge.
13. **K12 integration lane.** Convert all surfaces/providers, switch one head writer, execute compatibility/restart/rollback drills, then delete legacy paths only after deletion gates below.

## Native-language matrix

The native lanes share manifest/session/process mechanics but retain language authority and interpretation. Each lane's packet must specify:

| Language | Required corpus dimensions | Reuse/invalidation proof | Required negative cases |
|---|---|---|---|
| Clang | headers, macros, source spans, generated/foreign declarations | toolchain/sysroot/flags/include graph; scoped source changes | missing header, macro drift, span/evidence mismatch |
| C# | Roslyn project, nullable/generics, XML docs, source spans | SDK/references/options/source/XML manifest; workspace reset | project/reference mismatch, malformed XML, unsupported type graph |
| Go | modules, build tags, GOOS/GOARCH, methods/interfaces | toolchain/module graph/build flags; package-wide effects | missing module, tag change, stale generated source |
| Java | JDK release, classpath, doclet/source set, UTF-16 spans | JDK/classpath/doclet/options/source manifest | release mismatch, missing classpath, doclet failure |
| Python | interpreter/toolchain, imports, dynamic/typed constructs | environment/import closure; conservative unknown-scope fallback | unavailable dependency, parse/type error, dynamic uncertainty |
| Rust | rustc/sysroot/features, macro/build scripts, editions | toolchain/sysroot/features/dependency/build manifest | feature drift, macro failure, generated source change |
| TypeScript | project references, module resolution, checker options, declaration files | project/options/module/source manifest; persistent checker reset | declaration mismatch, module resolution drift, checker invalidation |

Each language reports complete, partial, unavailable, unsupported, and captured-empty states distinctly. Unknown invalidation scope falls back to complete authority recompute; no agent may claim a narrow delta solely from a hash. Preserve mature native oracles and wire fixtures as required by `SEMANTICS-AND-LAWS.md:61-109`.

## Performance regression gates

K0 establishes the baseline distribution and resource envelope. Every later performance result records the same workload manifest and reports: authority work, changed logical rows/nodes, output fan-out, support rows, copied/hashed/serialized bytes, allocations, retained RAM, disk I/O/read amplification, remote bytes, wasted speculation, planner time, and p50/p95/p99 interaction latency (`STRUCTURE-AND-MIGRATION.md:205-218`).

Use gates in two layers:

* **Hard structural gates:** no unchanged full-root traversal on the optimized value-edit path; no rebuild beyond equal subscribed facets; no unbounded subscriber/delta chain; no duplicate valid native analysis; no remote dependency for locally promised operations.
* **Measured gates:** thresholds come from K0 baseline and product SLOs. Require confidence intervals or repeated-run distributions, warm/cold separation, memory/CPU caps, and workload-specific tolerances. A single faster benchmark cannot offset a p99 regression, higher write amplification, or increased retained pins.

Run performance comparisons at each relevant slice, with a fixed control cohort. Fail closed on unexplained regression; do not “normalize away” a change in result coverage or work. For remote/hedged paths include duplicate CPU/egress, hedge rate, cancellation delay, and local-primary latency. For native sessions include RSS, process lifetime, restart count, and cold fallback cost.

## Evidence ledger and escalation

The evidence ledger is the gate record, not a prose status page. Each row contains:

```text
id, K-slice, owner, reviewer, train_commit, contract_versions,
input_manifest_digest, oracle_version, commands, raw_artifacts,
semantic_result, crash_result, trust_result, performance_result,
thresholds, unsupported_scope, decision, supersedes, timestamp
```

Decision values are `pass`, `pass-with-declared-difference`, `blocked`, or `rollback`. `pass-with-declared-difference` requires a contract reference and product owner acceptance; it cannot cover identity, coverage, authority, durability, or freshness violations.

Escalate immediately when an agent finds: an unassigned semantic field; a new upward dependency; a canonical byte/hash change without version mapping; old/new roots diverging after equivalent edits; a crash state other than old/new coherent roots; remote output without authority/input proof; a native negative corpus regression; unexplained p95/p99 or memory regression; an unsafe lifetime/concurrency proof gap; or a deletion that would remove an old-reader path before the compatibility window ends. The integration lead pauses promotion, records the smallest reproducer, and assigns the relevant contract/oracle owner. Do not resolve by weakening the comparator or widening an error to `String`.

## Final deletion gates

Delete a legacy mechanism only when all applicable conditions are recorded in the ledger:

1. Every ownership-map row has a final owner, converted callers, and deletion commit (`STRUCTURE-AND-MIGRATION.md:101-157`).
2. New and old paths have passed shadow and dual-read coverage for all declared scopes, including unavailable/partial states and exact provenance.
3. The new path is the sole selected head writer; durable intents have a tested old-reader-compatible export or the compatibility window is formally closed.
4. Restart and fault injection cover every prepare/flush/head/notification/transfer/native boundary, with old-or-new coherent recovery.
5. All seven native matrices pass their cold/session/subprocess and negative corpora; unsupported scopes are explicit.
6. Specialized providers retained as extensions have root/recipe/coverage validation and no competing authority. Providers scheduled for deletion have equivalent differential evidence.
7. Local/offline journeys work without remote dependency wherever the contract promises local capability; remote-only labels and freshness states are visible.
8. Performance distributions meet K0-derived gates under the same resource envelope, with no unexplained retained-memory, write-amplification, remote-cost, or build-size regression.
9. GC/pin audit proves no live reader, lease, branch, published root, or compatibility reader references the old materialization.
10. Rollback drill succeeds from the last pre-deletion release, and the deletion is reversible until the documented retention deadline.

Only after these gates may wrappers, old writers, duplicate shadows, independent lifecycle systems, obsolete test scaffolds, and compatibility reexports be removed. The final K12 claim is “evidence gates passed for declared scope,” never “all regressions are impossible.”
