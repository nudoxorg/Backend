//! Exercises the `backend-store` tests file-journal contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
mod harness;

use std::{fs, io::Write};

use harness::{
    ExpectedFailure, Fixture, ScenarioError, expect_commit, expect_journal, receipt_end,
};
use backend_store::journal::{
    CommitError, FileJournal, FrameSequence, HeaderError, JOURNAL_FRAME_BYTES,
    JOURNAL_HEADER_BYTES, JournalError, JournalOffset,
};
use backend_store::workflow::{
    Effect, EffectAction, EventKind, FailureCode, Phase, Recovery, StageKey, StageOutput,
    WORKFLOW_RECORD_BYTES, WorkflowEvent, WorkflowState, WorkflowVersion,
};

fn event(key: StageKey, kind: EventKind) -> WorkflowEvent {
    WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key,
        kind,
    }
}

fn recovery(key: StageKey, phase: Phase, action: Option<EffectAction>) -> Recovery {
    Recovery {
        state: WorkflowState::Keyed { key, phase },
        pending_effect: action.map(|action| Effect { key, action }),
    }
}

fn one_frame(event: WorkflowEvent) -> Result<(Fixture, Vec<u8>), ScenarioError> {
    let fixture = Fixture::new("valid-frame");
    let mut journal = FileJournal::create(fixture.path())?;
    let _receipt = journal.append(event)?;
    drop(journal);
    let bytes = fixture.bytes()?;
    Ok((fixture, bytes))
}

fn expect_header(
    result: Result<FileJournal, JournalError>,
    expected: HeaderError,
) -> Result<(), ScenarioError> {
    expect_journal(
        result,
        ExpectedFailure::Header,
        |observed| matches!(observed, JournalError::Header(actual) if *actual == expected),
    )
}

#[test]
fn every_restart_prefix_has_exact_receipts_and_independent_recovery() -> Result<(), ScenarioError> {
    let key = StageKey::from([3; 32]);
    let output = StageOutput::from_digest([5; 32]);
    let events = [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged { output },
        EventKind::Verified { output },
        EventKind::PublicationStarted { output },
        EventKind::Published { output },
    ];
    let expected = [
        Recovery {
            state: WorkflowState::New,
            pending_effect: None,
        },
        recovery(key, Phase::Requested, Some(EffectAction::Admit)),
        recovery(key, Phase::Admitted, Some(EffectAction::Stage)),
        recovery(
            key,
            Phase::Staged(output),
            Some(EffectAction::Verify { output }),
        ),
        recovery(
            key,
            Phase::Verified(output),
            Some(EffectAction::BeginPublication { output }),
        ),
        recovery(
            key,
            Phase::Publishing(output),
            Some(EffectAction::Publish { output }),
        ),
        recovery(key, Phase::Published(output), None),
    ];

    for (prefix, expected_recovery) in expected.into_iter().enumerate() {
        let fixture = Fixture::new("restart-prefix");
        let mut journal = FileJournal::create(fixture.path())?;
        for (sequence, kind) in events.iter().copied().take(prefix).enumerate() {
            let receipt = journal.append(event(key, kind))?;
            assert_eq!(
                receipt.sequence,
                FrameSequence::from(u64::try_from(sequence)?)
            );
            assert_eq!(
                receipt.durable_end,
                JournalOffset::from(receipt_end(sequence)?)
            );
            drop(journal);
            journal = FileJournal::open(fixture.path())?;
        }
        assert_eq!(journal.replay()?, expected_recovery);
        fixture.remove()?;
    }
    Ok(())
}

#[test]
fn duplicate_is_durable_but_conflicting_key_writes_nothing() -> Result<(), ScenarioError> {
    let fixture = Fixture::new("duplicate-conflict");
    let key = StageKey::from([7; 32]);
    let requested = event(key, EventKind::Requested);
    let mut journal = FileJournal::create(fixture.path())?;
    let first = journal.append(requested)?;
    let duplicate = journal.append(requested)?;
    assert_eq!(first.sequence, FrameSequence::from(0));
    assert_eq!(duplicate.sequence, FrameSequence::from(1));
    let durable_bytes = fs::metadata(fixture.path())?.len();

    let conflicting = event(StageKey::from([8; 32]), EventKind::Requested);
    expect_commit(
        journal.append(conflicting),
        ExpectedFailure::Reduction,
        |observed| {
            matches!(
                observed,
                CommitError::Reduction(backend_store::workflow::ReductionError::StageKeyMismatch {
                    expected,
                    observed,
                }) if *expected == key && *observed == conflicting.key
            )
        },
    )?;
    assert_eq!(fs::metadata(fixture.path())?.len(), durable_bytes);
    assert_eq!(
        journal.replay()?,
        recovery(key, Phase::Requested, Some(EffectAction::Admit))
    );
    fixture.remove()?;
    Ok(())
}

#[test]
fn post_publication_failure_reopens_only_as_reconciliation() -> Result<(), ScenarioError> {
    let fixture = Fixture::new("publication-unknown");
    let key = StageKey::from([9; 32]);
    let output = StageOutput::from_digest([10; 32]);
    let mut journal = FileJournal::create(fixture.path())?;
    for kind in [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged { output },
        EventKind::Verified { output },
        EventKind::PublicationStarted { output },
        EventKind::Failed {
            code: FailureCode::Runtime,
        },
    ] {
        let _receipt = journal.append(event(key, kind))?;
    }
    drop(journal);
    let mut reopened = FileJournal::open(fixture.path())?;
    assert_eq!(
        reopened.replay()?,
        recovery(
            key,
            Phase::PublicationUnknown {
                output,
                code: FailureCode::Runtime,
            },
            Some(EffectAction::ReconcilePublication { output }),
        )
    );
    fixture.remove()?;
    Ok(())
}

#[test]
fn every_nonempty_torn_tail_is_repaired_after_the_valid_prefix() -> Result<(), ScenarioError> {
    let key = StageKey::from([11; 32]);
    let expected = recovery(key, Phase::Requested, Some(EffectAction::Admit));
    for tail_bytes in 1..JOURNAL_FRAME_BYTES {
        let (fixture, _) = one_frame(event(key, EventKind::Requested))?;
        let tail = vec![0xa5; tail_bytes];
        fs::OpenOptions::new()
            .append(true)
            .open(fixture.path())?
            .write_all(&tail)?;
        let mut reopened = FileJournal::open(fixture.path())?;
        assert_eq!(reopened.replay()?, expected);
        assert_eq!(fs::metadata(fixture.path())?.len(), receipt_end(0)?);
        fixture.remove()?;
    }
    Ok(())
}

#[test]
fn every_truncated_header_reports_its_exact_observed_width() -> Result<(), ScenarioError> {
    for actual in 0..JOURNAL_HEADER_BYTES {
        let fixture = Fixture::new("header-truncation");
        fixture.replace(&vec![0; actual])?;
        expect_header(
            FileJournal::open(fixture.path()),
            HeaderError::Truncated {
                required: JournalOffset::from(u64::try_from(JOURNAL_HEADER_BYTES)?),
                actual: JournalOffset::from(u64::try_from(actual)?),
            },
        )?;
        fixture.remove()?;
    }
    Ok(())
}

#[test]
fn each_header_field_class_has_an_exact_diagnosis() -> Result<(), ScenarioError> {
    let fixture = Fixture::new("header-fields");
    let journal = FileJournal::create(fixture.path())?;
    drop(journal);
    let canonical = fixture.bytes()?;
    let cases = [
        (0, HeaderError::Magic { observed: [0; 8] }),
        (8, HeaderError::PhysicalVersion { observed: 0 }),
        (
            10,
            HeaderError::HeaderWidth {
                expected: 32,
                observed: 33,
            },
        ),
        (
            12,
            HeaderError::RecordWidth {
                expected: 68,
                observed: 69,
            },
        ),
        (14, HeaderError::Reserved { observed: 1 }),
        (16, HeaderError::Checksum),
    ];
    for (offset, expected) in cases {
        let mut mutated = canonical.clone();
        if offset == 0 {
            mutated[..8].fill(0);
        } else {
            mutated[offset] ^= 1;
        }
        fixture.replace(&mutated)?;
        expect_header(FileJournal::open(fixture.path()), expected)?;
    }
    fixture.remove()?;
    Ok(())
}

#[test]
fn every_workflow_record_byte_is_rejected_by_the_file_boundary_checksum()
-> Result<(), ScenarioError> {
    const FRAME_SEQUENCE_BYTES: usize = size_of::<u64>();

    let (fixture, canonical) = one_frame(event(StageKey::from([13; 32]), EventKind::Requested))?;
    let record_start = JOURNAL_HEADER_BYTES + FRAME_SEQUENCE_BYTES;
    let expected_offset = JournalOffset::from(u64::try_from(JOURNAL_HEADER_BYTES)?);

    for record_byte in 0..WORKFLOW_RECORD_BYTES {
        let mut mutated = canonical.clone();
        mutated[record_start + record_byte] ^= 1;
        fixture.replace(&mutated)?;
        expect_journal(
            FileJournal::open(fixture.path()),
            ExpectedFailure::FrameChecksum,
            |observed| {
                matches!(
                    observed,
                    JournalError::FrameChecksum {
                        sequence: FrameSequence::FIRST,
                        offset,
                    } if *offset == expected_offset
                )
            },
        )?;
        assert_eq!(fs::metadata(fixture.path())?.len(), receipt_end(0)?);
    }

    fixture.remove()?;
    Ok(())
}

#[test]
fn a_second_physical_owner_is_rejected_before_it_can_issue_a_receipt() -> Result<(), ScenarioError>
{
    let fixture = Fixture::new("exclusive-owner");
    let mut owner = FileJournal::create(fixture.path())?;
    expect_journal(
        FileJournal::open(fixture.path()),
        ExpectedFailure::ExclusiveOwnership,
        |observed| matches!(observed, JournalError::ExclusiveOwnership(_)),
    )?;
    let receipt = owner.append(event(StageKey::from([17; 32]), EventKind::Requested))?;
    assert_eq!(receipt.sequence, FrameSequence::FIRST);
    drop(owner);
    let mut reopened = FileJournal::open(fixture.path())?;
    assert_eq!(
        reopened.replay()?,
        recovery(
            StageKey::from([17; 32]),
            Phase::Requested,
            Some(EffectAction::Admit),
        )
    );
    fixture.remove()?;
    Ok(())
}
