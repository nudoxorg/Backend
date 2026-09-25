//! An OSV event range lists the catalog versions it covers.
//!
//! `introduced:0,fixed:1.6.1` covers `1.0.0` and leaves `1.6.1` out. The
//! follower still emits only the advisory upsert; the driver adds the listing
//! from versions already stored for that stem.

mod common;

use common::{migrated_writer, version_id};
use index::ingest::advisory::resolve_osv;
use index::store::MetaStore;
use index::ingest::driver::expand_advisory_ranges;
use index::protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates};

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
        CatalogOp::SetListing { version, reason, .. } => {
            assert_eq!(*version, inside);
            assert_eq!(reason.as_deref(), Some("RUSTSEC-1"));
        }
        other => panic!("expected the in-range listing, got {other:?}"),
    }
}

fn version(
    stem: index::ids::PackageStemId,
    id: index::ids::PackageId,
    name: &str,
) -> CatalogOp {
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
