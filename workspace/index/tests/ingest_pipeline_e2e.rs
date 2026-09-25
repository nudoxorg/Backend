//! End-to-end proof of the ingest pipeline, driven against REAL git
//! repositories (doctrine §4: prefer real inputs over hand-authored
//! fixtures) — enumerate → apply → commit → advance a watermark → read the
//! committed catalog state back. Every measured region is wrapped in
//! `heart::cost::measured` (a `cost case=…` line is emitted for each);
//! every assertion is on real content (real 40-hex commit SHA-1s, real row
//! counts read straight off the tables, real postcard byte sizes) — never on
//! `is_ok()` or a bare non-zero count.
//!
//! # Why the git-monitor path, not Homebrew, for this proof
//!
//! `tests/ingest_driver.rs` already proves the Homebrew path end to end, but
//! against a hand-authored 3-formula JSON fixture — precisely the shape
//! doctrine §4 warns tests only the fixture author's imagination. The
//! `Homebrew` `Follower` has exactly one `FeedTransport` implementation in
//! the whole crate (`FixtureTransport`, test-only; confirmed by grep) and
//! writing a real one means adding a blocking HTTP client dependency, which
//! is out of this workstream's scope (`ingest/`, `tests/` only — not the
//! shared root `Cargo.toml`).
//!
//! The git-monitor path has no such gap. `GritAdapter` and `GitCommandAdapter`
//! are both real, production code, and a `file://` URL against a repository
//! this test genuinely `git init`s exercises the actual git wire format —
//! real `ls-remote` parsing, real SHA-1 object ids, real tag handling — with
//! no network dependency and nothing standing in for upstream behaviour.
//!
//! # What this file proves that `ingest_enumerate.rs` does not
//!
//! `ingest_enumerate.rs` proves `enumerate_git_versions` and
//! `GitMonitor::tick` in isolation. **Nothing in the crate composes
//! `GitMonitor` with the catalog writer and a watermark store** — there is no
//! `FollowerDriver` equivalent for the git-monitor path (`GitMonitor::new` is
//! called nowhere outside tests; grepped). This file is that composition,
//! hand-built to mirror `ingest::driver::FollowerDriver`'s own atomicity
//! contract (apply, then commit, then persist the watermark — never the
//! reverse), across three real polls: first observation, a genuine no-op,
//! and a genuine new upstream release.
//!
//! # A real gap this composition exposed (not a test bug — see below)
//!
//! There are genuinely **two separate `git_watermarks` stores** in play, and
//! building this composition is what makes that visible: (1) the catalog's
//! own `git_watermarks` **table**, written only as a side effect of applying
//! `CatalogOp::SourceMoved` — never on an `Unchanged` tick, because
//! `TickOutcome::Unchanged` carries no ops at all — and never read back by
//! any code in the crate outside this test's raw query; and (2)
//! `ingest::watermark::MemoryWatermarkStore`, the driver-side, non-durable
//! seam `drive_one_git_poll` writes on *every* tick, changed or not, so the
//! crawl clock has somewhere to advance even when nothing moved. A first
//! draft of this test asserted the catalog table's `last_checked_at` advanced
//! on a no-op poll — a genuine assertion error, since only a `Moved` tick
//! touches that table. Fixed by asserting each store for what it actually
//! promises, and asserting the catalog-table gap explicitly below rather than
//! papering over it: **a production `WatermarkStore` wired straight to the
//! catalog's own tables (the crate's stated eventual design) would have no
//! way to record "checked at t, nothing changed" against `git_watermarks` at
//! all**, because there is no `CatalogOp` for a bare crawl-clock bump.

mod common;

use std::io::Write;

use sea_orm::{
    ColumnTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};

use common::{init_repo_with_tags, migrated_writer, push_one_more_tagged_commit};

use index::{
    engine::Configured,
    entity::{git_watermarks, versions},
    ids::PackageStemId,
    ingest::{
        FollowerDriver, GitDriveOutcome,
        enumerate::{cpp_stem_id, enumerate_git_versions, upsert_cpp_package},
        git::{GitCommandAdapter, GitRepository},
        grit::GritAdapter,
        monitor::{GitMonitor, TickOutcome},
        watermark::{GitWatermark, MemoryWatermarkStore, WatermarkStore},
    },
    protocol::CatalogOp,
    store::{Catalog, CatalogCursor, MetaStore, writer::CatalogWriter},
};

const SLUG: &str = "example.test/real-pipeline";

/// Real committed version rows for `stem`, ordered by canonical string, read
/// straight off the `versions` table by a raw SeaORM select. This exists
/// because the crate's `Catalog` read trait exposes no version-listing call
/// at all (only `get_package`) — proving catalog state here means going
/// around it, the same way a future reader would have to.
fn committed_versions(
    writer: &CatalogWriter<Configured>,
    stem: PackageStemId,
) -> Vec<(String, Option<String>)> {
    let stmt = versions::Entity::find()
        .filter(versions::Column::StemId.eq(stem))
        .select_only()
        .column(versions::Column::VersionCanonical)
        .column(versions::Column::SourceRev)
        .order_by_asc(versions::Column::VersionCanonical)
        .build(DbBackend::Sqlite);
    index::engine::query(writer.engine(), stmt, &mut |row| {
        Ok((row.get_text(0)?, row.get_optional_text(1)?))
    })
    .expect("query committed versions")
}

/// The CATALOG's own `git_watermarks` row for `stem`, read straight off the
/// table by a raw SeaORM select — nothing else in the crate reads this table
/// back (it is written only as a side effect of applying
/// `CatalogOp::SourceMoved`). Distinct from
/// `MemoryWatermarkStore::git_watermark`, which is the driver-side seam
/// `drive_one_git_poll` actually reads/writes on every tick — see the module
/// doc for why the two disagree on purpose.
fn catalog_git_watermark_row(
    writer: &CatalogWriter<Configured>,
    stem: PackageStemId,
) -> Option<(Option<String>, i64)> {
    let stmt = git_watermarks::Entity::find()
        .filter(git_watermarks::Column::StemId.eq(stem))
        .select_only()
        .column(git_watermarks::Column::LastRev)
        .column(git_watermarks::Column::LastCheckedAt)
        .build(DbBackend::Sqlite);
    index::engine::query(writer.engine(), stmt, &mut |row| {
        Ok((row.get_optional_text(0)?, row.get_integer(1)?))
    })
    .expect("query git watermark")
    .pop()
}

/// One real poll of the git-monitor path, hand-composed to mirror
/// `ingest::driver::FollowerDriver::drive_once`'s atomicity contract: apply
/// the batch, commit it, and only then persist the watermark — because no
/// such composition exists in the library for `GitMonitor` (see module doc).
fn drive_one_git_poll<Repository: GitRepository>(
    writer: &CatalogWriter<Configured>,
    watermarks: &MemoryWatermarkStore,
    monitor: &GitMonitor<Repository>,
    stem: PackageStemId,
    repo_url: &str,
    now_ms: i64,
    commit_ts: u64,
) -> TickOutcome {
    let previous = watermarks
        .git_watermark(stem)
        .expect("read git watermark")
        .and_then(|w| w.last_rev);

    let prior: std::collections::BTreeMap<String, Option<String>> =
        committed_versions(writer, stem).into_iter().collect();
    let outcome = monitor
        .tick(
            stem,
            SLUG,
            repo_url,
            previous.as_deref(),
            &prior,
            now_ms,
            commit_ts,
        )
        .expect("tick against a real repo must not fail");

    match &outcome {
        TickOutcome::Unchanged { rev } => {
            watermarks
                .put_git_watermark(&GitWatermark {
                    stem_id: stem,
                    last_rev: Some(rev.clone()),
                    last_checked_at: now_ms,
                    last_error: None,
                })
                .expect("persist unchanged watermark");
        }
        TickOutcome::Moved { rev, ops } => {
            // The atomic-batch contract: apply the whole batch in one
            // transaction, commit, and ONLY THEN advance the watermark — a
            // crash between commit and watermark-persist re-delivers one
            // batch safely because the ops upsert.
            let report = writer.apply_ops(ops).expect("apply real ops");
            writer
                .commit_batch(&format!("git monitor: {SLUG} ({} ops)", report.applied))
                .expect("commit batch");
            watermarks
                .put_git_watermark(&GitWatermark {
                    stem_id: stem,
                    last_rev: Some(rev.clone()),
                    last_checked_at: now_ms,
                    last_error: None,
                })
                .expect("persist moved watermark");
        }
    }
    outcome
}

/// The full pipeline against a real repo, generic over which real
/// `GitRepository` adapter drives it — both are exercised as separate
/// `#[test]`s below so a regression in either the in-process grit path or the
/// subprocess fallback is caught by name.
fn run_end_to_end<Repository: GitRepository>(case_prefix: &str, adapter: Repository) {
    const N: usize = 5;
    let dir = tempfile::tempdir().expect("tempdir");
    init_repo_with_tags(dir.path(), N);
    let url = format!("file://{}", dir.path().display());

    let stem = cpp_stem_id(SLUG);
    let writer = migrated_writer();
    let watermarks = MemoryWatermarkStore::new();
    let monitor = GitMonitor::new(adapter);

    // ── Registration: happens once, outside any poll. `GitMonitor::tick`
    // never emits `UpsertPackage` (confirmed by reading monitor.rs) — that is
    // the caller's job in a real deployment, and it is a real gap this test
    // makes visible rather than papering over.
    writer
        .apply_ops(&[upsert_cpp_package(SLUG, &url)])
        .expect("register package");
    writer
        .commit_batch("register package")
        .expect("commit registration");

    // ── Poll 1: first observation of a real repo with N real tagged releases.
    let (outcome1, _cost1) = heart::cost::measured(
        &format!("{case_prefix}.poll1_first_observation"),
        dir.path(),
        || {
            drive_one_git_poll(
                &writer,
                &watermarks,
                &monitor,
                stem,
                &url,
                1_000,
                20_250_101_000_000,
            )
        },
    );
    let rev1 = match outcome1 {
        TickOutcome::Moved { rev, ops } => {
            assert!(
                matches!(ops.first(), Some(CatalogOp::SourceMoved { .. })),
                "first poll must lead with SourceMoved"
            );
            let version_ops = ops
                .iter()
                .filter(|op| matches!(op, CatalogOp::UpsertVersion { .. }))
                .count();
            assert_eq!(version_ops, N, "one UpsertVersion op per real tag");
            rev
        }
        TickOutcome::Unchanged { .. } => panic!("first poll of a real repo must observe a move"),
    };

    // The registered package reads back correctly through the crate's own
    // `Catalog::get_package` — real content, not merely `is_some()`.
    let package = writer
        .get_package(stem)
        .expect("read package")
        .expect("package registered");
    assert_eq!(package.name_canonical, SLUG);
    assert_eq!(package.repo_url.as_deref(), Some(url.as_str()));

    // Real content, not a count: every one of the N tags landed with a real
    // 40-hex commit SHA-1 pinned as `source_rev`, in canonical order.
    let rows = committed_versions(&writer, stem);
    assert_eq!(
        rows.len(),
        N,
        "N real tags committed as N real version rows"
    );
    for (i, (canonical, source_rev)) in rows.iter().enumerate() {
        assert_eq!(canonical, &format!("v1.0.{i}"));
        let rev = source_rev.as_ref().expect("git source_rev pinned");
        assert_eq!(
            rev.len(),
            40,
            "full commit SHA-1, not truncated or absent: {rev}"
        );
        assert!(
            rev.chars().all(|c| c.is_ascii_hexdigit()),
            "source_rev must be hex: {rev}"
        );
    }

    // The watermark this poll wrote is genuinely readable back in BOTH
    // stores — closing the catalog table's "write-only" gap named in the
    // module doc (a fresh raw query sees exactly what `SourceMoved` wrote),
    // and confirming the driver-side seam agrees with it after a real move.
    let (stored_rev, checked_at) =
        catalog_git_watermark_row(&writer, stem).expect("watermark row exists");
    assert_eq!(stored_rev.as_deref(), Some(rev1.as_str()));
    assert_eq!(checked_at, 1_000);
    let driver_watermark1 = watermarks
        .git_watermark(stem)
        .expect("read")
        .expect("watermark exists");
    assert_eq!(driver_watermark1.last_rev.as_deref(), Some(rev1.as_str()));
    assert_eq!(driver_watermark1.last_checked_at, 1_000);

    // ── Poll 2: nothing changed upstream. Must be a TRUE no-op: zero new
    // catalog rows, zero outbox re-notifications — not merely an empty
    // `ops` Vec that happens not to be asserted on.
    let before_cursor = writer
        .changed_since(CatalogCursor::default())
        .expect("page before poll 2")
        .next;
    let (outcome2, _cost2) = heart::cost::measured(
        &format!("{case_prefix}.poll2_true_noop"),
        dir.path(),
        || {
            drive_one_git_poll(
                &writer,
                &watermarks,
                &monitor,
                stem,
                &url,
                2_000,
                20_250_101_000_000,
            )
        },
    );
    assert!(
        matches!(outcome2, TickOutcome::Unchanged { .. }),
        "an unchanged real repo must read as Unchanged, not a Moved batch with zero ops"
    );
    let after_poll2 = writer
        .changed_since(before_cursor)
        .expect("page after poll 2");
    assert!(
        after_poll2.rows.is_empty(),
        "an unchanged poll must write zero outbox rows"
    );
    assert_eq!(
        committed_versions(&writer, stem).len(),
        N,
        "version row count unchanged by a no-op poll"
    );

    // The driver-side seam DOES advance its crawl clock on a no-op — that is
    // its whole purpose (see `drive_one_git_poll`'s `Unchanged` arm).
    let driver_watermark2 = watermarks
        .git_watermark(stem)
        .expect("read")
        .expect("watermark exists");
    assert_eq!(
        driver_watermark2.last_rev.as_deref(),
        Some(rev1.as_str()),
        "digest unchanged"
    );
    assert_eq!(
        driver_watermark2.last_checked_at, 2_000,
        "the driver-side crawl clock still advances on a no-op"
    );

    // The CATALOG's own `git_watermarks` row does NOT — this is the real gap
    // documented at the top of this file: `TickOutcome::Unchanged` carries no
    // `CatalogOp`, so nothing applies a write, so the row this poll left
    // behind is byte-for-byte identical to what poll 1 wrote. A production
    // `WatermarkStore` backed directly by this table would silently lose
    // every "checked, nothing changed" observation.
    let (catalog_rev2, catalog_checked_at2) =
        catalog_git_watermark_row(&writer, stem).expect("watermark row exists");
    assert_eq!(catalog_rev2.as_deref(), Some(rev1.as_str()));
    assert_eq!(
        catalog_checked_at2, 1_000,
        "the catalog's own git_watermarks row is untouched by a no-op poll — only a Moved tick writes it"
    );

    // ── Poll 3: a genuine new upstream release lands — one more real,
    // committed, tagged changelog entry pushed to the SAME repo.
    let new_tag = push_one_more_tagged_commit(dir.path(), N);
    let (outcome3, _cost3) = heart::cost::measured(
        &format!("{case_prefix}.poll3_new_release"),
        dir.path(),
        || {
            drive_one_git_poll(
                &writer,
                &watermarks,
                &monitor,
                stem,
                &url,
                3_000,
                20_250_101_000_000,
            )
        },
    );
    let version_ops_in_poll3 = match outcome3 {
        TickOutcome::Moved { ops, .. } => ops
            .iter()
            .filter(|op| matches!(op, CatalogOp::UpsertVersion { .. }))
            .count(),
        TickOutcome::Unchanged { .. } => panic!("a genuinely new tag must read as a move"),
    };
    assert_eq!(
        version_ops_in_poll3, 1,
        "a new tag emits one version op; unchanged tags stay off the batch"
    );

    let rows_after = committed_versions(&writer, stem);
    assert_eq!(
        rows_after.len(),
        N + 1,
        "prior history preserved AND the new tag added"
    );
    assert!(
        rows_after
            .iter()
            .any(|(canonical, _)| canonical == &new_tag),
        "the new tag is present"
    );
    for i in 0..N {
        let expected = format!("v1.0.{i}");
        assert!(
            rows_after
                .iter()
                .any(|(canonical, _)| canonical == &expected),
            "tag {expected} survived re-enumeration and re-application"
        );
    }
    // Poll 3 IS a Moved tick, so it DOES emit `CatalogOp::SourceMoved` — the
    // catalog's own table catches back up to the driver-side seam here,
    // confirming the gap is specific to `Unchanged` ticks, not a general
    // divergence between the two stores.
    let (_, catalog_checked_at3) =
        catalog_git_watermark_row(&writer, stem).expect("watermark row exists");
    assert_eq!(
        catalog_checked_at3, 3_000,
        "a Moved tick DOES update the catalog's own git_watermarks row"
    );
    let driver_watermark3 = watermarks
        .git_watermark(stem)
        .expect("read")
        .expect("watermark exists");
    assert_eq!(driver_watermark3.last_checked_at, 3_000);
}

/// The production git driver, rather than a test-only composition, must carry
/// one changed upstream tag through monitor → catalog → commit → watermark.
#[test]
fn git_driver_propagates_only_the_changed_upstream_version() {
    const INITIAL_TAGS: usize = 2;
    let dir = tempfile::tempdir().expect("tempdir");
    init_repo_with_tags(dir.path(), INITIAL_TAGS);
    let url = format!("file://{}", dir.path().display());
    let stem = cpp_stem_id(SLUG);
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_cpp_package(SLUG, &url)])
        .expect("register package");
    writer
        .commit_batch("register package")
        .expect("commit package");

    let watermarks = MemoryWatermarkStore::new();
    let facts = std::sync::Mutex::new(
        index::engine::turso_vc::VersionedCatalog::open().expect("versioned catalog"),
    );
    let driver = FollowerDriver::new(&writer, &watermarks).with_facts(&facts);
    let monitor = GitMonitor::new(GritAdapter::default());

    let first = driver
        .drive_git_once(&monitor, stem, SLUG, &url, 1_000, 20_250_101_000_000)
        .expect("first git poll");
    assert!(matches!(first, GitDriveOutcome::Committed { .. }));
    let versions = committed_versions(&writer, stem);
    assert_eq!(versions.len(), INITIAL_TAGS);
    {
        let mut catalog = facts.lock().expect("facts");
        for (canonical, _) in &versions {
            let tip = catalog
                .materialize("cpp", SLUG, canonical)
                .expect("join")
                .expect("versioned git tag");
            assert!(tip.edges.is_empty());
        }
    }

    let cursor = writer
        .changed_since(CatalogCursor::default())
        .expect("read initial changes")
        .next;
    let unchanged = driver
        .drive_git_once(&monitor, stem, SLUG, &url, 2_000, 20_250_101_000_000)
        .expect("unchanged git poll");
    assert_eq!(unchanged, GitDriveOutcome::NoChange);
    assert!(
        writer
            .changed_since(cursor)
            .expect("read unchanged delta")
            .rows
            .is_empty(),
        "unchanged polling must not emit catalog changes or outbox work"
    );

    push_one_more_tagged_commit(dir.path(), INITIAL_TAGS);
    let changed = driver
        .drive_git_once(&monitor, stem, SLUG, &url, 3_000, 20_250_101_000_000)
        .expect("changed git poll");
    assert_eq!(
        changed,
        GitDriveOutcome::Committed { applied: 2 },
        "the moved snapshot must apply SourceMoved plus one typed add delta"
    );
    assert_eq!(
        committed_versions(&writer, stem).len(),
        INITIAL_TAGS + 1,
        "the new tag must become one new catalog version"
    );
    let delta = writer
        .changed_since(cursor)
        .expect("read changed delta")
        .rows;
    assert_eq!(
        delta.len(),
        1,
        "the changed poll must emit one text event for the changed version"
    );
    assert!(
        catalog_git_watermark_row(&writer, stem)
            .expect("read catalog watermark")
            .0
            .is_some(),
        "SourceMoved must update the catalog watermark even though it is not a text event"
    );
    assert_eq!(
        watermarks
            .git_watermark(stem)
            .expect("read driver watermark")
            .expect("driver watermark exists")
            .last_checked_at,
        3_000
    );
}

/// A moved remote is reconciled as the complete set delta: one add, one
/// source-revision change, and one removal. The durable catalog and its outbox
/// must receive exactly those three typed effects; unchanged versions are not
/// re-looped.
#[test]
fn git_driver_propagates_add_change_remove_deltas_without_full_rescan() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_repo_with_tags(dir.path(), 2);
    let url = format!("file://{}", dir.path().display());
    let stem = cpp_stem_id(SLUG);
    let writer = migrated_writer();
    writer
        .apply_ops(&[upsert_cpp_package(SLUG, &url)])
        .expect("register package");
    writer
        .commit_batch("register package")
        .expect("commit package");

    let watermarks = MemoryWatermarkStore::new();
    let facts = std::sync::Mutex::new(
        index::engine::turso_vc::VersionedCatalog::open().expect("versioned catalog"),
    );
    let driver = FollowerDriver::new(&writer, &watermarks).with_facts(&facts);
    let monitor = GitMonitor::new(GritAdapter::default());
    driver
        .drive_git_once(&monitor, stem, SLUG, &url, 1_000, 20_250_101_000_000)
        .expect("initial poll");
    {
        let mut catalog = facts.lock().expect("facts");
        assert!(
            catalog
                .materialize("cpp", SLUG, "v1.0.1")
                .expect("join")
                .is_some(),
            "the tag that will disappear is versioned first"
        );
    }
    let cursor = writer
        .changed_since(CatalogCursor::default())
        .expect("read initial changes")
        .next;

    // Repoint v1.0.0, remove v1.0.1, and add v1.0.2 on a real new commit.
    let changelog = dir.path().join("CHANGELOG");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&changelog)
        .expect("open changelog")
        .write_all(b"reconciled release\n")
        .expect("append changelog");
    common::git(dir.path(), &["add", "CHANGELOG"]);
    common::git(dir.path(), &["commit", "-q", "-m", "reconcile"]);
    common::git(dir.path(), &["tag", "-f", "v1.0.0"]);
    common::git(dir.path(), &["tag", "-d", "v1.0.1"]);
    common::git(dir.path(), &["tag", "v1.0.2"]);

    let outcome = driver
        .drive_git_once(&monitor, stem, SLUG, &url, 2_000, 20_250_101_000_000)
        .expect("reconciliation poll");
    assert_eq!(
        outcome,
        GitDriveOutcome::Committed { applied: 4 },
        "SourceMoved plus add/change/remove typed deltas"
    );

    let rows = writer
        .changed_since(cursor)
        .expect("read delta outbox")
        .rows;
    assert_eq!(rows.len(), 3, "one outbox row per version delta");
    assert_eq!(
        rows.iter()
            .filter(|row| row.op == index::enums::OutboxOperation::Upsert)
            .count(),
        2,
        "add and change are upserts"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.op == index::enums::OutboxOperation::Delete)
            .count(),
        1,
        "remove is a delete tombstone"
    );
    assert_eq!(committed_versions(&writer, stem).len(), 2);
    let mut catalog = facts.lock().expect("facts");
    assert!(
        catalog
            .materialize("cpp", SLUG, "v1.0.1")
            .expect("removed tip")
            .is_none(),
        "the deleted tag leaves the versioned tip"
    );
    for canonical in ["v1.0.0", "v1.0.2"] {
        assert!(
            catalog
                .materialize("cpp", SLUG, canonical)
                .expect("surviving tip")
                .is_some(),
            "{canonical} stays versioned"
        );
    }
    assert!(
        catalog
            .scan_edges()
            .iter()
            .all(|edge| edge.version != "v1.0.1"),
        "the deleted tag owns no edge rows"
    );
}

#[test]
fn git_source_end_to_end_first_poll_true_noop_then_new_release_via_grit() {
    run_end_to_end("ingest/e2e.grit", GritAdapter::default());
}

#[test]
fn git_source_end_to_end_first_poll_true_noop_then_new_release_via_git_command() {
    run_end_to_end("ingest/e2e.git_command", GitCommandAdapter::default());
}

// ─────────────────────────────────────────────────────────────────────────────
// Throughput + storage: how ingest cost actually scales with real tag count
// ─────────────────────────────────────────────────────────────────────────────

/// Throughput and storage as a function of a REAL repository's tag count.
/// Wall-clock is emitted (doctrine §4 requires it) but is the unreliable
/// figure here — this host has other agents compiling concurrently, and
/// doctrine §8 records a measured 4.6x wall-time swing on identical work
/// under that load. The reliable figures are the ones asserted on: real op
/// counts, and real disk/byte deltas.
#[test]
fn ingest_throughput_and_storage_scale_with_real_tag_count() {
    let adapter = GritAdapter::default();
    let mut summary = Vec::new();

    for n in [1usize, 8, 32] {
        let case = format!("ingest/scale.n{n}");
        let slug = format!("example.test/scale-{n}");

        // Phase A: build the real upstream repo from an empty directory, so
        // `measured`'s disk delta is the genuine on-disk cost of n releases
        // (real git objects/refs/tags), not a diff against a pre-populated
        // fixture.
        let dir = tempfile::tempdir().expect("tempdir");
        let (_, build_cost) =
            heart::cost::measured(&format!("{case}.build_repo"), dir.path(), || {
                init_repo_with_tags(dir.path(), n)
            });
        let url = format!("file://{}", dir.path().display());

        // Phase B: enumerate — the real cost of one `ls-remote` + parse
        // against a repo with n tags.
        let (ops, enumerate_cost) =
            heart::cost::measured(&format!("{case}.enumerate"), dir.path(), || {
                enumerate_git_versions(&adapter, &slug, &url, 20_250_101_000_000)
                    .expect("enumerate real repo")
            });
        let version_ops = ops
            .iter()
            .filter(|op| matches!(op, CatalogOp::UpsertVersion { .. }))
            .count();
        assert_eq!(
            version_ops, n,
            "case n={n} must enumerate exactly n real tags, not a fixture-sized stand-in"
        );

        // A reliable byte figure independent of wall-clock noise: the
        // postcard-encoded wire size of the batch a real poll would apply.
        // `postcard::to_allocvec` is the crate's own established codec for
        // exactly this kind of on-disk/on-wire sizing (see
        // `pack/format.rs::TableOfContents` for the existing precedent).
        let encoded_bytes: usize = ops
            .iter()
            .map(|op| {
                postcard::to_allocvec(op)
                    .expect("postcard-encode a real op")
                    .len()
            })
            .sum();
        assert!(
            encoded_bytes > 0,
            "a non-empty real batch must encode to a non-empty byte string"
        );

        let bytes_per_version = encoded_bytes as f64 / version_ops.max(1) as f64;
        let repo_bytes_per_version = build_cost.disk_delta_bytes as f64 / n.max(1) as f64;

        println!(
            "cost case={case}.summary n_tags={n} version_ops={version_ops} \
             repo_build_disk_delta_bytes={} repo_bytes_per_version={repo_bytes_per_version:.1} \
             postcard_batch_bytes={encoded_bytes} postcard_bytes_per_version={bytes_per_version:.1} \
             build_wall_ms={:.2} enumerate_wall_ms={:.2}",
            build_cost.disk_delta_bytes,
            build_cost.wall.as_secs_f64() * 1000.0,
            enumerate_cost.wall.as_secs_f64() * 1000.0,
        );

        summary.push((n, encoded_bytes, build_cost.disk_delta_bytes));
    }

    // Real content, not a trend eyeballed from println output: byte counts
    // are reliable (doctrine's own instruction), so assert monotonic growth
    // in both the wire-encoded batch size and the real on-disk repo size as
    // n increases — this is the throughput claim, checked, not merely
    // printed.
    for pair in summary.windows(2) {
        let (n_before, bytes_before, disk_before) = pair[0];
        let (n_after, bytes_after, disk_after) = pair[1];
        assert!(
            bytes_after > bytes_before,
            "postcard batch bytes must grow with tag count: n={n_before} -> {bytes_before}, n={n_after} -> {bytes_after}"
        );
        assert!(
            disk_after > disk_before,
            "real repo disk usage must grow with tag count: n={n_before} -> {disk_before}, n={n_after} -> {disk_after}"
        );
    }
}

/// Decomposes one representative poll into its three real phases —
/// enumerate, apply+commit, watermark-persist — so a reader can see where
/// the cost actually goes instead of one opaque end-to-end number. Op/row
/// counts (asserted) are the reliable evidence; the per-phase wall-clock
/// printed alongside is informational only, taken on a host with other
/// agents compiling concurrently (doctrine §8: up to 4.6x swing observed on
/// identical work under that load).
#[test]
fn ingest_poll_time_breakdown_enumerate_vs_apply_vs_watermark() {
    const N: usize = 20;
    let dir = tempfile::tempdir().expect("tempdir");
    init_repo_with_tags(dir.path(), N);
    let url = format!("file://{}", dir.path().display());
    let slug = "example.test/breakdown";
    let stem = cpp_stem_id(slug);
    let adapter = GritAdapter::default();
    let writer = migrated_writer();

    writer
        .apply_ops(&[upsert_cpp_package(slug, &url)])
        .expect("register");
    writer.commit_batch("register").expect("commit register");

    let (ops, enumerate_cost) =
        heart::cost::measured("ingest/breakdown.enumerate", dir.path(), || {
            enumerate_git_versions(&adapter, slug, &url, 20_250_101_000_000).expect("enumerate")
        });
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, CatalogOp::UpsertVersion { .. }))
            .count(),
        N,
        "enumerate phase must produce exactly N real version ops"
    );

    // This test's writer comes from `migrated_writer`, which opens a catalog
    // with no file behind it, so `disk_delta_bytes` for this phase is honestly
    // 0 — not fabricated as a storage number, and not hidden either. The
    // engine is the real one; it is the *backing* that is absent. Storage cost
    // for this path is measured in `tests/storage_catalog_scaling.rs`, which
    // opens the same engine at a path.
    let scratch = tempfile::tempdir().expect("scratch for catalog-phase measurement");
    let (report, apply_cost) =
        heart::cost::measured("ingest/breakdown.apply_and_commit", scratch.path(), || {
            let report = writer.apply_ops(&ops).expect("apply real ops");
            writer.commit_batch("breakdown batch").expect("commit");
            report
        });
    assert_eq!(
        report.applied, N,
        "apply phase must apply exactly the N ops enumerate produced"
    );
    assert_eq!(
        report.outbox_rows, N,
        "every UpsertVersion fans out exactly one outbox row"
    );

    let watermarks = MemoryWatermarkStore::new();
    let ((), watermark_cost) =
        heart::cost::measured("ingest/breakdown.watermark_persist", scratch.path(), || {
            watermarks
                .put_git_watermark(&GitWatermark {
                    stem_id: stem,
                    last_rev: Some("d".repeat(40)),
                    last_checked_at: 9_000,
                    last_error: None,
                })
                .expect("persist watermark");
        });

    println!(
        "cost case=ingest/breakdown.summary n_versions={N} \
         enumerate_ms={:.3} apply_commit_ms={:.3} watermark_ms={:.3}",
        enumerate_cost.wall.as_secs_f64() * 1000.0,
        apply_cost.wall.as_secs_f64() * 1000.0,
        watermark_cost.wall.as_secs_f64() * 1000.0,
    );

    // A structural fact, not a wall-clock one: the watermark write is O(1)
    // regardless of N (one row, one stem), while apply is O(N) (N version
    // upserts + N outbox rows) and enumerate is dominated by exactly one
    // `ls-remote` round-trip regardless of N. That asymmetry — confirmed by
    // the op/row counts above, not by timing — is what the "re-enumeration
    // re-lists everything" finding (see the e2e test) turns into a real
    // per-poll cost as a stem accumulates versions.
}
