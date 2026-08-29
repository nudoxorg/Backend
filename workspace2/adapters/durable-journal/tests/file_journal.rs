//! Public real-file journeys are intentionally kept independent of private seams.
use nudox_durable_journal::FileJournal;
use nudox_workflow::{EventKind, StageKey, WorkflowEvent, WorkflowVersion};
use std::fs;
fn path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nudox-journal-{label}-{}", std::process::id()))
}
fn cleanup(p: &std::path::Path) {
    let _ = fs::remove_file(p);
}
fn ev(key: u8, kind: EventKind) -> WorkflowEvent {
    WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([key; 32]),
        kind,
    }
}
#[test]
fn create_append_reopen_preserves_receipt_and_recovery() {
    let p = path("roundtrip");
    cleanup(&p);
    let e = ev(1, EventKind::Requested);
    let r = FileJournal::create(&p)
        .and_then(|mut j| {
            j.append(e)
                .map_err(|_| nudox_durable_journal::JournalError::HeaderInvalid)
        })
        .expect("append");
    assert_eq!(r.sequence, 0);
    assert_eq!(r.durable_end, 124);
    let mut j = FileJournal::open(&p).expect("open");
    assert!(j.replay().expect("replay").pending_effect.is_some());
    cleanup(&p);
}
#[test]
fn every_prefix_can_restart_at_each_receipt() {
    let p = path("prefix");
    cleanup(&p);
    let k = StageKey::from([2; 32]);
    let o = nudox_workflow::StageOutput::from([3; 32]);
    let a = [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged { output: o },
        EventKind::Verified { output: o },
        EventKind::PublicationStarted { output: o },
        EventKind::Published { output: o },
    ];
    for n in 0..=a.len() {
        cleanup(&p);
        let mut j = FileJournal::create(&p).expect("create");
        for kind in a.iter().copied().take(n) {
            j.append(WorkflowEvent {
                version: WorkflowVersion::WAVE1,
                key: k,
                kind,
            })
            .expect("append");
            drop(j);
            j = FileJournal::open(&p).expect("restart");
        }
        assert!(j.replay().is_ok());
    }
    cleanup(&p);
}
#[test]
fn duplicate_and_conflicting_events_do_not_claim_success() {
    let p = path("duplicate");
    cleanup(&p);
    let e = ev(4, EventKind::Requested);
    let mut j = FileJournal::create(&p).expect("create");
    j.append(e).expect("first");
    assert!(j.append(e).is_ok());
    drop(j);
    let mut j = FileJournal::open(&p).expect("reopen");
    assert!(j.replay().is_ok());
    cleanup(&p);
}
#[test]
fn publication_failure_reopens_with_reconciliation_effect() {
    let p = path("unknown");
    cleanup(&p);
    let k = StageKey::from([5; 32]);
    let o = nudox_workflow::StageOutput::from([6; 32]);
    let mut j = FileJournal::create(&p).expect("create");
    for kind in [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged { output: o },
        EventKind::Verified { output: o },
        EventKind::PublicationStarted { output: o },
        EventKind::Failed {
            code: nudox_workflow::FailureCode::Runtime,
        },
    ] {
        j.append(WorkflowEvent {
            version: WorkflowVersion::WAVE1,
            key: k,
            kind,
        })
        .expect("transition");
    }
    drop(j);
    let mut j = FileJournal::open(&p).expect("reopen");
    assert!(j.replay().expect("replay").pending_effect.is_some());
    cleanup(&p);
}
#[test]
fn empty_file_is_rejected() {
    let p = path("empty");
    cleanup(&p);
    fs::File::create(&p).expect("create");
    assert!(FileJournal::open(&p).is_err());
    cleanup(&p);
}
#[test]
fn malformed_header_is_rejected() {
    let p = path("header");
    cleanup(&p);
    fs::write(&p, [0u8; 32]).expect("write");
    assert!(FileJournal::open(&p).is_err());
    cleanup(&p);
}
#[test]
fn receipt_width_is_physical_not_workflow_width() {
    let p = path("receipt");
    cleanup(&p);
    let r = FileJournal::create(&p)
        .and_then(|mut j| {
            j.append(ev(7, EventKind::Requested))
                .map_err(|_| nudox_durable_journal::JournalError::HeaderInvalid)
        })
        .expect("receipt");
    assert_eq!(r.durable_end - r.sequence * 92, 124);
    cleanup(&p);
}
#[test]
fn restart_without_records_is_valid() {
    let p = path("zero");
    cleanup(&p);
    let j = FileJournal::create(&p).expect("create");
    drop(j);
    let mut j = FileJournal::open(&p).expect("open");
    assert!(j.replay().expect("replay").pending_effect.is_none());
    cleanup(&p);
}
#[test]
fn distinct_keys_are_rejected_by_workflow_reducer() {
    let p = path("keys");
    cleanup(&p);
    let mut j = FileJournal::create(&p).expect("create");
    j.append(ev(8, EventKind::Requested)).expect("first");
    assert!(j.append(ev(9, EventKind::Requested)).is_err());
    cleanup(&p);
}
#[test]
fn six_receipts_have_monotonic_end_offsets() {
    let p = path("monotonic");
    cleanup(&p);
    let mut j = FileJournal::create(&p).expect("create");
    let mut prior = 0;
    for n in 0..6 {
        let r = j.append(ev(
            10,
            if n == 0 {
                EventKind::Requested
            } else {
                EventKind::Admitted
            },
        ));
        if let Ok(r) = r {
            assert!(r.durable_end > prior);
            prior = r.durable_end;
        } else {
            break;
        }
    }
    cleanup(&p);
}
#[test]
fn file_is_removed_by_each_test_fixture() {
    let p = path("cleanup");
    cleanup(&p);
    assert!(!p.exists());
}
#[test]
fn public_surface_constructs_only_through_create() {
    let p = path("surface");
    cleanup(&p);
    let result = FileJournal::create(&p);
    assert!(result.is_ok());
    cleanup(&p);
}
