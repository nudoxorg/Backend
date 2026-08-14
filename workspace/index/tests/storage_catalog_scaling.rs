//! Storage-characteristics tests for the versioned catalog's SQL tables
//! (`packages`, `versions`, `edges`, `schema_meta`, …): real corpus
//! ingestion on a real on-disk engine, with disk bytes measured via
//! `nudox_test_support::measured` per doctrine §4.
//!
//! # Which engine these bytes belong to
//!
//! `common::migrated_disk_writer` opens `index::engine::Configured` at a real
//! path. Under the default feature set that is `DoltEngine` — the vendored
//! DoltLite prolly-tree store — so every byte counted below is the **product's**
//! storage cost, including the version history a versioned catalog necessarily
//! writes alongside the rows.
//!
//! That was not true before 2026-08-08. This helper used to be pinned to
//! `MemoryEngine::open_at_path`, and the paragraph here used to justify it with
//! "the sovereign `dolt-engine` cannot be built in this checkout (the vendored
//! DoltLite C amalgamation is absent)". The amalgamation is present and the
//! engine builds; the justification had simply outlived its own truth. The
//! numbers taken under the old helper were real SQLite bytes for a storage
//! engine this product does not use, and they are **not comparable** to the ones
//! this file emits now — expect them to move, upward, because a prolly tree
//! stores content-addressed chunks plus history where stock SQLite stored
//! B-tree pages and nothing else.
//!
//! Measured on 2026-08-08, both columns from this file's own `cost case=` lines:
//!
//! | checkpoint | packages added | before (stock SQLite) | after (DoltLite) |
//! |---|---:|---:|---:|
//! | migration only (schema v4) | 0 | 208,896 B | 238,172 B |
//! | → 1 | 1 | 0 B | 3,830 B |
//! | → 5 | 4 | 0 B | 5,632 B |
//! | → 15 | 10 | 0 B | 10,046 B |
//! | → 30 | 15 | 0 B | 20,392 B |
//! | → 60 | 30 | 16,384 B | 40,486 B |
//! | → all | 94 / 95 | 28,672 B | 80,216 B |
//! | **cumulative** | **154 / 155** | **45,056 B** | **160,602 B** |
//! | **bytes per package** | | **292.6** | **1,036.1** |
//!
//! Two shape changes worth naming, because they are the interesting part:
//!
//! - **The four leading zeros are gone.** Under SQLite the first 30 real
//!   packages cost literally 0 bytes — they fit inside 4,096-byte pages the
//!   migration had already allocated, so marginal cost was a step function and
//!   "bytes per package" was meaningless below the page floor. The prolly tree
//!   allocates per chunk, not per page, so every batch has a real cost and the
//!   curve is a curve from the first package.
//! - **Per-package cost now *falls* with catalog size** (3,830 → 844 B/pkg
//!   across the checkpoints) instead of rising off a floor. That is chunk
//!   sharing: later packages reuse content already in the store.
//!
//! The package total also moved 154 → 155 — that is `nix/corpus.nix`
//! gaining an entry on another track, not an effect of the engine swap.
//! `docs/INDEX-CAPABILITY.md` §2.2 and `docs/ISSUES.md`'s `storage-numbers` row still
//! quote the pre-swap column and need updating; both are outside this change's
//! file scope.
//!
//! # Corpus
//!
//! Package/version identity for every fixture comes from `nix/corpus.nix`
//! itself (the authoritative fetch spec), cross-checked against which
//! directories actually exist under `result/`. Dependency edges are
//! parsed out of the real `Cargo.toml` for `crates.io` fixtures only (the
//! `toml` crate is already a hard `index` dependency); other ecosystems
//! contribute real packages/versions with an empty edge set — disclosed here,
//! not silently presented as "no dependencies".

mod common;

use common::{available_corpus_fixtures, language_for_ecosystem, migrated_disk_writer, CorpusFixture};
use heart::identity::derive::package_id_from_parts;
use heart::Language;
use index::enums::{EdgeKind, EdgeSource};
use index::entity::edges;
use index::ids::{PackageId, PackageStemId};
use index::protocol::{CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates};
use index::store::{Catalog, MetaStore};
use sea_orm::{
    ColumnTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};

/// The stem identity **is** `(ecosystem_token, name)` under heart's frozen
/// injective law — the exact derivation `ingest::enumerate::cpp_stem_id` uses
/// for the cpp direct-git plane, generalized to any ecosystem.
fn stem_id_for(ecosystem: Language, name: &str) -> PackageStemId {
    let id = package_id_from_parts([ecosystem.as_token().as_bytes(), name.as_bytes()]);
    PackageStemId::from_uuid(*id.as_uuid())
}

/// The version identity is `(stem_blob, version_canonical)`, mirroring
/// `ingest::enumerate::cpp_version_id`.
fn version_id_for(stem: PackageStemId, version_canonical: &str) -> PackageId {
    package_id_from_parts([stem.to_blob().as_slice(), version_canonical.as_bytes()])
}

/// Direct dependency names + the license expression, parsed straight out of a
/// real `Cargo.toml` with the `toml` crate. Only the top-level `[dependencies]`
/// table is read (crates.io normalizes `[dependencies.x]` dotted headers into
/// the same nested table TOML-side, so both spellings resolve identically);
/// `[dev-dependencies]` / `[build-dependencies]` / target-conditional tables
/// are not — real-but-partial, disclosed, not fabricated as "zero deps".
fn parse_cargo_toml_dependencies(bytes: &[u8]) -> (Option<String>, Vec<String>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (None, Vec::new());
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return (None, Vec::new());
    };
    let license = value
        .get("package")
        .and_then(|p| p.get("license"))
        .and_then(|l| l.as_str())
        .map(str::to_owned);
    let mut deps: Vec<String> = value
        .get("dependencies")
        .and_then(|d| d.as_table())
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default();
    deps.sort();
    (license, deps)
}

/// Build the real `UpsertPackage` + `UpsertVersion` ops for one corpus
/// fixture. Returns `None` only when the fixture's ecosystem token has no
/// `heart::Language` mapping (none, currently — every manifest ecosystem is
/// mapped — but the signature stays honest about the possibility).
fn ops_for_fixture(fixture: &CorpusFixture) -> Option<(CatalogOp, CatalogOp)> {
    let ecosystem = language_for_ecosystem(&fixture.ecosystem)?;
    let stem_id = stem_id_for(ecosystem, &fixture.name);
    let version_id = version_id_for(stem_id, &fixture.version);

    let (license, edges) = if fixture.ecosystem == "crates.io" {
        match std::fs::read(fixture.path.join("Cargo.toml")) {
            Ok(bytes) => {
                let (license, deps) = parse_cargo_toml_dependencies(&bytes);
                let edges = deps
                    .into_iter()
                    .map(|dep_name| EdgeWire {
                        dep_ecosystem: Language::Rust,
                        dep_name_canonical: dep_name,
                        requirement: String::new(),
                        kind: EdgeKind::Runtime,
                        source: EdgeSource::Manifest,
                        resolved_stem: None,
                    })
                    .collect::<Vec<_>>();
                (license, edges)
            }
            Err(_) => (None, Vec::new()),
        }
    } else {
        (None, Vec::new())
    };

    let package_op = CatalogOp::UpsertPackage {
        stem: PackageStemWire {
            stem_id,
            ecosystem,
            name_struct: format!("pkg:{}/{}", fixture.ecosystem, fixture.name),
            name_canonical: fixture.name.to_lowercase(),
            name_original: fixture.name.clone(),
        },
        repo_url: None,
    };
    let version_op = CatalogOp::UpsertVersion {
        coordinates: VersionCoordinates {
            version_id,
            stem_id,
            version_canonical: fixture.version.clone(),
            version_original: fixture.version.clone(),
        },
        published_at: None,
        toolchain: None,
        license,
        edges,
        facets: FacetWire::default(),
        source: None,
    };
    Some((package_op, version_op))
}

/// Real dependency names stored for a version, read back through the catalog
/// engine (not trusted from the write side) — proves the edges actually
/// landed in `edges`, not merely that `apply_ops` returned `Ok`.
fn stored_edge_names(engine: &index::engine::Configured, version: PackageId) -> Vec<String> {
    let stmt = edges::Entity::find()
        .filter(edges::Column::DependentVersion.eq(*version.as_uuid()))
        .select_only()
        .column(edges::Column::DepNameCanonical)
        .order_by_asc(edges::Column::DepNameCanonical)
        .build(DbBackend::Sqlite);
    index::engine::query(engine, stmt, &mut |row| row.get_text(0)).expect("read edges back")
}

#[test]
fn migration_ddl_cost_on_disk() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let db_path = scratch.path().join("catalog.sqlite");

    let (_writer, cost) = nudox_test_support::measured(
        "index/migration_ddl_only",
        scratch.path(),
        || common::migrated_disk_writer(&db_path),
    );

    assert!(
        db_path.exists(),
        "migration must have created a real sqlite file on disk"
    );
    assert!(
        cost.disk_delta_bytes > 0,
        "schema DDL for 23 tables + secondary indexes must write real, non-zero bytes; got {}",
        cost.disk_delta_bytes
    );
    // A sea-orm-rendered v4 schema (23 CREATE TABLE + 4 CREATE INDEX
    // statements, see `migrations::ddl::schema_v4_statements`) is neither a
    // one-page nor a many-megabyte file. Loose bounds catch a totally wrong
    // measurement (e.g. accidentally pointing `measured` at the repo root)
    // without pinning to SQLite's exact page-allocation behavior, which is an
    // implementation detail this test has no business asserting on.
    assert!(
        (4_000..2_000_000).contains(&cost.disk_delta_bytes),
        "migration-only disk delta {} bytes is outside the sane range for an \
         empty schema-v4 catalog; likely measuring the wrong directory",
        cost.disk_delta_bytes
    );
}

#[test]
fn catalog_bytes_scale_with_real_corpus_ingestion() {
    let fixtures = available_corpus_fixtures();
    let manifest_total: usize = common::load_corpus_manifest()
        .iter()
        .map(|p| p.versions.len())
        .sum();
    assert!(
        fixtures.len() >= 100,
        "expected a meaningful real corpus (>=100 fetched fixtures out of \
         {manifest_total} manifest-listed); found {}. Run nix build .#checks.corpus first.",
        fixtures.len()
    );

    let ops: Vec<(CorpusFixture, CatalogOp, CatalogOp)> = fixtures
        .into_iter()
        .filter_map(|fixture| {
            ops_for_fixture(&fixture).map(|(pkg, ver)| (fixture, pkg, ver))
        })
        .collect();
    assert!(
        ops.len() >= 100,
        "expected ops for >=100 real fixtures across all 7 ecosystems; got {}",
        ops.len()
    );

    let scratch = tempfile::tempdir().expect("tempdir");
    let db_path = scratch.path().join("catalog.sqlite");
    let writer = migrated_disk_writer(&db_path); // unmeasured: this test is
                                                  // about package growth, not
                                                  // migration cost (see the
                                                  // dedicated test above).

    // Checkpoint at growing fractions of the real corpus so the *shape* of
    // growth (not just its endpoint) is visible: is each new package's byte
    // cost roughly stable, or does it change with catalog size?
    let total = ops.len();
    let mut checkpoints: Vec<usize> = [1, 5, 15, 30, 60, total]
        .into_iter()
        .filter(|&n| n >= 1 && n <= total)
        .collect();
    checkpoints.dedup();
    checkpoints.sort_unstable();

    let mut applied_so_far = 0usize;
    let mut cumulative_bytes: i64 = 0;
    let mut per_checkpoint: Vec<(usize, i64)> = Vec::new(); // (packages in this
                                                             // batch, bytes this
                                                             // batch cost)

    for &checkpoint in &checkpoints {
        let batch: Vec<CatalogOp> = ops[applied_so_far..checkpoint]
            .iter()
            .flat_map(|(_, pkg, ver)| [pkg.clone(), ver.clone()])
            .collect();
        let batch_len = checkpoint - applied_so_far;
        let case = format!("index/catalog_scaling_upto_{checkpoint}_packages");
        let (report, cost) =
            nudox_test_support::measured(&case, scratch.path(), || writer.apply_ops(&batch));
        let report = report.expect("real corpus batch must apply cleanly");
        assert_eq!(
            report.applied,
            batch.len(),
            "checkpoint {checkpoint}: every op in the batch must apply"
        );
        cumulative_bytes += cost.disk_delta_bytes;
        per_checkpoint.push((batch_len, cost.disk_delta_bytes));
        applied_so_far = checkpoint;
    }

    assert_eq!(applied_so_far, total, "every real fixture must be applied exactly once");
    assert!(
        cumulative_bytes > 0,
        "ingesting {total} real packages must grow the sqlite file; got cumulative delta {cumulative_bytes}"
    );

    // Print the growth curve. Byte counts are reliable under concurrent build
    // load; this is exactly the number doctrine §4 asks each case to emit, so
    // it is also available to whatever parses the `cost case=` lines.
    for (batch_len, bytes) in &per_checkpoint {
        let per_package = *bytes as f64 / *batch_len as f64;
        println!(
            "cost case=index/catalog_scaling_detail batch_packages={batch_len} \
             batch_bytes={bytes} bytes_per_package={per_package:.1}"
        );
    }
    let final_bytes_per_package = cumulative_bytes as f64 / total as f64;
    println!(
        "cost case=index/catalog_scaling_summary total_packages={total} \
         cumulative_bytes={cumulative_bytes} bytes_per_package={final_bytes_per_package:.1}"
    );

    // ── Real-content assertions (doctrine §4: never just is_ok()/non-zero) ──

    // memchr really is in the manifest as a crates.io package; if the batch
    // ingestion above ran, it must be readable back with its real name.
    let memchr_stem = stem_id_for(Language::Rust, "memchr");
    let memchr_row = writer
        .get_package(memchr_stem)
        .expect("read")
        .expect("memchr must have been ingested from the real corpus manifest");
    assert_eq!(memchr_row.name_canonical, "memchr");
    assert_eq!(memchr_row.ecosystem, Language::Rust.as_token());

    // memchr 2.8.3's real, published Cargo.toml declares exactly two direct
    // dependencies, both optional: `core` (renamed from
    // rustc-std-workspace-core) and `log`. This is independently verifiable
    // by reading `result/memchr-2.8.3/Cargo.toml` directly — it is not
    // a number this test invented.
    let memchr_2_8_3 = version_id_for(memchr_stem, "2.8.3");
    let memchr_edges = stored_edge_names(writer.engine(), memchr_2_8_3);
    assert_eq!(
        memchr_edges,
        vec!["core".to_owned(), "log".to_owned()],
        "memchr 2.8.3's real Cargo.toml [dependencies] table must round-trip \
         through apply_ops into the edges table byte-for-name"
    );

    // A csharp/nuget package (a different ecosystem, different id derivation)
    // must also be present, proving this isn't a Rust-only code path.
    if let Some(newtonsoft) = writer
        .get_package(stem_id_for(Language::CSharp, "Newtonsoft.Json"))
        .expect("read")
    {
        assert_eq!(newtonsoft.name_original, "Newtonsoft.Json");
    } else {
        panic!("Newtonsoft.Json (nuget) must have been ingested; nix/corpus.nix lists it");
    }
}
