//! A fresh versioned ledger adopts the SQL catalog tip, including the artifact
//! digest and runtime edges. Adopting that tip again revises nothing.

mod common;

use common::{migrated_writer, stem_id, version_id};
use index::{
    engine::turso_vc::VersionedCatalog,
    enums::{EdgeKind, EdgeSource, SourceKind},
    metadata::SearchFacets,
    pid::ContentDigest,
    protocol::{
        CatalogOp, EdgeWire, FacetWire, PackageStemWire, SourceAcquisitionWire, VersionCoordinates,
    },
    schema::catalog_map,
    store::MetaStore,
};

#[test]
fn a_fresh_ledger_adopts_the_sql_tip_and_a_second_pass_is_unchanged() {
    let writer = migrated_writer();
    let checksum = "ab".repeat(32);
    let facets = SearchFacets::default();
    let (keywords, quality_ppm, extras) = catalog_map::facets_to_row(&facets).expect("facets");
    writer
        .apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem_id(1),
                    ecosystem: heart::Language::Rust,
                    name_struct: "pkg:cargo/memchr".into(),
                    name_canonical: "memchr".into(),
                    name_original: "memchr".into(),
                },
                repo_url: None,
            },
            CatalogOp::UpsertVersion {
                coordinates: VersionCoordinates {
                    version_id: version_id(1),
                    stem_id: stem_id(1),
                    version_canonical: "2.8.0".into(),
                    version_original: "2.8.0".into(),
                },
                published_at: None,
                toolchain: None,
                license: Some("MIT".into()),
                edges: vec![EdgeWire {
                    dep_ecosystem: heart::Language::Rust,
                    dep_name_canonical: "libc".into(),
                    requirement: "^0.2".into(),
                    kind: EdgeKind::Runtime,
                    source: EdgeSource::Manifest,
                    resolved_stem: None,
                }],
                facets: FacetWire {
                    keywords,
                    quality_ppm,
                    extras,
                },
                source: Some(SourceAcquisitionWire {
                    source_kind: SourceKind::ReconstructedRegistryPackage,
                    source_pack: None,
                    source_rev: Some("0123456789abcdef0123456789abcdef01234567".into()),
                    registry_checksum: Some(checksum.clone()),
                    registry_package_uri: None,
                }),
            },
        ])
        .expect("sql");

    let mut ledger = VersionedCatalog::open().expect("ledger");
    let first = ledger.adopt_catalog(writer.engine()).expect("adopt");
    assert_eq!(first.revised, 1);
    assert_eq!(first.unchanged, 0);
    let row = ledger
        .materialize("rust", "memchr", "2.8.0")
        .expect("read")
        .expect("row");
    assert!(matches!(row.content, Some(ContentDigest::Sha256(_))));
    assert_eq!(row.edges.len(), 1);
    assert_eq!(row.edges[0].name.as_str(), "libc");
    assert_eq!(row.license.as_deref(), Some("MIT"));

    let second = ledger.adopt_catalog(writer.engine()).expect("replay");
    assert_eq!(second.revised, 0);
    assert_eq!(second.unchanged, 1);
}
