//! W4b: the archive conditional-GET cache persistence substrate
//! (`store::lifecycle::{get_archive_cache_meta, set_archive_cache_meta}`)
//! against the real migrated catalog engine — no server feature, no network,
//! just the durable store the conditional-GET plumbing is meant to sit on.

mod common;

use common::{migrated_writer, stem_id, version_id};

use index::protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates};
use index::store::lifecycle::{ArchiveCacheMeta, get_archive_cache_meta, set_archive_cache_meta};
use index::store::{MetaError, MetaStore};

fn upsert_package(seed: u8) -> CatalogOp {
    CatalogOp::UpsertPackage {
        stem: PackageStemWire {
            stem_id: stem_id(seed),
            ecosystem: heart::Language::Rust,
            name_struct: format!("pkg:cargo/pkg{seed}"),
            name_canonical: format!("pkg{seed}"),
            name_original: format!("Pkg{seed}"),
        },
        repo_url: Some(format!("https://example.test/pkg{seed}")),
    }
}

fn upsert_version(stem_seed: u8, version_seed: u8) -> CatalogOp {
    CatalogOp::UpsertVersion {
        coordinates: VersionCoordinates {
            version_id: version_id(version_seed),
            stem_id: stem_id(stem_seed),
            version_canonical: format!("1.0.{version_seed}"),
            version_original: format!("v1.0.{version_seed}"),
        },
        published_at: Some(1000),
        toolchain: None,
        license: Some("MIT".to_owned()),
        edges: Vec::new(),
        facets: FacetWire::default(),
        source: None,
    }
}

#[test]
fn unfetched_version_has_no_cached_validators() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(1), upsert_version(1, 1)])
        .expect("seed package+version");

    let meta = get_archive_cache_meta(writer.engine(), version_id(1)).expect("read");
    assert!(meta.is_empty());
    assert_eq!(meta, ArchiveCacheMeta::default());
}

#[test]
fn set_then_get_round_trips_both_validators() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(2), upsert_version(2, 2)])
        .expect("seed package+version");

    let meta = ArchiveCacheMeta {
        etag: Some("\"abc123\"".to_owned()),
        last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_owned()),
    };
    set_archive_cache_meta(writer.engine(), version_id(2), &meta).expect("write");

    let read_back = get_archive_cache_meta(writer.engine(), version_id(2)).expect("read");
    assert_eq!(read_back, meta);
    assert!(!read_back.is_empty());
}

#[test]
fn set_overwrites_a_previous_value() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(3), upsert_version(3, 3)])
        .expect("seed package+version");

    set_archive_cache_meta(
        writer.engine(),
        version_id(3),
        &ArchiveCacheMeta {
            etag: Some("\"first\"".to_owned()),
            last_modified: None,
        },
    )
    .expect("first write");

    // A later fetch observes a fresh ETag and no Last-Modified this time —
    // the new write must fully replace the old row, not merge with it.
    set_archive_cache_meta(
        writer.engine(),
        version_id(3),
        &ArchiveCacheMeta {
            etag: Some("\"second\"".to_owned()),
            last_modified: None,
        },
    )
    .expect("second write");

    let read_back = get_archive_cache_meta(writer.engine(), version_id(3)).expect("read");
    assert_eq!(read_back.etag.as_deref(), Some("\"second\""));
    assert_eq!(read_back.last_modified, None);
}

#[test]
fn set_on_a_version_with_no_row_is_a_missing_row_error() {
    // A conditional-GET write must never silently create a phantom version
    // row — `PackageId`s always come from a real `UpsertVersion`.
    let writer = migrated_writer();
    let never_inserted = version_id(99);

    let err = set_archive_cache_meta(
        writer.engine(),
        never_inserted,
        &ArchiveCacheMeta {
            etag: Some("\"x\"".to_owned()),
            last_modified: None,
        },
    )
    .expect_err("no such versions row");

    assert!(matches!(err, MetaError::MissingRow { .. }), "{err:?}");
}

#[test]
fn clearing_a_validator_the_origin_stopped_sending_round_trips_to_none() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(4), upsert_version(4, 4)])
        .expect("seed package+version");

    set_archive_cache_meta(
        writer.engine(),
        version_id(4),
        &ArchiveCacheMeta {
            etag: Some("\"had-one\"".to_owned()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_owned()),
        },
    )
    .expect("first write");

    // Caller explicitly clears both — e.g. the origin migrated away from
    // sending validators at all.
    set_archive_cache_meta(writer.engine(), version_id(4), &ArchiveCacheMeta::default())
        .expect("clear");

    let read_back = get_archive_cache_meta(writer.engine(), version_id(4)).expect("read");
    assert!(read_back.is_empty());
}
