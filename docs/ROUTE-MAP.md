# Route Map

The whole codebase, one line per folder and per file, as of the 2026-08
restructure. Each crate section leads with a **folder table of contents**
(high-level), then the **file-level map**. `#` = crate root / routing entry.

- **BIG PICTURE** (dependency flow): `heart` (vocabulary) ← everything. The
  producers (`compiler/languages`) turn source into IR (`ir/model`); `ir/vcs`
  patches/serves it; `index` stores+catalogs it and serves it over HTTP;
  `registry` answers graph+vector queries over it; `nudox-engine` drives it
  all from a local GUI; `gui` is the window on top.

## Workspace layout (top-level crates)

```
workspace/
  heart/               shared vocabulary: query algebra, identity, cache, telemetry, client, sync seam, cost
  ir/model/            nudox-ir: the light IR data model everyone reads
  ir/vcs/              ir-vcs: libpijul-linked patch engine / archive / diff / semver / sync / protocol
  transport/           the one shared iroh/bao content-transfer plane
  index/               the versioned catalog crate (+ server, ingest, pack, storage, search)
  registry/            graph + vector serving layer
  compiler/languages/  nudox-languages: producer contract + 7 language producers (rust/go/java/csharp/clang/typescript/python)
  compiler/sandbox/    smolvm compile isolation (cage/budget/job/seal)
  nudox-engine/        the whole local-first GUI backend: store, graph, engine, embed, mcp
  gui/                 lindsey (own lockfile, gpui-based) — not a workspace member
```

---

## heart — shared vocabulary

```
lib.rs                     — crate root: shared-vocabulary re-exports
access/federation.rs       — layered source federation (definitive base + overlays)
access/mod.rs              — source federation and provider identity
access/source.rs           — configured provider instance and its stable id
availability.rs -> content/availability.rs — dual-plane availability enums
cache/disk.rs              — node-local disk CAS (blake3-named files)
cache/error.rs             — content-addressed store (CAS) error types
cache/jitter.rs            — TTL jitter to desynchronize expiries
cache/memory.rs            — in-process memory CAS
cache/mod.rs               — shared cache tier and content-addressed storage
cache/single_flight.rs     — single-flight request coalescing
cache/stampede.rs          — stampede-resistant cache (XFetch + coalescing)
cache/tiered.rs            — L1/L2/L3 tiered CAS
client/authz.rs            — capability witnesses and tenant identity
client/dto.rs              — request/response wire DTOs
client/http.rs             — connecting reqwest client for nudox-serve
client/mod.rs              — client wire vocabulary (DTOs, authz, queries)
client/query.rs            — transport-free search request vocabulary
connection.rs              — Cold/Live typestate and Connect transition
content/object_pack.rs     — ObjectPack container vocabulary
cost.rs                    — integration-test cost measurement
cursor.rs -> query/cursor.rs — opaque keyset-pagination cursors
deployment.rs              — two deployment shapes and trusted remotes
ecosystem.rs               — Language enum, editions, toolchains
egress.rs                  — internal-mode egress guard predicate
error/connect.rs           — connection-failure vocabulary
error/failure.rs           — indexing phases, failures, resolution states
error/mod.rs               — backend kinds, retry classification, failures
examples/nudox_ping.rs     — smoke-test connecting client example
health.rs                  — readiness probes shared across stores
identity/derive.rs         — deterministic package-id byte-framing law
identity/id.rs             — type-tagged UUID identifier constructor
identity/mod.rs            — package/symbol identity re-exports
identity/namespace.rs      — UUIDv5 namespaces for id derivation
identity/package.rs        — package id, versions, registry origins
identity/symbol.rs         — symbol id and entry URI
object_pack.rs -> content/object_pack.rs — ObjectPack container vocabulary
package/coordinates.rs     — package (origin, name, version) address
package/mod.rs             — package coordinates, hits, name validation
page.rs -> query/page.rs   — keyset-pagination helper
progress.rs                — phased progress reporting for indexing jobs
query.rs -> query/mod.rs   — the one query algebra
query/cursor.rs            — opaque keyset-pagination cursors
query/mod.rs               — the one query algebra
query/page.rs              — keyset-pagination helper
query/score.rs             — score and scored-value wrapper
query/search.rs            — page of scored hits
score.rs -> query/score.rs — score and scored-value wrapper
search.rs -> query/search.rs — page of scored hits
sink.rs                    — remote-upload delivery extension trait
stream.rs                  — typed NDJSON streaming envelope
symbol.rs                  — canonical symbol record
sync.rs                    — generic content-addressed sync seam
telemetry/config.rs        — telemetry config resolution
telemetry/logging.rs       — fmt layer and level filter
telemetry/logs_otel.rs     — OTLP log provider
telemetry/metrics_bridge.rs — Prometheus + OTLP metrics bridge
telemetry/metrics_otel.rs  — OTLP meter provider
telemetry/mod.rs           — unified observability init
telemetry/pyroscope.rs     — continuous CPU profiling
telemetry/resource.rs      — OTel resource attributes
telemetry/tracing_otel.rs  — OTLP tracer provider
tenant.rs                  — tenant ownership and visibility
version.rs                 — versioned payloads and version selection
tests/access_control.rs    — federation vocabulary tests
tests/cache_roundtrip.rs   — CAS put/get/invalidate tests
tests/cache_stampede.rs    — stampede cache behaviour tests
tests/checkout_completeness.rs — git-tracked module completeness tests
tests/job_key.rs           — JobKey layout stability test
tests/query_serde.rs       — query algebra serde golden tests
tests/refinements.rs       — refinement newtype tests
tests/shared_types.rs      — cross-cutting primitive tests
```

## transport — shared iroh/bao content-transfer

```
lib.rs       — shared iroh/bao content-transfer plane: blob fetch + bao ranges
announce.rs  — control-ALPN announcement/ack push path for federation sync
bao.rs       — bao outboards for verified range streaming
blob.rs      — opaque content-addressed blob transfer over iroh-blobs
endpoint.rs  — shared iroh endpoint builder (minimal preset + key + ALPNs)
frame.rs     — length-prefixed postcard framing over a QUIC bi-stream
```

## ir/model — nudox-ir (light IR data model)

```
src/lib.rs                 — crate root: IR module tree + prelude
src/apply.rs               — materialized IR table: IntroId→Entry plus parent/children edges
src/body.rs                — merged tree-sitter + oracle body facts per entry
src/change/mod.rs          — content-addressed identity vocabulary and format version
src/change/encode.rs       — canonical little-endian length-prefixed encoding helpers
src/change/hash.rs         — domain-separated BLAKE3 identity newtypes (ContentBlake3, IntroId)
src/change/ids.rs          — non-hash ids: ecosystem, package name, lineage, StableRef
src/codec.rs               — canonical postcard envelope codec for both IR planes
src/content/mod.rs         — deterministic entry content hash (third identity layer)
src/content/tests.rs       — content-hash sensitivity, determinism, ref-lowering tests
src/continuity/mod.rs      — σ resolution: which new declarations continue prior ones
src/continuity/tests.rs    — continuity tests: rename, move, signature-edit matching
src/entry/mod.rs           — Entry node: symbol + tree edges + kind payload
src/entry/location.rs      — typed source location (file, bytes, line/column, reason)
src/entry/node.rs          — structural parent/children edges for one entry
src/entry/symbol.rs        — symbol metadata plus attribute/cfg/doc-link types
src/entry/typed.rs         — kind-typed view over an Entry
src/foreign.rs             — self-describing cross-package references and resolvers
src/id/mod.rs              — re-exports PackageId/UniqueId identity types
src/id/package.rs          — cheap-clone path-based package identifier
src/id/unique.rs           — package-scoped globally-unique entry identifier
src/index.rs               — EntryIndex/Ref, the one reference type across lifecycle
src/intro.rs               — deterministic IntroId minting with collision disambiguation
src/kind.rs                — Kind enum and frozen KindDiscriminant wire tags
src/kinds/mod.rs           — re-exports every kind payload
src/kinds/alias.rs         — type-alias / associated-type declaration kind
src/kinds/const_.rs        — compile-time constant declaration kind
src/kinds/facts.rs         — auto-trait facts, TriState, Sealed vocabulary
src/kinds/function.rs      — function kind plus Receiver/FnModifier enums
src/kinds/generics.rs      — generic params, where-clauses, lifetime_label helper
src/kinds/impl_.rs         — trait/inherent impl block kind
src/kinds/param.rs         — parameter kind plus ParamAttribute modifiers
src/kinds/record.rs        — record/field kinds with forms, keys, attributes
src/kinds/reexport.rs      — marker kind for public-alias entries
src/kinds/static_.rs       — static variable declaration kind
src/kinds/sum.rs           — enum/variant algebraic sum type kinds
src/kinds/trait_.rs        — trait kind with TraitFlags modifiers
src/kinds/ty.rs            — cross-language type-expression lattice
src/lower.rs               — flat, order-independent production API for IrPackage
src/manifest/mod.rs        — generation stamp, blob manifest, and outbox
src/manifest/tests.rs      — generation-stamp content-sensitivity and determinism tests
src/package/mod.rs         — built serializable package IR tree
src/package/builder.rs     — scoped closure-based entry tree construction
src/package/info.rs        — package id plus export/import index tables
src/package/seal.rs        — three-phase sealing: mint IntroIds, lower refs, materialize
src/package/tests.rs       — package build/wire tests walking every kind
src/reflect.rs             — exported/moniker read-model reflections over a table
src/registry/mod.rs        — lazy-loading registry resolving UniqueIds to entries
src/registry/resolver.rs   — resolver supplying entry data to the registry
src/registry/state.rs      — internal registry index allocation and lazy loading
src/registry/tests.rs      — registry build + resolve-by-id tests
src/relation.rs            — extrinsic directed graph edges with dedupe/merge
src/render.rs              — human-source-text rendering of Type
src/skeleton.rs            — structural byte fingerprints for collision disambiguation
src/test_helpers.rs        — shared test helpers (sym, entry, node builders)
src/view.rs                — unified read model: table + bodies + occurrences
src/visitor.rs             — crate-private Visitor derive walking every Ref
src/vocab.rs               — Confidence, ReferenceKind, RelSpan, Occurrence
tests/collision_recovery.rs — pins that two declarations never silently merge
tests/foreign_refs.rs      — pins cross-package refs surviving seal as named Foreign
tests/golden.rs            — golden byte-pin harness freezing IR wire preimages
tests/resolver.rs          — ExampleResolver over an in-memory VFS
```

## ir/vcs — the libpijul-linked patch engine / serve / diff / semver / transfer

```
lib.rs                     — crate root + re-exports (VcsError, F1Error, SealError aliases)
error.rs                   — unified VCS error type
ascii.rs                   — ASCII encode/decode for typeref/typeexpr
checkout.rs                — MaterializedIndex: incremental materialize + checkout
checkout_tests.rs          — tests for incremental materialize/checkout
checkpoint.rs              — demand-based checkpoint serving
checkpoint_tests.rs        — tests for checkpoint serving
lower.rs                   — wire→semantic type lowering
record_shape_tests.rs      — F1 field-aware diff acceptance tests
ref_probes.rs              — reference model adversarial probes
refs.rs                    — typed branch/tag/version reference model
serialize.rs               — working-tree naming and link wire form
serve_cache.rs             — hot cache for sealed serve archives
session.rs                 — RecordingSession incremental recording
size_tests.rs              — repo size and seal-overhead measurements
stream.rs                  — host-side stream driver
subst.rs                   — σ substitution cascade
vcs_tests.rs               — gate tests for the VCS
vcs_types.rs               — VCS-layer identity types
version.rs                 — typed version identity
wire.rs                    — wire types for F1/archive
archive/error.rs           — archive open/read error type
archive/header.rs          — POD header structs for the archive format
archive/index.rs           — in-memory index builders and zero-copy readers
archive/mod.rs             — archive module: header/index/seal/section/view
archive/open.rs            — archive opening and validation
archive/seal.rs            — deterministic archive sealing
archive/section.rs         — section build and read helpers
archive/tests.rs           — gate test stub
archive/view.rs            — zero-copy archive view
diff/apply.rs              — apply PackageDelta to PayloadTable
diff/delta.rs              — PackageDelta and PartialDelta types
diff/diff.rs               — matcher-free structural diff
diff/ir_op.rs              — IrOp structural-delta operation
diff/mod.rs                — structural delta module
diff/tests.rs              — diff adversarial tests
f1/mod.rs                  — NdIrF1 working-copy format serializer/parser
f1/tests.rs                — F1 round-trip and strict-parser tests
protocol/error.rs          — protocol error type
protocol/frame.rs          — stream frame types
protocol/io.rs             — length-prefixed frame I/O
protocol/mod.rs            — guest→host streaming protocol
protocol/receiver.rs       — host-side protocol state machine
protocol/sink.rs           — producer-side protocol state machine
protocol/tests.rs          — protocol integration tests
repo/delta_tests.rs        — O(delta) materialization tests
repo/mod.rs                — IrRepository: libpijul-backed versioning
semver/classify.rs         — classify: two surfaces → ApiReport
semver/config.rs           — ConfigId content address
semver/mod.rs              — semver module
semver/report.rs           — semver report types
semver/surface.rs          — ApiSurface projection + hash classes
semver/tests.rs            — semver end-to-end tests
semver/packs/a.rs          — pack A CSC-parity lints
semver/packs/mod.rs        — law pack module
semver/packs/tests.rs      — pack A lint tests
sync/mem_io.rs             — in-memory ContentIo test double
sync/mod.rs                — iroh sync module
sync/repo_glue.rs          — FsChangeIo + RepoApplyHook glue
sync/tests_repo_glue.rs    — repo glue integration tests
sync/tests.rs              — end-to-end sync tests
sync/transport.rs          — iroh sync transport
sync/types.rs              — sync core types
```

---

## registry — graph + vector serving layer

**Folders.** `graph/` answers GraphQL-shaped queries over IR via Trustfall.
`vector/` is the whole vector-search plane, split by responsibility:
`vector/core/` = pure vocabulary (models, keys, recipes, traits — no impl),
`vector/embed/` = the embedding runtime (ONNX embedder, scheduler, stage,
weights), `vector/local/` = the embedded qdrant-edge store, `vector/remote/`
= the qdrant-client / Voyage remote path. `vector/{cache,gate}.rs` sit at the
plane facade.

```
lib.rs                     — crate root: graph + vector re-exports
graph/mod.rs               — re-exports reverse index + trustfall adapter
graph/reverse_index.rs     — disposable reverse-position index over IrView
graph/trustfall_adapter.rs — Trustfall adapter, neighbour methods, execute stub
vector/mod.rs              — vector plane facade; re-exports core vocabulary
vector/cache.rs            — content-addressed embedding cache (moka-backed)
vector/gate.rs             — SemanticGate capability token for semantic search
vector/core/mod.rs         — pure-plane facade; re-exports + error aliases
vector/core/admission.rs   — hot-set admission, HitEma decay
vector/core/embed.rs       — EmbedRole, Embedder trait, runtime info
vector/core/embedding.rs   — branded Embedding value + error type
vector/core/fusion.rs      — reciprocal-rank fusion of ranked lists
vector/core/key.rs         — embed-key + tool-digest derivation
vector/core/license.rs     — model license deny-list + assert_licensed
vector/core/model.rs       — sealed embedding-model brands
vector/core/quant.rs       — quantization ladder + rescore policy
vector/core/recipe.rs      — frozen EmbedText v2 text builder
vector/core/routing.rs     — query routing table decision logic
vector/core/shard.rs       — edgepack shard schema + cache key
vector/core/store.rs       — VectorStore trait, points, filters, hits
vector/embed/mod.rs        — local embedding runtime facade + constants
vector/embed/gate.rs       — EmbedGate concurrency cap + idle unload
vector/embed/mock.rs       — deterministic offline MockEmbedder
vector/embed/runtime.rs    — FastembedOrt ONNX embedder (feature onnx)
vector/embed/scheduler.rs  — EmbedScheduler priority queue + batch coalescing
vector/embed/stage.rs      — EmbedStage durable-vector write path
vector/embed/tokens.rs     — HfTokenCounter over tokenizer.json
vector/embed/weights.rs    — pinned weights artifact policy + sha256 verify
vector/local/mod.rs        — local plane facade + error helpers
vector/local/actor.rs      — single-writer Edge shard actor
vector/local/compact.rs    — automatic compaction policy
vector/local/depshard.rs   — dep-shard install/upgrade/evict flow
vector/local/fanout.rs     — cross-shard fan-out + raw-score merge
vector/local/hotset.rs     — hot-set manager + install plan diff
vector/local/lock.rs       — advisory multi-window file lock
vector/local/pack.rs       — shard artifact tar.zst pack/unpack
vector/local/shard.rs      — schema-validated shard open-or-create
vector/local/store.rs      — LocalShardStore VectorStore impl over Edge
vector/remote/mod.rs       — remote plane facade + re-exports
vector/remote/hedged.rs    — hedged local+remote search, RRF merge
vector/remote/rerank.rs    — Reranker trait + Http/Voyage impls
vector/remote/store.rs     — RemoteStore VectorStore over Qdrant
vector/remote/voyage.rs    — VoyageEmbedder REST embedder
tests/vector/common/mod.rs — shared local-plane fixtures + builders
tests/vector/support/mod.rs— offline doubles (traces/CAS/store/counter)
tests/vector/store_shard.rs — store/actor/shard/lock integration
tests/vector/pack_unpack.rs — pack→unpack→load roundtrip + hostile inputs
tests/vector/depshard_install.rs — dep-shard install fallback ladder
tests/vector/fanout_hotset.rs — fan-out merge + hot-set planning
tests/vector/gate_tests.rs — EmbedGate lifecycle (paused clock)
tests/vector/scheduler_tests.rs — scheduler priority/coalesce/cancel
tests/vector/stage_tests.rs — EmbedStage acceptance (offline)
tests/vector/weights_tests.rs — weights sha verify + missing-weights
tests/vector/onnx_live.rs  — live-model ONNX tests (ignored)
tests/vector/engine_relevance.rs — semantic relevance through nudox-engine
tests/vector/adversarial_depshard.rs — depshard adversarial cases
tests/vector/adversarial_durability.rs — kill-9/schema/dimension attacks
tests/vector/adversarial_fanout.rs — partial-failure + merge attacks
tests/vector/adversarial_hotset_lock_compact.rs — hotset/lock/compact attacks
tests/vector/adversarial_pack.rs — pack/unpack hardening
tests/vector/adversarial_scheduler_gate.rs — scheduler/gate adversarial
tests/vector/adversarial_stage.rs — EmbedStage adversarial
```

## compiler/sandbox — smolvm compile isolation

**Folders.** `cage/` = the isolation contract (`Cage` trait, `Policy`,
`DevPassthrough`, `SmolvmCage`). `budget/` = the capability budget vocabulary
and the per-package override/profiles. `job/` = the acquire→seal typestate,
worker pool, cancel token. `seal/` = turning a command + budget into a sealed
job. `vm/` = the VM facade (`VmRuntime`/`VmHandle`) + the test fake.
`backend/` = the real smolvm runtime + supervisor run loop. `probe/`,
`observer/`, `toolchains/`, `golden/`, `error/` are the supporting planes.

```
lib.rs                     — crate root; re-exports grouped modules; boot_check probe
spec.rs                    — isolation vocabulary: Env, Mounts, Spec, Output, ProcessEnd
node.rs                    — stable per-node identity (NodeId)
cgroup.rs                  — cgroup v2 controller for scheduling fairness
error/mod.rs               — SandboxError + CageError + KillReason + to_io_error
budget/mod.rs              — CapabilityBudget, FsGrant, NetGrant, EgressAllowlist, ThreatTier
budget/limits.rs           — Limits, LimitOverride, Network + serde
budget/overrides.rs        — OverrideTable, SandboxKey per-package/profile overlays
budget/profiles.rs         — ProducerProfile limit presets
cage/mod.rs                — Cage trait, CageCaps, CageId, Policy, run_sealed
cage/dev.rs                — DevPassthrough (rlimits + fairness cgroup)
cage/smolvm.rs             — SmolvmCage, GoldenPool, budget→VmConfig projection fns
job/mod.rs                 — acquire→seal typestate; VmForge witness
job/cancel.rs              — CancelToken cooperative cancellation
job/worker.rs              — WorkerPool, JobRequest/Response, lower_once
seal/mod.rs                — SealedCommand, SealedInput, Sealer
vm/mod.rs                  — VM facade types, VmRuntime/VmHandle/VmError
vm/fake.rs                 — FakeVmRuntime/FakeVmHandle recording runtime
backend/mod.rs             — backend module index (supervisor + smolvm)
backend/supervisor.rs      — shared run loop (spawn/cgroup/wall/output caps)
backend/smolvm.rs          — SmolvmRuntime + RootfsStore over vendored smolvm
toolchains.rs              — ToolchainSet resolved toolchain store paths
toolchains/images.rs       — OCI image types, ImageDigest, ToolchainPlane digests
golden/mod.rs              — GoldenContentIo (ContentIo over golden snapshots)
probe/mod.rs               — host virtualization probe + isolation policy
observer/mod.rs            — ForgeObserver metrics boundary
tests/cage_pool.rs         — integration: worker pool + cage vocabulary
tests/escape.rs            — integration: escape tests + VM projection asserts
tests/typestate.rs         — integration: typestate + threat-tier invariants
```

## Pending crates (agents in flight / queued)

- `compiler/languages` — producer contract + rust/go/java/csharp/clang/typescript/python
- `index` — the versioned catalog crate (+ server, ingest, pack, storage, search)
- `nudox-engine` — the whole local-first GUI backend (store/graph/engine/embed/mcp)
- `gui` — lindsey

## Rename ledger

| crate | old → new |
|---|---|
| heart | `ConnectError→Error`, `StoreError→Error`, `CursorError→Error`, `WireError→Error`, `SyncError→Error`, `ClientError→Error`, `QueryError→Error`, `CoordinateError→Error` (all keep `pub use … as OldName` at crate root) |
| transport | `BaoError→Error` (alias `transport::bao::BaoError` kept), `IoErrorString→ErrorText` |
| nudox-ir | `CodecError→Error`, `LoweringError→Error`, `OutboxError→Error` (aliases kept); `test_helpers::n→node` |
| ir-vcs | `VcsError→Error`, `ArchiveError→Error`, `SealError→Error`, `StreamError→Error`, `ApplyError→Error`, `AsciiError→Error`, `F1Error→Error`, `SyncGlueError→Error`; `hash_to_hex_pub→hash_to_hex` |
| registry | `GraphQueryError→Error` (graph), `StoreError→Error` (core::store, alias kept), `EmbedError→Error` (core::embedding, alias kept), `LicenseError→Error` (core::license, alias kept) |
| sandbox | structural only: flat root → `budget/ cage/ job/ seal/ vm/ backend/ golden/ probe/ observer/ error/` |
