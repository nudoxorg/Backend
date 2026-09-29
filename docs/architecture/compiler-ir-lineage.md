# Compiler and IR lineage audit

This checkpoint extends the established compiler and IR rather than creating
a parallel semantic representation. The historical audit compared the current
tree with commits more than 3,000 commits back, as well as the recent
versioned-IR cutover.

| Foundation | Historical commit | Current boundary |
| --- | --- | --- |
| Borrowed Rust semantic authority and higher-ranked callback | `8b6823604` | `frontends/rust/src/legacy/authority.rs` |
| Cargo edition discovery and loaded-graph edition proof | `c06c6b674`, `1f4e0525e` | `RustProject` and `RustWorkspace` |
| IR-native history and replay | `4a79abefe`, `12aadbd2e` | `backend-semantic` manifests and versioned history |
| Compiler IR's canonical semantic crate | `15a1b9b50` | `crates/semantic/src/ir` |
| Borrowed IR VCS over canonical entities and links | `3ad45579e`, `f67484332` | `crates/semantic/src/ir/vcs.rs` |
| Strict semantic delta cursor | `4faee5661` | `SemanticDeltaCursor` in `crates/semantic/src/ir/versioned.rs` |
| Bounded segment residency | `5997b4606` | `crates/replication/src/ir_residency.rs` |
| Immutable-backing validation cache | `94fa7ad42` | `crates/semantic/src/ir/semantic_image/full_wire/view.rs` |

The package-level Rust session shares one rust-analyzer database and VFS
across selected sources. It preserves the existing borrowed authority API:
semantic HIR data cannot escape its callback, and source membership and edition
are checked against the loaded crate graph. Earlier code loaded a separate
analyzer owner for each source; session reuse is new work within that boundary.
The internal exact-edition capability rows correct a mismatch between compiler
identity and the intentionally shared public Rust language lane.

The IR VCS itself remains active in `crates/semantic/src/ir/vcs.rs`. It compares
canonical IR snapshots through borrowed entity and link deltas, including
validated semantic-image views, without a second lowered VCS model. The
residency path still consumes the canonical selected manifest and strict
delta cursor. Its cold-history optimization uses checked historical descriptor
claims to locate possible reuse, then verifies bytes against the newly selected
target segment commitment. Neither a stored descriptor nor an S3 receipt is
semantic publication authority. The older whole-archive `ServeCache` in
`12aadbd2e` used scan-resistant admission, but cached a different unit of work;
the current segment cache did not replace an active archive cache. The old
workspace IR was explicitly retired in `79be235c8` after migration.

Further measurements should bound peak rust-analyzer database memory on large
packages, compare historical-descriptor scanning with the strict cursor on
adversarial histories, and benchmark cache admission under repeated version
scans against the old scan-resistant workload. These are performance questions,
not evidence that the older compiler or IR implementation was lost.
