# Capability ownership after the complete cutover

Status: executable-source inventory, 2026-09-12. Scope excludes
`server/index/turso` as requested. “Closed” means a current product owner and
an executable implementation are present; it does not mean the old tree is
safe to delete before the journeys in `docs/operations/complete-cutover.md`
pass. “Partial” names the exact remainder. “Missing” means the legacy tree is
still the only implementation in this checkout.

The current architecture has one authority spine:

`version -> store -> flow/replication -> execution -> semantic/compile/library -> engine -> local-service -> apps`

Frontends and extensions are leaves. They may produce or accelerate a result,
but they cannot select a workspace head. `backend-store` owns immutable bytes
and the authoritative `WorkspaceRoot`; `backend-engine` is the only product
composition and mutation owner; `backend-local-service` is the only local
process owner; `backend-library` owns portable commands, queries, and
`ViewRoot`; `backend-client` owns a client session. This replaces the old
pattern in which compiler publication, index publication, catalog state,
workflow state, GUI state, and each backend could all appear to own progress.

`backend-control` is deliberately absent from this spine. It lives at
`tools/control`, has no runtime product dependent, and operates the repository
agent/cutover ledger. Treating repository promotion policy as tooling instead
of a twelfth product core removes one package and two dependency edges from
the runtime graph without weakening any product capability.

## Complete legacy-crate ledger

The inventory contains **56** old manifests: 15 compiler, 12 heart, 10
interface, and 19 server manifests. The older
`docs/architecture/crate-map.json` says 54 sources because it includes
`agent-control-dispatch` from `tools/control`, which is neither a legacy
manifest nor a runtime product package, while it
omits `nudox-gui2`, `server-index-explorer`, and `server-index-registry`.
Those three omissions are included below.

| Old crate | New owner | State | Exact disposition |
|---|---|---:|---|
| `compiler-application` | `backend-engine`, `backend-local-service` | Partial | Compiler orchestration and durable publication belong to the one engine owner. Local filesystem syntax ingestion and generic registry artifact acquisition are composed; native semantic helpers and command-driven compilation of acquired packages remain. Delete the crate after J2/J3/J4. |
| `compiler-driver` | `backend-compile`, `frontends/*` | Partial | Shared manifests, sessions, supervision, cancellation, coverage, and admission live in `compile`; authority-specific construction lives in seven leaves. Delete after every native authority has a shipped helper and negative process oracle. |
| `compiler-ir` | `backend-semantic` | Partial | Semantic relations and versioned facts replace mandatory whole-image ownership. Keep old binary-image readers only as migration/oracle code until a shared corpus proves fact parity. |
| `compiler-ir-vocabulary` | `backend-semantic` | Partial | Dense semantic coordinates belong beside their relation codecs. Port any fact kind found by the corpus diff; do not recreate a vocabulary crate. |
| `compiler-languages` | `backend-compile` | Superseded | Delete the umbrella. The closed `SourceLanguage`/authority contract replaces all-language imports. |
| `compiler-languages-clang` | `backend-frontend-clang` | Partial | Tree-sitter syntax is live. Direct libclang semantic authority is not closed until an explicit helper is shipped and exercised. |
| `compiler-languages-csharp` | `backend-frontend-csharp` | Partial | Syntax is live; Roslyn output requires an external helper. Port/repackage the oracle as the leaf’s explicit helper. |
| `compiler-languages-go` | `backend-frontend-go` | Partial | Syntax is live; `go/packages` semantic output requires an external helper. |
| `compiler-languages-java` | `backend-frontend-java` | Partial | Syntax is live; javac/doclet semantic output requires an external helper. |
| `compiler-languages-python` | `backend-frontend-python` | Partial | Syntax is live; Ruff/pyrefly semantic parity needs a shipped authority path and corpus proof. |
| `compiler-languages-rust` | `backend-frontend-rust` | Partial | Syntax is live; HIR/rust-analyzer semantic parity needs a shipped authority path and corpus proof. |
| `compiler-languages-typescript` | `backend-frontend-typescript` | Partial | Syntax is live; OXC/checker semantic parity needs a shipped authority path and corpus proof. |
| `compiler-publication` | `backend-semantic`, `backend-store`, `backend-engine` | Partial | Semantic produces a complete fact transition, store admits it atomically, engine fences composition. Keep image import only until J3 proves equivalence. |
| `compiler-registry` | `backend-compile`, `backend-local-service` | Partial | Pure authority descriptions are closed. Active registration is explicit in local-service’s seven-member `FrontendSet`; helper discovery/packaging remains. |
| `compiler-vocabulary` | `backend-compile`, `backend-semantic` | Partial | Authority/discovery policy belongs in compile; portable provenance and facts belong in semantic. Corpus parity is the deletion gate. |
| `heart-adaptive` | `backend-execution` | Closed | One typed placement/reservation/hedge policy replaces a separate local/remote chooser. Delete after J8 regression evidence. |
| `heart-frame` | `backend-version`, `backend-replication` | Closed | Validated identities/frame bounds are values; replication owns negotiation and transfer framing. |
| `heart-hydration` | `backend-store`, `backend-replication` | Closed | Store owns local admission/pins; replication owns missing-object transfer. There is no second hydration progress authority. |
| `heart-identity` | `backend-version` | Closed | Domain marker types and checked constructors make object, relation, root, attempt, cursor, and recipe substitution ill-typed. |
| `heart-memory` | `backend-store`, `backend-flow` | Closed | Immutable ownership/pinning belongs to store; bounded mutable operator scratch belongs to flow. |
| `heart-object` | `backend-version`, `backend-store` | Closed | Logical descriptors are values in version; physical admission and residency live in store. |
| `heart-object-pack` | `backend-store` | Closed | One canonical object/tree codec and durable CAS replace a separate pack lifecycle. |
| `heart-observe` | `backend-version`, `backend-execution` | Closed | Observation values sit below the scheduler; execution records resource/work observations. No observation owner crate remains. |
| `heart-root` | `backend-store` | Closed | One persistent relation/tree root with exact path-copy delta and atomic selected head replaces parallel root notions. |
| `heart-schema` | `backend-version` | Closed | Canonical schema/version/witness types are shared below storage and transport. |
| `heart-telemetry` | composition root (`backend-local-service`/apps) | Partial | Lower layers emit observations, but production exporter/setup parity must be demonstrated by J9 before deletion. |
| `heart-view` | `backend-store`, `backend-library` | Closed | A store pin protects physical objects; a library `ViewRoot` is a bounded derived product projection. The types prevent confusing the two. |
| `interface-cli` | `backend-cli`, `backend-client`, `backend-runtime` | Closed for local product | The CLI uses the shared typed session and zero-setup endpoint discovery. Broader old commands remain subject to the journey parity table. |
| `interface-core` | `backend-library`, `backend-engine` | Closed | One portable command/reply algebra and one engine interpreter replace a competing service facade. |
| `interface-documents` | `backend-library` | Closed for syntax view | Version-bound document/outline/name recipes and bounded rows are live. Native semantic richness is gated by J3. |
| `interface-gui` | `backend-desktop`, `backend-client`, `backend-runtime` | Partial | A launchable GPUI binary and typed live subscription exist. Account/MCP/remote product features from the old shell need an explicit keep/drop decision and journey. |
| `nudox-gui2` | `backend-desktop`, `backend-local-service`, `backend-library` | Partial | Its package/docs/source mental model is implemented by the native desktop. Live registry metadata and remote package acquisition are missing; delete only after J4 or an explicit scope decision. |
| `interface-identity` | `backend-library`, `backend-semantic`, `backend-version` | Closed | Product selections, semantic entity keys, and physical identities are different types with no unchecked interchange. |
| `interface-library` | `backend-library`, `backend-engine` | Closed for local product | Durable intent/query recipes are interpreted by the engine. Registry-backed package history remains J4. |
| `interface-mcp` | `backend-mcp`, `backend-client`, `backend-runtime` | Closed for current commands | JSON-RPC framing and shared typed queries are live, including Trustfall view queries. Golden parity over the full corpus remains J7. |
| `interface-protocol` | `backend-library`, `backend-client`, `backend-replication` | Closed | Product DTOs, client session state, and transport mechanics have separate owners; callers cannot mint a trusted basis without daemon admission. |
| `interface-search` | `backend-library`, `backend-semantic` | Partial | Deterministic in-memory name/search recipes exist. Real Tantivy and composed embedding/Qdrant product journeys are missing. |
| `server-index-acquire` | `backend-compile`, `backend-local-service`, `backend-replication` | Missing for remote packages | Local bounded filesystem discovery exists. HTTP/archive acquisition, provenance, retry, and immutable remote input transfer need a product owner and J4. |
| `server-index-build` | `backend-flow`, `backend-semantic`, `backend-local-service` | Partial | Shared retained arrangements and local view construction replace a private build lifecycle. Real lexical/vector materializers remain J5/J6. |
| `server-index-catalog` | `backend-library`, `backend-store`, `backend-engine`, `backend-extension-turso` | Closed for derived projection | Canonical package/source/view relations are versioned store data; Turso is a rebuildable, root-fenced SQL projection. Feed checkpoints are not covered and remain J4. |
| `server-index-core` | `backend-flow`, `backend-semantic` | Closed for current relations | Generic retained arrangements and semantic relation recipes replace snapshot-specific core ownership. Backend parity remains at provider leaves. |
| `server-index-explorer` | `backend-library`, `backend-extension-tantivy` | Partial | Bounded deterministic paging/ranking contracts exist; durable Tantivy projection/query is missing. |
| `server-index-graph-vector` | `backend-flow`, `backend-semantic`, `backend-extension-qdrant` | Partial | Exact graph and ANN overlay/delta contracts and a bounded Qdrant HTTP provider exist. A real embedding/model producer and product composition do not. |
| `server-index-ingest` | `backend-compile`, `backend-semantic`, `backend-local-service` | Partial | One scan produces a content-versioned source relation and exact delta with bounded admission. Remote feeds and full native facts remain open. |
| `server-index-publish` | `backend-store`, `backend-flow`, `backend-engine` | Closed for canonical state | Store CAS plus root-bound derived outputs replace an independent index snapshot authority. Specialized provider publication remains J5/J6. |
| `server-index-qdrant` | `backend-extension-qdrant` | Partial | `QdrantHttpClient` validates collection dimension/metric, performs bounded authenticated retry, strongly ordered writes/deletes, verifies the projection, and yields an admitted `AnnSource`; binding, coverage, quality, tombstone, overlay, and rerank types remain enforced. Model acquisition/inference, product composition, and a real-Qdrant process oracle remain J6. |
| `server-index-registry` | `backend-engine`, `backend-local-service` | Partial | One generic engine effect now owns authenticated bounded HTTP fetches, content-checked immutable archives, `FeedCursor<RemoteRegistry, CanonicalFeedV1>`, a synced intent/receipt journal, atomic cursor/publication recovery, and explicit offline/unavailable/retry outcomes. Native ecosystem API translators, signature/transparency verification, and the full J4 external fixture matrix remain. |
| `server-index-retrieval` | `backend-library`, `backend-semantic`, extensions | Partial | One typed query surface composes local exact/name/document/graph reads. Concrete Tantivy/Qdrant retrieval is absent. |
| `server-index-routing` | `backend-execution`, `backend-replication` | Closed for pure work | Typed reservations, attempts, cancellation, fencing, transfer closure, and retry replace a query-specific router. Provider-specific routing awaits J5/J6. |
| `server-index-tantivy` | `backend-extension-tantivy` | Missing concrete provider | Strong root/read-manifest/coverage/cursor and exact overlay types exist, but there is no `tantivy` dependency or durable Tantivy implementation. |
| `server-index-trustfall` | `backend-extension-trustfall`, `backend-mcp` | Closed for `ViewRoot` | The pinned async Trustfall dependency, `execute_query_async`, immutable-view adapter, cancellation, and MCP exposure are live. J7 must compare old graph answers. |
| `server-index-vocabulary` | `backend-semantic`, `backend-version` | Closed | Semantic relation schemas own meaning; version owns shared identities, coverage, and root bindings. |
| `server-journal` | `backend-store`, `backend-engine` | Closed | Hash-chained durable publication/effect journals, checkpoints, compaction, repair, and typed receipts replace the standalone journal. |
| `server-operation` | `backend-flow`, `backend-engine` | Closed for current operations | Lending batches/arrangements and engine effects replace a cross-layer operation crate. Remaining capabilities enter as typed recipes/providers. |
| `server-runtime` | `backend-execution`, `backend-runtime` | Closed | Execution owns bounded work, resources, attempts, cancellation, and wakeups; runtime only discovers/spawns the local product. |
| `server-workflow` | `backend-store`, `backend-execution`, `backend-engine` | Closed | Durable intent/effect receipts, physical retry/cancel, and domain interpretation are distinct typed stages under one engine owner. |

## Capability owners and forbidden dependencies

| Capability | Sole authoritative owner | Leaf/provider contract | Dependency that must stay impossible |
|---|---|---|---|
| Identity, schema, coverage | `backend-version` | Typed IDs, canonical encodings, `CoverageWitness` | No provider-defined canonical ID or stringly root |
| Immutable bytes and selected root | `backend-store` | Pins, leases, relation handles, publication authority | No app/extension mutation of a head |
| Retained incremental computation | `backend-flow` | Arrangements, exact deltas, bounded cursors | No backend-private snapshot lifecycle |
| Transfer and wire | `backend-replication` | Negotiated frames, closure transfer, certificates | No product semantics or head selection in transport |
| Work placement | `backend-execution` | Typed budget/reservation/attempt/result | No second scheduler in compiler or query code |
| Semantic facts | `backend-semantic` | Relations and recipes | No frontend-owned durable semantic root |
| Compilation | `backend-compile` + one frontend leaf | Authority manifest/session/evidence | No umbrella importing every native frontend |
| Product commands and views | `backend-library` | `Command`, `Query`, `ViewRoot`, cursor | No CLI/MCP/desktop-specific semantics |
| Product mutation/composition | `backend-engine` | Typed intent and admitted effects | No extension or client compare-and-set |
| Local process | `backend-local-service` | Owner service and Unix protocol | No GUI-owned embedded mutation authority |
| Specialized search/query | extension leaf | Root-bound provider trait and checked result | No provider identity promoted to semantic truth |

The missing provider implementations do not justify restoring the old owner
graph. They should enter behind the existing leaf traits, receive immutable
root-bound inputs, and return coverage-bearing results. The engine remains the
only code allowed to publish their admitted effect.
