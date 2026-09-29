# Compiler and IR lineage audit

Audited against `1febd758f` on 2026-09-29. The compiler and semantic IR in this
tree are established production foundations. New output-layout work must extend
them. A matching commit message or tree on a sibling branch is historical
context, not proof that the commit is an ancestor of this checkout.

| Foundation in the active ancestry | Commit | Current production boundary |
| --- | --- | --- |
| Borrowed Rust semantic authority | `05bce6487` | `frontends/rust/src/legacy/authority.rs` |
| Cargo edition discovery and loaded-graph edition checks | `c06c6b674`, `1f4e0525e` | Rust authority and package compiler |
| Canonical compiler IR in `backend-semantic` | `f67484332` | `crates/semantic/src/ir` |
| Borrowed canonical entity/link VCS | `f67484332`, `4faee5661` | `crates/semantic/src/ir/vcs.rs` |
| Strict versioned segment delta cursor | `4faee5661` | `crates/semantic/src/ir/versioned.rs` |
| Immutable-backing semantic-image proof cache | `94fa7ad42` | `crates/semantic/src/ir/semantic_image/full_wire/view.rs` |
| Package-wide Rust analyzer reuse | `42f818e76` | `RustWorkspace` and `crates/engine/src/application/package_authority.rs` |
| Bounded segment residency and cold historical reuse | `281ec6686`, `42f818e76` | `crates/replication/src/ir_residency.rs` |

The package compiler opens one rust-analyzer database and VFS for its selected
Rust sources. Its higher-ranked callback keeps borrowed HIR data inside the
analysis scope; it validates source bytes, crate membership, and the edition
from the loaded graph. This is package-wide reuse during one compilation. A
retained analyzer session *across edits* remains new work; persistent process
sessions elsewhere in `backend-compile` do not already provide that database.

The current [semantic VCS](../../crates/semantic/src/ir/vcs.rs) compares complete
canonical readers through borrowed declaration, facet, provenance, and link
changes. Its facets include declaration shape, documentation, visibility,
attributes, source, and language extensions. Occurrence-site changes are a
separate relation. The [segment delta cursor](../../crates/semantic/src/ir/versioned.rs)
is a different mechanism: it checks exact roots, build identity, coverage,
segment IDs, and input frontier before deciding `Reuse`, `Fetch`, or `Remove`.
The local client already uses checked hydration and bounded residency over
these admitted segments. Embeddings have their own typed plane identity and
segment path; do not add a parallel embedding authority.

The main remaining layout gap is at the [compiler producer](../../crates/engine/src/application/compiler.rs):
it builds a full canonical NXFI image, copies the staged complete image into
an in-memory package buffer, then `versioned_planes` cuts it into fixed 1 MiB
byte chunks with ordinal keys under one `Ir(Core)` plane. An early edit can
shift later chunks even when their declarations are unchanged. The plane
vocabulary already includes types, relations, occurrences, documentation,
source provenance, and language extensions, but this producer does not yet
emit those independent planes. Improve the producer and its full-image
prerequisite incrementally while retaining the current manifest, verifier,
input witnesses, selected-head authority, hydration cursor, CAS, object/closure
machinery, and semantic diff. The [streaming target](compiler-artifact-streaming.md)
is a design contract, not evidence that stable-key microsegments already ship.

Persistent IR commit/replay history is a distinct gap. The older Pijul-like
`workspace/ir-vcs` supplied replay, branches/tags, archive serving, and a
scan-resistant whole-archive `ServeCache` in active-history commits
`c91c654ac` and `1ec859f8a`; the package was removed in `23530d79b`.
Today's `vcs.rs` is a snapshot-diff API, not that repository. Reintroduce
historical semantics only where the product needs them, using the canonical IR
and current store rather than restoring the old parallel implementation. The
archive cache's scan-resistant admission is a benchmarking candidate for
multi-version access, not a replacement for segment residency.

Historical references `8b6823604`, `4a79abefe`, `12aadbd2e`,
`15a1b9b50`, `3ad45579e`, `5997b4606`, and `79be235c8` are outside this HEAD's
ancestry. Some are equivalent work on sibling histories. Use the active commits
above to trace what is actually integrated, and compare old algorithms as
reference implementations only. The next measurements are peak retained
analyzer memory, changed-byte/segment ratios after edits at several positions,
strict-cursor parity under adversarial histories, and cache admission under
repeated multi-version scans.
