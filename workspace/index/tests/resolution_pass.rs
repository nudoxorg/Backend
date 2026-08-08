//! Integration tests for the registryless resolution pass (REGISTRYLESS §8, P8/P10).
//!
//! Exercises the full path against the in-memory catalog: seed system model
//! packages + aliases, ingest a version whose edges carry literal C/C++ tokens,
//! run [`resolution::resolve_unresolved_edges`], and assert the derived
//! `resolved_stem` bindings, idempotence, and non-destructiveness.

mod common;

use common::migrated_writer;

use heart::Language;
use index::engine::{CatalogEngine, Value};
use index::enums::{EdgeKind, EdgeSource};
use index::ids::{PackageStemId, version_id};
use index::protocol::{CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates};
use index::resolution::{ResolutionReport, resolve_unresolved_edges};
use index::seed_models::{system_model_seed_ops, system_stem_id};
use index::store::{MetaStore, writer::CatalogWriter};

/// A deterministic version id from a seed.
fn version(seed: u8) -> heart::PackageId {
    let mut bytes = [0u8; 16];
    bytes[15] = seed;
    heart::PackageId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

/// A deterministic consumer stem id.
fn stem(seed: u8) -> PackageStemId {
    let mut bytes = [0u8; 16];
    bytes[0] = seed;
    PackageStemId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

/// Count how many edges in the given ecosystem have a non-null resolved_stem.
fn count_resolved<E: CatalogEngine>(engine: &E) -> i64 {
    let rows: Vec<i64> = engine
        .query_rows(
            "SELECT count(*) FROM edges WHERE dep_ecosystem = ?1 AND resolved_stem IS NOT NULL",
            &[Value::Text("cpp".to_owned())],
            &mut |row| row.get_integer(0),
        )
        .expect("count resolved");
    rows.into_iter().next().unwrap_or(0)
}

/// Read the resolved_stem for a specific (version, token, kind) edge, if any.
fn resolved_stem_for<E: CatalogEngine>(
    engine: &E,
    v: heart::PackageId,
    token: &str,
    kind: EdgeKind,
) -> Option<PackageStemId> {
    use index::enums::TextEnum;
    let rows: Vec<Option<Vec<u8>>> = engine
        .query_rows(
            "SELECT resolved_stem FROM edges WHERE dependent_version = ?1 \
             AND dep_name_canonical = ?2 AND kind = ?3",
            &[
                Value::Blob(version_id::to_blob(&v).to_vec()),
                Value::Text(token.to_owned()),
                Value::Text(kind.as_token().to_owned()),
            ],
            &mut |row| row.get_optional_blob(0),
        )
        .expect("read resolved_stem");
    rows.into_iter()
        .next()
        .flatten()
        .map(|bytes| PackageStemId::from_blob(&bytes).expect("valid stem blob"))
}

/// Seed a consumer package + version whose edges name `ZLIB` (find_package) and
/// `Threads` (find_package), plus an unresolvable `ObscureLib`.
fn ingest_consumer(writer: &CatalogWriter<index::engine::Configured>) {
    writer
        .apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem(1),
                    ecosystem: Language::Cpp,
                    name_struct: "pkg:cpp/github.com/acme/app".to_owned(),
                    name_canonical: "github.com/acme/app".to_owned(),
                    name_original: "github.com/acme/app".to_owned(),
                },
                repo_url: Some("https://github.com/acme/app".to_owned()),
            },
            CatalogOp::UpsertVersion {
                coordinates: VersionCoordinates {
                    version_id: version(1),
                    stem_id: stem(1),
                    version_canonical: "1.0.0".to_owned(),
                    version_original: "1.0.0".to_owned(),
                },
                published_at: Some(1000),
                toolchain: None,
                license: None,
                edges: vec![
                    EdgeWire {
                        dep_ecosystem: Language::Cpp,
                        dep_name_canonical: "ZLIB".to_owned(),
                        requirement: "*".to_owned(),
                        kind: EdgeKind::FindPackage,
                        source: EdgeSource::Manifest,
                        resolved_stem: None,
                    },
                    EdgeWire {
                        dep_ecosystem: Language::Cpp,
                        dep_name_canonical: "Threads".to_owned(),
                        requirement: "*".to_owned(),
                        kind: EdgeKind::FindPackage,
                        source: EdgeSource::Manifest,
                        resolved_stem: None,
                    },
                    EdgeWire {
                        dep_ecosystem: Language::Cpp,
                        dep_name_canonical: "ObscureLib".to_owned(),
                        requirement: "*".to_owned(),
                        kind: EdgeKind::FindPackage,
                        source: EdgeSource::Manifest,
                        resolved_stem: None,
                    },
                ],
                facets: FacetWire::default(),
                source: None,
            },
        ])
        .expect("ingest consumer");
}

#[test]
fn threads_edge_resolves_to_system_pthread() {
    let writer = migrated_writer();
    // Seed the ZLIB alias (curated) and the system models (Threads→system/pthread).
    writer
        .apply_ops(&[CatalogOp::UpsertAlias {
            ecosystem: Language::Cpp,
            kind: smol_str::SmolStr::new("find_package"),
            alias: smol_str::SmolStr::new("zlib"),
            stem: stem(200),
            confidence: index::enums::AliasConfidence::Curated,
        }])
        .expect("seed zlib alias");
    writer
        .apply_ops(&system_model_seed_ops())
        .expect("seed system models");
    ingest_consumer(&writer);

    let report = resolve_unresolved_edges(writer.engine(), Language::Cpp).expect("resolve");
    // ZLIB → stem(200), Threads → system/pthread; ObscureLib stays unresolved.
    assert_eq!(report.resolved, 2, "ZLIB + Threads resolve");
    assert_eq!(report.unresolved, 1, "ObscureLib stays Absent-tier");

    let threads = resolved_stem_for(
        writer.engine(),
        version(1),
        "Threads",
        EdgeKind::FindPackage,
    );
    assert_eq!(
        threads,
        Some(system_stem_id("pthread")),
        "Threads must bind to system/pthread"
    );
    let zlib = resolved_stem_for(writer.engine(), version(1), "ZLIB", EdgeKind::FindPackage);
    assert_eq!(zlib, Some(stem(200)), "ZLIB binds to its curated stem");
    let obscure = resolved_stem_for(
        writer.engine(),
        version(1),
        "ObscureLib",
        EdgeKind::FindPackage,
    );
    assert_eq!(obscure, None, "unmatched token is never invented");
}

#[test]
fn resolution_is_idempotent_and_never_overwrites() {
    let writer = migrated_writer();
    writer
        .apply_ops(&system_model_seed_ops())
        .expect("seed system models");
    ingest_consumer(&writer);

    let first = resolve_unresolved_edges(writer.engine(), Language::Cpp).expect("first run");
    assert_eq!(
        first.resolved, 1,
        "only Threads resolves (no zlib alias this time)"
    );
    let resolved_after_first = count_resolved(writer.engine());

    // Rerun: nothing new to resolve, and no existing binding is disturbed.
    let second = resolve_unresolved_edges(writer.engine(), Language::Cpp).expect("second run");
    assert_eq!(
        second.resolved, 0,
        "a rerun resolves nothing new (idempotent)"
    );
    assert_eq!(
        count_resolved(writer.engine()),
        resolved_after_first,
        "the set of resolved edges is unchanged on rerun"
    );

    // The Threads binding is exactly what the first run produced (not re-derived
    // to something else).
    let threads = resolved_stem_for(
        writer.engine(),
        version(1),
        "Threads",
        EdgeKind::FindPackage,
    );
    assert_eq!(threads, Some(system_stem_id("pthread")));
}

#[test]
fn empty_ecosystem_scope_resolves_nothing() {
    let writer = migrated_writer();
    ingest_consumer(&writer);
    // Rust scope: the cpp edges are untouched, so nothing resolves and no error.
    let report = resolve_unresolved_edges(writer.engine(), Language::Rust).expect("rust scope");
    assert_eq!(
        report,
        ResolutionReport::default(),
        "no rust edges to resolve"
    );
}
