//! The dependents sweep counts catalog runtime edges.
//!
//! A version that stored a runtime edge in its own ecosystem contributes that
//! name. Facet names count only when the version has no such edge. A
//! cross-ecosystem edge and a build edge do not increment the depender's
//! language.

mod common;

use std::sync::Arc;

use common::{migrated_writer, stem_id, version_id};
use heart::{
    Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain, content::ContentHash,
};
use index::{
    Package,
    catalog::{GlobalStore, InstanceToken},
    engine::CatalogEngine,
    enums::{EdgeKind, EdgeSource},
    metadata::SearchFacets,
    package::{Coordinates, PackageName},
    protocol::{CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates},
    schema::catalog_map,
    store::{MetaStore, lifecycle},
};
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
            vec![runtime(heart::Language::Python, "serde"), EdgeWire {
                dep_ecosystem: heart::Language::Rust,
                dep_name_canonical: "criterion".to_owned(),
                requirement: String::new(),
                kind: EdgeKind::Build,
                source: EdgeSource::Manifest,
                resolved_stem: None,
            }],
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
    assert_eq!(
        dependents_of(engine, 2),
        1,
        "tokio is a facet-only fallback"
    );
    assert_eq!(dependents_of(engine, 3), 0, "leftover facet is not an edge");
    assert_eq!(
        dependents_of(engine, 4),
        0,
        "a build edge is not a dependent"
    );
}

#[tokio::test]
async fn two_versions_of_one_package_union_their_runtime_edges() {
    let writer = Arc::new(migrated_writer());
    let ops = vec![
        package(1, "serde"),
        package(2, "tokio"),
        package(5, "app"),
        version(1, "1.0.0", vec![], facets_of(&[])),
        version(2, "1.0.0", vec![], facets_of(&[])),
        version(
            5,
            "1.0.0",
            vec![runtime(heart::Language::Rust, "serde")],
            facets_of(&[]),
        ),
        CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id: version_id(9),
                stem_id: stem_id(5),
                version_canonical: "2.0.0".to_owned(),
                version_original: "2.0.0".to_owned(),
            },
            published_at: None,
            toolchain: None,
            license: None,
            edges: vec![runtime(heart::Language::Rust, "tokio")],
            facets: facets_of(&[]),
            source: None,
        },
    ];
    writer.apply_ops(&ops).expect("catalog writes");
    let store = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/dependents-union").expect("instance"),
    );
    store.refresh_dependents().await.expect("sweep");
    let engine = writer.engine();
    assert_eq!(dependents_of(engine, 1), 1, "serde from the first version");
    assert_eq!(dependents_of(engine, 2), 1, "tokio from the second version");
}

fn stored_package(name: &str, dependencies: &[&str]) -> index::GlobalPackage {
    let coordinates = Coordinates {
        origin: RegistryOrigin::CratesIo,
        name: PackageName::from_canonical(Language::Rust, name, name),
        version: PackageVersion::try_from((Language::Rust, "1.0.0")).expect("version"),
    };
    let mut facets = index::metadata::SearchFacets::default();
    facets.dependencies = dependencies.iter().copied().map(SmolStr::new).collect();
    index::GlobalPackage {
        id: coordinates.id(),
        package: Package {
            coordinates,
            toolchain: Toolchain::Rust {
                compiler: semver::Version::new(1, 88, 0),
                edition: heart::Edition::E2024,
            },
        },
        state: ResolutionState::Stored {
            hash: ContentHash::from_bytes([9u8; 32]),
        },
        facets: Some(facets),
    }
}

#[tokio::test]
async fn a_stored_package_keeps_its_state_when_edges_are_replaced() {
    let writer = Arc::new(migrated_writer());
    let store = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/stored-edges").expect("instance"),
    );
    let first = stored_package("app", &["serde"]);
    store.upsert(&first).await.expect("first publish");
    assert!(matches!(
        store.get_state(first.id).await.expect("state"),
        ResolutionState::Stored { .. }
    ));

    let revised = stored_package("app", &["tokio"]);
    store
        .replace_feed_edges(&revised)
        .await
        .expect("replace edges");
    assert!(matches!(
        store.get_state(first.id).await.expect("state unchanged"),
        ResolutionState::Stored { .. }
    ));

    let edges = lifecycle::scan_runtime_edges(writer.engine()).expect("edges");
    let names: Vec<_> = edges.into_iter().map(|(_, name)| name).collect();
    assert_eq!(names, vec!["tokio".to_owned()]);
}

fn build_edge(name: &str) -> EdgeWire {
    EdgeWire {
        dep_ecosystem: heart::Language::Rust,
        dep_name_canonical: name.to_owned(),
        requirement: String::new(),
        kind: EdgeKind::Build,
        source: EdgeSource::Manifest,
        resolved_stem: None,
    }
}

fn edge_kinds(
    engine: &index::engine::Configured,
    version: index::ids::PackageId,
) -> Vec<(String, String)> {
    engine
        .query_rows(
            "SELECT kind, dep_name_canonical FROM edges WHERE dependent_version = ? ORDER BY kind, dep_name_canonical",
            &[index::engine::Value::Blob(
                version.as_uuid().as_bytes().to_vec(),
            )],
            &mut |row| Ok((row.get_text(0)?, row.get_text(1)?)),
        )
        .expect("edge kinds")
}

/// A stored generation and a later feed republish share one runtime replace.
/// The build edge stays. An empty dependency list does not clear runtime edges.
/// The sweep then counts the surviving runtime name.
#[tokio::test]
async fn stored_generation_and_feed_republish_share_one_runtime_replace() {
    let writer = Arc::new(migrated_writer());
    writer
        .apply_ops(&[
            package(1, "serde"),
            package(2, "tokio"),
            package(3, "criterion"),
            version(1, "1.0.0", vec![], facets_of(&[])),
            version(2, "1.0.0", vec![], facets_of(&[])),
            version(3, "1.0.0", vec![], facets_of(&[])),
        ])
        .expect("target packages");

    let store = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/stored-runtime").expect("instance"),
    );
    let app = stored_package("app", &["serde"]);
    store.upsert(&app).await.expect("first publish");
    writer
        .apply_ops(&[CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id: app.id,
                stem_id: GlobalStore::<index::engine::Configured>::stem_id(
                    &app.package.coordinates,
                ),
                version_canonical: "1.0.0".into(),
                version_original: "1.0.0".into(),
            },
            published_at: None,
            toolchain: None,
            license: None,
            edges: vec![build_edge("criterion")],
            facets: facets_of(&["serde"]),
            source: None,
        }])
        .expect("plant build edge beside the runtime edge");
    assert_eq!(edge_kinds(writer.engine(), app.id), vec![
        ("build".to_owned(), "criterion".to_owned()),
        ("runtime".to_owned(), "serde".to_owned()),
    ]);

    let outbox = index::coordination::Outbox::new(Arc::clone(&writer));
    let mut stored_facets = SearchFacets::default();
    stored_facets.dependencies = vec![SmolStr::new("tokio")];
    outbox
        .record_stored(
            &store,
            app.id,
            ContentHash::from_bytes([4u8; 32]),
            Some(&stored_facets),
        )
        .await
        .expect("record stored");
    assert_eq!(edge_kinds(writer.engine(), app.id), vec![
        ("build".to_owned(), "criterion".to_owned()),
        ("runtime".to_owned(), "tokio".to_owned()),
    ]);

    let mut empty = SearchFacets::default();
    empty.dependencies.clear();
    outbox
        .record_stored(
            &store,
            app.id,
            ContentHash::from_bytes([5u8; 32]),
            Some(&empty),
        )
        .await
        .expect("empty dependency list");
    assert_eq!(
        edge_kinds(writer.engine(), app.id),
        vec![
            ("build".to_owned(), "criterion".to_owned()),
            ("runtime".to_owned(), "tokio".to_owned()),
        ],
        "an empty dependency list leaves the runtime edge"
    );

    let revised = stored_package("app", &["serde"]);
    store
        .replace_feed_edges(&revised)
        .await
        .expect("feed republish");
    assert_eq!(edge_kinds(writer.engine(), app.id), vec![
        ("build".to_owned(), "criterion".to_owned()),
        ("runtime".to_owned(), "serde".to_owned()),
    ]);

    store.refresh_dependents().await.expect("sweep");
    let engine = writer.engine();
    assert_eq!(dependents_of(engine, 1), 1, "serde is the runtime edge");
    assert_eq!(dependents_of(engine, 2), 0, "tokio was replaced");
    assert_eq!(
        dependents_of(engine, 3),
        0,
        "a build edge is not a dependent"
    );
}

#[tokio::test]
async fn ledger_degree_matches_the_sql_sweep_on_same_ecosystem_edges() {
    use index::edge_project::records_from_ops;
    use index::engine::turso_vc::VersionedCatalog;

    let writer = Arc::new(migrated_writer());
    let ops = vec![
        package(1, "serde"),
        package(2, "tokio"),
        package(4, "criterion"),
        package(5, "app"),
        package(6, "facet-only"),
        version(1, "1.0.0", vec![], facets_of(&[])),
        version(2, "1.0.0", vec![], facets_of(&[])),
        version(4, "1.0.0", vec![], facets_of(&[])),
        version(6, "1.0.0", vec![], facets_of(&["tokio"])),
        version(
            5,
            "1.0.0",
            vec![
                runtime(heart::Language::Rust, "serde"),
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
            facets_of(&["tokio"]),
        ),
    ];
    writer.apply_ops(&ops).expect("catalog writes");
    let store = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/ledger-degree").expect("instance"),
    );
    store.refresh_dependents().await.expect("sweep");

    let mut ledger = VersionedCatalog::open().expect("ledger");
    for record in records_from_ops(&ops) {
        ledger.put_record(&record).expect("versioned row");
    }
    let degree = ledger.dependents();
    let rust = |name: &str| (Language::Rust, SmolStr::new(name));
    let python = |name: &str| (Language::Python, SmolStr::new(name));

    assert_eq!(dependents_of(writer.engine(), 1), 1);
    assert_eq!(degree.get(&rust("serde")).copied(), Some(1));
    assert!(!degree.contains_key(&python("serde")));
    assert!(!degree.contains_key(&rust("criterion")));
    assert_eq!(dependents_of(writer.engine(), 2), 1, "facet fallback stays on SQL");
    assert!(!degree.contains_key(&rust("tokio")));
}

#[tokio::test]
async fn a_homebrew_recipe_edge_counts_on_the_sweep_and_the_adopted_ledger() {
    let writer = Arc::new(migrated_writer());
    let openssl = PackageStemWire {
        stem_id: stem_id(2),
        ecosystem: Language::Cpp,
        name_struct: "pkg:generic/openssl".into(),
        name_canonical: "openssl".into(),
        name_original: "openssl".into(),
    };
    let curl = PackageStemWire {
        stem_id: stem_id(5),
        ecosystem: Language::Cpp,
        name_struct: "pkg:generic/curl".into(),
        name_canonical: "curl".into(),
        name_original: "curl".into(),
    };
    writer
        .apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: openssl,
                repo_url: None,
            },
            CatalogOp::UpsertPackage {
                stem: curl,
                repo_url: None,
            },
            version(2, "1.0.0", vec![], facets_of(&[])),
            version(
                5,
                "8.0.0",
                vec![EdgeWire {
                    dep_ecosystem: Language::Cpp,
                    dep_name_canonical: "openssl".into(),
                    requirement: String::new(),
                    kind: EdgeKind::Recipe,
                    source: EdgeSource::Feed,
                    resolved_stem: None,
                }],
                facets_of(&[]),
            ),
        ])
        .expect("catalog writes");
    let store = GlobalStore::new(
        Arc::clone(&writer),
        InstanceToken::new("test/recipe-degree").expect("instance"),
    );
    store.refresh_dependents().await.expect("sweep");
    assert_eq!(dependents_of(writer.engine(), 2), 1);

    let mut ledger = index::engine::turso_vc::VersionedCatalog::open().expect("ledger");
    ledger.adopt_catalog(writer.engine()).expect("adopt");
    assert_eq!(
        ledger
            .dependents()
            .get(&(Language::Cpp, SmolStr::new("openssl")))
            .copied(),
        Some(1)
    );
}
