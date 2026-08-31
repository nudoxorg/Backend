use std::{
    fs::{self},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use nudox_workflow::{
    Effect, EffectAction, EventKind, Phase, Recovery, StageKey, WorkflowEvent, WorkflowRecord,
    WorkflowState, WorkflowVersion,
};
use thiserror::Error;
use zerocopy::IntoBytes;

use super::{FileJournal, GroupCommitError, PersistFailure, persist_header};
use crate::{
    CommitError, CommitIoStep, FrameSequence, JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES,
    JournalError, JournalIoStep,
    format::{FrameRecord, HeaderRecord},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
enum InjectedFault {
    #[error("injected header write failure")]
    HeaderWrite,
    #[error("injected header sync failure")]
    HeaderSync,
    #[error("injected frame write failure")]
    FrameWrite,
    #[error("injected frame sync failure")]
    FrameSync,
    #[error("injected parent-directory sync failure")]
    DirectorySync,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpectedFault {
    HeaderWrite,
    HeaderSync,
    DirectorySync,
    FrameWrite,
    FrameSync,
    Sequence,
    Decode,
    Poisoned,
}

#[derive(Debug, Error)]
enum FaultTestError {
    #[error("test filesystem I/O failed")]
    Io(#[from] io::Error),
    #[error("journal operation failed unexpectedly")]
    Journal(#[from] JournalError),
    #[error("journal append failed unexpectedly")]
    Commit(#[from] CommitError),
    #[error("test fixture length cannot be represented")]
    Length(#[from] core::num::TryFromIntError),
    #[error("test workflow record width is invalid")]
    Record(#[from] core::array::TryFromSliceError),
    #[error("expected {expected:?}, but the journal operation succeeded")]
    JournalSuccess { expected: ExpectedFault },
    #[error("expected {expected:?}, observed {observed}")]
    JournalMismatch {
        expected: ExpectedFault,
        #[source]
        observed: JournalError,
    },
    #[error("expected {expected:?}, but append returned a stable receipt")]
    CommitSuccess { expected: ExpectedFault },
    #[error("expected {expected:?}, observed {observed}")]
    CommitMismatch {
        expected: ExpectedFault,
        #[source]
        observed: CommitError,
    },
}

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Self {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "nudox-journal-{label}-{}-{ordinal}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn remove(self) -> Result<(), io::Error> {
        fs::remove_file(self.0)
    }

    fn bytes(&self) -> Result<Vec<u8>, io::Error> {
        fs::read(&self.0)
    }

    fn replace(&self, bytes: &[u8]) -> Result<(), io::Error> {
        fs::write(&self.0, bytes)
    }
}

fn requested(key_byte: u8) -> WorkflowEvent {
    WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([key_byte; 32]),
        kind: EventKind::Requested,
    }
}

fn admitted(key_byte: u8) -> WorkflowEvent {
    WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([key_byte; 32]),
        kind: EventKind::Admitted,
    }
}

fn injected(fault: InjectedFault) -> io::Error {
    io::Error::other(fault)
}

fn persist_failure(step: CommitIoStep, source: io::Error) -> PersistFailure {
    PersistFailure { step, source }
}

fn observed_fault(source: &io::Error) -> Option<&InjectedFault> {
    source
        .get_ref()
        .and_then(|source| source.downcast_ref::<InjectedFault>())
}

fn expect_journal<Value>(
    result: Result<Value, JournalError>,
    expected: ExpectedFault,
    accepts: impl FnOnce(&JournalError) -> bool,
) -> Result<(), FaultTestError> {
    match result {
        Err(observed) if accepts(&observed) => Ok(()),
        Err(observed) => Err(FaultTestError::JournalMismatch { expected, observed }),
        Ok(_) => Err(FaultTestError::JournalSuccess { expected }),
    }
}

fn expect_commit<Value>(
    result: Result<Value, CommitError>,
    expected: ExpectedFault,
    accepts: impl FnOnce(&CommitError) -> bool,
) -> Result<(), FaultTestError> {
    match result {
        Err(observed) if accepts(&observed) => Ok(()),
        Err(observed) => Err(FaultTestError::CommitMismatch { expected, observed }),
        Ok(_) => Err(FaultTestError::CommitSuccess { expected }),
    }
}

#[test]
fn every_header_write_prefix_preserves_the_fault_and_never_constructs_a_journal()
-> Result<(), FaultTestError> {
    let header = HeaderRecord::canonical();
    assert_eq!(header.as_bytes()[8..16], [1, 0, 32, 0, 68, 0, 0, 0]);
    let frame = FrameRecord::encode(
        FrameSequence::from(0x0102_0304_0506_0708),
        WorkflowRecord::from(requested(7)),
    );
    assert_eq!(frame.as_bytes()[..8], [8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(core::mem::align_of::<HeaderRecord>(), 1);
    assert_eq!(core::mem::align_of::<FrameRecord>(), 1);
    for prefix in 0..JOURNAL_HEADER_BYTES {
        let fixture = Fixture::new("header-prefix");
        let result = FileJournal::create_using(
            fixture.path(),
            |file, header| {
                file.write_all(&header.as_bytes()[..prefix])
                    .map_err(|source| JournalError::io(JournalIoStep::WriteHeader, source))?;
                Err(JournalError::io(
                    JournalIoStep::WriteHeader,
                    injected(InjectedFault::HeaderWrite),
                ))
            },
            |_| Ok(()),
        );
        expect_journal(result, ExpectedFault::HeaderWrite, |observed| {
            matches!(observed, JournalError::Io {
                step: JournalIoStep::WriteHeader, source,
            } if observed_fault(source) == Some(&InjectedFault::HeaderWrite))
        })?;
        assert_eq!(fs::metadata(fixture.path())?.len(), u64::try_from(prefix)?);
        fixture.remove()?;
    }

    let fixture = Fixture::new("header-sync");
    let result = FileJournal::create_using(
        fixture.path(),
        |file, header| {
            file.write_all(header.as_bytes())
                .map_err(|source| JournalError::io(JournalIoStep::WriteHeader, source))?;
            Err(JournalError::io(
                JournalIoStep::SyncHeader,
                injected(InjectedFault::HeaderSync),
            ))
        },
        |_| Ok(()),
    );
    expect_journal(result, ExpectedFault::HeaderSync, |observed| {
        matches!(observed, JournalError::Io {
            step: JournalIoStep::SyncHeader, source,
        } if observed_fault(source) == Some(&InjectedFault::HeaderSync))
    })?;
    assert_eq!(
        fs::metadata(fixture.path())?.len(),
        u64::try_from(JOURNAL_HEADER_BYTES)?
    );
    fixture.remove()?;
    Ok(())
}

#[test]
fn directory_sync_failure_prevents_a_created_journal_from_escaping() -> Result<(), FaultTestError> {
    let fixture = Fixture::new("directory-sync");
    let result = FileJournal::create_using(fixture.path(), persist_header, |_| {
        Err(JournalError::io(
            JournalIoStep::SyncParentDirectory,
            injected(InjectedFault::DirectorySync),
        ))
    });
    expect_journal(result, ExpectedFault::DirectorySync, |observed| {
        matches!(observed, JournalError::Io {
            step: JournalIoStep::SyncParentDirectory, source,
        } if observed_fault(source) == Some(&InjectedFault::DirectorySync))
    })?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn every_frame_write_prefix_returns_no_receipt_poisons_and_reopens_to_the_prior_state()
-> Result<(), FaultTestError> {
    for prefix in 0..JOURNAL_FRAME_BYTES {
        let fixture = Fixture::new("frame-prefix");
        let event = requested(11);
        let attempted = WorkflowRecord::from(event);
        let mut journal = FileJournal::create(fixture.path())?;
        let result = journal.append_using(event, |file, frame| {
            file.seek(SeekFrom::End(0))
                .map_err(|source| persist_failure(CommitIoStep::Position, source))?;
            file.write_all(&frame.as_bytes()[..prefix])
                .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
            Err(PersistFailure {
                step: CommitIoStep::WriteFrame,
                source: injected(InjectedFault::FrameWrite),
            })
        });
        expect_commit(result, ExpectedFault::FrameWrite, |observed| {
            matches!(observed, CommitError::OutcomeUnknown {
                attempted: observed_record,
                sequence: FrameSequence::FIRST,
                step: CommitIoStep::WriteFrame,
                source,
            } if *observed_record == attempted
                && observed_fault(source) == Some(&InjectedFault::FrameWrite))
        })?;
        expect_commit(journal.append(event), ExpectedFault::Poisoned, |observed| {
            matches!(observed, CommitError::Poisoned)
        })?;
        drop(journal);
        let mut reopened = FileJournal::open(fixture.path())?;
        assert_eq!(
            reopened.replay()?,
            Recovery {
                state: WorkflowState::New,
                pending_effect: None,
            }
        );
        assert_eq!(
            fs::metadata(fixture.path())?.len(),
            JOURNAL_HEADER_BYTES as u64
        );
        fixture.remove()?;
    }
    Ok(())
}

#[test]
fn sync_failure_reconciles_both_absent_and_present_physical_images() -> Result<(), FaultTestError> {
    for frame_present in [false, true] {
        let fixture = Fixture::new("sync-image");
        let event = requested(13);
        let header_end = u64::try_from(JOURNAL_HEADER_BYTES)?;
        let mut journal = FileJournal::create(fixture.path())?;
        let result = journal.append_using(event, |file, frame| {
            file.seek(SeekFrom::End(0))
                .map_err(|source| persist_failure(CommitIoStep::Position, source))?;
            file.write_all(frame.as_bytes())
                .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
            if !frame_present {
                file.set_len(header_end)
                    .map_err(|source| persist_failure(CommitIoStep::SyncFrame, source))?;
            }
            Err(PersistFailure {
                step: CommitIoStep::SyncFrame,
                source: injected(InjectedFault::FrameSync),
            })
        });
        expect_commit(result, ExpectedFault::FrameSync, |observed| {
            matches!(observed, CommitError::OutcomeUnknown {
                attempted,
                sequence: FrameSequence::FIRST,
                step: CommitIoStep::SyncFrame,
                source,
            } if *attempted == WorkflowRecord::from(event)
                && observed_fault(source) == Some(&InjectedFault::FrameSync))
        })?;
        expect_journal(journal.replay(), ExpectedFault::Poisoned, |observed| {
            matches!(observed, JournalError::Poisoned)
        })?;
        drop(journal);

        let mut reopened = FileJournal::open(fixture.path())?;
        let expected = if frame_present {
            let key = event.key;
            Recovery {
                state: WorkflowState::Keyed {
                    key,
                    phase: Phase::Requested,
                },
                pending_effect: Some(Effect {
                    key,
                    action: EffectAction::Admit,
                }),
            }
        } else {
            Recovery {
                state: WorkflowState::New,
                pending_effect: None,
            }
        };
        assert_eq!(reopened.replay()?, expected);
        fixture.remove()?;
    }
    Ok(())
}

#[test]
fn legal_fragmented_writes_issue_one_receipt_only_after_sync() -> Result<(), FaultTestError> {
    const PREFIX_CHUNKS: [usize; 6] = [1, 2, 3, 5, 8, 13];

    let fixture = Fixture::new("fragmented-success");
    let mut journal = FileJournal::create(fixture.path())?;
    let receipt = journal.append_using(requested(17), |file, frame| {
        file.seek(SeekFrom::End(0))
            .map_err(|source| persist_failure(CommitIoStep::Position, source))?;
        let mut written = 0;
        for bytes in PREFIX_CHUNKS {
            let end = written + bytes;
            file.write_all(&frame.as_bytes()[written..end])
                .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
            written = end;
        }
        file.write_all(&frame.as_bytes()[written..])
            .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
        file.sync_all().map_err(|source| PersistFailure {
            step: CommitIoStep::SyncFrame,
            source,
        })
    })?;
    let expected_end = u64::try_from(JOURNAL_HEADER_BYTES + JOURNAL_FRAME_BYTES)?;
    assert_eq!(receipt.sequence, FrameSequence::FIRST);
    assert_eq!(*receipt.durable_end, expected_end);
    assert_eq!(fs::metadata(fixture.path())?.len(), expected_end);
    fixture.remove()?;
    Ok(())
}

#[test]
fn sequence_and_canonical_decode_failures_remain_distinct() -> Result<(), FaultTestError> {
    let fixture = Fixture::new("decode-classes");
    let mut journal = FileJournal::create(fixture.path())?;
    let _receipt = journal.append(requested(23))?;
    drop(journal);
    let canonical = fixture.bytes()?;

    let mut wrong_sequence = canonical.clone();
    wrong_sequence[JOURNAL_HEADER_BYTES] = 1;
    fixture.replace(&wrong_sequence)?;
    expect_journal(
        FileJournal::open(fixture.path()),
        ExpectedFault::Sequence,
        |observed| {
            matches!(
                observed,
                JournalError::Sequence {
                    expected: FrameSequence::FIRST,
                    observed,
                } if *observed == FrameSequence::from(1)
            )
        },
    )?;

    let record = WorkflowRecord::from(requested(23));
    let mut record_bytes = [0; nudox_workflow::WORKFLOW_RECORD_BYTES];
    record_bytes.copy_from_slice(record.as_bytes());
    let event_offset = core::mem::size_of::<WorkflowVersion>() + core::mem::size_of::<StageKey>();
    record_bytes[event_offset] = u8::MAX;
    let invalid = WorkflowRecord::try_from(record_bytes.as_ref())?;
    let frame = FrameRecord::encode(FrameSequence::FIRST, invalid);
    let mut invalid_file = canonical;
    invalid_file[JOURNAL_HEADER_BYTES..].copy_from_slice(frame.as_bytes());
    fixture.replace(&invalid_file)?;
    expect_journal(
        FileJournal::open(fixture.path()),
        ExpectedFault::Decode,
        |observed| {
            matches!(
                observed,
                JournalError::Decode(nudox_workflow::WorkflowRecordError::UnknownEvent {
                    observed,
                }) if *observed == u8::MAX
            )
        },
    )?;
    fixture.remove()?;
    Ok(())
}

#[test]
fn grouped_append_reduces_before_one_write_and_one_sync() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::new("grouped-success");
    let mut journal = FileJournal::create(fixture.path())?;
    let events = [requested(29), admitted(29)];
    let mut frames = vec![0xA5; JOURNAL_FRAME_BYTES * events.len()];
    let mut writes = 0;
    let mut syncs = 0;
    let receipt = journal.append_group_using(&events, &mut frames, |file, bytes| {
        writes += 1;
        file.seek(SeekFrom::End(0))
            .map_err(|source| persist_failure(CommitIoStep::Position, source))?;
        file.write_all(bytes)
            .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
        syncs += 1;
        file.sync_all().map_err(|source| PersistFailure {
            step: CommitIoStep::SyncFrame,
            source,
        })
    })?;
    assert_eq!(writes, 1);
    assert_eq!(syncs, 1);
    assert_eq!(
        receipt.receipt_at(0),
        Some(crate::StableReceipt::committed(
            FrameSequence::from(0),
            crate::JournalOffset::from((JOURNAL_HEADER_BYTES + JOURNAL_FRAME_BYTES) as u64),
        ))
    );
    assert!(receipt.receipt_at(2).is_none());
    assert_eq!(
        fs::metadata(fixture.path())?.len(),
        (JOURNAL_HEADER_BYTES + JOURNAL_FRAME_BYTES * events.len()) as u64
    );
    drop(journal);
    let reopened = FileJournal::open(fixture.path())?;
    assert_eq!(reopened.last_receipt(), receipt.receipt_at(1));
    fixture.remove()?;
    Ok(())
}

#[test]
fn grouped_append_fault_retains_attempt_and_reopens_to_durable_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    for prefix in 0..=(JOURNAL_FRAME_BYTES * 2) {
        let fixture = Fixture::new("grouped-prefix");
        let mut journal = FileJournal::create(fixture.path())?;
        let events = [requested(31), admitted(31)];
        let attempted = WorkflowRecord::from(events[0]);
        let mut frames = vec![0; JOURNAL_FRAME_BYTES * events.len()];
        let result = journal.append_group_using(&events, &mut frames, |file, bytes| {
            file.seek(SeekFrom::End(0))
                .map_err(|source| persist_failure(CommitIoStep::Position, source))?;
            file.write_all(&bytes[..prefix])
                .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
            Err(PersistFailure {
                step: CommitIoStep::SyncFrame,
                source: injected(InjectedFault::FrameSync),
            })
        });
        match result {
            Err(GroupCommitError::OutcomeUnknown {
                attempted: observed,
                first_sequence: FrameSequence::FIRST,
                count: 2,
                step: CommitIoStep::SyncFrame,
                source,
            }) => {
                assert_eq!(*observed, attempted);
                assert_eq!(observed_fault(&source), Some(&InjectedFault::FrameSync));
            }
            Err(other) => panic!("unexpected grouped failure: {other:?}"),
            Ok(_) => panic!("group fault unexpectedly produced a receipt"),
        }
        assert!(matches!(journal.replay(), Err(JournalError::Poisoned)));
        assert_eq!(
            fs::metadata(fixture.path())?.len(),
            (JOURNAL_HEADER_BYTES + prefix) as u64
        );
        drop(journal);
        let mut reopened = FileJournal::open(fixture.path())?;
        let recovery = reopened.replay()?;
        if prefix == JOURNAL_FRAME_BYTES * 2 {
            assert_eq!(recovery.state.phase(), nudox_workflow::PhaseName::Admitted);
        } else if prefix >= JOURNAL_FRAME_BYTES {
            assert_eq!(recovery.state.phase(), nudox_workflow::PhaseName::Requested);
        } else {
            assert_eq!(recovery.state, WorkflowState::New);
        }
        fixture.remove()?;
    }
    Ok(())
}
