//! Adversarial store tests: apply_ops atomicity, outbox monotonicity, watermark
//! non-regression, changed_since cursor stability, and AsOf boundary cases.

mod common;

use common::{gen_stamp, migrated_writer, stem_id, version_id};
use proptest::prop_assert_eq;

use heart::query::{AsOf, UnixMilliseconds};
use index::{
    engine::CatalogEngine,
    enums::{EdgeKind, EdgeSource, IrStatus, ListingStatus, SinkKind},
    protocol::{CatalogOp, EdgeWire, FacetWire, PackageStemWire, VersionCoordinates},
    store::{Catalog, CatalogCursor, MetaError, MetaStore},
};

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

/// Build a valid UpsertVersion op tying a version to a stem. Each
/// `version_seed` yields a distinct `version_canonical` so that two different
/// version ids never collide on the `UNIQUE(stem_id, version_canonical)`
/// constraint (a version's identity *is* its stem + canonical string).
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
        edges: index::protocol::EdgeSnapshot::unobserved(),
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
    let (report, _cost) = heart::cost::measured("store/apply_ops_batch", scratch.path(), || {
        writer.apply_ops(&[upsert_package(1), upsert_version(1, 1)])
    });
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
fn reapplying_identical_package_and_version_emits_no_duplicate_outbox() {
    let writer = migrated_writer();
    let ops = [upsert_package(1), upsert_version(1, 1)];

    let first = writer.apply_ops(&ops).expect("first application");
    assert_eq!(first.outbox_rows, 1);

    let second = writer.apply_ops(&ops).expect("identical reapplication");
    assert_eq!(
        second.outbox_rows, 0,
        "an unchanged package/version must not enqueue another upsert"
    );

    let claimed = writer.outbox_claim(SinkKind::Text, 10).expect("claim");
    assert_eq!(
        claimed.len(),
        1,
        "only the first application should enqueue"
    );
}

fn edge(requirement: &str) -> EdgeWire {
    EdgeWire {
        dep_ecosystem: heart::Language::Rust,
        dep_name_canonical: "serde".to_owned(),
        requirement: requirement.to_owned(),
        kind: EdgeKind::Runtime,
        source: EdgeSource::Feed,
        resolved_stem: None,
        optional: false,
    }
}

#[test]
fn reapplying_the_same_edge_emits_nothing_and_a_new_requirement_does() {
    let writer = migrated_writer();
    let mut first = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut first {
        *edges = index::protocol::EdgeSnapshot::manifest(vec![edge("1")]);
    }
    writer
        .apply_ops(&[upsert_package(1), first.clone()])
        .expect("seed");

    let again = writer.apply_ops(&[first]).expect("identical edge");
    assert_eq!(again.outbox_rows, 0, "an unchanged edge must not notify");

    let mut revised = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut revised {
        *edges = index::protocol::EdgeSnapshot::manifest(vec![edge("^1")]);
    }
    let report = writer.apply_ops(&[revised]).expect("requirement change");
    assert_eq!(report.outbox_rows, 1, "a changed requirement must notify");
}

#[test]
fn changing_version_facets_emits_one_delta_outbox_without_duplicates() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_package(1), upsert_version(1, 1)])
        .expect("seed");

    let mut changed = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { facets, .. } = &mut changed {
        facets.keywords = Some("changed keyword".to_owned());
    }

    let report = writer
        .apply_ops(&[changed.clone()])
        .expect("facet delta application");
    assert_eq!(
        report.outbox_rows, 1,
        "a facet-only change must notify the text projection"
    );
    assert_eq!(
        writer
            .apply_ops(&[changed])
            .expect("identical facet reapplication")
            .outbox_rows,
        0,
        "reapplying unchanged facets must not enqueue a duplicate"
    );

    assert_eq!(
        writer
            .outbox_claim(SinkKind::Text, 10)
            .expect("claim")
            .len(),
        2,
        "the seed and the one real facet delta should be queued"
    );
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

fn named_edge(name: &str) -> EdgeWire {
    EdgeWire {
        dep_ecosystem: heart::Language::Rust,
        dep_name_canonical: name.to_owned(),
        requirement: String::new(),
        kind: EdgeKind::Runtime,
        source: EdgeSource::Manifest,
        resolved_stem: None,
        optional: false,
    }
}

fn edge_names(
    writer: &index::store::writer::CatalogWriter<index::engine::Configured>,
) -> Vec<String> {
    writer
        .engine()
        .query_rows(
            "SELECT dep_name_canonical FROM edges ORDER BY dep_name_canonical",
            &[],
            &mut |row| row.get_text(0),
        )
        .expect("edges")
}

#[test]
fn a_shorter_edge_list_drops_the_omitted_name_and_an_empty_list_does_not() {
    let writer = migrated_writer();
    let mut first = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut first {
        *edges =
            index::protocol::EdgeSnapshot::manifest(vec![named_edge("serde"), named_edge("tokio")]);
    }
    writer
        .apply_ops(&[upsert_package(1), first])
        .expect("first");
    assert_eq!(edge_names(&writer), vec![
        "serde".to_owned(),
        "tokio".to_owned()
    ]);

    let mut shorter = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut shorter {
        *edges = index::protocol::EdgeSnapshot::manifest(vec![named_edge("serde")]);
    }
    writer.apply_ops(&[shorter]).expect("shrink");
    assert_eq!(edge_names(&writer), vec!["serde".to_owned()]);

    writer
        .apply_ops(&[upsert_version(1, 1)])
        .expect("empty keeps");
    assert_eq!(edge_names(&writer), vec!["serde".to_owned()]);

    let mut cleared = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut cleared {
        *edges = index::protocol::EdgeSnapshot::manifest(Vec::new());
    }
    writer.apply_ops(&[cleared]).expect("clear");
    assert!(edge_names(&writer).is_empty());
}

#[test]
fn a_runtime_replace_leaves_a_recipe_edge() {
    let writer = migrated_writer();
    let mut seeded = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut seeded {
        let mut recipe = named_edge("openssl");
        recipe.kind = EdgeKind::Recipe;
        *edges = index::protocol::EdgeSnapshot::recipe(vec![recipe]);
    }
    writer
        .apply_ops(&[upsert_package(1), seeded])
        .expect("recipe");
    let mut runtime = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut runtime {
        *edges = index::protocol::EdgeSnapshot::feed(vec![named_edge("serde")]);
    }
    writer.apply_ops(&[runtime]).expect("runtime");
    assert_eq!(edge_names(&writer), vec![
        "openssl".to_owned(),
        "serde".to_owned()
    ]);
}

#[test]
fn an_optional_wire_lands_in_sql_and_on_the_ledger() {
    use index::engine::turso_vc::VersionedCatalog;

    let writer = migrated_writer();
    let mut seeded = upsert_version(1, 1);
    if let CatalogOp::UpsertVersion { edges, .. } = &mut seeded {
        let mut wire = named_edge("serde");
        wire.optional = true;
        wire.requirement = "^1".into();
        *edges = index::protocol::EdgeSnapshot::feed(vec![wire]);
    }
    let ops = [upsert_package(1), seeded];
    writer.apply_ops(&ops).expect("optional feed");
    let stored = writer
        .engine()
        .query_rows("SELECT optional, requirement FROM edges", &[], &mut |row| {
            Ok((row.get_integer(0)?, row.get_text(1)?))
        })
        .expect("read");
    assert_eq!(stored, vec![(1, "^1".to_owned())]);

    let mut ledger = VersionedCatalog::open().expect("ledger");
    for effect in index::edge_project::effects_from_ops(&ops, None) {
        ledger.apply_effect(&effect).expect("ledger");
    }
    let tip = ledger
        .materialize("rust", "pkg1", "1.0.1")
        .expect("read")
        .expect("row");
    assert_eq!(tip.edges.len(), 1);
    assert!(tip.edges[0].optional);
    assert_eq!(tip.edges[0].requirement.as_deref(), Some("^1"));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(24))]

    #[test]
    fn random_edges_match_on_sql_and_the_ledger(
        rows in proptest::collection::vec(
            (0usize..16, proptest::bool::ANY, "[~^0-9]{0,6}"),
            1..8,
        )
    ) {
        use index::enums::TextEnum;
        use index::engine::turso_vc::VersionedCatalog;

        let kinds = EdgeKind::all_variants();
        let wires: Vec<EdgeWire> = rows
            .iter()
            .enumerate()
            .map(|(index, (kind, optional, requirement))| EdgeWire {
                dep_ecosystem: heart::Language::Rust,
                dep_name_canonical: format!("d{index}"),
                requirement: requirement.clone(),
                kind: kinds[*kind % kinds.len()],
                source: EdgeSource::Manifest,
                resolved_stem: None,
                optional: *optional,
            })
            .collect();
        let mut version = upsert_version(1, 1);
        if let CatalogOp::UpsertVersion { edges, .. } = &mut version {
            *edges = index::protocol::EdgeSnapshot::carrying(wires.clone());
        }
        let ops = [upsert_package(1), version];
        let writer = migrated_writer();
        writer.apply_ops(&ops).expect("sql");
        let mut sql = writer
            .engine()
            .query_rows(
                "SELECT kind, dep_name_canonical, requirement, optional FROM edges ORDER BY kind, dep_name_canonical",
                &[],
                &mut |row| Ok((row.get_text(0)?, row.get_text(1)?, row.get_text(2)?, row.get_integer(3)?)),
            )
            .expect("sql rows");
        let mut ledger = VersionedCatalog::open().expect("ledger");
        for effect in index::edge_project::effects_from_ops(&ops, None) {
            ledger.apply_effect(&effect).expect("ledger");
        }
        let tip = ledger.materialize("rust", "pkg1", "1.0.1").expect("read").expect("row");
        let mut ledger_rows: Vec<_> = tip
            .edges
            .iter()
            .map(|edge| (
                edge.kind.as_token().to_owned(),
                edge.name.to_string(),
                edge.requirement.as_deref().unwrap_or("").to_owned(),
                i64::from(edge.optional),
            ))
            .collect();
        ledger_rows.sort();
        sql.sort();
        prop_assert_eq!(sql, ledger_rows);
        prop_assert_eq!(ledger.dependents(), ledger.dependents_from_tips());
    }
}
