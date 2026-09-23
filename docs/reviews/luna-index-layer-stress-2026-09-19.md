# Index-layer stress baseline

Date: 2026-09-19
Branch: `codex/versioned-engine-v2`

This is the first adversarial baseline for the live package-index path. It
separates behavior that has been measured from architecture that exists only
in design documents. The exercised path is:

```text
local directory -> syntax scan -> ProductSourceRelation -> library view
                -> Turso projection -> desktop/CLI/MCP query surfaces
```

The requested production path is broader:

```text
typed coordinate -> acquisition/cache -> immutable source revision
                 -> package/manifest/dependency resolution
                 -> versioned index delta
                 -> lexical, graph, SQL, and interface projections
```

The second path is not implemented end to end yet.

## Results

### Source ingest

The ignored `stress_scan_project_reports_reuse_and_mutation_costs` test in
`crates/local-service/src/builtin/ingest.rs` creates 2,048 Rust files, one
vendored Rust file, and file/directory symlinks. A representative debug run:

| Operation | Time | Declaration allocation reuse |
| --- | ---: | ---: |
| Cold scan | 246.23 ms | cold |
| Unchanged scan | 23.25 ms | 2,049 / 2,049 |
| One-file edit | 23.86 ms | 2,048 / 2,049 |
| Rename | 23.71 ms | measured by root change |
| Delete | 26.30 ms | measured by root and row-count change |

Unchanged roots were deterministic. Edit, rename, and delete changed the root.
Symlinks were skipped. Vendored source was indexed.

The result proves parse/declaration allocation reuse. It also disproves full
delta locality: every warm scan still enumerates, stats, reads, hashes, and
encodes every supported file. Warm unchanged and one-file-edit times are nearly
identical. The current scan also has fixed 512 KiB/file, 64 MiB/project, and
100,000-file limits; one malformed supported file aborts the whole project.

### Search arrangements

Release-mode synthetic rows produced:

| Rows | Cold build | Narrow query | Broad query | One-row update |
| ---: | ---: | ---: | ---: | ---: |
| 1,000 | 60.9 ms | 0.026 ms | 1.41 ms | 45.1 ms |
| 10,000 | 796 ms | 0.070 ms | 17.6 ms | 501 ms |
| 50,000 | 5.31 s | 0.276 ms | 111 ms | 2.82 s |

At 50,000 rows, a one-row update performed 38,033 index probes, copied 12,636
nodes, and reused 583. Query arrangements are useful, but update work is not
proportional to the changed row yet. Broad queries collect and sort an
unbounded candidate set before applying the requested page.

Stress testing found a correctness defect: postings covered display labels but
not signature/document-only terms. The arrangement now indexes the complete
searchable projection, and update/remove/re-add regression coverage passes.

The crate named `backend-extension-tantivy` has no Tantivy dependency and is
not consumed by the live product. It implements an in-memory ordered-map base
and overlay contract. The production search path is the library arrangement;
Turso separately performs substring `LIKE` scans. This naming and composition
must be resolved before claiming Tantivy-backed package search.

### Turso projection

The current Turso database is a disposable current-view accelerator with three
tables and two indexes. It is not the authoritative versioned package store.
Rebuild performs `DELETE` followed by one upsert per row. Hot transitions issue
one SQL mutation per changed row. Search uses leading-wildcard `LIKE` across
label, signature, and document, which the query planner reports as a scan.

The live workspace sample contained 20,547 projected rows and 4,863,543 bytes
of searchable text:

| File | Bytes |
| --- | ---: |
| `projection.turso` | 20,709,376 |
| `projection.turso-wal` | 84,183,992 |
| `view.journal` | 28,509,196 |
| `workspace.journal` | 9,261,257 |

On a noisy development machine, a fresh-process missing-term scan over those
rows averaged 75.3 ms, versus 33.0 ms for a common term that reached its limit
early. These timings are diagnostic only; the query plan is the structural
finding.

Stress testing fixed four projection correctness gaps:

- metadata and rows are now read in one transaction;
- already-committed delta replay returns reuse instead of a stale-transition
  failure;
- malformed root/version/row-count metadata fails closed; and
- rebuild rechecks its root fence inside the write transaction before deleting
  rows.

The retained commit table is capped at 128 audit roots and exposes no history,
checkpoint, or garbage-collection API.

### Metadata and package graphs

The workspace currently resolves 36 Cargo packages and 173 dependency
declarations: 115 path, 52 registry, six Git, and seven target-specific
declarations. `cargo metadata --no-deps --offline` produced a deterministic
84,840-byte description in roughly 40-60 ms.

The desktop metadata projection now uses Cargo metadata when available and
retains aliases, real package names, source kind, optionality, target,
features, and default-feature state. Its deterministic manifest fallback now
expands workspace patterns, applies excludes, skips ignored/symlinked trees,
and retains dependency dimensions. Git `remote.origin.url` is discovered for
an already checked-out forge-only project.

This remains a presentation-layer improvement. Package/version/manifest and
dependency facts do not exist as canonical versioned relations. There is no
reverse-dependent arrangement. Cycles, diamonds, multiple versions, feature
selection, target selection, and dev/build scopes cannot be queried end to
end. Trustfall currently models generic graph rows and symbol parent/child
edges, not a package dependency graph.

Graph delta validation no longer clones every retained value vector. The
remaining overlay merge still clones its complete replacement and tombstone
maps for each delta.

### Forge-only packages

An HTTPS, SSH, SCP-style, or `file://` repository coordinate is not acquired.
`Command::Add` tests whether the raw string is a local directory; every other
coordinate is accepted as an empty project. The successful response therefore
conflates a real empty project, a registry coordinate, an offline cache miss,
an unavailable forge, an auth failure, and an unsupported transport.

The product has no active repository adapter, ref resolver, immutable checkout
cache, source selector, package-subdirectory selector, mirror identity, or
acquisition outcome. The full URL/source matrix and the behavior available as
an oracle in `backend_1` are recorded in `docs/reviews/luna-forge-index.md`.

## Repairs validated in this pass

- Full-text postings now cover names, signatures, and documentation.
- Turso root/row reads are snapshot-consistent and delta replay is idempotent.
- Turso metadata corruption fails closed and rebuild closes its fence race.
- Trustfall graph validation borrows retained values instead of cloning them.
- Cargo metadata projection retains dependency and feature dimensions and has
  deterministic fallback behavior.
- Local ingest has executable ignore-policy and 2,048-file mutation probes.
- Forge behavior has a documented local-fixture test matrix and explicit P0
  failure modes.

Integrated validation passed:

```text
backend-desktop             25 passed
backend-extension-trustfall 16 passed
backend-extension-turso      1 passed
backend-library             63 passed
backend-local-service       41 passed, 1 opt-in stress test ignored
opt-in ingest stress         1 passed
```

## Failure hierarchy

### P0: correctness and product identity

1. Replace the local-directory/empty-placeholder branch with typed source
   coordinates and typed acquisition outcomes.
2. Add canonical relations for repository, resolved revision, source blob,
   package, package version, manifest, dependency declaration, resolved edge,
   and acquisition/coverage state.
3. Stage or acquire one immutable source snapshot before parsing. Bind every
   declaration and span to its source blob; retry or mark partial when a live
   worktree changes during capture.
4. Emit one exact `IndexDelta` from discovery through metadata, semantic,
   dependency, lexical, graph, SQL, and interface projections. Remove the live
   full-view hydration/diff path.
5. Stop reporting remote/registry inputs as successfully indexed empty
   packages.

### P1: work avoidance and scale

1. Replace whole-project file frontiers with independently keyed project/file
   membership or chunked persistent membership roots.
2. Add a durable file-identity cache and source CAS so an unchanged warm scan
   avoids reads and hashes, with safe fallback for coarse timestamps and
   changed inodes.
3. Use cost-aware bounded work stealing rather than equal path-count chunks.
4. Replace map-cloning lexical/graph overlays with immutable delta segments,
   tombstone bitmaps, byte-bounded leases, and scheduled compaction.
5. Use keyset cursors and bounded top-k selection. Normalize indexed text once
   at ingest rather than rebuilding lowercase searchable strings at query
   time.
6. Maintain forward and reverse dependency arrangements keyed by exact package
   version, source, edge kind, target predicate, and feature activation.
7. Keep Turso rebuildable from canonical objects, add content/count integrity
   checks, and use a real FTS/Tantivy projection for lexical search.

### P1: evidence

Every experiment should report files enumerated/read/hashed, source and CAS
bytes, parse reuse, objects/nodes visited/copied/reused, rows hydrated,
changed rows, posting candidates, allocations, retained RAM, SQL statements
and bytes, remote bytes, and CPU avoided. Counters are checked against an
independent rebuild oracle; they are never accepted as their own proof.

The next scale grid is 2k/10k/100k/1M files and rows, crossed with unchanged,
one-file body edit, docs-only edit, rename, duplicate declaration insertion,
dependency-only edit, branch movement, crash/replay, corruption, offline cache
hit/miss, and local/remote parity.
