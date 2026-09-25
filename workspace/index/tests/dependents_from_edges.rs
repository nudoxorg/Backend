//! The dependents sweep counts catalog runtime edges.
//!
//! A version that stored a runtime edge in its own ecosystem contributes that
//! name. Facet names count only when the version has no such edge. A
//! cross-ecosystem edge and a build edge do not increment the depender's
//! language.

mod common;

use std::sync::Arc;

use common::{migrated_writer, stem_id, version_id};
use index::catalog::{GlobalStore, InstanceToken};
use index::store::MetaStore;
use index::enums::{EdgeKind, EdgeSource};
use index::metadata::SearchFacets;
use index::protocol::{CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates};
use index::schema::catalog_map;
use index::store::lifecycle;
use smol_str::SmolStr;

fn facets_of(names: &[&str]) -> FacetWire {
    let mut facets = SearchFacets::default();
    facets.dependencies = names.iter().copied().map(SmolStr::new).collect();
    let (keywords, quality_ppm, extras) = catalog_map::facets_to_row(&facets).expect("facets");
    FacetWire {
        keywords,
        quality_ppm,
        extras,
    }
}

fn package(seed: u8, name: &str) -> CatalogOp {
    CatalogOp::UpsertPackage {
        stem: PackageStemWire {
            stem_id: stem_id(seed),
            ecosystem: heart::Language::Rust,
            name_struct: format!("pkg:cargo/{name}"),
            name_canonical: name.to_owned(),
            name_original: name.to_owned(),
        },
        repo_url: None,
    }
}

fn version(seed: u8, version_name: &str, edges: Vec<EdgeWire>, facets: FacetWire) -> CatalogOp {
    CatalogOp::UpsertVersion {
        coordinates: VersionCoordinates {
            version_id: version_id(seed),
            stem_id: stem_id(seed),
            version_canonical: version_name.to_owned(),
            version_original: version_name.to_owned(),
        },
        published_at: None,
        toolchain: None,
        license: None,
        edges,
        facets,
        source: None,
    }
}

fn runtime(ecosystem: heart::Language, name: &str) -> EdgeWire {
    EdgeWire {
        dep_ecosystem: ecosystem,
        dep_name_canonical: name.to_owned(),
        requirement: String::new(),
        kind: EdgeKind::Runtime,
        source: EdgeSource::Feed,
        resolved_stem: None,
    }
}

fn dependents_of(engine: &index::engine::Configured, seed: u8) -> u32 {
    let (_keywords, _quality, extras) = lifecycle::facets_for(engine, version_id(seed))
        .expect("facets row")
        .expect("facets present");
    catalog_map::facets_from_extras(extras.as_deref())
        .expect("decode")
        .expect("facets")
        .dependents
        .unwrap_or(0)
}

#[tokio::test]
async fn sweep_counts_runtime_edges_and_falls_back_to_facets() {
    let writer = Arc::new(migrated_writer());
    let ops = vec![
        package(1, "serde"),
        package(2, "tokio"),
        package(3, "leftover"),
        package(4, "criterion"),
        package(5, "app"),
        package(6, "facet-only"),
        package(7, "mixed"),
        version(1, "1.0.0", vec![], facets_of(&[])),
        version(2, "1.0.0", vec![], facets_of(&[])),
        version(3, "1.0.0", vec![], facets_of(&[])),
        version(4, "1.0.0", vec![], facets_of(&[])),
        version(
            5,
            "1.0.0",
            vec![runtime(heart::Language::Rust, "serde")],
            facets_of(&["leftover", "serde"]),
        ),
        version(6, "1.0.0", vec![], facets_of(&["tokio"])),
        version(
            7,
            "1.0.0",
            vec![
                runtime(heart::Language::Python, "serde"),
                EdgeWire {
                    dep_ecosystem: heart::Language::Rust,
                    dep_name_canonical: "criterion".to_owned(),
                    requirement: String::new(),
                    kind: EdgeKind::Build,
                    source: EdgeSource::Manifest,
                    resolved_stem: None,
                },
            ],
            facets_of(&[]),
        ),
    ];
    writer.apply_ops(&ops).expect("catalog writes");

    let store = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/dependents").expect("instance"),
    );
    let updated = store.refresh_dependents().await.expect("sweep");
    assert!(updated >= 2, "targets whose in-degree moved were written");

    let engine = writer.engine();
    assert_eq!(dependents_of(engine, 1), 1, "serde is a runtime edge");
    assert_eq!(dependents_of(engine, 2), 1, "tokio is a facet-only fallback");
    assert_eq!(dependents_of(engine, 3), 0, "leftover facet is not an edge");
    assert_eq!(dependents_of(engine, 4), 0, "a build edge is not a dependent");
}
