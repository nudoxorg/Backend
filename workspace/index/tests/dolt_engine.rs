//! End-to-end integration test against the **real** DoltLite engine
//! (`--features dolt-engine`, INDEX-PLAN IP-1 gate).
//!
//! Exercises the full production path with no fakes: open an in-memory
//! `catalog.dolt` → migrate to schema v4 → apply a batch (package + version +
//! edges + outbox) in one transaction → commit the batch (a real `dolt_commit`)
//! → claim the outbox → page `changed_since` → read a table **as of** the pinned
//! commit through the `dolt_at_<table>` time-travel path.
#![cfg(feature = "dolt-engine")]

mod common;

use heart::query::{AsOf, UnixMilliseconds};
use index::engine::dolt::DoltEngine;
use index::engine::{CatalogEngine, VersioningEngine};
use index::enums::{EdgeKind, EdgeSource, SinkKind};
use index::ids::{PackageId, PackageStemId};
use index::migrations::runner::migrate_to_v4;
use index::protocol::{CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates};
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
    let report = writer
        .apply_ops(&batch)
        .expect("batch applies on real engine");
    assert_eq!(report.applied, 2);
    assert_eq!(report.outbox_rows, 1, "only the version upsert fans out");

    // ── commit the batch: a real dolt_commit ──────────────────────────────────
    let commit = writer
        .commit_batch("ingest axum@0.7.9")
        .expect("dolt_commit");
    assert!(!commit.0.is_empty(), "commit produced a hash");

    // ── read-back through the facade ──────────────────────────────────────────
    let package = writer
        .get_package(stem(1))
        .expect("read package")
        .expect("package present after commit");
    assert_eq!(package.name_canonical, "axum");

    // ── outbox claim + changed_since cursor ───────────────────────────────────
    let claimed = writer
        .outbox_claim(SinkKind::Text, 16)
        .expect("claim outbox");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].version_id, Some(*version(1).as_uuid()));

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
    tip.from(index::entity::packages::Entity)
        .expr(Func::count(Expr::col(Asterisk)));
    let now_count: Vec<i64> =
        stmt::query_select(writer.engine(), tip, &mut |row| row.get_integer(0)).expect("count now");
    assert_eq!(now_count, vec![2]);

    // As of one commit back (HEAD~1) there was exactly one package.
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
        .at(&AsOf::Commit(heart::query::CatalogCommitHash(
            commit.0.clone(),
        )))
        .expect("pin AsOf::Commit");
    assert_eq!(pinned.commit.0, commit.0);

    // `AsOf::Time` far in the future resolves to the latest commit (not an error).
    let future = writer.at(&AsOf::Time(UnixMilliseconds(32_503_680_000_000)));
    assert!(future.is_ok(), "future time resolves to newest commit");
}

/// The regression test for the defect this whole engine wiring exists to make
/// impossible: `index` links **two** SQLite implementations into every binary
/// that enables `dolt-engine` — DoltLite for the versioned catalog, and the stock
/// SQLite that `rusqlite`'s `bundled` feature statically embeds for the ephemeral
/// scratch store (INDEX-PLAN ID-2).
///
/// They used to be indistinguishable at link time. `rusqdoltlite` declared bare
/// `sqlite3_*` externs with no `#[link]`, and their 291 exported names overlap
/// stock SQLite's 280 exactly, so the linker satisfied both crates from whichever
/// archive it happened to scan first. When the DoltLite amalgamation was absent
/// entirely, the catalog silently *became* the scratch store's engine: plain SQL
/// worked, history did not exist, and the only symptom was `dolt_commit` failing
/// as "no such function" somewhere else entirely.
///
/// Asserting on content, not on `is_ok()`: the two connections must report
/// **different** SQLite versions, and `dolt_version()` must exist on exactly one
/// of them. A build where one engine had been substituted for the other passes
/// neither check.
#[test]
fn catalog_and_scratch_store_are_two_distinct_engines_in_the_same_binary() {
    // The scratch store's engine: stock SQLite via rusqlite/bundled.
    let scratch = rusqlite::Connection::open_in_memory().expect("open rusqlite scratch store");
    let scratch_sqlite_version: String = scratch
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .expect("stock SQLite answers sqlite_version()");
    let scratch_dolt = scratch.query_row("SELECT dolt_version()", [], |row| row.get::<_, String>(0));
    assert!(
        scratch_dolt.is_err(),
        "the scratch store must be plain SQLite; it answered dolt_version() with {scratch_dolt:?}, \
         which means DoltLite's symbols captured rusqlite's calls"
    );

    // The catalog's engine: DoltLite, live in the same process.
    let catalog = DoltEngine::open_in_memory().expect("open DoltLite catalog");
    let catalog_dolt: Vec<String> = catalog
        .query_rows("SELECT dolt_version()", &[], &mut |row| row.get_text(0))
        .expect("DoltLite answers dolt_version()");
    assert_eq!(catalog_dolt.len(), 1);
    assert!(
        !catalog_dolt[0].is_empty(),
        "dolt_version() returned an empty string"
    );

    let catalog_sqlite_version: Vec<String> = catalog
        .query_rows("SELECT sqlite_version()", &[], &mut |row| row.get_text(0))
        .expect("DoltLite answers sqlite_version()");
    assert_ne!(
        catalog_sqlite_version[0], scratch_sqlite_version,
        "both connections reported SQLite {scratch_sqlite_version}, so one engine is serving \
         both crates — the substitution this wiring exists to prevent"
    );

    // And the catalog really keeps history, not just a distinct version string.
    catalog
        .execute("CREATE TABLE probe(id INTEGER PRIMARY KEY)", &[])
        .expect("create table on catalog");
    catalog.dolt_add_all().expect("stage working set");
    let head = catalog.dolt_commit("probe").expect("real dolt_commit");
    assert!(
        head.0.len() == 40 && head.0.chars().all(|c| c.is_ascii_hexdigit()),
        "dolt_commit returned {:?}; a real commit is a 40-character hex content hash, and a stub \
         or a substituted engine cannot produce one",
        head.0
    );
}

/// The guard on the mechanism this file's existence depends on.
///
/// Everything above proves `DoltEngine` works. Nothing above proves the *rest of
/// the suite* runs on it — and for the whole life of this file, it did not.
/// `default = ["test-engine"]` routed every other test binary through
/// `MemoryEngine`, whose `dolt_commit` appends to a `Vec`, and this file was
/// gated behind a non-default feature so it never compiled to say otherwise. The
/// suite was uniformly green and uniformly measuring a fake.
///
/// So this asserts on the shared harness, not on a locally-opened engine:
/// `common::migrated_writer` is the constructor the other eight test binaries
/// call, and a commit taken through it must be a real DoltLite content hash —
/// 40 hex characters. The fake cannot produce one; its hashes are 64-character
/// BLAKE3 hex, so the assertion fails on length before it can fail on content.
/// Flipping the default back, or making [`index::engine::Configured`] prefer the
/// fake when Cargo unifies both features on, fails here and nowhere else.
#[test]
fn the_shared_test_harness_runs_on_the_real_versioned_engine() {
    let writer = common::migrated_writer();
    writer
        .apply_ops(&[CatalogOp::UpsertPackage {
            stem: PackageStemWire {
                stem_id: stem(3),
                ecosystem: heart::Language::Rust,
                name_struct: "pkg:cargo/serde".to_owned(),
                name_canonical: "serde".to_owned(),
                name_original: "serde".to_owned(),
            },
            repo_url: None,
        }])
        .expect("harness writer applies");

    let commit = writer
        .commit_batch("harness engine identity probe")
        .expect("the harness engine must be able to commit");

    assert!(
        commit.0.len() == 40 && commit.0.chars().all(|c| c.is_ascii_hexdigit()),
        "`common::migrated_writer` committed {:?} ({} chars). A real DoltLite \
         commit is a 40-character hex content hash; the 64-character value the \
         MemoryEngine fake produces means the shared harness — and therefore \
         every other test binary in this crate — is running against the fake \
         again.",
        commit.0,
        commit.0.len()
    );

    // And the engine type itself is the real one, stated directly rather than
    // inferred from the hash shape, so a future engine with 40-hex hashes cannot
    // satisfy this test by coincidence.
    assert_eq!(
        std::any::TypeId::of::<index::engine::Configured>(),
        std::any::TypeId::of::<DoltEngine>(),
        "`index::engine::Configured` must resolve to DoltEngine whenever the \
         `dolt-engine` feature is on, including when feature unification also \
         turns `test-engine` on"
    );
}
