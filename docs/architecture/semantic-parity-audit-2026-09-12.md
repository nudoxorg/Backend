# Whole-system parity audit — 2026-09-12

This is an independent, source-backed comparison of the active cutover tree
with the deleted implementation and available historical workspaces. Its
purpose is to prevent a smaller structural implementation from being presented
as a replacement for the removed compiler and product behavior.

Machine-readable companions:

- [Audit data](semantic-parity-audit-2026-09-12.json)
- [Source and fixture inventory](semantic-parity-audit-2026-09-12.inventory.json)

## Semantic-plane cutover checkpoint — 2026-09-13

This checkpoint supersedes older status statements below while retaining them
as an audit trail. The code now has one semantic authority route:
`compiler-language-*` authority output → canonical `compiler-ir` → immutable
`compiler-publication` → admitted product semantic relation. Tree-sitter is
confined to structural parsing under `frontends/*` and
`crates/compile/src/syntax.rs`. The executable boundary law rejects generic
`ProductProjection`, `product_output_bytes`, and `ViewRoot` reconstruction in
the local replication, worker, and Trustfall semantic adapters; all three laws
pass.

Semantic facts now retain their authority after product composition. Local and
worker publication use the same typed semantic relation payload and have a
byte-equality regression. Search joins every selected presentation row to an
admitted `SemanticQueryFact`; its compiler variants retain exact `PackageKey`,
`PackageUrl`, `LanguageProfile`, immutable image identity, provenance, and
either `DeclarationIdentity` or the distinct `ExternalTargetIdentity`.
Trustfall no longer consumes `ViewRoot` as semantic truth. Compiler declaration
rows must equal their canonical package/declaration ID, package rows must equal
their canonical package ID, and external targets are scoped to package plus
image so two packages cannot collapse an identical external spelling. External
edges are admitted only between a declaration and target carrying the same
package, image, profile, coordinate, and image facts. The corpus commitment is
tagged and length framed across option and collection boundaries.

The direct Trustfall API now validates query and variable count, bytes, and
nesting before graph allocation. A corpus retains its admitted `Limits`, and
the product selects an explicit 65,536-row, 64 MiB envelope instead of silently
falling back to the extension's 4,096-row default. The active Trustfall suite
passes 17/17, including a query that traverses a compiler external edge,
hostile relabel/image/package rejection, structurally distinct edge-digest
commitments, and 4,097-row admission. The CLI/MCP/desktop process journey passes
after this cutover.

Locald no longer materializes the Trustfall stream with
`block_on(stream.collect::<Vec<_>>())`. One reusable bounded executor polls the
non-`Send` stream lazily and returns events through a one-event rendezvous.
This preserves request identity, bounded paging, cooperative cancellation, and
producer/consumer backpressure inside locald. CLI and MCP still return one
materialized bounded `GraphQueryPage`; this is not chunked transport streaming.
The executor costs one process-wide OS thread rather than one thread per query,
but queries serialize there and have no independent wall-clock cancellation.
The live MCP process gate admits a constant-size, capability-proven
continuation, resumes the next page, and exercises cancellation. Locald uses a
graph-specific owner-context decoder: it requires the committed root digest to
equal the durable owner's already admitted root, admits the remaining cursor
claims, and then applies the ordinary request/owner comparison. The standalone
decoder remains strict and still requires a canonical root. This closes resume
without attaching the full canonical view to every request.

Exact package/profile generation history is now retained and selected by a
typed persisted intent. The selected generation survives daemon restart,
reopens its exact immutable image, and supports names, selected-symbol graph,
and document journeys. Cross-package, unknown, and stale selection claims are
rejected. The semantic publication relation is wire version 3 and the public
transport is DTO version 6. `SemanticVersions` and `SelectSemanticVersion` are in the 35-command
canonical registry, the shared client exposes typed calls, and the native
desktop package view loads and changes versions. The exact restart journey
passes 1/1. The registry itself contains all 33 historical commands plus these
two additions; the remaining interface issue is CLI/MCP/product reachability
for registry rows that do not yet have named routes.

Capabilities now advertise exact `LanguageProfile` rather than a collapsed
language family, and bind typed compiler tool, toolchain, package-authority,
task, and recipe evidence. C and C++ remain separate profiles while both map
through the one checked Clang tool family. `Probing` is explicit, and compiler
tool discovery runs off the listener readiness path; a hanging explicit fake
Go probe does not block locald startup. Embedding capability identity is
recomputed from model, tokenizer, extraction policy, byte/token bounds,
chunking, dimensions, metric, pooling, normalization, encoding, and query and
document treatments. Hostile cross-language tool and mutated embedding claims
are rejected. The focused capability canonicality suite passes 8/8.

Qdrant's prior product envelope was internally inconsistent: the generic 4,096
candidate cap, 4,096-byte payload cap, and 4 MiB aggregate rejected a normal
1,536-dimensional vector whose payload is 6,148 bytes. Product composition now
uses a typed, dimension-aware `ProjectionEnvelope` with checked arithmetic,
preflight before embedding, a 65,536-document ceiling, and a 512 MiB hard
vector-fact budget. The admitted envelope is required by vector-fact and ANN
construction. Qdrant 25/25, including shared-coordinate reuse and bounded
provider-page fetching, and the product envelope tests pass. A deterministic
local fake HTTP provider proves complete activation/count verification,
semantic result delivery, lexical composition/fallback, exact embedding
capability identity, and stale-view rejection without provider reuse. This is
real adapter/product composition against a fake provider; it does not prove a
real external Qdrant restart, durable
generation reuse, delta-only mutation, or 4,097-row external-service behavior.
Those five provider tests remain skipped because `QDRANT_URL` is absent.

Tantivy 23/23 proves leaf-level typed delta, incremental overlay, durable
directory reopen, and restarted-result equivalence. The live product still
builds its process-owned `TantivySource::local_adapter`; durable incremental
Tantivy is not yet composed into local-service restart. The restored 14-crate
server-index cluster passes 205 tests, with five real-Qdrant tests ignored, but
local-service actively consumes only `server-index-core`,
`server-index-graph-vector`, and `server-index-trustfall`. Restored acquire,
catalog, ingest, publish, retrieval, routing, and older provider crates remain
library-only and receive no product-reachability credit.

The current language evidence is deliberately asymmetric:

| Authority lane | Current executable evidence | Honest disposition |
|---|---|---|
| Rust | 10/10 | Leaf authority green; Rust drives the exact publication, client, selection, and restart journey. |
| TypeScript / TSX | 33/33 | Authority/protocol suite green; full product restart corpus parity remains broader than this leaf gate. |
| Python | 36/36 | Authority suite green; the rich historical package/import corpus still needs a product journey. |
| Go | 33/33 protocol/offline/fake tests; real authority cases explicitly skipped | No real Go execution credit. The ambient `/opt/homebrew/Cellar/go/1.27.1/libexec/bin/go` was not invoked. |
| Java | No JDK installed; three authority cases report `MissingEnvironment` | External blocker: an explicit `NUDOX_JDK`/JDK is required. |
| C# | Unit 4/4 and image 6/6; producer parity has three red cases | `AuthorityImage.cs` direct-child collection is fixed, but committed fixtures are stale and cannot be regenerated without dotnet. |
| C and C++ | Clang default 9/9; native feature suite 17/17 including eight live C/C++ cases | Real distinct C/C++ compiler authority is proven on this host. |

The source reintegration ledger is complete. Comparing the ChatGPT/backend
implementation sources into this tree maps 730 files directly: 522 are
byte-identical and 208 evolved. The 28 source-only entries are 26 control-crate
files deliberately moved to `tools/control` (24 identical, with Cargo metadata
and the ledger evolved), one local authority secret that must not migrate, and
two `__pycache__` files. The old Git compiler mapping is 360/360 present: 234
byte-identical, 126 evolved, and zero missing.

Remaining whole-system work is narrower and explicit:

- Remote worker transport carries the typed semantic relation and proves
  local/remote payload equivalence, reuse, fencing, and fallback. A remote
  worker still does not execute or lend a compiler authority.
- Qdrant external-service restart/delta/generation behavior and product durable
  Tantivy composition remain unproved.
- Telemetry has library and adapter evidence but no configured startup exporter
  journey.
- `backend-version` and restored heart root/locality models still duplicate
  ownership; object-pack and hydration have strong library tests but are not a
  single active product composition.
- The large historical `NativeSemanticAdapter` and helper implementations under
  `frontends/*` are unreachable from the product except through
  `syntax_frontend()`. They must be feature-mapped against the canonical
  compiler-language adapters before removal; they must never be reactivated as
  a second semantic plane. Likewise, compiler-driver's private `mod native` and
  `NativeRecipe` are unreachable legacy code and must not be wired back into
  semantic authority.
- Full language parity cannot be claimed until the Java environment and C#
  fixture blockers are cleared and Go is exercised with a healthy explicitly
  selected toolchain. The current architecture enforces one plane across all
  lanes, but executable authority parity is not yet equal across all languages.

## Historical post-audit integration checkpoint — 2026-09-13 (superseded)

The following checkpoint records the intermediate state that the semantic-plane
cutover above replaced. Its negative claims about generation history, desktop
selection, generic Trustfall authority, seven collapsed capability slots, and
the old Qdrant envelope are historical evidence, not the current disposition.

The audit findings remain release gates. Subsequent integration work has
narrowed four of them without closing the larger end-to-end proof:

- `SemanticSnapshot`, `SemanticEntityChanges`, and `SemanticStableLinks` now
  run allocation-free canonical merges over any complete `SemanticReader`,
  including a validated borrowed `SemanticImageView`. The regression test
  compares owned and encoded/reopened images directly, so durable publications
  no longer require a reconstructed `Ir` or an archive model to enter the VCS
  entity and graph paths.
- live `SurfaceCommand::Diff` no longer compares generic `ViewRoot` labels and
  signatures. It resolves both indexed packages to complete semantic
  publication claims, reopens their immutable image closures, joins exact
  `DeclarationIdentity` values, re-pairs singleton-family variant churn as one
  change, and emits `Indeterminate` rather than a false removal for ambiguous
  overload families. It also merge-diffs canonical relation keys and returns
  typed endpoints, relation kinds, confidence, and retained source spans for
  added, removed, and evidence-changed links. It rejects missing or partial
  semantic authority.
- `ProductSemanticPublicationKey` now retains an admitted product
  `PackageReference`, the exact version-pinned compiler `PackageUrl`, and the
  `LanguageProfile`. Compiler requests borrow lineage directly from that URL.
  Every activated image is checked against the key's language authority,
  lower-IR recipe, ecosystem, and namespace/name before product projection, so
  a valid manifest cannot be rebound to another package or profile.
- live Graph and Related commands activate the source package's complete
  semantic publication. Graph traverses typed semantic links; Related combines
  outgoing and incoming occurrence evidence. Both reject external targets that
  the current product snapshot cannot represent instead of claiming a complete
  truncated neighborhood.

The CLI now exposes `diff <FROM> <TO>` and the MCP server exposes
`backend.diff`; both parse package references into the same typed
`SurfaceCommand::Diff`, cross the shared admitted protocol, and return the
bounded typed `SurfaceReply::Diff`. Semantic diff dispatch also bypasses the
registry catalog, so catalog availability cannot disable an already published
compiler comparison. The desktop crate now exports the same daemon-backed
`diff_endpoint` seam and rejects package syntax before endpoint I/O; the native
application still has no version-selection action. The public transport is DTO
version 5. CLI and MCP tests assert that typed link evidence reaches their
actual JSON projections.

Diff indexing no longer materializes the complete compatibility view or clones
row documents into several `BTreeMap`s. Exact package and declaration rows are
borrowed from the immutable relation, declaration summaries occupy two bounded
sorted vectors, and an overload-aware linear merge enforces the 256-row result
limit while producing output. Canonical pinned package URLs already create
distinct `PackageKey` values, so separately indexed registry releases coexist;
only successive generations of one exact package key still lack history.
The shared client now owns the reply-shape proof in `Session::diff`, so MCP and
desktop receive `Box<[DiffRecord]>` and cannot accidentally accept another
Surface reply. The CLI preflight now measures the actual typed Surface payload;
the previous blanket estimate was larger than the transport frame and rejected
every successful Surface result before serialization. Focused CLI, MCP, client,
desktop, library, and semantic-diff tests pass.

At this historical checkpoint generation selection was absent. The cutover
checkpoint above resolves that state with retained exact package/profile
history, persisted selection, native desktop selection, and a passing restart
journey. `CorePayloadHash` remains deliberately narrower than every semantic
plane, so typed Diff and provider evidence reopen immutable images instead of
treating that summary hash as complete authority.

### Compiler and semantic-IR port provenance

The compiler was moved, not replaced by a smaller parser. A path-for-path
comparison against `git archive HEAD` finds all 99 files from `compiler/ir` in
`crates/compiler-ir`, all 11 files from `compiler/ir-vocabulary` in
`crates/compiler-ir-vocabulary`, and all 27 files from `compiler/publication`
in `crates/compiler-publication`; none are missing. Of the 99 IR files, 44 are
byte-identical and 55 contain integration or hardening changes. The changed IR
files grew by a net 4,099 lines at the final path-audit checkpoint. `vcs.rs` retains
the old implementation and currently adds 584 lines while deleting 19,
principally to let validated borrowed semantic images use the same VCS entity
and canonical-link merges.

The active compiler and language trees contain 341 Rust files and 181,309
lines. Tree-sitter references are confined to the seven `frontends/*`
structural fallback crates. No compiler semantic crate uses Tree-sitter as its
authority interface. The authoritative path remains the restored language
adapters/oracles → compiler IR → immutable semantic publication → checked
activation route.

A broader mapped-tree audit found one omission: the 22-file
`server/operation` crate had not been copied into the new layout. It is now
restored at `crates/server-operation` with only workspace-relative dependency
and fixture-path changes. Its public lending `BatchSource`, terminal coverage,
and pinned-object authority types compile under strict Clippy. The library,
canonical local-closure, semantic publication/reopen/index, and Rust compiler
publication/reopen tests pass. Across the mapped compiler, seven language,
twelve heart, fourteen server-index, journal, operation, runtime, and workflow
trees, the post-repair path audit maps exactly 43 systems and 737 legacy files
to 737 active files with zero missing.

Active locald opens `<workspace>/projection.turso` at startup, synchronizes it,
and applies every published view delta with a rebuild fallback. It is a
write-only derived runtime projection today: no active product call invokes
`TursoProjection::search`, and canonical Search instead uses process-owned
Tantivy plus optional remote Qdrant. The restored server-index acquire path
accepts `TursoCatalog`, but that acquire/catalog/ingest design remains
library-only; locald uses the engine `RegistryOwner` instead.

## Scope and method

The requested path, /Users/mileswirht/Downloads/workspace_1, does not exist.
The following closest evidence sources were used instead:

| Evidence | Resolved location | Use |
|---|---|---|
| Active product | /Users/mileswirht/Downloads/backend | Current source, composition, and test inventory |
| Deleted implementation | git show HEAD:path in the active checkout | Removed compiler, heart, interface, and server source, helpers, fixtures, and tests |
| Historical workspace | /Users/mileswirht/Downloads/backend_1/workspace | Rich compiler/oracle, heart/transport, GUI, index, registry, and corpus |
| Historical workspace 2 | /Users/mileswirht/Downloads/backend_1/workspace2 | Alternate compiler/heart/interface/server layout and object-pack, hydration, operation, runtime, and observability tests |
| Language comparison worktrees | /Users/mileswirht/Downloads/backend-go-* and backend-ts-* when present | Additional Go and TypeScript parity evidence only |

The audit reads live uncommitted source as well as HEAD. Since other work was
in progress, every claim names its actual composition point and distinguishes
source existence from a user-reachable product path.

| Status | Meaning |
|---|---|
| **Proven parity** | A current test reaches comparable observable user behavior. |
| **Stronger foundation** | Current control is better specified or safer but does not replace missing behavior. |
| **Regression** | Historical source and fixtures establish behavior the active product does not provide. |
| **Partial / narrower** | A current route exists but sees less scope or emits less information. |
| **Library-only** | Code exists but no current command/client path calls it. |
| **Untested / external** | A real tool, provider, workload, or corpus was not executed. |

## Historical decision at the initial audit cutoff (superseded)

The migration is **not yet proven to bring more with less**.

The replacement has material strengths: typed identity, checked ownership,
bounded storage, journal recovery, authenticated local worker transport,
hardened source-root traversal, real local Tantivy, and a real package
acquisition route. Those are meaningful improvements.

They do not substitute for rich compiler behavior. The deleted compiler/IR,
publication, heart, and server-index source now exist as a connected restored
workspace cluster, including canonical `compiler-ir`, `compiler-driver`, and
`compiler-publication`. `Command::Add` starts a durable `LocalCompilerHost`,
groups scanned files by exact `LanguageProfile`, compiles and publishes each
package frontier, and persists a manifest/binding claim in
`BuiltinSemanticRelation`.

The current locald view path now consumes that durable authority: every rebuild
pages the relation, activates each `Complete` claim through the compiler owner,
reopens every admitted image, and projects canonical entities, rendered
type/document content, documentation fragments, stable external-link
identities, and parent links into generic `ViewRoot` rows. PSR7 is
structural-only; its bounded PSR6 decoder skips the removed legacy semantic
section. Structural rows remain only for an exact package/profile that is
Partial or Unavailable. This is real local product wiring, and the resulting
generic rows reach the existing CLI, MCP, desktop, lexical, Trustfall, and
Qdrant routes.

It is still not complete rich semantic parity. The generic row model discards
typed occurrences, source spans, diagnostics, dependency facts, non-parent
graph relations, extension pools, and canonical image ownership after local
projection. Graph/Related and Diff now reopen and consume typed semantic images
before producing their bounded public replies, so IR-VCS is product-reachable
for separately indexed package versions. The worker does not run or lend a
compiler authority. No
source-to-client/restart journey has exercised the new composition, and the
host's wedged Go toolchain prohibits local-service probes here. Qdrant remains
a process-local generic-row projection with no durable incremental lifecycle;
Trustfall remains a generic hierarchy; telemetry export composition remains
absent.

### Historical release blockers (superseded by the cutover checkpoint)

| ID | Severity | Finding | Required executable gate |
|---|---:|---|---|
| PARITY-SEM-001 | P0 narrowed | `view_build` activates complete publication claims and projects entities, canonical documents/types, documentation fragments, links, and parent edges into generic rows. Graph/Related consume typed image links internally, and public Diff now preserves exact declaration identities plus typed link endpoints, kinds, confidence, and source spans. Generic search, arbitrary Trustfall, Qdrant, and remote replies still collapse occurrences, diagnostics, dependencies, extension pools, and most graph evidence into `Row`. | One real authority fixture must preserve typed entity/type/doc/occurrence/edge/diagnostic/dependency relations through search, Trustfall, Qdrant, remote worker, all clients, and restart. |
| PARITY-IR-PUB-001 | P0 narrowed | Complete persisted claims are activated and reopened during locald view rebuild, with exact-profile structural fallback suppressed only after projection. A single activation helper now rejects image authority, recipe, stage, ecosystem, or package lineage that disagrees with the persisted product key. Diff reopens two complete package claims and reaches typed CLI, MCP, client, and desktop seams with entity and link evidence. Same-key generation history, rich provider transport, remote authority, exact coordinate commitment inside image provenance, and an executable restart journey remain unproved. | Exercise a real authority through Add, durable restart, exact closure reopen, same-key generation selection, typed link/entity diff, and typed Trustfall/Tantivy/Qdrant/remote-worker output. |
| PARITY-IR-VCS-PRODUCT-001 | P0 narrowed | The original IR-VCS remains intact and now accepts any complete `SemanticReader`, including a validated borrowed `SemanticImageView`. Canonical entity and stable-link cursors compare durable images without rebuilding owned IR. Live Surface Diff returns bounded, typed declaration and graph evidence through CLI, MCP, shared client, and the desktop service seam. The relation still replaces successive generations of one identical package/profile key, and the native desktop has no version picker. | Retain two generations for one exact package/profile, reopen them after restart, reject hostile cross-claim substitution, and add the native version-selection action. |
| PARITY-PACKAGE-IDENTITY-001 | P0 narrowed | `PackageUrl` is the shared parser/type for Surface, interface-core, engine registry, compiler input, and semantic-publication keys. The publication key retains the exact product `PackageReference`, version-pinned compiler coordinate, and profile; compiler lineage borrows the URL namespace/name without reparsing, and activated images must match its lineage/profile. Semantic-image provenance still commits lineage/profile rather than the exact coordinate identity/version/qualifiers/subpath, and no restart journey proves those fields through the index plane. | Commit or otherwise prove the exact admitted coordinate at the immutable image/publication boundary. Assert all seven language package types, generic/Cpp acquisition distinction, hostile percent encodings, and stable identity through restart. |
| PARITY-SEMANTIC-LOCATOR-001 | P0 | The whole-publication claim removes the individual-image pairing surface, and locald now activates/reopens it during view rebuild. There is still no hostile substitution test at the active boundary, no executed restart/client lifecycle, and no remote semantic owner. | Require hostile substitution rejection at the owner reopen boundary, then prove local/remote/client lifecycle and restart. |
| PARITY-SEMANTIC-PUBLICATION-CANONICALITY-001 | P0 source repair pending lifecycle proof | The relation retains locality-independent manifest/binding facts and checked partial coverage. Add writes it and locald now activates complete claims before view coverage is credited. No two-journal equality, forged-coverage product attack, executed restart reopen, or typed client consumer exists. | Prove byte equality across two journals, reject forged coverage at construction and decode, reject image substitution at reopen, and require exact closure reopen before any view is lent. |
| BUILD-LOCAL-SERVICE-001 | Resolved audit checkpoint | The semantic-relation E0639 and the intermediate Qdrant compile failures are repaired. An independent fresh `cargo check -p backend-local-service` now exits 0, with warnings only. | Keep this compilation gate green; it does not establish semantic-publication or provider lifecycle parity. |
| BUILD-WORKSPACE-ALL-TARGETS-001 | Resolved integration checkpoint | The first `cargo check --workspace --all-targets` exposed 14 stale compiler-driver `include_bytes!` paths into deleted `compiler/languages/{typescript,csharp}`. Those paths and the later `Coverage::is_complete` and deadline fractures were repaired. A final post-integration all-target workspace check exits successfully. | Keep the all-target compilation gate green and run the selected executable journeys on a host with a healthy Go toolchain. |
| BUILD-INTERFACE-CORE-001 | Resolved audit checkpoint | The stale local parser was removed: exact `PackageUrl` now lives in `compiler-vocabulary` and `interface-core` re-exports it. Independent `cargo test -p interface-core --test package` passes 2/2; library protocol and engine registry focused gates pass. | Keep the shared parser suite green. The next blocked boundary is the protocol wire mapping below. |
| BUILD-INTERFACE-PROTOCOL-PACKAGE-001 | Resolved audit checkpoint | The settled vocabulary distinguishes canonical PURL `generic` from acquisition protocol `cpp`; the protocol wire correctly represents the former. The typed `server-index-ingest`/retrieval/routing all-target suite now passes. | Keep this wire contract green. This suite is library parity only; active product composition remains blocked under PARITY-INDEX-COMPOSITION-001. |
| BUILD-HEART-ROOT-BENCH-001 | Compile gate resolved; execution pending | The restored capacity-planning bench was migrated to the canonical IR/build APIs and now compiles inside the successful workspace all-target check. Its capacity/derived-store workload has not been executed in this host session. | Run the capacity/derived-store workload on a healthy toolchain host. |
| PARITY-ROOT-LOCALITY-001 | P1 | `backend-version::WorkspaceRoot` is the active locald/store/worker authority, while restored `heart-root::GenerationRoot` and `heart-hydration::ValidatedLocality` supply a parallel unused model. No route proves which one owns locality, remote availability, or restart recovery. | Choose one canonical root/locality owner; prove cold-local with available remote, offline fallback, restart, stale-refresh contention, and rejection of a conflicting second root model. |
| PARITY-SURFACE-TYPES-001 | Resolved library/type repair | The prior raw String and string-map Surface model has been replaced in live source by ProductText, PackageReference, project and tree identities, typed SurfaceCommand operands, and typed SurfaceReply records. Independent protocol tests pass 23/23, including strict unknown fields, bounds, replies, and certificates. | Keep the library/protocol suite green; product reachability remains under PARITY-SURFACE-WIRING-001. |
| PARITY-SURFACE-WIRING-001 | P1 narrowed | locald owns Surface dispatch. CLI and MCP expose one typed `surface` route derived from the canonical registry, and a real-locald journey covers all 24 `SurfaceCommand` variants through both processes with typed success or typed unavailable outcomes. The shared client and desktop retain the same service seam. ProductState JSON persistence still lacks full crash/concurrency proof and owner/dependent catalog fields explicitly return `NotRecorded`. | Add native desktop actions for the remaining operations plus fault/restart/concurrency tests for the durable owner and explicit unsupported-data behavior. |
| PARITY-LANG-001 | P0 | Add now attempts every scanned `LanguageProfile`; complete claims are reopened and partially rendered into generic rows, while Partial/Unavailable profiles retain structural fallback. No seven-language locald authority-to-client journey has passed, and rich per-language relations are still lost. | Seven language-specific source-to-client tests with a real authority selected by locald; missing tools must report unavailable/partial coverage rather than a completed semantic result. |
| PARITY-LANG-002 | P1 | PSR7 ingestion no longer invokes the Go/Python semantic helpers; all semantic publication attempts use the compiler owner. The current host's Go 1.27.1 is wedged, so no Go/local-service execution receives parity credit. Python and Go still lack complete rich client journeys, including dependency/build-condition/tool identity evidence. | Multifile, dependency, build-condition, tool identity, and client-result tests for both routes on a healthy host. |
| PARITY-CAPABILITY-AUTHORITY-001 | Resolved locally; remote authority open | Health now exposes exact `LanguageProfile`, task, toolchain, package-authority, and recipe evidence for nine profiles and 18 task slots; C and C++ remain distinct. Embedding identity commits all 13 configured fields, and hostile claim mutation is rejected. | Carry the same closure through remote compiler execution and withdraw it on remote drift. |
| PARITY-INDEX-COMPOSITION-001 | P0 | The restored acquire/catalog/ingest/registry/retrieval/routing packages are byte-identical library clusters, but `cargo tree --workspace -i` finds no local-service/locald/worker/client consumer. Current Add/search/worker routes use the generic backend registry/view/projection plane instead. | One typed package coordinate and compiler publication must flow through acquire/catalog/reconciliation/publish/retrieval/routing into all clients, with the old sealed-boundary and deterministic retry/merge attacks exercised on the active command route. |
| PARITY-BINARY-OBJECT-BOUNDARY-001 | P1 | Restored heart-frame, heart-view, and heart-object-pack pass their own hostile-wire, borrowed-view, and zero-allocation suites, but have no non-library product consumer. Current store pack is an allocating `Vec`/`BTreeMap` map pack, and JSON/ViewRoot do not replace a validated borrowed immutable-image/object boundary. | Use the restored frame/view/object-pack boundary for immutable compiler-publication and remote hydration; prove cold reopen, hostile frame/object rejection, exact borrowed body boundaries, and no full-pack staging in a product lifecycle. |
| PARITY-REMOTE-001 | P1 | Local and worker product execution call ProductProjectionBuilder, which counts and hashes source rows. No authority is called and no semantic result root is published. | Same corpus must yield equal local/remote semantic rows and roots; helper or tool drift must be rejected. |
| PARITY-INTERFACE-001 | P1 narrowed | The authoritative registry has 35 entries and its typed surface projection has 24. CLI and MCP both expose the generic typed Surface route, with registry-derived MCP operation enumeration and a real-locald matrix for every variant. Native desktop affordances remain incomplete even though its service seam accepts Surface commands. | Add native desktop journeys for each retained behavior and keep the registry/process exhaustiveness gate green. |
| PARITY-QDRANT-001 | P0 lifecycle proof open | `Command::Search` reaches mutable `RemoteSemantic::reconcile/search`; binding-scoped IDs, logical-ID verification, verified replacement, and bounded retirement are active. A typed dimension-aware envelope admits up to 65,536 documents under 512 MiB and shares cached coordinates. Changed views and restarts still full re-embed/upsert, and no real external-service restart/delta/recovery journey has run. | Prove restart reuse, delta mutation, cross-process retirement, outage/offline recovery, and typed capability withdrawal against a real service. |
| PARITY-TRUSTFALL-001 | P1 narrowed | Generic `backend.query` still runs over ViewRoot hierarchy. Direct Graph and Related now reopen the compiler image and use typed links plus outgoing/incoming occurrence evidence to select targets, but the reply erases edge kind, confidence, source evidence, types, and dependencies. | Carry typed semantic edge evidence through the Trustfall provider and MCP query using the historical cross-file occurrence/dependency fixture. |
| PARITY-TANTIVY-001 | P2 | Product makes a real in-memory Tantivy adapter per search. Durable build_in_dir and open_in_dir remain leaf APIs. | Product restart test must reopen a bound durable projection or explicitly prove the intended ephemeral design. |
| PARITY-TELEMETRY-001 | P1 | `crates/execution::Telemetry` now has seven closed counter families; scheduler lifecycle/queue and listener transport hooks can record them. But both locald and worker startup bind listeners at `Telemetry::disabled()`, there is no concrete exporter or export loop, and Compiler/Search/Remote/Recovery have no product producer. Deleted heart telemetry exported bounded tracing/OTel logs, spans, periodic runtime gauges, and root/hydration/store/runtime/workflow events. | Configure a bounded exporter at locald/worker startup; prove periodic/bounded export, redaction, exporter-failure isolation, and one actual compiler/query/remote/recovery event per family. |
| PARITY-UX-001 | P2 | Desktop describes a complete typed index even though active semantic coverage is partial and most language lanes are structural. | Render actual language/capability coverage and reserve typed wording for admitted typed relation output. |

`IR-EXEC-BASELINE-001` is resolved at this checkpoint: an independent
`cargo test -p compiler-ir --all-targets` passed every target, including the
previous schema-three type-facts failure. This proves the restored library
suite, not product composition.

REG-DECODER-001 was fixed during this audit. The Maven decoder stripped the
opening angle bracket then compared against a tag containing that bracket, so
release elements were missed. The isolated decoder fix in
crates/engine/src/registry/ecosystem_decoders.rs passed cargo test -p
backend-engine registry --lib and cargo fmt --check -p backend-engine.

Two cutover regressions were also repaired while the full graph was being
checked: stale compiler-driver fixture paths now point at
`crates/compiler-language-*`, and a stale journey `Coverage::is_complete`
signature was updated. The Rust authority's monotonic deadline and
`DeadlineExceeded` terminal were restored through the rust-analyzer lowering
boundary; the integrator reports its all-target Rust/driver check and focused
expired-entry test pass. These repairs require the final full-workspace
all-target gate below.

## Current product trace

~~~mermaid
flowchart LR
    C[CLI / MCP / desktop] --> D[locald command adapter]
    D --> A{Add target}
    A -->|directory| I[scan_project]
    A -->|canonical package URL| R[Registry acquire verify stage]
    R --> I
    I --> S[PSR7 structural declarations]
    I --> P[Package sources grouped by LanguageProfile]
    P --> O[LocalCompilerClient compile publish]
    O --> M[BuiltinSemanticRelation whole-publication claim]
    S --> F[Fallback rows for Partial or Unavailable exact profile]
    M --> X[view rebuild activates exact Complete claim]
    X --> V[SemanticImageView reopened]
    V --> G[Presentation rows for lexical and vector retrieval]
    V --> SG[Typed Graph Related Diff and Trustfall evidence]
    F --> SQ[Explicit structural SemanticQueryFact]
    SG --> SQ[Compiler SemanticQueryFact]
    SQ --> TF[Bounded Trustfall corpus]
    F --> W[ViewRoot presentation]
    G --> W
    W --> Q[process-cached QueryCoordinator and in-memory Tantivy corpus]
    Q --> L[CLI MCP desktop]
    Q --> RQ[RemoteSemantic reconcile and Qdrant search]
    M --> WK[worker semantic-publication projection]
    WK --> H[canonical typed relation result]
~~~

Complete publication images now contribute local presentation rows and feed
typed Graph, Related, Diff, and Trustfall selection. Trustfall admits compiler
declarations and external targets only with exact package/profile/image/fact
evidence; structural fallback is a distinct evidence variant. Public search and
graph replies remain narrower than the semantic image because they expose
bounded result projections rather than the full relation vocabulary. Qdrant is
reachable from Search through a process-local semantic-evidence-bound
materialization; it is rebuilt after a view change or restart and has no durable
incremental generation lifecycle.

### Remote search lifecycle checkpoint

The live route is narrower than the earlier source-only claim that every
identical search rebuilds. `compose_owner` owns one `SearchSnapshotOwner` and
one `RemoteSemantic`; `SearchSnapshotOwner::select` retains a coordinator when
workspace root, view root, and coverage are equal. `ConfiguredQdrant::reconcile`
also retains an `ActiveQdrant` when its binding matches that selection. The
local-service unit suite exercises the pointer reuse assertion for the local
snapshot owner.

That is process-memory reuse, not durable semantic reuse. A daemon restart
starts with no coordinator, active binding, or document-embedding cache. A new
view materializes every selected generic row, embeds each noncached document,
runs `ensure_collection`, full `upsert`, and full count verification. Within
one process, a replacement is verified before it swaps in; exactly one retired
binding is retained and its binding-scoped physical UUIDs are retried/deleted
before another generation can be admitted. This repairs overwrite and
unbounded-retirement faults, but it does not establish cross-process cleanup,
durable reuse, delta mutation, or a typed publication-generation lifecycle.

The configured producer now correctly binds program/model/tokenizer bytes and
checks each before and after an embedding invocation. The transport layer's
gzip response path is bounded after decompression. Those are repaired
foundation controls. They do not prove a product activation/search against a
real Qdrant service, reuse after restart, diff-only mutation, collection
retirement, or a compiler-image input.

## Canonical semantic IR and IR-VCS audit

The restored IR is not a tree-sitter substitute. A path-by-path comparison
against `git show HEAD:<path>` found the deleted compiler/IR, compiler driver,
compiler publication, heart, and server-index source restored under matching
`crates/*` packages; their Cargo changes rewrite paths for the new layout. The
`semantic_attacks` fixture's four-byte semantic-root envelope change is an
intentional correction, not a grammar change: the restored schema already has
a six-`u32` header and trailing per-product entity-root cell, while the deleted
test still expected its older five-cell layout. The corrected hostile-wire
suite now passes. An independent fresh `cargo test -p compiler-ir --all-targets`
also passes every suite, including the expanded owned/reopened semantic IR-VCS
coverage, semantic attacks, schema-three type-facts reopen case, and wire
attacks. The earlier
`type_facts` failure is repaired. This establishes restored-library executable
parity for that suite; it does not establish that the active product compiles,
publishes, reopens, or projects an image.

A later independent re-run caught a second identity-specific fixture fault.
`SCHEMA_ONE_FIXTURE_HEX` in `type_facts.rs` had been frozen in commit
`827e7f67f` after commit `628a6698c` mechanically copied workspace2's
`nudox-id` code into `heart/identity` while changing its durable hash labels to
`heart.*`. The historical workspace's `nudox-id` still names those labels
`nudox.*`; the fixture was therefore a post-migration test artifact, not the
historical persisted contract. The fixture now pins the regenerated
historical-nudox identity bytes, with the schema layout unchanged. A fresh
`cargo test -p compiler-ir --all-targets --quiet` passes after that correction.

`cargo tree --workspace -i compiler-ir` shows the restored intra-cluster
path through `compiler-driver`, `compiler-publication`, and restored
server-index packages. `Command::Add` opens a workspace-owned
`LocalCompilerHost`, compiles each scanned package/profile frontier, publishes
it, and writes a `ProductSemanticPublicationRelation` claim into
`BuiltinSemanticRelation`.

The current rebuild path is now a genuine handoff:
`compose_owner` opens the compiler owner, `view_for_workspace` pages the
semantic relation, `rows_for_indexed_sources` activates every complete claim,
and `SemanticImageView` is reopened to render canonical entity/type/document
rows. It derives stable row identity from `PackageKey + DeclarationIdentity`,
rejects duplicate semantic identities, retains documentation fragments and
derives stable generic link keys from typed external targets, and suppresses
structural fallback only for the exact complete `LanguageProfile`. A startup
or repair rebuild reaches this source path, but no executable locald restart
journey has proved it.

The projection remains lossy. `Row` does not encode the IR's occurrence,
diagnostic, dependency, source-span, arbitrary graph-edge, extension-pool, or
VCS semantics, and providers and workers receive only that generic view.
Thus the compiler cluster is now locally product-projected but not a
full typed semantic authority across client, provider, remote, and restart
lifecycle.


### Historical IR-VCS seam and smallest faithful graft

The deleted git lineage and workspace snapshot describe two different layers.
At active-tree commit 2c41385aad032279c6a91803d3381d3d1e88c989, the deleted
compiler/ir/vcs.rs supplied a zero-copy Snapshot plus Diff over an owned Ir.
Commit e3a321d34c1f26e3ef3844fe59d71ec5f32bac4c then wired immutable
publication/reopen work around it. A source search of that deleted tree finds
the direct Diff consumer only in heart/root/benches/capacity_planning/runner/
semantic.rs, so that small VCS API was not itself a shipped command route.

Workspace at 1db1d688331972509e872da792552d48a0c3a8df has the earlier, much
larger compiler/ir/vcs plane: repository, stream receiver, checkpoints,
archive, structural delta/apply, semver, protocol, and sync. Its real
user-facing version-diff seam was instead
nudox-engine/src/mcp/tools/mod.rs do_diff_versions to
EngineHandle::diff_versions in nudox-engine/src/versions/mod.rs and
versions/diff.rs. That route held two version-registry IrViews, re-paired
churned declaration keys, refused a false removal where identity provenance
was insufficient, paged typed PackageDiff results, and has real-package MCP
tests. The same workspace's store/persistence.rs explicitly calls
IrRepository versioning-only and says it is not consulted on the read path;
index/server/coordination/indexing/ir_stream.rs consumes the VCS stream
grammar but does not make the repository its query authority. The audit
therefore credits the historical behavioral seam without pretending that every
heavy VCS module was active product authority.

The live equivalent is materially weaker. library SurfaceCommand::Diff reaches
local-service builtin/product_state.rs diff, which compares generic ViewRoot
labels and signatures and returns only Added, Removed, or Changed. The live
ProductSemanticPublicationRelation key intentionally replaces its value when
source, recipe, or generation changes, so it cannot resolve two exact
publication claims for a package version comparison. compiler-ir Snapshot
accepts only an owned Ir, while a reopened publication yields
SemanticImageSnapshot and SemanticImageView; no adapter, claim pair resolver,
or typed client/protocol route joins them.

The smallest faithful graft is not a second repository or a reimport of
libpijul. Extend the existing compiler-publication owner with lossless
PackageUrl-plus-LanguageProfile claim history or exact pair resolution. Reopen
both claims through LocalCompilerClient, then factor the existing compiler-ir
stable entity/link merge over the sealed semantic-reader surface so it can
borrow SemanticImageView as well as Ir. Project the existing typed entity and
link deltas, including identity-family ambiguity rather than fabricated
deletion, through one bounded typed reply used by CLI, MCP, and desktop.
Generic ViewRoot can remain a search presentation projection; it must not
become the semantic diff authority. The gate needs two real authority
generations, restart reopen, malicious cross-claim substitution, C/C++ profile
separation, and a typed all-client result.

| Historical responsibility | Restored source status | Active product status | Required proof |
|---|---|---|---|
| Typed entities, raw-byte atoms, columnar lanes, stable identities, canonical ordering, interning, type lattice, occurrence, docs, and graph facts | Present in `compiler-ir` and its closed vocabulary crates; independent `--all-targets` passes. | Complete claims render entity/type/document/link/parent fields into generic rows; remaining typed facts have no product relation. | Authority fixture must preserve typed relation roots rather than only rendered strings. |
| Full semantic image, hostile-wire validation, mmap/range/view reopening, and extension pools | Present, including full-wire encode/decode/validation and old attack fixtures. | Locald later reopens exact complete claims; hostile substitution and restart behavior have no product execution proof. | Corrupt/truncate/unknown-tag/offset/extension and hostile substitution tests through durable publication and restart. |
| Language extension pools and typed rendering of signatures, docs, and embedding text | Present, including per-language semantic renderers. | Canonical type/document render is used for generic rows; extension-pool facts and embedding inputs are not exposed as typed product data. | Each authority must expose its extension pool and render a returned declaration/document through a typed client relation. |
| IR-VCS `Snapshot` and borrowed stable entity/link `Diff` | The restored compiler-ir VCS retains the deleted small zero-copy API and adds generic canonical entity/link cursors over complete `SemanticReader` implementations; workspace1's larger repository/stream/archive plane is separate historical source. | Surface Diff reopens complete images and returns typed entity and graph evidence through CLI, MCP, client, and desktop. Exact same-key history is retained and the native desktop can select a generation; restart and hostile selection tests pass. | Expand typed deltas to remaining occurrence, diagnostic, dependency, extension, and source-provenance relations. |
| Immutable semantic publication, paired fragment/image manifest, verified reopen, and generation recovery | Present in restored `compiler-publication` with its old durable tests. | Locald persists and later reopens a whole-publication claim for view rebuild; no executed crash/restart lifecycle proves it. | Bind the persisted claim to owner reopen, then crash/restart it and project the reopened artifacts. |
| Tantivy, Trustfall, and Qdrant projection from the canonical image | Adapter-ready source existed historically; current IR retains direct reader/render paths. | Providers consume generic `ViewRoot` rows, now partly rendered from complete images. | Project canonical image relations directly and prove provider lifecycle parity. |

`PARITY-IR-PUB-001` remains a source/test/executable hard gate: local product
projection and typed public Diff are real, but same-key history and full typed
provider/remote/restart parity are not.

## Live package identity and semantic-publication checkpoint

The package boundary has been consolidated. `compiler-vocabulary` owns one
exact version-pinned `PackageUrl`; Surface, interface-core, engine registry,
compiler input, and semantic-publication keys retain or re-export it. Compiler
IR still represents the package scope as lineage/profile, and the restored
index vocabulary has its own version type, so exact coordinate commitment and
restart proof remain downstream gates.

| Boundary | Actual grammar and behavior | Proven seam | Parity verdict |
|---|---|---|---|
| `compiler-vocabulary`, `crates/library/surface.rs`, `interface-core`, and `crates/engine/src/registry/identity.rs` | One closed `PackageUrl` parser owns `cargo`, `npm`, `pypi`, `golang`, `maven`, `nuget`, and `generic`; `generic` is the C/C++ PURL type while `cpp` is only the remote acquisition protocol. It retains exact spelling/ranges and a domain-separated identity. | Independent package interface test passes 2/2, library protocol passes 23/23, engine registry focused test passes 10/10, and the restored typed index suite compiles/passes. | **Proven shared boundary type.** This resolves the former duplicate parser and spelling mismatch. |
| `crates/compiler-application/compiler.rs` and `crates/engine/src/builtin/semantic_relation.rs` | The compiler borrows ecosystem and namespace/name lineage directly from the admitted URL. The persisted relation key retains the product `PackageReference`, exact `PackageUrl`, and `LanguageProfile`; its relation family is `(domain 0x97, type 3, version 2)`. Activation rejects images whose authority, lower-IR recipe, ecosystem, or lineage disagrees with that key. | Focused vocabulary, engine, and local-service tests cover namespace retention, exact coordinate/profile key round-trip, and hostile package/profile rebinding rejection. | Stronger durable product identity. Image provenance still commits lineage/profile rather than the exact coordinate identity/version/qualifiers/subpath. |
| `crates/compiler-ir-vocabulary` and `crates/server-index-vocabulary` | `PackageLineage::new` still accepts any nonempty non-colon/non-backslash ecosystem/name, and index `PackageVersion` still admits arbitrary nonempty text. | Restored index tests exercise synthetic strings; no active acquisition-to-compiler-to-index lifecycle exists. | Rich internal lineage remains a different model. It needs an explicit typed projection from the one owned URL, not another admission grammar. |

The parser and durable-key split are repaired, but the full type-system
requirement remains. The semantic relation can recover the exact coordinate
after the request; the image itself cannot independently prove all of those
fields, and no acquisition-to-publication-to-index restart lifecycle proves
that they survive unchanged. The release gate must define a lossless typed projection
from the owned URL and transport it through registry, compiler provenance,
publication, and index without a second parser or free-form lineage admission.

The live replacement is named `ProductSemanticPublicationRelation`. Its key
contains the exact product reference, compiler coordinate, and language
profile; it has no image-byte payload; and coverage uses `u32`. Source and
recipe changes replace that row's value rather than becoming key material.
`SemanticPublicationClaim` retains only manifest and binding facts, so journal
receipt/head locality cannot alter semantic relation bytes. Partial coverage
fields are private and admit a nonzero checked total through
`PartialSemanticCoverage::new`. Focused engine tests cover round-trip,
manifest/binding mutation, and cross-package/profile image rejection.

Add now persists that relation, and view rebuild activates each complete claim
through the compiler owner before it lends rendered generic rows. The direct
relation suite also rejects a foreign manifest or binding. There is still no
two-journal equality test, no adversarial coverage-construction test, no
executed restarted-product reopen journey, and no typed client or worker
consumer. The publisher must retain receipt/head locality evidence and remain
the sole reopen authority. The old manifest-image membership problem remains a
required hostile-substitution test at that active reopen boundary, rather than
an image field on the relation itself.

## Historical scale, isolation, and topology responsibilities

`backend_1/workspace` and `workspace2` contain responsibilities beyond the
compiler image: `heart/cache/tiered.rs` has tiered CAS and promotion/fallback,
`heart/tests/cache_stampede.rs` proves 64-way single-flight and leader-failure
retry, and `index/coordination/outbox.rs` has transactional/idempotent fan-out
with stale-claim recovery. The workspaces also contain sandboxed producers and
distributed derived-store/session topology. Current root, store, replication,
and worker code provides several safer primitives, but that source overlap does
not prove those operational paths are retained. The combined executable gate
is: cold local cache with an available remote source; offline fallback followed
by restart; single-flight stale refresh under contention; poisoned
producer/outbox recovery; and one canonical root/locality authority. The test
must reject a second conflicting `heart-*` versus `backend-version` root model
rather than merely compare equivalent digests.

### Telemetry live check

The active `Telemetry` counter is a real safety improvement at the library
boundary: its seven families and four terminal outcomes give fixed cardinality,
the disabled form avoids evaluating an observation, and exporter failures are
isolated. Scheduler lifecycle/queue code and local-service/worker transport
listeners now have recording hooks. The recheck found no production caller of
`with_telemetry`, no concrete `TelemetryExporter` implementation, no call to
`Telemetry::export`, and no periodic scheduling or exporter-health route.
`run_with_owner` and worker `run_process` bind their listeners with the default
disabled value. There are also no active compiler, search, remote, or recovery
observations. Therefore the exporter trait preserves a bounded in-memory
snapshot contract, not actual telemetry export or the historic operational
dimensions.

The restored `heart-telemetry` adapter is now independently green: its exact
trace/log/periodic-metric contract passes after the test subscriber was made
interested in each explicit event target (`server.root` and the four
`heart.*` targets), rather than only the request-span target. That is library
parity for the old adapter, not a product route: `cargo tree --workspace -i
heart-telemetry` still has only restored-library consumers and locald/worker
do not construct its providers.

## Restored unmatched-subsystem checkpoint

The live workspace has 81 Cargo packages. The former unmatched systems below
were restored while this audit was running; representative production source
files hash byte-for-byte to `git show HEAD:<path>`. Their current status is
therefore not deletion. A restored parallel authority still fails parity when
the active product uses another representation or never calls it.

| Deleted system | Live source and executable evidence | Active composition result | Audit priority and narrowest graft/test |
|---|---|---|---|
| `heart/frame` | `crates/heart-frame`; `cargo test -p heart-frame -p heart-object-pack -p heart-view --all-targets` passed 66 tests, including stable vectors and exact no-write bounds. | `cargo tree --workspace -i heart-frame` has only `heart-view` as a dev dependency. | **P1.** Use frames for immutable publication/hydration transport and test corrupt/truncated frames through local/remote reopen. |
| `heart/object-pack` | `crates/heart-object-pack`; same 66-test run proves sparse direct writes, index validation, borrowed bodies, and no allocation after warmup. | No non-library inbound dependency; active `backend-store::Pack` owns `Vec<u8>` and `BTreeMap` and decodes into a second map. | **P1.** Make publication store/reopen one verified object pack and prove bounded borrowed lookup without full staging. |
| `heart/view` | `crates/heart-view`; same run proves hostile descriptor/padding/truncation handling and a 24-byte borrowed witness. | No product user; ViewRoot paging is a different, higher-level representation. | **P1.** Bind one ViewRoot/page source to a validated immutable image-frame view and run hostile-wire plus 100k-page hydration. |
| `heart/telemetry` | `crates/heart-telemetry`; `cargo test -p heart-telemetry --test adapter` passes 7/7, including correlated trace/log/periodic metrics and queue overload. | Only restored library consumers; active locald/worker start disabled telemetry and never export it. | **P1.** Compose the adapter or an equivalent configured exporter at both process starts, then prove bounded export and all product families. |
| `server/index/acquire` | `crates/server-index-acquire` is restored under the typed catalog/ingest cluster. Current `backend-engine::RegistryOwner` separately proves bounded acquisition, integrity, cursor recovery, and an Add/restart journey. | The restored cluster has no product consumer; current Add is a different generic registry owner. | **P1.** Preserve the current hardened network route but carry its parsed coordinate into typed acquisition/catalog authority; test conditional/no-op, cancellation, snapshot continuation, and recovery. |
| `server/index/catalog` | `crates/server-index-catalog` is restored with typed immutable image history, feed checkpoint, stale writer, and atomic page tests. | No locald/client owner selects it. This finding does not require retaining Turso; it concerns typed catalog history and atomicity. | **P0.** Select one catalog owner for compiler publication lineage and test reopen/history/rebind/stale and exact coordinate propagation. |
| `server/index/ingest` | `crates/server-index-ingest` is restored with typed image containment and deterministic reconciliation tests. | Active scan now persists structural-only PSR7 rows plus a separate compiler-publication claim; complete claims are rendered into generic view rows rather than `IngestedVersion`/verified entity locators. | **P0.** Carry the compiler publication through typed ingestion and run cross-image/duplicate/rebind/delete reconciliation through the command route. |
| `server/index/registry` | `crates/server-index-registry` is restored as the typed registry boundary. | Current registry has a real product route, but Surface → engine → compiler still reparses/internally renames package identity. | **P0.** Make `compiler_vocabulary::RegistryEcosystem` and one parsed coordinate the only authority across Surface, acquisition, compiler provenance, catalog, and client. |
| `server/index/retrieval` | `crates/server-index-retrieval` is restored; its sealed-boundary public/attack suites remain the executable specification. | Only restored `interface-protocol` consumes it; active search/graph/vector use generic `ViewRoot` rows. | **P0.** Give the command route a publication-pinned retrieval boundary and execute foreign-authority, wrong-snapshot, partial/degraded, and source-resolution attacks. |
| `server/index/routing` | `crates/server-index-routing` is restored with deterministic rendezvous, retry, cancellation, typed reply, and merge tests. | No active consumer; `BuiltinReplication` transports a generic source projection. | **P1.** Route typed published-image segments with deterministic assignment/retry/merge, then compare local/remote semantic outputs and cancellation. |
| `compiler/languages` umbrella | The historical umbrella contained only a no-op workspace grouping library. All seven authority crates, covering nine exact product profiles, are restored under `crates/compiler-language-*` and are internal compiler dependencies. | The umbrella package itself is intentionally absent; locald groups every scanned exact `LanguageProfile` for compiler publication, then falls back structurally only for Partial/Unavailable profiles. | **P0 through PARITY-LANG-001.** Prove all nine exact profiles through all clients and worker; do not treat the empty umbrella package as behavior. |

## Whole-system capability matrix

| Historic system and retained evidence | Active source existence | Actual product wiring | Verdict and exact missing proof |
|---|---|---|---|
| **Compiler, IR, publication.** Deleted compiler/application, driver, ir, ir-vocabulary, languages, publication, registry, and vocabulary contain canonical images, type/occurrence/docs/graph facts, authority lowering, package provenance, and seven authority implementations covering nine exact language profiles. | The old Git compiler maps 360/360 files into `crates/compiler-*`: 234 byte-identical, 126 evolved, zero missing. Engine owns the immutable publication relation. | Add persists exact per-profile history; selection and restart reopen the chosen image. Graph, Related, Diff, and Trustfall consume typed image evidence through CLI, MCP, client, and desktop seams. Worker transports the same typed semantic-publication projection but does not execute compiler authority. | **Direct port and stronger publication model.** Rich leaf corpora still need all-client runtime journeys for every language, and remote authority lending remains open. |
| **Heart identity, memory, hydration, object/object-pack, root, schema, view.** Deleted heart identity/memory/hydration/object/object-pack/root/schema/view and workspace2 nudox crates prove typed roots, object packing, hydration plans, and ownership compile-fail boundaries. | `backend-version::WorkspaceRoot` is actively used by locald/store/worker; restored `heart-root::GenerationRoot` and `heart-hydration::ValidatedLocality` are present with their own tests. | locald opens WorkspaceOwner, durable source relation, view journal, and client subscription routes. No active local-service/locald/worker path consumes the heart locality authority, so the two models remain parallel rather than composed. | **Stronger foundation; duplicated-authority regression.** Name one owner and reuse legacy object-pack/hydration fixtures for cold-local/remote/offline restart, paged desktop/MCP hydration, byte bounds, ownership rejection, and conflicting-root rejection. |
| **Heart adaptive/frame/runtime.** Deleted adaptive outage/policy tests and framing had explicit retry/capability behavior. | crates/execution, replication, runtime, engine contain admission, scheduling, closure, resources, and fallback controls. | BuiltinReplication composes reconnect, authenticated transport, recovery, and local fallback. | **Current mechanics tested; semantic workload unproven.** Run real language authority work through loss, reconnect, cancellation, and ensure no stale semantic facts publish. |
| **Server journal/workflow/operation.** Deleted server journal/workflow/operation include durable events, publication, compiler-corpus operations, and recovery. | The three systems are restored path-for-path as `crates/server-journal`, `crates/server-workflow`, and `crates/server-operation`; engine, store, execution, flow, and replication supply the product owner. | locald composes owner and view journal; `server-operation` remains a typed composition library rather than the product dispatcher. | **Source and core executable parity restored; product lifecycle proof open.** Replay a language publication across every fault boundary and compare typed roots plus client pages. |
| **Registry acquisition/catalog.** Deleted server index acquire/catalog/registry and historical ecosystem/ingest code cover acquisition and catalog behavior. | engine registry provides closed coordinates, native decoders, verification, durable owner, bounded HTTP; local-service registry stages tar/gzip and zip/deflate. | Command Add routes canonical package URLs through RegistryGateway acquire, stage, normal ingest, and publish. | **Partial proven wiring.** registry_process is a CLI acquire/search/restart journey. Add MCP/desktop, native ecosystem, checksum/offline/retry/malformed archive, and browse/profile compatibility journeys. |
| **Ingest/build/core/publish/retrieval/routing.** Deleted server index build/core/ingest/publish/retrieval/routing/vocabulary contain typed semantic ingestion/publication tests. | engine, local-service, library provide reconciliation, view publication, query, and worker routing. | scan_project to ProductSourceRelation to publish_builtin_view; BuiltinReplication sends product snapshots. | **Partial / regression.** Input and publication are real; published model is generic rows, not the historic semantic image. Test edit/delete/restart of canonical semantic relations. |
| **Tantivy.** Deleted server index Tantivy has durable/differential tests. | `server-index-tantivy::TantivyLexical` is restored but caps one in-memory projection at 256 documents and allocates a 15 MB writer; extensions/tantivy has newer RAM and disk APIs. | execute_search uses the newer `TantivySource::local_adapter` and normal library reply IDs. | **Proven local lexical execution; restored implementation is not a scalable replacement.** Compose historic typed snapshot/document IDs and deterministic score with newer durable incremental Tantivy; prove >256 documents and multisegment restart/query. |
| **Qdrant / graph-vector.** Deleted server index qdrant/graph-vector include real-service and lifecycle tests. | `extensions/qdrant` and local-service query/remote use an artifact-bound producer and semantic-evidence digest. A dimension-aware admitted envelope allows up to 65,536 documents under a 512 MiB vector-fact budget; unchanged cached coordinates are shared with `Arc<[f32]>`. | `Command::Search` retains an unchanged selected view, verifies a replacement before activation, scopes physical UUIDs to the binding, and checks logical IDs. A changed view or restart still re-embeds and full-upserts the corpus; remote state, retirement, and deltas are not durable. | **Bounded and product-reachable; durable lifecycle remains P0.** Prove external-service restart reuse, add/edit/delete delta mutation, cross-process retirement, outage recovery, offline fallback, and capability withdrawal. |
| **Trustfall.** Deleted server index Trustfall supports semantic async graph/occurrence tests. | `extensions/trustfall` now admits `SemanticQueryFact` values backed by exact compiler declaration or scoped external-target evidence, with an explicit structural fallback variant. | Product composition reopens selected compiler images, validates package/profile/image/facts on edges, and sends the immutable corpus through CLI, MCP, and desktop. The end-to-end journey and 17 provider tests pass. | **Typed authority cut over.** Expand the schema from declarations/external edges to occurrences, dependencies, diagnostics, and extension facts without introducing a second graph truth. |
| **CLI/core/documents/GUI/identity/library/MCP/protocol/search.** Deleted interface tree had one 33-command registry shared by all surfaces. | Apps and shared protocol now use a canonical 35-command registry, adding semantic-version list/select. Command operands, replies, identities, and package references are typed. | locald owns `ProductState`; Diff and semantic history/selection are wired through CLI, MCP, typed client, and desktop, including the native version action. Product state still needs stronger fsync/journal/fault evidence and several historical registry rows lack named interface routes. | **Core cutover proven; route-completeness and fault proof remain.** Complete the disposition/journey matrix and durable-state fault tests. |
| **Local/remote worker.** Deleted compiler application/server routing executed native compiler work under durable operation control. | Apps/worker, `BuiltinReplication`, CAS/closure, scheduler, and typed semantic-publication projection exist. | local and worker traverse the same semantic relation and produce byte-equal canonical payloads with reuse, fencing, and local fallback. The worker does not yet execute or lend compiler authority. | **Typed transport proven; remote compilation remains open.** Give a worker a verified language profile/toolchain recipe and bind returned images to the publication claim. |
| **Capabilities.** Historical health exposes toolchain/capability state. | Library and engine share exact `LanguageProfile`, `LanguageOracleTask`, typed toolchain/package-authority evidence, and canonical embedding recipes. | Health reports nine distinct product profiles and 18 language task slots; C and C++ remain distinct. Embedding identity commits 13 model/tokenizer/extraction/vector fields. Forged profile, tool, object-version, and embedding claims are rejected. Remote compiler authority is still absent. | **Typed local capability truth proven.** Extend the same recipe closure to remote compiler admission and withdraw it on remote tool or owner drift. |
| **Recovery and security.** Deleted journal/object-pack/source-resolution/provider tests are broad. | Source root uses openat and NOFOLLOW; peers require same effective UID; authority secrets, helper bounds, archive bounds, durable CAS, and recovery exist. | locald, worker, registry, and ingest use these controls. | **Stronger foundation.** Add registry staging symlink-race and semantic-helper substitution/restart tests; staging is path-based after checks, not descriptor-relative. |
| **Scaling and observability.** Historical root capacity, index scaling, telemetry, and real package workloads exist. | performance, laws, store, flow, replication provide structural scale suites; execution now has seven closed telemetry families plus scheduler/listener hooks. | Locald/worker default telemetry to disabled. No concrete exporter, `Telemetry::export` caller, periodic reader, tracing bridge, or product emission for compiler/search/remote/recovery exists. | **Stronger counter foundation; telemetry export and semantic scale are regressions.** Publish seven-language/provider workload envelopes and bounded observable latency/memory/recovery distributions. |

## Authority collision map

Restoring a second implementation under a new crate name does not restore a
responsibility if the active product still obtains its answer from the earlier
owner. The table names the owner selected by the current product path, rather
than the owner that should eventually win after an intentional cutover.

| Responsibility | Active product owner | Parallel restored owner | Current verdict and closure condition |
|---|---|---|---|
| Semantic compile/frontends | Add invokes `LocalCompilerHost`; view rebuild activates complete claims and renders selected image fields into generic rows, while search/graph/worker still use generic `ViewRoot` projections | `compiler-ir`, `compiler-driver`, `compiler-languages`, `compiler-publication` | **P0 partial authority.** Make the persisted compiler publication the source of every typed semantic result, then remove the lossy generic-row boundary from typed paths. |
| Version, root, replication, store | `backend-version::WorkspaceRoot`, `backend-store`, `backend-replication` | `heart-identity`, `heart-root`, `heart-object`, `heart-memory`, `heart-schema`, `heart-hydration` | **Parallel root/locality authority.** Select one model and demonstrate cold-local/remote/offline/restart behavior against it. |
| Workflow, runtime, journal | `backend-flow`, `backend-execution`, `backend-runtime`, engine/view journal | `server-workflow`, `server-runtime`, `server-journal` | **Parallel publication/recovery authority.** Route one semantic publication through the selected durable owner and fault/reopen it; do not keep two independent publication truths. |
| Search and graph/vector providers | `extensions/tantivy`, `extensions/trustfall`, and product-reachable Qdrant over a process-cached generic coordinator | `server-index-{build,core,graph-vector,publish,qdrant,tantivy,trustfall,vocabulary}` | **P0 parallel projection authority.** Preserve typed snapshot/document identities and compose one durable, generation-isolated, incremental projection route for lexical, vector, and graph results. |
| Source/index publication | engine `ProductSourceRelation`, `publish_builtin_view`, and `view_journal` | `compiler-publication` and `server-index-publish` | **P0 semantic publication collision.** A product relation claim is not a publisher. The selected owner must immutably publish and reopen the complete compiler closure before client projection. |

Each row remains failed until its product route names one authority and all
other implementations become either adapters to it or explicitly non-product
tools. Merely retaining both sources leaves identity, recovery, and cache
semantics ambiguous.

## Language-by-language audit

FrontendSet installs seven structural grammar frontends covering nine exact profiles as an explicit
fallback. Add groups compiler sources by exact `LanguageProfile`, invokes the
restored compiler for each profile, and persists Published or Unavailable
claims. During view rebuild, every Complete claim is activated/reopened and
rendered into generic rows; only its exact package/profile loses the
structural fallback. Partial and Unavailable claims remain honest structural
fallback. This corrects the former C/C++ family collapse by keying fallback
and coverage per `LanguageProfile`, not `Language`.

| Language | Active product route | Historical evidence | Verdict | Exact gate |
|---|---|---|---|---|
| Rust | Complete Rust claims are reopened and partially rendered into generic rows; otherwise structural fallback remains. Rust deadline control was restored through the rust-analyzer lowering boundary. | Deleted Rust authority/PURL and driver semantic/HIR/feature/corpus tests; workspace cfg/build-script/dependency/reference fixtures. | **Partial product reachability; rich parity still a regression.** | Cargo workspace with macro, cfg feature, build script, cross-crate reference, PURL, diagnostic, typed relation, and worker journey. |
| Python | Complete Python compiler claims follow the same route; PSR7 no longer invokes the old CPython helper. | Deleted checker/facts/quoted-annotation fixtures; workspace Pyrefly inference/reference/type-lattice corpus. | **Partial product reachability; rich type authority unproven.** | Pyrefly-grade inference/import test with cross-file modules, quoted annotations, unresolved import, type reason, coverage, and client output. |
| TypeScript | Complete TS/TSX claims are reopened and partially rendered; structural fallback remains when unavailable. | Deleted checker/authority/coordinate/error plus workspace conditional/template/mapped/Unicode/reference/NPM corpus. | **Partial product reachability; rich parity still a regression.** | tsconfig/NPM/UTF-16/conditional/mapped/template/narrowing/diagnostic/reference corpus through all clients and worker. |
| Go | Complete Go claims use the compiler route; no legacy helper facts enter PSR7. This host's Go 1.27.1 is wedged, so there is no local execution credit. | Deleted Go oracle/image/docs/serialize plus workspace cross-package/build-constraint/grpc/reference corpus. | **Untested on this host; rich parity unproven.** | Multifile package, sibling type, build tag, module replacement, missing dependency, cross-package method/docs, and remote result on a healthy host. |
| Java | Complete Java claims are reopened and partially rendered; structural fallback remains when unavailable. | Deleted javac/doclet/JAR/Maven/PURL/repo and occurrence tests; workspace overload/classpath/producer snapshots. | **Partial product reachability; rich parity still a regression.** | Maven/JAR/classpath module with overload, Javadoc, PURL, occurrence, diagnostic, and graph relations. |
| C# | Complete C# claims are reopened and partially rendered; structural fallback remains when unavailable. | Deleted Roslyn image/protocol/producer and Unicode/XML docs; workspace NuGet/oracle/lowering/reference tests. | **Partial product reachability; rich parity still a regression.** | Roslyn XML docs/nullability/Unicode/NuGet/diagnostic/cross-file graph journey. |
| C/C++ | C and C++ claims and structural fallback are keyed by distinct profiles even though both use the Clang language family. | Deleted collect/facts/ffi/input/PURL/live authority plus workspace compile_commands/include/template/reference corpus. | **Partial product reachability; rich parity still a regression.** | compile_commands fixture with include/template/type/reference/diagnostic/PURL through locald, MCP, desktop, and worker. |

The current compatibility test only guards the declaration floor:
tests/compatibility/tests/compiler_parity.rs asserts normalized declarations
and that syntax fallback does not invent cross-file semantic facts. It is not a
rich-language parity suite.

**Host execution limitation at this cutoff.** `/opt/homebrew` Go 1.27.1 is
wedged on this host and leaves kernel-uninterruptible child processes. Three
Go-dependent local-service probes timed out. Per the task direction, no more
Go or local-service probes will run here. These are host-toolchain-unavailable
results: neither a deterministic code regression nor pass credit.

## Public interface delta

The deleted interface/library/command.rs declares 33 shared commands. The
active CommandId now carries most of that vocabulary. During this audit the
raw surface payload was replaced with ProductText, PackageReference,
ProjectName/ProjectSelector/ProjectId, TreeNodeId/TreeSubject/TreeOpener,
typed SurfaceCommand operands, and typed SurfaceReply records. The registry and
process exhaustiveness gates now cover the complete typed Surface projection.

CLI retains its named commands and adds `surface <TAGGED_JSON>` for every typed
Surface command. MCP retains its named tools and adds `backend.surface`; its
operation enum is derived from the same 35-row canonical registry rather than a
second untyped grammar.

| Historical commands without a like-for-like active route | Missing behavior |
|---|---|
| source, related, read, diff | Source range, relation/vector relatedness, batch read, and version diff. |
| explore, package, dependents, owner, index-search, package-versions, package-profile | Registry browse/profile/dependents/owner/version surfaces. |
| subscribe, unsubscribe, subscriptions, releases | Follow/release product surfaces. |
| project-create, project-delete, project-add, project-remove, project-sync | Folder and lockfile project management. |
| tree, tree-open, tree-close | Shared session-tree API. |

CLI, MCP, and desktop share one locald owner and certified reply boundary.
locald owns `ProductState` and dispatches `Command::Surface`, while
`Library::execute` correctly refuses those owner-only mutations. The real-locald
matrix exercises all 24 variants through CLI and MCP. Native desktop actions
and a crash/fault persistence contract for the owner remain open.

## Unported behavior inventory

| Area | Executable historical evidence without an equivalent product journey |
|---|---|
| Canonical compiler image and IR-VCS | `compiler/ir` semantic_image, semantic_facts, type_facts, docs_facts, semantic_render, vcs; `compiler/ir-vocabulary` entity, occurrence, type_lattice; compiler/publication. IR/vocabulary source is restored under `crates/compiler-*`; Add and view rebuild now use publication claims and reopened images, but IR-VCS and typed relations have no product caller. |
| Go | compiler/languages/go image and oracle; workspace compiler/languages/tests/go cross_package_implements, cross_target_build_constraints, grpc_corpus, ref_snapshots, reference_hardening |
| Python | compiler/languages/python checker; workspace Python pyrefly_feature_path, pyrefly_inference_spot_check, refs_resolution, type_lattice_census |
| TypeScript | compiler/languages/typescript authority/checker/coordinate/error; workspace real_npm_packages, ref_snapshots, refs_resolution |
| Rust | compiler driver rust corpus/features/HIR/semantic lane/traits; workspace build_script_cfgs, dependency_resolution, ref_snapshots, universal_pipeline |
| Java | compiler languages Java doclet/JAR/PURL/repo; workspace overload_survival, producer_tests, ref_snapshots |
| C# | compiler languages C# helper and fidelity/unicode fixtures; workspace NuGet/oracle/reference corpus |
| C/C++ | compiler languages Clang collect/facts/ffi/input/PURL; workspace corpus_sweep/real_package/ref_snapshots/refs_resolution |
| Publication | server operation compiler_corpus/compiler_publication/semantic_publication; server index publish compiler_snapshot |
| Providers | server index Tantivy/Qdrant/Trustfall/retrieval/graph-vector tests; historical index semantic_search_gating/vector_adversarial/ranking_eval_product |
| Heart/server | heart hydration/object-pack/observe/telemetry tests; server journal/workflow/runtime tests; workspace2 nudox tests |
| Interfaces | interface library/protocol/MCP/GUI/core command, source, semantic-image, client-sync, and framed-process tests |

## Exact composition gates

1. Extend the existing authority → immutable generation → claim → reopened
   image path with exact PackageUrl-plus-LanguageProfile pair resolution. Diff
   borrowed reopened semantic images through compiler-ir's canonical entity/link
   algorithm and feed that typed result to local, remote, search, graph, and
   client projections. Do not make generic rendered rows the only semantic authority.
2. Add a `semantic_client_parity` journey that proves typed entity/type/doc/
   occurrence/edge/diagnostic/dependency behavior rather than presentation
   strings alone.
3. Validate the new typed Surface values with compile-fail/domain-substitution
   and protocol unknown-field tests, then compose a durable locald owner route
   for every restored operation and client route.
4. Keep exact `LanguageProfile`-keyed fallback and coverage, and add
   language-specific end-to-end tests that fail if structural declarations
   alone satisfy them.
5. Prove verified project/package manifests and toolchain closure for every
   language authority. Preserve Partial until scope is complete.
6. Make worker output an admitted semantic binding, not only a source digest.
   Add semantic_remote_matches_local_and_rejects_tool_drift.
7. At QueryCoordinator composition, bind durable incremental Tantivy or prove
   the selected ephemeral lifecycle and its per-request cost is bounded.
8. Add an owned embedding producer after typed semantic publication. Persist
   one generation-namespaced projection, avoid rebuilding or re-upserting it
   for an unchanged search, mutate only add/edit/delete deltas, and retire
   stale points only after no exact binding can reference them.
9. Project canonical semantic relations into Trustfall; retain hierarchy as a
   separate edge family.
10. Keep Add as registry acquire/stage/ingest seam; add MCP/desktop/native
   ecosystem/failure/archive-race journeys.
11. Add a checked legacy-command disposition file and all-client tests.
12. Compose a concrete bounded exporter at locald/worker startup. Cover its
    bounded queue/periodic flush, trace/log/runtime dimensions, redaction,
    failure isolation, and compiler/search/remote/recovery producers.
13. Define the publication relation as a semantic identity only: generation,
    manifest, binding, and constructor-validated coverage. Keep journal
    receipt/head facts behind the publication owner. Assert equal relation
    bytes for one generation published in two independent journals and reject
    forged partial coverage before encoding.
14. Select one root/locality authority. Exercise cold-local available-remote
    hydration/search, offline fallback, restart, single-flight stale refresh,
    poisoned outbox recovery, and a conflicting-root rejection in one product
    journey.
15. Project Health from the same `compiler_vocabulary::LanguageProfile`,
    `LanguageOracleTask`, resolved toolchain, and activated compiler owner used
    to compile/publish. Cover all seven languages and exact tool/helper/model
    withdrawal before stale results can be admitted.
16. Use the restored frame/view/object-pack boundary for the immutable compiler
    image and its remote hydration path. Prove borrowed reopen and hostile
    frame/object rejection without requiring it to replace the JSON user API.
17. Put the restored typed acquire/catalog/ingest/retrieval/routing algorithms
    behind the active command owner, rather than maintaining a parallel index
    plane. The executable gate must include snapshot continuation, typed
    reconciliation, sealed retrieval attacks, deterministic routing retries,
    and all-client output.

## Evidence ledger

| Evidence | Result | Limit |
|---|---|---|
| cargo test -p backend-engine registry --lib | Passed during this audit after REG-DECODER-001. | Registry library behavior, not full client parity. |
| cargo fmt --check -p backend-engine | Passed during this audit. | Formatting for isolated decoder change. |
| `git show HEAD` path comparison of compiler/IR/driver/publication, heart, and server-index paths | Restored source matches deleted source except expected Cargo path rewrites. The semantic-attack fixture corrects a stale five-cell header expectation to the existing six-cell schema-3 layout. | Source comparison is not a compiled or product-wired result. |
| `cargo test -p compiler-ir --all-targets --quiet` | Fresh post-graft rerun passed every target, including expanded owned/reopened entity-and-link VCS, semantic attacks, type-facts, wire attacks, image, extension, docs, occurrence, and render suites. The schema-one type-facts golden was corrected from a proven post-migration `heart.*` identity artifact to its historical `nudox.*` persisted identity. | Restored-library evidence plus borrowed-image VCS proof; broader product composition is covered by separate client and local-service gates. |
| `cargo check --workspace --all-targets --quiet` | Final post-graft target graph passed. Earlier passes found and drove repairs for 14 stale compiler-driver fixture paths and a stale `Coverage::is_complete` journey signature. | Passed with inherited warnings; executable Go-dependent and external-provider journeys remain separate gates. |
| `cargo check -p backend-engine -p backend-local-service --all-targets` | The compiler owner reports this exact all-target package checkpoint passes after the projection cutover, exact-profile coverage fix, and restored Rust deadline control. | Integrator-reported compilation evidence only; it does not prove a locald restart, seven-language source-to-client journey, external Qdrant behavior, or typed semantic output. |
| `cargo test -p backend-local-service --lib --quiet` | The final safe library gate is recorded below after the cutover. Go-dependent external execution remains disabled because the installed Go toolchain is wedged. | No credit is claimed for real Go, external Qdrant lifecycle, or remote compiler-authority execution. |
| Qdrant focused library gates | Final rerun passes `extensions/qdrant` 25/25; `server-index-qdrant` library previously passed 11/11. The extension suite includes a regression proving small searches do not ask for the default 4,096-row provider page. Evidence covers binding-scoped UUIDs, logical-ID validation, decoded-gzip bounds, bounded activation/cleanup, and shared coordinate reuse. | Provider-boundary evidence is not an external product restart or cross-process retirement journey. |
| `cargo test -p backend-engine semantic_relation --lib --quiet` | Independent fresh run passed 8/8: relation encode/decode, manifest/binding mutation rejection, publisher-owner activation, foreign manifest/binding activation rejection, exact PURL/profile key identity (including C/C++), same-key generation value distinction, and closed unavailable/trailing-byte rejection. | It does not prove cross-journal equality, forged coverage through a product boundary, an executed restart reopen, or typed product projection. |
| `cargo test -p backend-library protocol --lib` | Includes strict unknown fields, bounds, full reply shape, certificates, compact subscription behavior, and an owner-context graph-continuation regression that leaves standalone decoding strict. | Protocol proof is separate from native desktop affordances. |
| `cargo test -p backend-journeys --test surface_interfaces` | Real locald/CLI and locald/MCP processes cover all 24 typed `SurfaceCommand` variants from the authoritative 35-command registry, accepting typed success or the command's typed unavailable result. The same suite resumes and cancels a bounded Trustfall query while preserving request IDs. | Trustfall pages are materialized transport replies; this is not chunked transport streaming. Native desktop actions are not covered. |
| `cargo tree --workspace -i compiler-ir`, `cargo tree -p backend-locald`, and source consumer search | IR has restored compiler/publication/server-index consumers. Active local-service Add persists a publication claim; `view_for_workspace` now pages, activates, reopens, and renders complete claims into the generic view. | Source wiring does not prove typed relation parity, remote authority, or an executed restart/client journey. |
| tests/journeys/tests/registry_process.rs | Executable current CLI acquire/search/restart journey. | Re-run after final integration; no MCP/desktop/browse coverage. |
| tests/journeys/tests/cutover_e2e.rs | Current shared-owner client and generic remote product journeys. | Its own comments describe structural baseline, not semantic authority. |
| frontends/go/tests/native_helper.rs and frontends/python/tests/native_helper.rs | Current direct helper tests. | Helper facts are not package-complete or typed client parity. |
| extensions Tantivy, Trustfall, Qdrant suites | Concrete provider tests exist. | Leaf correctness does not establish product composition. |
| crash, concurrency, laws, performance suites | Strong recovery, contention, bounds, and structural-scale evidence. | No nine-profile semantic corpus or external provider workload executed here. |

## Adversarial bounded gates (Luna)

The independent process and boundary gates below were rerun after the
compiler-publication owner restart repair. They are deliberately bounded and
do not invoke the wedged host Go executable.

| Gate | Result | Interpretation |
|---|---:|---|
| `cargo test -p backend-compatibility-tests --tests` | **13 passed, 0 failed** | Declaration and simulated-authority compatibility floor. Go coverage is fixture-driven. |
| `cargo test -p backend-laws --tests` before the new boundary gate | **28 passed, 0 failed** | Existing law suite. The new semantic-plane boundary gate is listed separately because it is intentionally red until cutover removes the remaining adapters. |
| `cargo test -p backend-journeys --test cutover_e2e` echo and cancellation fixtures | **2 passed, 0 failed** | Bounded process protocol and cancellation fixtures. |
| `cargo test -p backend-journeys --test cutover_e2e unix_journeys::real_locald_restart_reopens_checked_workspace_and_rejects_unadmitted_completion` | **1 passed, 0 failed** | Locald restart/reopen and rejection path. |
| `cargo test -p backend-journeys --test cutover_e2e unix_journeys::real_locald_socket_dispatches_to_worker_then_reuses_fallback_bytes` | **1 passed, 0 failed** | Local/remote negotiation, fallback, reuse, and fence path. |
| `env -u COMPILER_GO_COMPILER -u NUDOX_GO_ORACLE_BIN cargo test -p compiler-languages-go --test protocol -- --nocapture` | **31 passed, 0 failed, 0 ignored** | Go decoder, mutation, ordering, and fake bounded-child evidence. The three real authority tests report an explicit configured-toolchain skip; no ambient host Go process was started. |
| `cargo test -p backend-laws --test semantic_plane_boundary` | **3 passed, 0 failed** | Tree-sitter confinement, sole semantic-adapter authority, and bounded Go-probe checks pass. Local replication, worker, and Trustfall use the canonical semantic publication relation. |
| `cargo test -p backend-store root_only_extension_durably_writes_every_registered_relation_root -- --nocapture` | **1 passed, 0 failed, 73 filtered** | Root-only closure regression: every registry-admitted secondary relation root is physically durable and present after reopen. |
| `timeout 60s cargo test -p compiler-vocabulary --all-targets --quiet` | **15 passed, 0 failed** | The complete 32-profile wire round-trip matrix passes, including distinct C and C++ profile encodings. |
| fake-Go locald readiness gate | **1 passed, 0 failed, 5 filtered** | A parent-aware hanging `NUDOX_GO` fixture leaves locald's child live while the listener becomes ready (2.65s), proving startup no longer waits synchronously on an ambient Go probe. |
| bounded CLI/MCP/desktop Trustfall journey after closure repair | **1 passed, 0 failed, 5 filtered** | Durable closure, typed Trustfall, MCP, and desktop hydration now complete through one shared locald process root in 4.83s. |

The three skipped Go authority-image tests remain uncredited until a healthy,
explicitly selected Go toolchain is available. The semantic-plane boundary is
now green: product replication and the worker project the canonical typed
semantic publication relation, while Tree-sitter remains structural fallback.

## Promotion rule

Do not call the compiler migration complete, or describe the product as a
complete typed index, until every supported language has a shipped
content-identified authority selected by locald; its typed output is persisted
and published canonically; local and remote execution agree; every client
exposes honest coverage; and normalized historical fixtures prove the behavior
users lost has returned. Apply the same rule to registry surfaces, vector
search, semantic graph queries, telemetry, and retained interface behavior.
