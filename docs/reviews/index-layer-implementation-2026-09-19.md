# Versioned index layer implementation — 2026-09-19

This cut moves package discovery into the authenticated product relation and
treats query indexes as disposable, root-fenced accelerators. Source files,
package manifests, dependency edges, and shared dependency-source coordinates
are independently versioned objects. A project retains only two sorted key
frontiers plus independent source and manifest-graph versions.

## Canonical model

- `Project`: canonical coordinate, source version, manifest-graph version,
  sorted file frontier, and sorted package-fact frontier.
- `File`: content and producer versions plus shared declaration storage.
- `Package`: manifest identity and bounded package metadata.
- `Dependency`: stable edge identity, requirement, kind, target, flags,
  features, and an optional dependency-source reference.
- `DependencySource`: one content-addressed registry, git, or path coordinate
  shared by every edge that uses it.

Keys exclude mutable edge values. Changing a version requirement, feature set,
or source replaces one edge while retaining its identity and every sibling
edge. Removing a project deletes exactly the rows named by its frontiers.
Canonical decoders bound all text, counts, files, declarations, and features
before allocating.

## Work avoidance and memory reuse

Source and package invalidation are separate. Unchanged source bytes and parser
versions reuse the exact declaration `Arc`. A deterministic digest over every
admitted `Cargo.toml` gates the package graph; an unchanged digest skips
`cargo metadata` and reuses the prior facts. Package and dependency text uses
shared immutable slices, so the warm path clones pointers rather than graph
bytes. The manifest digest walk rejects symlinks, generated trees, oversized
manifests, excessive manifest counts, and excessive aggregate bytes.

Cargo is used offline with `--no-deps`; a bounded TOML parser remains the
fallback. Both paths retain aliases, dependency kind, optionality, default
features, explicit features, target predicates, and registry/git/path source
coordinates. Package and dependency facts are projected into the live library
view, so CLI, MCP, and desktop queries consume the same versioned rows.

## Turso and Tantivy cache

Turso remains derived state. Its metadata row binds the complete SQL snapshot
to one immutable view root and version. Schema mismatch fails closed; there is
no migration path in this greenfield cache. A fresh cache rebuilds from the
canonical root.

The projection enables Turso's native FTS index method, backed by Tantivy.
Cold rebuilds use 512-row SQL statements, producing a bounded number of
immutable search segments, then run one `OPTIMIZE INDEX` before committing the
root fence. Hot deltas append only their changed rows. Root metadata, row
changes, commit audit data, and FTS updates commit atomically.

## Measured results

All numbers are real debug-build measurements on the development machine.

| Probe | Result |
|---|---:|
| Package graph construction | 50,000 dependency edges in 243.0 ms |
| Canonical package graph size | 8,488,992 bytes, 169.8 bytes/fact |
| Source-coordinate interning | 12.4% smaller than repeated inline coordinates |
| Turso/Tantivy cold build | 20,000 rows in 5.99 s |
| Rare two-term FTS query | 5.03 ms |
| Broad two-term FTS query, limit 25 | 3.51 ms |
| One-row SQL/FTS delta | 22.19 ms |
| SQL/FTS cache size | 12,496,896 bytes |

The first FTS layout issued one statement per row. It took 219.34 s to build,
3.17 s for a rare query, 1.78 s for a broad query, 2.43 s for a one-row update,
and occupied 64.79 MB. Batched segment construction made the cold build 36.6×
faster, rare search 631× faster, broad search 506× faster, one-row updates 110×
faster, and the cache 5.2× smaller.

The filesystem stress probe remains reproducible at 2,048 files. Its last run
measured 246.23 ms cold, 23.25 ms unchanged, 23.86 ms for one edit, 23.71 ms
for one rename, and 26.30 ms for one deletion while reusing 2,049 unchanged
declaration allocations.

## Verified properties

- A manifest-only requirement edit preserves the source version and exact
  declaration allocation, retains all object identities, and changes one edge.
- An unchanged manifest graph reuses the complete fact set and shared edge
  strings.
- Project and fact frontiers reject duplicates and noncanonical order.
- Package, dependency, source, and project records round-trip through the
  bounded canonical codec.
- SQL replay is idempotent, stale-root updates fail closed, and cold rebuilds
  recheck the root fence inside the transaction.
- Engine, local service, and Turso suites pass 157 tests; the earlier complete
  desktop/library integration run passes another 88 tests.

## Remaining owner-path work

The canonical storage and indexed cache are now suitable foundations, but four
owner-path costs still deserve direct cuts:

1. View publication still hydrates the selected relation and constructs a full
   target view before computing a delta. It should project relation changes
   directly into row changes and reserve full hydration for recovery.
2. Remote forge and registry coordinates still need a content-addressed
   acquisition object with credential-scoped fetch, immutable revision pins,
   partial-clone support, and negative-cache leases. A non-directory add must
   never silently become an empty project.
3. Workspace dependencies are searchable versioned edges, but a dedicated
   forward/reverse adjacency arrangement is still needed for constant-time
   dependency and dependent traversal across packages and repositories.
4. The desktop still owns a second presentation metadata parser. Its README
   projection can remain a UI concern, but package facts must be read from the
   canonical graph so every surface shares one resolver.

These are cutover tasks rather than reasons to weaken the new record model.
The stable identities, independent versions, exact frontiers, and root-fenced
cache are the boundaries those changes should preserve.
