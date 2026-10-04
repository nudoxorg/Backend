# Typed PackageReference identity cutover: final library slice

Worktree: `/private/tmp/nudox-typed-graph-library-20261004`  
Base: `9a896bf197aed9028e9dbf14f0532eb00daad077`  
Branch: `codex/typed-graph-library-20261004`

Only the four assigned files are modified: `crates/library/lib.rs`, `surface.rs`, `package_graph.rs`, and `package_graph_page.rs`.

## Final behavior

The original typed-coordinate cutover is preserved: public kind tags and explicit constructor, spelling-plus-kind comparator, canonical graph-source order, typed forward index keys, exact-kind forward page selection, and v2 kind-bound cursors. Existing tagged serde, witness tags, valid target hashes, and ordinary PURL lookup semantics stay the same.

The follow-up reverse-target seam is closed:

- `package_graph.rs:761-796` adds allocation-free `PackageDependencyTarget::admit`; `new` delegates to it, preserving the prior resolved-target contract: resolution must be a PURL whose registry ecosystem and lineage match the declared target.
- `package_graph.rs:294-317` and `1009-1024` recheck that contract when checked facts and standalone edge collections are admitted. This catches public-field and serde-created invalid targets while allowing Local source coordinates (including `pkg:`-prefixed labels).
- `package_graph.rs:1175-1179,1283-1288,1385-1388,1474-1477,1564-1573` stores reverse resolved postings under typed `PackageReference` keys and uses exact typed equality in indexed pages, source queries, and the independent linear scan.
- `package_graph_page.rs:282-294,360-369` maps invalid row targets to `PageShape` and compares resolved targets with typed equality.
- Independent malformed fixtures use valid recomputed row digests for a same-spelling Local resolution, a wrong-ecosystem PURL, and a wrong-lineage PURL. Checked graph admission and page admission reject all three; the raw indexed and linear reverse lookup oracles do not alias the Local resolution to the same-spelling PURL.

## Validation status

No Cargo command, build, or test was run. `rustfmt --edition 2024` parsed/formatted the two edited graph files and `git diff --check` is clean. Suggested controlled validation:

```text
cargo test -p backend-library package_reference_kind_preserves_explicit_local_identity
cargo test -p backend-library package_reference_kind_separates_lookups_and_canonicalizes_equal_spellings
cargo test -p backend-library typed_coordinate_selection_and_authority_match_only_the_requested_kind
cargo test -p backend-library package_graph_cursors_reject_cross_kind_replay
cargo test -p backend-library resolved_target_admission_and_reverse_lookup_preserve_typed_identity
cargo test -p backend-library page_admission_rechecks_resolved_target_contract
cargo test -p backend-library
```

## Patch packets

Original frozen identity patch remains unchanged: `/private/tmp/nudox-typed-graph-library-20261004.patch`  
Original receipt remains unchanged: `/private/tmp/nudox-typed-graph-library-20261004.md`  
Incremental reverse-target seam patch, applies on top of the original patch: `/private/tmp/nudox-typed-graph-library-20261004-reverse-target-seam.patch`  
Full final patch from base: `/private/tmp/nudox-typed-graph-library-20261004-final.patch`

SHA-256:

- Original patch: `6a4d88b2c20eff7f55fdb30933b8e55ab347d9a4638de3d1038e7624a30bec15`
- Original receipt: `eae944fe9aeb0896d5fc0c881254324910ad50a7a11c02f256024f92c2f40133`
- Incremental seam patch: `2d104847966cbcab07a53495c3c887b1db80fe82ab51a0cbe40c02579e6b4579`
- Full final patch: `e55d153ed2b283eca755ee3530d082d76e95f75aa4d9b7b3cc7a24b4338526af`

Current source file SHA-256:

- `crates/library/surface.rs`: `2b5bfffdb0f0cb141be40b03b23ec2399c330709e2801a6f46e4040ec59f707c`
- `crates/library/package_graph.rs`: `ce374e7293ba914fa1f736f562a5dce5d73fb041ec9fa1917cf190d2a0ef4471`
- `crates/library/package_graph_page.rs`: `68246c25e5aac8cdfc7a29ca1d5dc0a93d2a1c6ddf4039f79b68c0fa9b31819c`
- `crates/library/lib.rs`: `e7f3eb4c2d75931e1de430f806c59fec6ebd97a2ef896fc26cea4c87c33ebb00`
