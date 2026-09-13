# Complete old-to-v2 cutover ledger

Status: cutover contract, 2026-09-12. The legacy `compiler/`, `heart/`,
`interface/`, and `server/` trees are capability oracles only and are outside
the root workspace. `server/index/turso` is outside this comparison. No legacy
directory may be deleted merely because the v2 workspace compiles.

## Decision and current P0 gaps

The structural cutover is credible: 56 legacy crates collapse into 28 product
packages with a single authority spine, a launchable desktop, a local daemon,
a real root-fenced Turso projection, and async Trustfall. The complete product
cutover is not yet releasable because three old capabilities have no current
executable replacement:

1. **Native ecosystem registry adapters and the full package journey.** The
   engine now owns a canonical bounded HTTP acquisition effect with typed
   coordinates/cursors, immutable artifact publication, and restart-safe feed
   progression; local-service exposes that composition seam. Native
   crates.io/npm/PyPI/Maven/NuGet/Go/C++ API translators, signature or
   transparency-log verifiers, and the compile/publish/query J4 process matrix
   remain to be exercised against pinned external fixtures.
2. **Concrete lexical and complete semantic execution.** The Tantivy leaf has
   strong admission and incremental-overlay contracts but no Tantivy engine.
   Qdrant now has a bounded authenticated HTTP client and admitted `AnnSource`,
   but the product has no model acquisition/inference path or real-Qdrant
   process journey from semantic facts through search.
3. **Shipped semantic compiler authorities.** All seven frontends execute local
   tree-sitter syntax extraction. Their richer native semantic paths require
   explicit external helper executables; the repository does not yet provide
   a packaged, end-to-end helper matrix proving parity with the old compiler.

These are implementation gaps, not reasons to reintroduce old ownership. Add
remote acquisition as an engine effect, native authorities as frontend leaf
providers, Tantivy as a disposable root-bound extension provider, and model
production/composition above the root-bound Qdrant provider.

The 2026-09-09 `luna-backend1-cutover.md` review remains evidence for what the
old product did, but several negative findings are stale in this checkout. The
root now has a Turso dependency and implemented derived projection;
`extensions/trustfall` now depends on Trustfall and calls
`execute_query_async`; `apps/desktop` now has a GPUI `main`; and
`backend-local-service` composes deterministic seven-language local syntax
ingestion. Release review must inspect current code rather than repeat the old
24-package/no-GUI/no-Trustfall conclusions.

## What is being simplified

The raw inventory is reproducible with `find`, `wc`, the manifests, and
`docs/architecture/package-dag.json`. Counts deliberately omit tests/tools
from the target package count and omit `server/index/turso` from the legacy
tree.

| Measure | Legacy product trees | v2 product graph | Change |
|---|---:|---:|---:|
| Product manifests | 56 | 28 | -28 (-50.0%) |
| Direct internal `[dependencies]` edges | 213 | 76 | -137 (-64.3%) |
| Rust source files | 836 | 613 | -223 (-26.7%) |
| Rust source lines | 308,693 | 198,101 | -110,592 (-35.8%) |
| State/owner-like declarations matching `State|Status|Phase|Owner|Coordinator|Runtime|Service|Engine` | 328 | 60 | -268 (-81.7%) |
| Root/head publication authorities intended after cutover | compiler publication + index publication + catalog + workflow + GUI/service paths | `backend-store` selected head through `backend-engine` | many to 1 |
| Product command definitions intended after cutover | CLI, GUI, MCP, interface/core/library/search variants | `backend-library::Command`/`Query` | many to 1 |

The 28-package count deliberately excludes `tools/control`. `backend-control`
has zero runtime product dependents and governs repository-agent promotion,
so classifying it as tooling removes one package and its two store/version
edges from the product DAG. This is an architectural boundary, not count
cosmetics: product code cannot import the promotion control plane.

LOC is not itself a design goal. The meaningful contraction is 141 direct
internal edges and 268 owner/state-shaped declarations: fewer components can
invent progress, recovery, cancellation, or publication. Re-run the counts at
the deletion commit because parallel integration work can change the v2 side.
Use `cargo metadata --no-deps --format-version 1 --offline` as the final crate
count; a hand-maintained JSON count is supporting evidence only.

## Mechanisms to delete and their stronger replacements

| Duplicated legacy mechanism | Delete from | Stronger invariant in v2 | Proof required before deletion |
|---|---|---|---|
| Multiple content/generation/snapshot ID wrappers and unchecked byte/string conversion | `heart-identity`, compiler/index vocabularies, interface identity | Domain-marker IDs and checked canonical constructors in `backend-version`; cross-domain substitution does not type-check | Compile-fail identity laws plus persisted-fixture decode |
| Compiler generation, index snapshot, catalog head, workflow progress, and GUI-local progress as peer authorities | compiler publication, index publish/catalog, server workflow, interface hosts | Store owns one immutable selected `WorkspaceRoot`; engine alone holds publication authority; `ViewRoot` and backend indexes are derived and root-fenced | Crash at each publication boundary yields old or new root, never mixed; stale provider cannot advance head |
| Full-image ownership and repeated full-image diff at compiler, ingest, build, and each backend | compiler IR/publication, index ingest/build/publish | Typed relation delta with exact base/target, producer/schema/read manifest, frontier, and coverage witness; retained arrangements update only changed keys | Shared corpus delta equals cold rebuild after add/edit/delete/re-add |
| Per-subsystem schedulers and retry loops | compiler application/driver, server runtime/workflow/routing | `backend-execution` owns reservation -> attempt -> terminal typestate, cancellation, fencing, waiter identity, hedge/fallback, and resource accounting | Process loss, duplicate completion, cancellation, reconnect, and resource-law journeys |
| Hydration, memory, object-pack, and view lifecycles with overlapping residency concepts | heart hydration/memory/object/object-pack/view | Store-owned immutable handle/pin/lease; flow-only mutable scratch; replication-only transfer custody | GC cannot collect any pinned/leased/catalog/transferring object; recovery releases abandoned custody |
| Language umbrella and driver import of all native implementations | compiler languages/driver/application | `backend-compile` defines an authority protocol; each `frontends/*` leaf supplies a closed language implementation; local-service registers an explicit fixed set | Seven-language positive/negative helper process matrix; adding a language changes the registry exhaustively |
| Backend-specific canonical identities and publication | index Tantivy/Qdrant/Trustfall/catalog | Extensions are derived providers. Every request/result binds workspace root, relation root, recipe, read manifest, frontier, and coverage; provider IDs never cross admission | Provider substitution/stale-root/forged-coverage tests plus real backend restart journey |
| Three product protocol/facade stacks | interface CLI/MCP/GUI/protocol/core/library/search | One `backend-library` command/query algebra, one `backend-client::Session`, replication wire mechanics, thin app adapters | Same corpus and basis produce golden-equivalent CLI/MCP/desktop answers and errors |
| Catalog as a second canonical database | index catalog | Canonical package/source/view relations live in the store; Turso is transactionally synchronized to an immutable `ViewRoot` and can be deleted/rebuilt | Hot delta equals cold rebuild; corrupt/delete Turso then rebuild without changing workspace root |
| Poller-local crawl clocks and catalog-local source movement | acquire/registry/catalog/ingest | One durable typed `FeedCursor<Registry, Schema>` committed with the acquired input manifest; advancement and publication share an effect receipt | Kill before/after fetch, admission, publication, and acknowledgement; no skipped or duplicated version |
| GUI-owned refresh and embedded service state | interface GUI/GUI2 | `backend-local-service` is the owner; desktop holds a client session and certified bounded cursor subscription | Two clients observe the same ordered deltas; gap forces explicit bounded reset |

The key implementation rule is that a new provider cannot add a new progress
enum visible to the engine. Its only durable facts are an engine effect receipt
and immutable provider objects named by the root-bound recipe. Recovery derives
what to retry from those facts.

## Product journeys that close the cutover

Each journey starts from a clean temporary data root, crosses a real process
boundary, asserts typed identities and user-visible behavior, kills/restarts at
named boundaries, and writes a machine-readable receipt. In-process tests may
diagnose a failure but cannot satisfy a journey.

| Gate | End-to-end journey | Required assertions | Closes |
|---|---|---|---|
| J1 Local syntax product | Start `locald`; `backend-cli add <fixture-dir>` containing all seven languages; query packages, outline, document, name, search, graph; open desktop; repeat through MCP | Same certified basis and logical rows through all clients; unsupported/binary/symlink/oversize files fail or skip by declared policy | Local acquire/compile/view/protocol composition |
| J2 Incremental project lifecycle | Index, edit one file, add one, delete one, rename one, re-add identical bytes, remove project; restart between every transition | Hot result equals cold rebuild; unchanged content reuses analysis; tombstones hide stale rows; cursors are monotone and bounded | Ingest/build/publish simplification |
| J3 Native semantic matrix | Run the shipped helper for Clang, C#, Go, Java, Python, Rust, and TypeScript on positive, malformed, unresolved, dependency, and cancellation fixtures | Helper executable/toolchain/config identity is in the manifest; semantic facts match legacy oracle after normalization; no syntax fallback is labeled semantic | Compiler crates and IR readers |
| J4 Registry acquisition | For all supported ecosystems, fetch a pinned fixture registry/archive through the production adapter, verify checksum/provenance, compile, publish, query, advance feed, restart, and resume | Typed registry coordinate and cursor; bounded archive/network behavior; idempotent refetch; no watermark advance without committed input/publication | `server-index-acquire` and `server-index-registry` P0 |
| J5 Tantivy provider | Materialize a fixed corpus to real on-disk Tantivy; lexical/prefix/field query; edit/delete/re-add; kill during refresh; reopen and page | Root/recipe/read-manifest/coverage binding; deterministic final ranking; stale index rejected; hot overlay equals rebuilt index | `server-index-tantivy`/explorer P0 |
| J6 Qdrant semantic product | Acquire/pin a model, derive document/query vectors, start a real pinned Qdrant, publish through `QdrantHttpClient`, query/rerank through the product, edit/delete/re-add, inject stale provider IDs, kill/restart provider | Model/metric/treatment/root binding retained; collection schema and projection count verified; approximate quality never promoted to exact; tombstones and refill are correct; bounded authentication/retry behavior holds against the real service | Remaining `server-index-qdrant` P0 |
| J7 Trustfall and client parity | Execute the same nontrivial graph query over an ingested corpus directly and through MCP; cancel midstream and page | Async query uses the pinned fork, rows share one immutable view binding, terminal is exactly once, answers match normalized legacy graph oracle | Trustfall/interface deletion |
| J8 Remote compute | Start locald and worker over the production transport; transfer changed Merkle frontier, execute a real semantic/view recipe, hedge/fallback, disconnect/reconnect, restart both | Remote and local canonical bytes match; losing attempt cannot publish; pending custody survives failure; warm update transfers only changed closure | Runtime/workflow/routing deletion |
| J9 Recovery and operations | Clean install/configure; run J1/J4; kill at store journal, effect journal, feed acknowledgement, derived projection, and client delivery; corrupt each rebuildable projection | Canonical head is old/new only; feed resumes; pending work retries; Turso/Tantivy/Qdrant rebuild; health/readiness explain recovery; shutdown drains or records custody | Journal/catalog/telemetry/setup deletion |
| J10 Shared benchmark | Run old and v2 release builds on the identical pinned multi-ecosystem corpus | Acquire/compile/publish/index/query p50/p95/p99, peak RSS, retained bytes, disk bytes, restart time, transfer bytes; machine/toolchain/repetitions recorded | Performance and capacity sign-off |
| J11 Rollback | Promote v2 against a migrated copy, write/read data, stop it, invoke documented rollback or forward-only recovery | No old binary opens an incompatible path accidentally; snapshot/export restore is timed and checksummed; user-visible rollback point is explicit | Irreversible cutover authorization |

J1/J2 should extend the existing process journeys rather than add a parallel
harness. J5/J6 should implement the provider traits already enforced by the
extension leaves. J4 must introduce one generic acquisition effect and typed
registry adapters; it must not create seven poller state machines.

## Deletion ledger

Deletion happens by capability slice, not by top-level directory. A slice may
be removed only when its journeys pass from a clean checkout and an `rg` scan
shows no product manifest, source, configuration, generated code, fixture, or
documentation path importing it.

| Order | Delete | Preconditions | Rollback asset retained |
|---:|---|---|---|
| 1 | Superseded heart crates | J2, J8, J9; identity/store/flow/execution laws green | Persisted object/root/journal fixtures and decoder |
| 2 | Interface core/protocol/identity/document/search crates | J1, J2, J7; golden answers/errors accepted | Protocol golden corpus and command mapping |
| 3 | Old compiler umbrella/driver/application/publication/vocabularies | J2, J3 and corpus fact diff clean | Raw sources, old normalized fact output, IR import tool |
| 4 | Seven old compiler-language crates | Each language’s J3 row passes independently | Per-language source/toolchain/output oracle fixture |
| 5 | Server operation/runtime/workflow/journal | J8/J9 and resource/crash laws pass | Journal fixtures and recovery reader |
| 6 | Index core/build/ingest/publish/vocabulary | J2, J4, J5, J6, J7 hot-vs-cold equality | Normalized snapshot/query oracle |
| 7 | Index Trustfall/retrieval/routing | J7/J8 and full client parity | Graph query corpus and normalized rows |
| 8 | Index acquire/registry/explorer/Tantivy/Qdrant/catalog | J4/J5/J6/J9; feed and backend recovery proven | Pinned registry responses, backend data snapshot, migration/export tool |
| 9 | Old GUI and GUI2 | J1/J2 plus explicit product-feature keep/drop decision | UI journey script and screenshots are supporting evidence only; data stays in canonical export |

At every order boundary:

1. Run `cargo metadata --no-deps --format-version 1 --offline` and archive the
   exact package/edge graph.
2. Run the complete workspace gate and the named journeys in a clean target and
   clean data root.
3. Search `Cargo.toml`, Rust source, Nix/Nu configuration, tests, generated
   inputs, and docs for both crate names and paths.
4. Remove the now-unreachable source and configuration in one reviewable
   change; never leave a shadow implementation “for fallback.”
5. Exercise J11 before deleting the rollback asset for that slice.

## Release acceptance

Complete cutover means all of the following are simultaneously true:

- J1–J11 have immutable receipts tied to the same source revision and pinned
  dependencies.
- Every one of the 56 rows in `docs/architecture/capability-map.md` is Closed or
  explicitly Removed by an approved product-scope decision; no row is Partial
  or Missing.
- Cargo metadata contains only the v2 product graph and intended test/tool
  packages; no legacy manifest is reachable.
- The selected-head mutation path has one owner, every provider is rebuildable
  from canonical roots, and no compatibility adapter can publish.
- A clean-machine setup reaches the local and registry journeys without
  unpublished helper binaries or ambient services.
- The measured benchmark records an accepted regression budget and the
  rollback drill meets its recovery objective.

Only then is deletion evidence stronger than coexistence. The desired end
state is not “old plus v2 behind adapters”; it is the 28-package typed engine,
with the old capability corpus retained as fixtures and receipts rather than a
second executable architecture.
