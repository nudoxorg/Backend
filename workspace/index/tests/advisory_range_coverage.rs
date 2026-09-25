//! An OSV event range lists the catalog versions it covers.
//!
//! `introduced:0,fixed:1.6.1` covers `1.0.0` and leaves `1.6.1` out. The
//! follower still emits only the advisory upsert; the driver adds the listing
//! from versions already stored for that stem.

mod common;

use common::{migrated_writer, version_id};
use index::{
    ingest::{
        advisory::{parse_rustsec, resolve_osv},
        driver::expand_advisory_ranges,
    },
    protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates},
    store::MetaStore,
};

#[test]
fn a_fixed_range_lists_only_the_versions_inside_it() {
    let body = br#"{"id":"RUSTSEC-1","affected":[{"package":{"ecosystem":"crates.io","name":"smallvec"},"ranges":[{"events":[{"introduced":"0"},{"fixed":"1.6.1"}]}]}]}"#;
    let source = &resolve_osv(body, 1).expect("resolve")[0];
    let stem = source.stem_id.expect("stem");
    let writer = migrated_writer();
    let inside = version_id(1);
    let fixed = version_id(2);
    writer
        .apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem,
                    ecosystem: heart::Language::Rust,
                    name_struct: "pkg:cargo/smallvec".to_owned(),
                    name_canonical: "smallvec".to_owned(),
                    name_original: "smallvec".to_owned(),
                },
                repo_url: None,
            },
            version(stem, inside, "1.0.0"),
            version(stem, fixed, "1.6.1"),
        ])
        .expect("versions");

    let ops = expand_advisory_ranges(writer.engine(), &[source.to_upsert_op()]).expect("expand");
    assert!(matches!(ops[0], CatalogOp::UpsertAdvisory { .. }));
    assert_eq!(ops.len(), 2);
    match &ops[1] {
        CatalogOp::SetListing {
            version, reason, ..
        } => {
            assert_eq!(*version, inside);
            assert_eq!(reason.as_deref(), Some("RUSTSEC-1"));
        }
        other => panic!("expected the in-range listing, got {other:?}"),
    }
}

#[test]
fn last_affected_is_inclusive_and_a_rustsec_floor_is_exclusive() {
    let osv = br#"{"id":"RUSTSEC-2020-0001","affected":[{"package":{"ecosystem":"crates.io","name":"smallvec"},"ranges":[{"events":[{"introduced":"1.2.0"},{"last_affected":"1.2.5"}]}]}]}"#;
    let source = &resolve_osv(osv, 1).expect("resolve")[0];
    assert_eq!(
        source.version_range.as_deref(),
        Some("introduced:1.2.0,last_affected:1.2.5")
    );
    let rustsec = parse_rustsec(
        r#"
        [advisory]
        id = "RUSTSEC-2020-0001"
        package = "smallvec"
        date = "2020-01-15"
        [versions]
        patched = [">= 1.2.6"]
        "#,
        1,
    )
    .expect("rustsec");
    let stem = source.stem_id.expect("stem");
    assert_eq!(rustsec.stem_id, Some(stem));
    let writer = migrated_writer();
    let below = version_id(1);
    let edge = version_id(2);
    let above = version_id(3);
    writer
        .apply_ops(&[
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem,
                    ecosystem: heart::Language::Rust,
                    name_struct: "pkg:cargo/smallvec".to_owned(),
                    name_canonical: "smallvec".to_owned(),
                    name_original: "smallvec".to_owned(),
                },
                repo_url: None,
            },
            version(stem, below, "1.1.0"),
            version(stem, edge, "1.2.5"),
            version(stem, above, "1.2.6"),
        ])
        .expect("versions");

    let osv_ops = expand_advisory_ranges(writer.engine(), &[source.to_upsert_op()]).expect("osv");
    let rustsec_ops =
        expand_advisory_ranges(writer.engine(), &[rustsec.to_upsert_op()]).expect("rustsec");
    let listed = |ops: &[CatalogOp]| {
        ops.iter()
            .filter_map(|op| match op {
                CatalogOp::SetListing { version, .. } => Some(*version),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(listed(&osv_ops), vec![edge]);
    assert_eq!(listed(&rustsec_ops), vec![below, edge]);
}

fn version(stem: index::ids::PackageStemId, id: index::ids::PackageId, name: &str) -> CatalogOp {
    CatalogOp::UpsertVersion {
        coordinates: VersionCoordinates {
            version_id: id,
            stem_id: stem,
            version_canonical: name.to_owned(),
            version_original: name.to_owned(),
        },
        published_at: None,
        toolchain: None,
        license: None,
        edges: Vec::new(),
        facets: FacetWire::default(),
        source: None,
    }
}
