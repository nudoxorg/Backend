mod harness;

use std::{fs, io::Write};

use blake3::Hasher;
use harness::{FailureClass, Fixture, ScenarioError, receipt_end};
use nudox_durable_journal::{
    CommitError, FileJournal, FrameSequence, HeaderError, JOURNAL_FRAME_BYTES,
    JOURNAL_HEADER_BYTES, JournalError, JournalOffset,
};
use nudox_workflow::{
    Effect, EffectAction, EventKind, FailureCode, Phase, Recovery, StageKey, StageOutput,
    WORKFLOW_RECORD_BYTES, WorkflowEvent, WorkflowRecordError, WorkflowState, WorkflowVersion,
};

const FRAME_SEQUENCE_BYTES: usize = 8;
const FRAME_CHECKSUM_BYTES: usize = 16;
const WORKFLOW_VERSION_BYTES: usize = 2;
const STAGE_KEY_BYTES: usize = 32;
const WORKFLOW_EVENT_OFFSET: usize = WORKFLOW_VERSION_BYTES + STAGE_KEY_BYTES;

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
    match result {
        Err(JournalError::Header(observed)) if observed == expected => Ok(()),
        Err(observed) => Err(ScenarioError::UnexpectedJournalError {
            expected: FailureClass::Header,
            observed,
        }),
        Ok(_) => Err(ScenarioError::UnexpectedJournalSuccess {
            expected: FailureClass::Header,
        }),
    }
}

#[test]
fn every_restart_prefix_has_exact_receipts_and_independent_recovery() -> Result<(), ScenarioError> {
    let key = StageKey::from([3; 32]);
    let output = StageOutput::from([5; 32]);
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
            assert_eq!(receipt.sequence, FrameSequence(u64::try_from(sequence)?));
            assert_eq!(receipt.durable_end, JournalOffset(receipt_end(sequence)?));
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
    assert_eq!(first.sequence, FrameSequence(0));
    assert_eq!(duplicate.sequence, FrameSequence(1));
    let durable_bytes = fs::metadata(fixture.path())?.len();

    let conflicting = event(StageKey::from([8; 32]), EventKind::Requested);
    match journal.append(conflicting) {
        Err(CommitError::Reduction(nudox_workflow::ReductionError::StageKeyMismatch {
            expected,
            observed,
        })) if expected == key && observed == conflicting.key => {}
        Err(observed) => {
            return Err(ScenarioError::UnexpectedCommitError {
                expected: FailureClass::Reduction,
                observed,
            });
        }
        Ok(_) => {
            return Err(ScenarioError::UnexpectedCommitSuccess {
                expected: FailureClass::Reduction,
            });
        }
    }
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
    let output = StageOutput::from([10; 32]);
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
                required: JournalOffset(u64::try_from(JOURNAL_HEADER_BYTES)?),
                actual: JournalOffset(u64::try_from(actual)?),
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
fn every_workflow_record_byte_is_covered_by_the_complete_frame_checksum()
-> Result<(), ScenarioError> {
    let key = StageKey::from([13; 32]);
    let (fixture, canonical) = one_frame(event(key, EventKind::Requested))?;
    let record_start = JOURNAL_HEADER_BYTES + FRAME_SEQUENCE_BYTES;
    for record_byte in 0..WORKFLOW_RECORD_BYTES {
        let mut mutated = canonical.clone();
        mutated[record_start + record_byte] ^= 1;
        fixture.replace(&mutated)?;
        match FileJournal::open(fixture.path()) {
            Err(JournalError::FrameChecksum {
                sequence: FrameSequence::FIRST,
                offset,
            }) if offset == JournalOffset(u64::try_from(JOURNAL_HEADER_BYTES)?) => {}
            Err(observed) => {
                return Err(ScenarioError::UnexpectedJournalError {
                    expected: FailureClass::FrameChecksum,
                    observed,
                });
            }
            Ok(_) => {
                return Err(ScenarioError::UnexpectedJournalSuccess {
                    expected: FailureClass::FrameChecksum,
                });
            }
        }
        assert_eq!(fs::metadata(fixture.path())?.len(), receipt_end(0)?);
    }
    fixture.remove()?;
    Ok(())
}

#[test]
fn sequence_and_canonical_decode_failures_remain_distinct() -> Result<(), ScenarioError> {
    let key = StageKey::from([15; 32]);
    let (fixture, canonical) = one_frame(event(key, EventKind::Requested))?;
    let mut wrong_sequence = canonical.clone();
    wrong_sequence[JOURNAL_HEADER_BYTES] = 1;
    fixture.replace(&wrong_sequence)?;
    match FileJournal::open(fixture.path()) {
        Err(JournalError::Sequence {
            expected: FrameSequence::FIRST,
            observed: FrameSequence(1),
        }) => {}
        Err(observed) => {
            return Err(ScenarioError::UnexpectedJournalError {
                expected: FailureClass::Sequence,
                observed,
            });
        }
        Ok(_) => {
            return Err(ScenarioError::UnexpectedJournalSuccess {
                expected: FailureClass::Sequence,
            });
        }
    }

    let mut unknown_event = canonical;
    let record_start = JOURNAL_HEADER_BYTES + FRAME_SEQUENCE_BYTES;
    unknown_event[record_start + WORKFLOW_EVENT_OFFSET] = u8::MAX;
    rewrite_frame_checksum(&mut unknown_event);
    fixture.replace(&unknown_event)?;
    match FileJournal::open(fixture.path()) {
        Err(JournalError::Decode(WorkflowRecordError::UnknownEvent { observed }))
            if observed == u8::MAX => {}
        Err(observed) => {
            return Err(ScenarioError::UnexpectedJournalError {
                expected: FailureClass::Decode,
                observed,
            });
        }
        Ok(_) => {
            return Err(ScenarioError::UnexpectedJournalSuccess {
                expected: FailureClass::Decode,
            });
        }
    }
    fixture.remove()?;
    Ok(())
}

fn rewrite_frame_checksum(file: &mut [u8]) {
    let frame_start = JOURNAL_HEADER_BYTES;
    let checksum_start = frame_start + JOURNAL_FRAME_BYTES - FRAME_CHECKSUM_BYTES;
    let mut hasher = Hasher::new();
    hasher.update(b"nudox.journal.frame.v1\0");
    hasher.update(&1_u16.to_le_bytes());
    hasher.update(&file[frame_start..checksum_start]);
    file[checksum_start..].copy_from_slice(&hasher.finalize().as_bytes()[..FRAME_CHECKSUM_BYTES]);
}
