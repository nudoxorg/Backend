# Rust package indexing and Cargo target contexts

## Finding

Package indexing currently treats a captured source path as having one Rust
semantic context. That assumption is false for Rust packages that use
`#[path]` (or equivalent module declarations) to include one physical file in
more than one Cargo target. The same bytes can be interpreted under different
crate roots, `cfg` values, features, and module namespaces. Until the package
API names the intended target context or represents every context, source
ownership ambiguity must remain a refusal; selecting the first owner or
calling the file detached would report an unsupported semantic result.

The failure is visible in the exact Ryu 1.0.18 case. The fixture archive at
`/tmp/nudox-ryu-1.0.18.crate` has SHA-256
`f3cb5ba0dc43242ce17de99c180e96db90b235b8a9fdc9543c96d2209116bd9f`, matching
the repository pin. Its normalized manifest explicitly declares test
targets including `s2f_test`, `s2d_test`, `d2s_table_test`,
`d2s_intrinsics_test`, and others. Those test crate roots use `#[path]` to
include files under `src/`; for example, `s2f_test.rs` and `s2d_test.rs`
include `common.rs`, `d2s.rs`, and `d2s_intrinsics.rs`, while
`d2s_table_test.rs` also includes `common.rs` and `d2s_intrinsics.rs`.
`d2s_test.rs` and `f2s_test.rs` both declare `mod macros`, resolving to the
same `tests/macros/mod.rs` source under distinct integration-test crate roots.

The current de9 T2 run reports eight `RustSourceOutsideActiveCargoTarget`
gaps and only 19/27 source rows covered. That label does not identify the
underlying authority error. In
`frontends/rust/src/legacy/authority.rs`,
`build_source_ownership_index` turns a second owner of a `FileId` into
`AmbiguousSourceOwner`, while an absent owner becomes `DetachedSource`. In
`crates/engine/src/driver/types/authority.rs`, both variants are mapped to
`AuthorityDiagnosticClass::SourceScope`. Then
`crates/engine/src/application/compiler.rs` maps that class to
`RustSourceOutsideActiveCargoTarget`. This explains why the current record
cannot distinguish valid multi-target source from inactive source.

The existing test
`source_ownership_reports_shared_and_complete_absence_from_one_retained_index`
constructs separate library and binary targets that include the same file.
It asserts that shared-file analysis returns `AmbiguousSourceOwner` with two
owners and refuses to analyze it. This guard is correct: package indexing
cannot infer which crate root the caller intended.

The ownership entry currently stores only `(Crate, RustSourceScope)`, not a
Cargo target identity or module identity. This is insufficient both to bind a
caller request to one manifest target and to represent multiple module/target
contexts for one path. Deduplicating entries based on the current fields
would discard context evidence. A target-aware repair needs an explicit
contract, not an owner-order heuristic.

## Supported design directions

### 1. Caller selects one Cargo target

Add a target selector to package indexing, resolved only against the admitted
`RustCargoWorkspaceFactsV1` target rows. Bind the selected target's exact
package identity, target name/kind, and `source_path` to the RA crate root;
refuse if the mapping is missing, duplicated, or inconsistent. Pass that
bound target context through Rust source analysis so a shared file is analyzed
under the selected root and its actual feature/`cfg` settings.

The API must define what happens to package files outside that target. They
must be explicitly out of scope, not coverage gaps and not silently absent.
The request/manifest therefore needs a target-scoped source frontier and a
coverage ledger whose denominator is that selected target's admitted source
closure. Files in the original package inventory but outside that closure
remain available as package inputs/provenance, but cannot count as missing
semantic rows for this target. If the caller wants another target, it issues
another target-scoped operation. The selected target and the source-closure
binding must be retained in result identity so callers cannot mistake one
target's facts for a package-wide result.

This is the smaller product surface when callers already operate on a named
Cargo target. It is not acceptable if existing package queries promise
package-wide facts without asking for a target.

### 2. Represent every active target context

Extend source coordinates and semantic records so a source instance is bound
to `(package, target, relative path, content identity)`, rather than just
`(package, relative path, content identity)`. Build and analyze each Cargo
target with its validated root and cfg context. Preserve separate fact sets
for shared physical files; do not merge distinct declarations or resolution
results merely because their byte spans match.

Package-wide queries must state how they return multiple contexts. The safe
default is to return target-qualified answers. If a query requests an
unqualified answer, it may collapse records only when their complete
canonical semantic records are equal and the result retains the set of
supporting target identities. Conflicting answers stay distinct or produce
an explicit ambiguity result. Coverage is measured per target context, so a
file shared by three targets contributes three context rows, while a file
outside a target's closure contributes none for that target.

This keeps package-wide indexing useful without pretending that a file has
one meaning. It requires coordinated changes to source manifests, artifact
identity/provenance, coverage accounting, storage, query results, and
freshness checks.

## Recommendation and invariants

Choose caller-selected target if product operations can truthfully narrow
their promise to one Cargo target. Choose all-context representation if
existing package-level query behavior must cover the union of library,
binary, test, example, and benchmark targets. Do not implement a silent
library-target default: it would drop authored test-only facts while making
the result look package-complete.

Either direction must preserve exact source-byte binding, package-root
confinement, manifest target identity, active target cfg/features, bounded
source/target counts, and ambiguity refusal when a target cannot be proven.
The current 19/27 partial result is truthful under the existing package
contract; the eight files must not be removed from the expected frontier or
reclassified as successful without this contract change.

## Selected-target product contract (W2, October 2026)

Robert selected direction 1 (D17/Q18). `add --cargo-target NAME` and MCP
`cargo_target` explicitly narrow the Rust semantic result to one admitted Cargo
target. An unset selector retains the package-scoped behavior and its ownership
refusals. A name that is absent or appears in multiple target rows is refused;
there is no implicit library target.

The compiler owner resolves the name using its configured Rust toolchain and
admitted Cargo metadata before source capture. The exact target name, kinds,
root, and complete captured HIR module closure are retained. An active target
module that is outside the package or missing from captured inputs refuses the
operation. Staging recomputes that closure and refuses a changed identity.

There is one latest selected Rust context per package/profile. Selecting a
different target replaces that context; immutable generations remain historical.
The durable caller request, compiler input observation, source capture, manifest,
and publication bind the selected context, and package-profile/operation receipts
show the target and source-closure identity/count. Coverage counts the selected
target closure. All acquired package inputs remain in source provenance, including
files explicitly outside the selected semantic context. A later unscoped add
returns to the original package-scoped contract.

Selected-target compilation uses the checked local compiler route while the
remote protocol lacks the pre-resolved source-closure pin. It does not substitute
a detached analysis context or claim remote full-workspace authority.
