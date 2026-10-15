//! `status`, `health` and `packages` must tell one story about one revision.
//!
//! Every case builds a real catalog whose package row and lane coverage are
//! the pair the owner publishes for one situation, then reads the shelf and
//! the status from it exactly as the surfaces do. The assertions are the
//! agreement the three commands owe each other: the same readiness word, the
//! same published row count, and no count the reply did not carry.
#![allow(clippy::expect_used, clippy::panic)]

use crate::{
    Answer, Detail, Fault, FaultSlug, KeyTag, Readiness, ReadinessState, RowCount, Shelf, Status,
    Theme, encode_answer, markdown, shelf_from_snapshot, shelf_from_snapshot_with_health, text,
};
use backend_library::{
    Basis, CapabilityInventory, Command, CommandReply, Coverage, Cursor, Frontier, HealthReport,
    Lane, Library, Reason, RevisionReceipt, Row, RowId, RowState, ViewRoot, object_version,
    package_key, symbol_key, view_key, view_state_root,
};

const PROJECT: &str = "/workspace/next-asp";
const DECLARATIONS: u64 = 160;

struct Revision {
    library: Library,
    health: HealthReport,
    total: u64,
}

/// One revision: a single project row in `state`, `DECLARATIONS` readable
/// declarations, and the lane coverage the owner published beside them.
fn revision(state: RowState, coverage: Vec<Coverage>) -> Revision {
    let basis = Basis::new(
        view_state_root(&[(PROJECT.to_owned(), "published".to_owned())]),
        object_version(b"readiness agreement source"),
    );
    let package = package_key(PROJECT);
    let mut project = Row::in_package(RowId::Package(package), basis, package, PROJECT);
    project.state = state;
    let mut rows = vec![project];
    rows.extend((0..DECLARATIONS).map(|index| {
        let coordinate = format!("{PROJECT}::src/lib/scheduler.ts:{}::function_{index}", index + 1);
        Row::in_package(
            RowId::Symbol(symbol_key(&coordinate)),
            basis,
            package,
            coordinate,
        )
    }));
    let root = ViewRoot::new_incomplete(
        view_key(b"readiness agreement"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 2),
        rows,
        Vec::new(),
    )
    .expect("explicit incomplete fixture");
    let total = root.row_count();
    let cursor = Cursor::for_view_root(&root);
    let library = Library::from_view(root.clone(), cursor).expect("actual immutable catalog");
    // The owner's health report carries the lane coverage it published. A
    // fixture root cannot itself claim a complete lane, so the report is
    // admitted from the same basis with the coverage under test.
    let health = HealthReport::from_admitted_parts(
        RevisionReceipt::new(root.root(), cursor, root.basis().object),
        root.basis(),
        coverage.into_boxed_slice(),
        total,
        CapabilityInventory::explicitly_unavailable(),
    );
    Revision {
        library,
        health,
        total,
    }
}

fn shelf_of(revision: &Revision) -> Shelf {
    let Ok(CommandReply::Packages(snapshot)) = revision.library.execute(Command::Packages) else {
        panic!("the catalog answers packages");
    };
    shelf_from_snapshot_with_health(&snapshot, KeyTag::from_key(&[7; 32]), &revision.health)
}

fn semantic(coverage: Coverage) -> Vec<Coverage> {
    vec![Coverage::Complete, coverage]
}

#[test]
fn a_live_op_reads_indexing_with_the_same_published_count_on_status_and_packages() {
    let revision = revision(
        RowState::Loading,
        semantic(Coverage::Partial {
            lane: Lane::Semantic,
            completed: 0,
            total: 2,
        }),
    );
    let total = revision.total;
    let status = Status::from_report(&revision.health, None);
    let shelf = shelf_of(&revision);

    assert_eq!(status.readiness(), "indexing");
    assert_eq!(status.rows().get(), total);
    assert_eq!(
        shelf.entries()[0].readiness(),
        &Readiness::Indexing {
            rows: Some(RowCount::new(total))
        },
        "packages must carry the count status prints, never a fabricated zero"
    );
    let rendered = text::shelf(&shelf, Theme::plain());
    assert!(
        rendered.contains(&format!("◐ next-asp  indexing · {total} row(s)")),
        "{rendered}"
    );
    assert!(!rendered.contains("0 row(s)"), "{rendered}");
    assert!(rendered.contains("lanes indexing"), "{rendered}");
    assert!(markdown::shelf(&shelf).contains(&format!("indexing · {total} row(s)")));
}

#[test]
fn a_published_project_with_settled_source_gaps_reads_ready_and_partial_everywhere() {
    let revision = revision(
        RowState::Ready,
        semantic(Coverage::Gapped {
            lane: Lane::Semantic,
        }),
    );
    let total = revision.total;
    let status = Status::from_report(&revision.health, None);
    let shelf = shelf_of(&revision);

    assert_eq!(status.readiness(), "partial", "not unavailable, not indexing");
    assert_eq!(
        status.coverage().state(),
        ReadinessState::Partial,
        "the lane summary and the word come from one state"
    );
    let entry = &shelf.entries()[0];
    assert_eq!(entry.readiness(), &Readiness::Ready);
    assert_eq!(entry.partial_lanes(), &[Lane::Semantic]);
    assert_eq!(entry.published_rows(), Some(RowCount::new(total)));

    let human = text::shelf(&shelf, Theme::plain());
    assert!(
        human.contains("● next-asp  ready · semantic partial"),
        "{human}"
    );
    assert!(human.contains("lanes partial"), "{human}");
    assert!(human.contains("semantic◐ partial"), "{human}");
    assert!(!human.contains("failed"), "{human}");
    assert!(!human.contains("lanes unavailable") && !human.contains('✗'), "{human}");
    let status_line = text::status(&status, Theme::plain());
    assert!(status_line.contains("lanes partial"), "{status_line}");
    assert!(status_line.contains("semantic◐ partial"), "{status_line}");
    let agent = markdown::shelf(&shelf);
    assert!(agent.contains("ready · semantic partial"), "{agent}");

    for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
        let encoded = encode_answer(
            &Answer::Shelf(Box::new(shelf.clone())),
            detail,
            None,
            crate::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("shelf fits");
        let json: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
        assert_eq!(json["projects"][0]["readiness"], "ready");
        assert_eq!(json["projects"][0]["partial_lanes"][0], "semantic");
        assert_eq!(json["readiness"], "partial");
        assert_eq!(json["rows"], total);
    }
}

#[test]
fn a_refused_index_reads_failed_with_the_retry_on_both_surfaces() {
    let revision = revision(
        RowState::Failed,
        semantic(Coverage::Unavailable {
            lane: Lane::Semantic,
            reason: Reason::Incomplete,
        }),
    );
    let status = Status::from_report(&revision.health, None);
    let shelf = shelf_of(&revision);
    assert_eq!(status.readiness(), "unavailable");
    let Readiness::Failed { fault } = shelf.entries()[0].readiness() else {
        panic!("a failed row reads failed");
    };
    assert_eq!(fault.slug(), FaultSlug::Rejected);
    assert_eq!(
        fault.affordance().shell().as_deref(),
        Some("nudox add /workspace/next-asp")
    );
    let human = text::shelf(&shelf, Theme::plain());
    assert!(human.contains("✗ next-asp  failed"), "{human}");
    assert!(human.contains("lanes unavailable"), "{human}");
    assert!(human.contains("→ nudox add /workspace/next-asp"), "{human}");
}

#[test]
fn a_ready_project_reads_ready_with_nothing_to_qualify() {
    let revision = revision(RowState::Ready, vec![Coverage::Complete]);
    let status = Status::from_report(&revision.health, None);
    let shelf = shelf_of(&revision);
    assert_eq!(status.readiness(), "ready");
    assert_eq!(shelf.entries()[0].readiness(), &Readiness::Ready);
    assert!(shelf.entries()[0].partial_lanes().is_empty());
}

#[test]
fn a_shelf_read_without_a_health_report_never_invents_a_count() {
    let revision = revision(
        RowState::Loading,
        semantic(Coverage::Partial {
            lane: Lane::Semantic,
            completed: 1,
            total: 2,
        }),
    );
    let Ok(CommandReply::Packages(snapshot)) = revision.library.execute(Command::Packages) else {
        panic!("the catalog answers packages");
    };
    let shelf = shelf_from_snapshot(&snapshot, KeyTag::from_key(&[7; 32]));
    assert_eq!(
        shelf.entries()[0].readiness(),
        &Readiness::Indexing { rows: None },
        "an unread count is not zero"
    );
    let rendered = text::shelf(&shelf, Theme::plain());
    assert!(rendered.contains("◐ next-asp  indexing"), "{rendered}");
    assert!(!rendered.contains("indexing · 0"), "{rendered}");
}

#[test]
fn a_health_report_of_another_revision_is_ignored_not_trusted() {
    let revision_a = revision(RowState::Loading, semantic(Coverage::Partial {
        lane: Lane::Semantic,
        completed: 0,
        total: 1,
    }));
    // Same project, different basis: the report describes another revision.
    let other_basis = Basis::new(
        view_state_root(&[(PROJECT.to_owned(), "later".to_owned())]),
        object_version(b"a later source"),
    );
    let package = package_key(PROJECT);
    let root = ViewRoot::new_incomplete(
        view_key(b"later revision"),
        other_basis,
        Frontier::new(
            other_basis.branch,
            other_basis.log,
            other_basis.schema,
            other_basis.root,
            9,
        ),
        vec![Row::in_package(
            RowId::Package(package),
            other_basis,
            package,
            PROJECT,
        )],
        Vec::new(),
    )
    .expect("fixture");
    let later = Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("library");
    let stale_health = HealthReport::from_root(later.view(), later.cursor());
    let Ok(CommandReply::Packages(snapshot)) = revision_a.library.execute(Command::Packages)
    else {
        panic!("packages");
    };
    let shelf =
        shelf_from_snapshot_with_health(&snapshot, KeyTag::from_key(&[7; 32]), &stale_health);
    assert_eq!(shelf.facts().rows(), None);
    assert_eq!(
        shelf.entries()[0].readiness(),
        &Readiness::Indexing { rows: None }
    );
}

#[test]
fn an_indexing_fault_is_never_built_without_a_retry() {
    let revision = revision(
        RowState::Failed,
        semantic(Coverage::Unavailable {
            lane: Lane::Semantic,
            reason: Reason::Cancelled,
        }),
    );
    let shelf = shelf_of(&revision);
    let fault: &Fault = shelf.entries()[0]
        .readiness()
        .fault()
        .expect("a failed row carries its fault");
    assert!(fault.cause().sentence().contains("cancelled"));
    assert!(fault.affordance().shell().is_some());
}
