//! Adversarial store tests: apply_ops atomicity, outbox monotonicity, watermark
//! non-regression, changed_since cursor stability, and AsOf boundary cases.

mod common;

use common::{gen_stamp, migrated_writer, stem_id, version_id};

use heart::query::{AsOf, UnixMilliseconds};
use index::enums::{IrStatus, ListingStatus, SinkKind};
use index::protocol::{CatalogOp, FacetWire, PackageStemWire, VersionCoordinates};
use index::store::{Catalog, CatalogCursor, MetaError, MetaStore};

/// Build a valid UpsertPackage op for a stem seed.
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

/// Build a valid UpsertVersion op tying a version to a stem. Each `version_seed`
/// yields a distinct `version_canonical` so that two different version ids never
/// collide on the `UNIQUE(stem_id, version_canonical)` constraint (a version's
/// identity *is* its stem + canonical string).
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
fn apply_ops_persists_package_and_fans_out_outbox() {
    let writer = migrated_writer();
    // Doctrine §4: the measured region is one atomic catalog batch. Note the
    // `disk_delta_bytes` on this line is 0 *by construction* — `migrated_writer`
    // opens a catalog with no file behind it, so there is nothing on disk to
    // grow. That is a property of this constructor, not of the engine: the
    // storage suite (`tests/storage_catalog_scaling.rs`) takes the real numbers
    // through `migrated_disk_writer` on the same real engine. The reliable
    // figures *here* are the applied/outbox row counts asserted below.
    // The measured directory is a scratch tempdir, not the repo root: `measured`
    // walks the directory recursively, and pointing it at `.` would stat the
    // whole `target/` tree and report build output as this test's cost.
    let scratch = tempfile::tempdir().expect("tempdir");
    let (report, _cost) = nudox_test_support::measured(
        "store/apply_ops_batch",
        scratch.path(),
        || writer.apply_ops(&[upsert_package(1), upsert_version(1, 1)]),
    );
    let report = report.expect("batch applies");
    assert_eq!(report.applied, 2);
    // Only the version upsert fans out to the text sink.
    assert_eq!(report.outbox_rows, 1);

    let fetched = writer
        .get_package(stem_id(1))
        .expect("read")
        .expect("present");
    assert_eq!(fetched.name_canonical, "pkg1");

    let claimed = writer.outbox_claim(SinkKind::Text, 10).expect("claim");
    assert_eq!(claimed.len(), 1);
    assert!(claimed[0].version_id.is_some());
}

#[test]
fn apply_ops_is_atomic_a_failing_op_leaves_no_rows() {
    let writer = migrated_writer();
    // The first op is valid. The second collides on
    // UNIQUE(ecosystem, name_canonical) with a *different* stem_id, so it is a
    // hard error mid-batch — the whole transaction must roll back.
    let good = upsert_package(1);
    let mut collide = upsert_package(2);
    if let CatalogOp::UpsertPackage { stem, .. } = &mut collide {
        // Same (ecosystem, name_canonical) as pkg1 but a different stem_id ⇒
        // UNIQUE(ecosystem, name_canonical) violation.
        stem.name_canonical = "pkg1".to_owned();
    }

    let result = writer.apply_ops(&[good, collide]);
    assert!(result.is_err(), "the colliding op must fail the batch");

    // Atomicity: the *good* op's row must have been rolled back too.
    let present = writer.get_package(stem_id(1)).expect("read");
    assert!(
        present.is_none(),
        "a failing batch must leave no partial rows"
    );
    // And no outbox rows leaked.
    let claimed = writer.outbox_claim(SinkKind::Text, 10).expect("claim");
    assert!(
        claimed.is_empty(),
        "a failing batch must leave no outbox rows"
    );
}

#[test]
fn a_failing_op_after_an_outbox_producing_op_leaves_zero_outbox_rows() {
    let writer = migrated_writer();
    // A successful package+version (which fans out one outbox row), then a
    // colliding package that fails on UNIQUE(ecosystem, name_canonical). The
    // whole batch must roll back — including the already-emitted outbox row.
    let mut collide = upsert_package(9);
    if let CatalogOp::UpsertPackage { stem, .. } = &mut collide {
        stem.name_canonical = "pkg1".to_owned();
    }
    let result = writer.apply_ops(&[
        upsert_package(1),
        upsert_version(1, 1), // emits an outbox row mid-batch
        collide,              // hard error → rollback
    ]);
    assert!(result.is_err(), "the colliding op must fail the batch");

    let claimed = writer.outbox_claim(SinkKind::Text, 10).expect("claim");
    assert!(
        claimed.is_empty(),
        "an outbox row emitted before a later failure must roll back too"
    );
    assert!(
        writer.get_package(stem_id(1)).expect("read").is_none(),
        "the successful rows before the failure must roll back too"
    );
}

#[test]
fn as_of_time_on_empty_history_is_typed_error_not_panic() {
    // A freshly migrated catalog that has never been committed: an AsOf::Time
    // before any commit must be MetaError::NoCommitAtInstant, never a panic.
    let engine = index::engine::Configured::open_in_memory().expect("open");
    index::migrations::runner::migrate_to_v4(&engine).expect("migrate");
    let writer = index::store::writer::CatalogWriter::new(engine);
    let before = writer.at(&AsOf::Time(UnixMilliseconds(0)));
    assert!(matches!(before, Err(MetaError::NoCommitAtInstant)));
}

#[test]
fn outbox_claim_is_monotonic_and_watermark_cannot_regress() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(1), upsert_version(1, 1)])
        .expect("batch one");
    writer
        .apply_ops(&[upsert_version(1, 2)])
        .expect("batch two");

    let first = writer.outbox_claim(SinkKind::Text, 10).expect("claim");
    assert_eq!(first.len(), 2);
    let seqs: Vec<i64> = first.iter().map(|row| row.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq strictly increases"
    );

    // Advance the watermark past the first row.
    writer
        .advance_sink_watermark(SinkKind::Text, seqs[0], 1)
        .expect("advance once");
    let after = writer.outbox_claim(SinkKind::Text, 10).expect("claim");
    assert_eq!(after.len(), 1, "claim only rows after the watermark");
    assert_eq!(after[0].seq, seqs[1]);

    // A regression is rejected.
    let regress = writer.advance_sink_watermark(SinkKind::Text, seqs[0] - 1, 2);
    assert!(matches!(
        regress,
        Err(MetaError::WatermarkRegression { .. })
    ));
}

#[test]
fn changed_since_cursor_is_stable_under_interleaved_writes() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(1), upsert_version(1, 1)])
        .expect("first");

    let page_one = writer
        .changed_since(CatalogCursor::default())
        .expect("page one");
    assert_eq!(page_one.rows.len(), 1);
    let cursor = page_one.next;

    // Interleave more writes after taking the cursor.
    writer.apply_ops(&[upsert_version(1, 2)]).expect("second");
    writer.apply_ops(&[upsert_version(1, 3)]).expect("third");

    // Resuming from the cursor sees exactly the new rows, none repeated.
    let page_two = writer.changed_since(cursor).expect("page two");
    assert_eq!(page_two.rows.len(), 2);
    assert!(
        page_two.rows.iter().all(|row| row.seq > cursor.after_seq),
        "cursor never repeats an already-seen row"
    );
}

#[test]
fn set_listing_and_ir_status_apply() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(1), upsert_version(1, 1)])
        .expect("seed");
    writer
        .apply_ops(&[
            CatalogOp::SetListing {
                version: version_id(1),
                status: ListingStatus::Withdrawn,
                valid_from: 5,
                reason: Some("yanked".to_owned()),
            },
            CatalogOp::SetIrStatus {
                version: version_id(1),
                status: IrStatus::Pending,
                generation: Some(index::protocol::GenStampWire {
                    gen_stamp: gen_stamp(9),
                    channel_tip: None,
                }),
            },
        ])
        .expect("listing + ir status apply");
}

/// The current wall-clock instant in unix milliseconds — the same scale
/// `AsOf::Time` speaks, and the only scale a real engine's own commit clock can
/// be compared against.
fn now_unix_milliseconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_millis() as i64
}

/// Apply one package op and commit it, returning the resulting commit hash.
fn apply_and_commit(
    writer: &index::store::writer::CatalogWriter<index::engine::Configured>,
    seed: u8,
    message: &str,
) -> String {
    writer.apply_ops(&[upsert_package(seed)]).expect("stage");
    writer.commit_batch(message).expect("commit").0
}

#[test]
fn as_of_time_resolves_each_instant_to_the_newest_commit_at_or_before_it() {
    let writer = migrated_writer();

    // The real engine stamps a commit from *its own* clock; an instant cannot be
    // dictated to it, only observed. (The previous form of this test called
    // `MemoryEngine::stage_commit_time(100)` — a hook that exists on the fake
    // and nowhere else, which is precisely why this test had never met the
    // engine it is about.) It also asserted nothing about *which* commit came
    // back: three `let _ = at_…;` bindings, all of which a resolver returning
    // any arbitrary commit would have satisfied. Both are fixed here.
    let first = apply_and_commit(&writer, 1, "first batch");
    let after_first = now_unix_milliseconds();

    // DoltLite's `dolt_log.date` is whole-second text (`YYYY-MM-DD HH:MM:SS`),
    // so two commits inside one clock second are indistinguishable to the
    // resolver and "between them" would not be an expressible instant. This
    // sleep buys a distinguishable boundary; it is the test's subject, not
    // padding.
    std::thread::sleep(std::time::Duration::from_millis(1_100));

    let second = apply_and_commit(&writer, 2, "second batch");
    let after_second = now_unix_milliseconds();

    assert_ne!(first, second, "two batches must mint two distinct commits");

    // An instant at or after the newest commit resolves to the newest commit.
    assert_eq!(
        writer
            .at(&AsOf::Time(UnixMilliseconds(after_second)))
            .expect("resolve newest")
            .commit
            .0,
        second,
        "an instant after the second commit must pin the second commit"
    );

    // An instant *between* the two resolves to the earlier one — the case that
    // distinguishes a real newest-at-or-before search from `return head`.
    assert_eq!(
        writer
            .at(&AsOf::Time(UnixMilliseconds(after_first)))
            .expect("resolve between")
            .commit
            .0,
        first,
        "an instant between the two commits must pin the FIRST, not the head"
    );

    // Far future still resolves to the newest commit rather than failing.
    assert_eq!(
        writer
            .at(&AsOf::Time(UnixMilliseconds(32_503_680_000_000)))
            .expect("resolve future")
            .commit
            .0,
        second,
        "an instant past every commit must pin the newest, not error"
    );

    // Before the first commit is a typed NoCommitAtInstant error, not a panic.
    let before = writer.at(&AsOf::Time(UnixMilliseconds(1)));
    assert!(matches!(before, Err(MetaError::NoCommitAtInstant)));
}
