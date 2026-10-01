# Index tentpole: interpreted notes and executable laws

Status: design and cutover brief, 2026-09-27. The source is the user's ten photographed notebook pages, `IMG_3551 2.HEIC` through `IMG_3560 2.HEIC`. I inspected each page and overlapping top/bottom crops. The photographs are design input; uncertain handwriting is recorded as uncertainty rather than converted into a requirement. This brief is read together with [compiler-artifact-streaming](compiler-artifact-streaming.md).

## The unifying object

The index is a **versioned relationship graph** with one selected frontier per workspace branch, package source, and environment. Turso holds authoritative, transactional *bindings* from stable identities to immutable facts and object roots. The content-addressed store holds source, compiler IR, docs, and derived artifact closures. Tantivy, graph traversal, ANN, and GUI shelves are projections over a pinned frontier, never separate authorities. The compiler is a horizontally placed producer of typed closures; the index is the vertically coherent selector and query planner.

This yields one query contract for GUI, CLI, MCP, and remote readers:

```text
QueryContext = (package coordinate, source authority, branch,
                environment, selected frontier, coverage requirement)
QueryResult  = Complete<T> | Pending<T? + reason> | Unavailable(reason)
             | Stale<T + observed_at + desired_frontier>
```

The distinctions matter. A registry may know a release and its metadata before IR exists. A package may have IR in a remote store but not yet have an indexed head. A local branch can exist without a published release. Download counts can be old even when immutable release facts are current. No consumer may manufacture a semantic generation to make these cases look complete.

## Identity and scope

Use framed, domain-separated identities, not string concatenation or path-dependent snapshot hashes:

* `PackageSource` identifies registry, code forge, local path, or vendored source and the authority that asserted it. Same package name from two sources is distinct.
* `PackageLineage` identifies the logical package through releases and local branches. `Release` identifies an immutable published artifact. `RegistryObservation` identifies time-varying yanks, advisories, downloads, and popularity with source and observation time.
* `BuildRecipe` commits native toolchain, feature/cfg set, target platform, environment variables that affect semantics, lockfile resolution, and known-negative reads. `SessionLineage` excludes the edit revision but includes compatibility boundaries; `CompileAttempt` includes the exact input root and attempt fence.
* `GenerationRoot` commits a typed manifest of semantic planes and coverage. `LayoutId` is a physical storage variant; repacking cannot change `GenerationRoot`. `SelectedHead` binds a generation and registry observation to a branch/frontier in one index transaction.

Use stable declaration identities plus a package/target namespace for search document IDs. Revisions change payload versions, not identities for every unchanged symbol. A structural edit may alter a declaration's fingerprint; re-pair only where the candidate set is unambiguous and carry that uncertainty to consumers.

## The package DAG is a query plan

A project manifest expresses requirements; a lockfile plus target environment resolves exact package identities. The resolved graph can share transitive dependencies and platform variants. Each node has a source/metadata/IR state and an exact `have` witness. A local query first reads its selected local frontier. For missing dependencies it requests only absent object IDs and semantic planes from a remote index/store; the scheduler decides whether to await an already queued remote compiler, compile locally for interaction, or assign a remote node. Remote work must never gate an already answerable local query.

The handwritten notes emphasize a local *branch marker* even where there is no meaningful published version, and a small local set of packages whose IR differs from published IR. Model that as `BranchOverlay { base: PublishedHead?, local_source_root, local_generation?, environment }`, not a fake release. An overlay composes with a pinned dependency DAG; package and symbol resolution are deterministic for the same `QueryContext`.

## Storage and search relationship

The SQL model is a tree of declarations and explicit directed graph edges, with parent and child relationships and stable keys. The notebook's `P/Q` idea describes bounded parent/child neighborhoods: for a function, parent and ancestor context on one side, its direct children on the other. Derive these bounded windows and structural grams as *search features* from a complete public API tree. Do not bake them into IR identity or use a fixed-radius neighborhood as the only semantic diff; a deep type/signature change can have wider consequences. Internal compiler IR remains richer than the public searchable projection.

Partition immutable semantic facts by package, target, plane, and stable-key ranges. A changed symbol should rewrite only its affected segments and Merkle path. Large docs/source blobs have separate content-defined chunks. Keep the local hot layout and remote S3 pack layout as two physical arrangements for the same logical object IDs. Readers pin a layout manifest; compaction may replace packs after a safe epoch. A remote worker can stream to the index or directly to scoped object storage, but only the index owner can select the new head after checking a complete, durable closure and exact input fence.

Tantivy is the package/symbol full-text projection of selected facts. Its commit marker must name the exact index frontier. On startup or projection lag, replay deltas from the durable selected log or rebuild from a pinned root. A stale Tantivy cursor cannot be presented as current; either answer from an exact delta overlay or return an explicit stale/pending state. Graph edges, registry facts, and semantic docs obey the same rule. Search filters include ecosystem, source, version/branch, language, platform, feature set, advisory/yank state, and coverage; ambiguous source identity is never silently merged.

## Publication and failure laws

1. Immutable bytes, manifests, and closure proof become durable before an index transaction selects them. A crash before selection leaves collectable orphans; a crash after selection has a readable closure. S3 upload success is a storage receipt, not publication.
2. A generation is valid only for its exact source/config/lockfile/toolchain/environment read set. Editing an unrelated file does no compiler work. Adding a formerly missing import, changing config, or recovering a failed toolchain invalidates the affected cells even if source file bytes are unchanged.
3. A graph projection's reuse gate compares the selected root **and** its facts witness. Same root with changed dependency facts must update edges. Every identity hash frames variable fields and records all scopes, requirements, and source authorities.
4. Data source absence is not an empty result. Registry errors, code-forge-only projects, malformed manifests, yanked releases, and security warnings retain explicit source/freshness/coverage status. A malformed member cannot erase unrelated already admitted packages.
5. Local and remote compiler nodes use the same request, IR encoding, and closure protocol. Local edits may be queued for remote replication while immediately visible locally. Concurrent remote attempts are fenced; stale completions never overwrite newer local or remote heads.
6. Query-time object hydration is range/plane bounded. Cache admission is byte-budgeted and keyed by stable logical IDs plus environment where relevant. Cold restart from an empty hot cache must produce the same answers as warm reads at the same frontier.

## Cutover sequence and evidence

Each slice must state its invariant before implementation, and its tests must distinguish a correct implementation from the plausible bug being replaced. Dan Luu's [agentic-testing study](https://danluu.com/agentic-testing/) finds that agents often use test-library names while testing weak or irrelevant properties. Here the acceptance bar is a separate reference model or exact observed production fixture, fault injection at the relevant boundary, and a mutation check that a deliberately broken reuse/freshness/closure rule makes the test fail.

The dependency order is: canonical identities and fact witnesses; durable CAS reception and closure admission; exact compiler read manifests and retained session lineage; one package/branch/environment frontier; registry/source observations; Tantivy/graph/ANN projections; local/remote placement and hydration; GUI/CLI/MCP parity. A slice may be developed concurrently if files are disjoint, but integration selects one coherent frontier before declaring any surface complete.

Record p50/p95/p99 warm and cold latency, changed bytes, bytes allocated, peak RSS, read/write amplification, object-store requests and billed bytes, rebuild ratio, and no-op work. Run multi-language projects, multiple package sources, yanked/retagged releases, code-forge-only projects, malformed/oversized files, S3/index outages, and restart at each durability boundary. Compare results against the current branch and the old checkout; do not assert a performance gain from a design sketch. Gate final cutover on GUI, MCP, CLI, local offline, remote sync, and cold-restart journeys reading the same selected facts.

## Ambiguities left as experiments

Some handwritten phrases on pages 3552, 3554, and 3559 are not legible enough to state as exact requirements. In particular, the proposed `P/Q` window/distance bound and whether remote direct storage should be preferred over index streaming need measured locality and cost data. The typed contracts above permit both without giving either a separate semantic authority.
