//! End-to-end integration test against the **real** DoltLite engine
//! (`--features dolt-engine`, INDEX-PLAN IP-1 gate).
//!
//! Exercises the full production path with no fakes: open an in-memory
//! `catalog.dolt` → migrate to schema v4 → apply a batch (package + version +
//! edges + outbox) in one transaction → commit the batch (a real `dolt_commit`)
//! → claim the outbox → page `changed_since` → read a table **as of** the pinned
//! commit through the `dolt_at_<table>` time-travel path.
#![cfg(feature = "dolt-engine")]

use heart::query::{AsOf, UnixMilliseconds};
use index::engine::dolt::DoltEngine;
use index::engine::{CatalogEngine, VersioningEngine};
use index::enums::{EdgeKind, EdgeSource, SinkKind};
use index::ids::{PackageId, PackageStemId};
use index::migrations::runner::migrate_to_v4;
use index::protocol::{
    CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates,
};
use index::store::writer::CatalogWriter;
use index::store::{Catalog, CatalogCursor, MetaStore};

fn stem(seed: u8) -> PackageStemId {
    let mut bytes = [0u8; 16];
    bytes[0] = seed;
    PackageStemId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn version(seed: u8) -> PackageId {
    let mut bytes = [0u8; 16];
    bytes[15] = seed;
    PackageId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn migrated_writer() -> CatalogWriter<DoltEngine> {
    let engine = DoltEngine::open_in_memory().expect("open in-memory catalog.dolt");
    migrate_to_v4(&engine).expect("migrate to schema v4 on real engine");
    CatalogWriter::new(engine)
}

#[test]
fn end_to_end_apply_commit_claim_changed_since_and_historical_read() {
    let writer = migrated_writer();

    // ── apply a batch: package + version (with edges) → one transaction ────────
    let batch = vec![
        CatalogOp::UpsertPackage {
            stem: PackageStemWire {
                stem_id: stem(1),
                ecosystem: heart::Language::Rust,
                name_struct: "pkg:cargo/axum".to_owned(),
                name_canonical: "axum".to_owned(),
                name_original: "axum".to_owned(),
            },
            repo_url: Some("https://github.com/tokio-rs/axum".to_owned()),
        },
        CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id: version(1),
                stem_id: stem(1),
                version_canonical: "0.7.9".to_owned(),
                version_original: "0.7.9".to_owned(),
            },
            published_at: Some(1_700_000_000_000),
            toolchain: None,
            license: Some("MIT".to_owned()),
            edges: vec![EdgeWire {
                dep_ecosystem: heart::Language::Rust,
                dep_name_canonical: "tokio".to_owned(),
                requirement: "^1".to_owned(),
                kind: EdgeKind::Runtime,
                source: EdgeSource::Manifest,
                resolved_stem: None,
            }],
            facets: FacetWire {
                keywords: Some("web http".to_owned()),
                quality_ppm: Some(900_000),
                extras: None,
            },
            source: None,
        },
    ];
    let report = writer.apply_ops(&batch).expect("batch applies on real engine");
    assert_eq!(report.applied, 2);
    assert_eq!(report.outbox_rows, 1, "only the version upsert fans out");

    // ── commit the batch: a real dolt_commit ──────────────────────────────────
    let commit = writer.commit_batch("ingest axum@0.7.9").expect("dolt_commit");
    assert!(!commit.0.is_empty(), "commit produced a hash");

    // ── read-back through the facade ──────────────────────────────────────────
    let package = writer
        .get_package(stem(1))
        .expect("read package")
        .expect("package present after commit");
    assert_eq!(package.name_canonical, "axum");

    // ── outbox claim + changed_since cursor ───────────────────────────────────
    let claimed = writer.outbox_claim(SinkKind::Text, 16).expect("claim outbox");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].version_id, Some(version(1)));

    let page = writer
        .changed_since(CatalogCursor::default())
        .expect("changed_since page");
    assert_eq!(page.rows.len(), 1);
    assert!(page.next.after_seq > 0, "cursor advanced");

    // ── historical read: a SECOND commit, then read the FIRST via dolt_at ──────
    writer
        .apply_ops(&[CatalogOp::UpsertPackage {
            stem: PackageStemWire {
                stem_id: stem(2),
                ecosystem: heart::Language::Rust,
                name_struct: "pkg:cargo/tower".to_owned(),
                name_canonical: "tower".to_owned(),
                name_original: "tower".to_owned(),
            },
            repo_url: None,
        }])
        .expect("second batch applies");
    writer.commit_batch("ingest tower").expect("second commit");

    // Present state has two packages.
    use index::engine::stmt;
    use sea_orm::sea_query::{Asterisk, Expr, Func, Query};
    let mut tip = Query::select();
    tip.from(index::entity::packages::Entity::default())
        .expr(Func::count(Expr::col(Asterisk)));
    let now_count: Vec<i64> = stmt::query_select(writer.engine(), tip, &mut |row| {
        row.get_integer(0)
    })
    .expect("count now");
    assert_eq!(now_count, vec![2]);

    // As of one commit back (HEAD~1) there was exactly one package.
    use sea_orm::sea_query::{Asterisk, Expr, Func};
    let past_count: Vec<i64> = writer
        .engine()
        .query_rows_at::<index::entity::packages::Entity, _>(
            "HEAD~1",
            &mut |q| {
                q.expr(Func::count(Expr::col(Asterisk)));
            },
            &mut |row| row.get_integer(0),
        )
        .expect("historical count via packages::Entity DOLT_AT");
    assert_eq!(past_count, vec![1], "one commit back had a single package");

    // The `at()` view resolves a pinned commit for the first commit's instant.
    let pinned = writer
        .at(&AsOf::Commit(heart::query::CatalogCommitHash(commit.0.clone())))
        .expect("pin AsOf::Commit");
    assert_eq!(pinned.commit.0, commit.0);

    // `AsOf::Time` far in the future resolves to the latest commit (not an error).
    let future = writer.at(&AsOf::Time(UnixMilliseconds(32_503_680_000_000)));
    assert!(future.is_ok(), "future time resolves to newest commit");
}
