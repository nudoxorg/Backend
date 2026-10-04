# PackageReference kind-collision audit

Read-only audit of immutable candidate `9a896bf197aed9028e9dbf14f0532eb00daad077` (the inspected file hashes below match that commit). No source edits, builds, or tests were performed.

## Finding: typed `Local` and `Purl` references can alias in lookup, page selection, and persistence

**Severity: P1 correctness/data-identity issue for an admitted edge case.** `PackageReference` is a public tagged enum with distinct `Purl` and `Local` variants, derives typed equality/order/serde, and hashes their kind as identity. A `Local(ProductText)` whose label is a valid PURL spelling is constructible directly and through explicit tagged deserialization. `PackageReference::parse("pkg:...")` would instead produce `Purl`, but neither `CheckedPackageGraphFacts::new` nor the borrowed/admission constructors prohibit the explicit `Local` variant. Unknown/unavailable facts require no row-level checks that could incidentally reject it. See `crates/library/surface.rs:25-55,89-120` and `crates/library/package_graph.rs:280-305`.

Use this minimal independent fixture (not executed):

```rust
let text = "pkg:cargo/widget@1.0.0";
let purl = PackageReference::parse(text.to_owned())?; // Purl
let local = PackageReference::Local(ProductText::new(text.to_owned())?);
let authority = PackageGraphSourceAuthority::Local([0x41; 32]);
let facts = vec![(
    PackageGraphSourceKey::new(local.clone(), authority),
    DependencyFacts::Unknown(ProductText::new("local facts")?),
)];
let graph = IndexedCheckedPackageGraph::new(facts, limits)?;
```

Current expected behavior from source inspection: `graph.dependencies(&purl)` returns `Exact` for the `Local` source because `PackageGraphIndex::by_coordinate` is keyed only by `source.coordinate.as_str()` and `dependencies` looks up `package.as_str()` (`package_graph.rs:1023-1027,1239-1249,1302-1328`). A typed PURL query therefore receives facts attached to a distinct local identity. With both `Purl(text)` and `Local(text)` facts at the same authority, checked admission allows both because the uniqueness `BTreeSet` uses derived typed equality (`:284-288`), but the string index combines them and both typed queries return `Ambiguous`. In contrast, `dependencies_for_source` uses the typed `PackageGraphSourceKey` map (`:1331-1338`). These public paths disagree about identity.

The checked source sort is not a total order for this pair. It compares spelling, authority kind, and authority bytes, omitting the coordinate variant (`package_graph.rs:308-317`). The two distinct source keys therefore compare equal. `sort_unstable_by` does not provide a canonical order inside that equivalence class, so `facts()` and its parallel `source_witnesses()` slice have no guaranteed input-order-independent order for a Purl/Local collision. This is a real canonicalization defect even if a particular toolchain happens to leave a two-item tie in its input order.

The **global facts witness remains input-order-independent** for this case: each per-source witness includes reference kind tag 1 for Purl or 2 for Local (`package_graph.rs:579-616`), and `package_dependency_facts_witness_from_sources` sorts those digests before hashing (`:910-934`). The cached witness therefore commits to both typed facts independent of input order; it does not repair the unstable fact/source-witness array ordering or string-keyed lookup.

## Page behavior compounds the identity mismatch

`select_checked_source` partitions and gathers candidates by spelling only (`crates/library/package_graph_page.rs:708-720`), then, when an authority is selected, takes the first matching authority without requiring `source.coordinate == request.package` (`:732-738`). Without an authority it emits ambiguity across both typed coordinates. `PackageGraphPage::admit` later requires every selected/ambiguous source coordinate to equal the request’s typed `PackageReference` (`:294-334`), so a Purl request that selected a same-text Local source becomes a `PageShape` rejection; the outcome can depend on the comparator tie order. The adapter performs the SQL read and then this resident-facts authentication (`crates/local-service/src/builtin/commands/adapter.rs:3205-3218`).

The cursor recipe also hashes only `as_str()` for both the requested package and selected source (`package_graph_page.rs:168-204`). Thus otherwise-identical Purl and Local requests have the same recipe. For reverse pages, cursors have no selected source and request admission does not compare a typed source coordinate (`:140-158,454-465`), so a reverse cursor can be accepted for the other kind if the snapshot and other request fields match. Add kind to the recipe and version the page cursor contract.

The product-state consumer uses `graph.dependencies(package)` directly (`crates/local-service/src/builtin/product_state.rs:3281-3314`), so the string alias can return or describe the wrong source facts before SQL paging is involved.

## Durable Turso key loses the kind

The projection’s primary keys are `(source TEXT, source_authority_kind, source_authority_id)` for sources, source witnesses, and states. Edge rows and their source index use the same string-plus-authority identity (`extensions/turso/src/schema.rs:46-87`). Writers persist only `source.coordinate.as_str()` (`extensions/turso/src/graph.rs:889-912,940-945`); reconciliation compares the same fields (`:791-809`). Read inventory reconstructs a source key by copying the *query’s* coordinate kind (`extensions/turso/src/package_graph_read.rs:391-425`), while `decode_source_key` and `decode_edge` parse the stored text, making any `pkg:` spelling a Purl (`extensions/turso/src/graph.rs:817-830; package_graph_read.rs:665-668`).

Consequences for the fixture:

- A Local unknown-state witness is kind-bound. Reading its row under a Purl request recomputes a different source witness and fails the integrity check (`package_graph_read.rs:605-623`).
- A Local known edge with a `pkg:` source is decoded as Purl; recomputation then differs from the persisted typed `facts_version` and read fails (`package_graph_read.rs:713-737`).
- Purl and Local entries with the same spelling and authority cannot both occupy source/state/witness keys. Upserts collapse them, while the in-memory global witness can commit to both. This is not fixed by adding a sort tie-break alone.

## Minimal complete identity cutover

Use one explicit coordinate-kind tag (the existing stable witness tags 1=Purl, 2=Local) throughout the identity path:

1. **Canonical order:** compare spelling, then reference-kind tag, then authority kind/id. This retains today’s order for all entries with distinct display spellings and resolves only the omitted tie.
2. **Resident lookup:** key `by_coordinate` by typed `PackageReference` (or a small equivalent borrowed/owned typed key); query exact typed coordinate before applying authority ambiguity. Make page range selection use the same identity comparator and exact equality.
3. **Cursor recipe:** hash the package kind and selected-source kind. Bump the recipe domain from `page.v1` and the exported `PACKAGE_GRAPH_PAGE_SCHEMA` so old cursors are rejected explicitly.
4. **Persistence/read/write:** add coordinate kind to source identity in edge, source, state, and source-witness tables; include it in primary keys, relevant indexes and SQL predicates, keyset reconciliation/order, writer values, inventory results, and source/edge decoders. Decode by explicit kind rather than `PackageReference::parse` guessing from display text.

Do **not** silently reject `Local("pkg:...")`: the public type and serde representation currently admit it, and the stated Local variant means its identity is typed. If product policy instead wants to reserve that prefix, make it an explicit admission rule and compatibility decision after confirming the Local-label contract; that policy would not be a sort-only fix.

This is a data-contract/persistence-key change. `extensions/turso/src/schema.rs` is at schema version 8, and `extensions/turso/src/connection.rs:300-310` refuses any projection whose version differs; a new durable key therefore needs a versioned rebuild or explicit migration. Old rows cannot be disambiguated from `source` text alone. Per-source witnesses can help identify a single old row’s type by recomputing candidates, but a previously collapsed Purl+Local pair is not recoverable from that primary key. Do not infer/relabel ambiguous old data silently. The package-facts global witness domain need not change solely for this fix because source witness encoding already includes the kind and global aggregation sorts source digests. The page recipe/schema should change because they currently omit the kind.

## Candidate-capturing source references

- `crates/library/surface.rs:25-55,89-120` — bounded text, public enum, parser, explicit variant.
- `crates/library/package_graph.rs:230-263,280-330,579-616,910-934,1023-1027,1239-1249,1302-1338` — source-key equality, admission, sort, witness, and index keys/queries.
- `crates/library/package_graph_page.rs:140-158,168-204,294-334,412-465,708-750` — request/cursor admission, recipes, page shape, and selection.
- `crates/local-service/src/builtin/product_state.rs:3281-3314`; `crates/local-service/src/builtin/commands/adapter.rs:3205-3218` — actual query consumers.
- `extensions/turso/src/schema.rs:3,46-87`; `extensions/turso/src/connection.rs:300-310`; `extensions/turso/src/graph.rs:791-830,889-912,940-945`; `extensions/turso/src/package_graph_read.rs:391-425,605-623,665-737` — durable key, version gate, and typed readback.

## Exact inspected hashes

All hashes below are SHA-256 of the candidate files at `9a896bf197aed9028e9dbf14f0532eb00daad077` (also equal to the corresponding inspected working-tree files):

```text
crates/library/surface.rs                           2fe4886026cdb206ed8362b7c65d78d138dd94ff40188a2c98a405d987ad8e78
crates/library/package_graph.rs                     b4d8d494a6c9a9b5b1ee57c3f1d55e2dd913fba68428ce127d8e126430604913
crates/library/package_graph_page.rs                557443f02a193a5f6f49aa3dee4fe68f6bf01b0319e17ca2b27c4661c92be5b6
extensions/turso/src/schema.rs                      5e2440743ef2fda410ac00f09cda5f1c107ba71729a064851f65e4a56f9eec94
extensions/turso/src/graph.rs                       3c00316e9bceda031bf2f0652e11abdbbd91ad3c282ac7e9f69ae49ce9ddd3c6
extensions/turso/src/package_graph_read.rs          1aaa0ccda878b47b63599807f624216873e4a4e768c45534c952438c7e0a1dba
crates/local-service/src/builtin/product_state.rs  7ce031fda4d6f8e4a60413f25d693ae92d504b98373309e36e4daf908db8fb62
crates/local-service/src/builtin/commands/adapter.rs b1e4988a98bb3f621e4223908cc44fbb95bda3f56e050206a02c22ad2c22137b
```
