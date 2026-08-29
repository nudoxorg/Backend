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

use super::{FileJournal, PersistFailure};
use crate::{
    CommitError, CommitIoStep, FrameSequence, JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES,
    JournalError, JournalIoStep, JournalOffset,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpectedFault {
    HeaderWrite,
    HeaderSync,
    FrameWrite,
    FrameSync,
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
    #[error("expected {expected:?}, observed a stable receipt ending at {observed:?}")]
    UnexpectedReceipt {
        expected: ExpectedFault,
        observed: JournalOffset,
    },
    #[error("expected {expected:?}, observed {observed}")]
    UnexpectedJournal {
        expected: ExpectedFault,
        #[source]
        observed: JournalError,
    },
    #[error("expected {expected:?}, observed {observed}")]
    UnexpectedCommit {
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
}

fn requested(key_byte: u8) -> WorkflowEvent {
    WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([key_byte; 32]),
        kind: EventKind::Requested,
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

#[test]
fn wire_records_have_exact_layout_and_little_endian_golden_bytes() {
    let header = HeaderRecord::canonical();
    assert_eq!(JOURNAL_HEADER_BYTES, 32);
    assert_eq!(JOURNAL_FRAME_BYTES, 92);
    assert_eq!(header.as_bytes()[8..16], [1, 0, 32, 0, 68, 0, 0, 0]);

    let record = WorkflowRecord::from(requested(7));
    let frame = FrameRecord::encode(FrameSequence(0x0102_0304_0506_0708), record);
    assert_eq!(
        frame.as_bytes()[..8],
        [8, 7, 6, 5, 4, 3, 2, 1],
        "sequence memory representation is canonical little-endian"
    );
    assert_eq!(core::mem::align_of::<HeaderRecord>(), 1);
    assert_eq!(core::mem::align_of::<FrameRecord>(), 1);
}

#[test]
fn every_header_write_prefix_preserves_the_fault_and_never_constructs_a_journal()
-> Result<(), FaultTestError> {
    for prefix in 0..JOURNAL_HEADER_BYTES {
        let fixture = Fixture::new("header-prefix");
        let result = FileJournal::create_using(fixture.path(), |file, header| {
            file.write_all(&header.as_bytes()[..prefix])
                .map_err(|source| JournalError::io(JournalIoStep::WriteHeader, source))?;
            Err(JournalError::io(
                JournalIoStep::WriteHeader,
                injected(InjectedFault::HeaderWrite),
            ))
        });
        match result {
            Err(JournalError::Io {
                step: JournalIoStep::WriteHeader,
                source,
            }) if observed_fault(&source) == Some(&InjectedFault::HeaderWrite) => {}
            Err(observed) => {
                return Err(FaultTestError::UnexpectedJournal {
                    expected: ExpectedFault::HeaderWrite,
                    observed,
                });
            }
            Ok(_) => {
                return Err(FaultTestError::UnexpectedReceipt {
                    expected: ExpectedFault::HeaderWrite,
                    observed: JournalOffset(u64::try_from(prefix)?),
                });
            }
        }
        assert_eq!(fs::metadata(fixture.path())?.len(), u64::try_from(prefix)?);
        fixture.remove()?;
    }

    let fixture = Fixture::new("header-sync");
    let result = FileJournal::create_using(fixture.path(), |file, header| {
        file.write_all(header.as_bytes())
            .map_err(|source| JournalError::io(JournalIoStep::WriteHeader, source))?;
        Err(JournalError::io(
            JournalIoStep::SyncHeader,
            injected(InjectedFault::HeaderSync),
        ))
    });
    match result {
        Err(JournalError::Io {
            step: JournalIoStep::SyncHeader,
            source,
        }) if observed_fault(&source) == Some(&InjectedFault::HeaderSync) => {}
        Err(observed) => {
            return Err(FaultTestError::UnexpectedJournal {
                expected: ExpectedFault::HeaderSync,
                observed,
            });
        }
        Ok(_) => {
            return Err(FaultTestError::UnexpectedReceipt {
                expected: ExpectedFault::HeaderSync,
                observed: JournalOffset(u64::try_from(JOURNAL_HEADER_BYTES)?),
            });
        }
    }
    assert_eq!(
        fs::metadata(fixture.path())?.len(),
        u64::try_from(JOURNAL_HEADER_BYTES)?
    );
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
        match result {
            Err(CommitError::OutcomeUnknown {
                attempted: observed,
                sequence: FrameSequence::FIRST,
                step: CommitIoStep::WriteFrame,
                source,
            }) if observed == attempted
                && observed_fault(&source) == Some(&InjectedFault::FrameWrite) => {}
            Err(observed) => {
                return Err(FaultTestError::UnexpectedCommit {
                    expected: ExpectedFault::FrameWrite,
                    observed,
                });
            }
            Ok(receipt) => {
                return Err(FaultTestError::UnexpectedReceipt {
                    expected: ExpectedFault::FrameWrite,
                    observed: receipt.durable_end,
                });
            }
        }
        match journal.append(event) {
            Err(CommitError::Poisoned) => {}
            Err(observed) => {
                return Err(FaultTestError::UnexpectedCommit {
                    expected: ExpectedFault::Poisoned,
                    observed,
                });
            }
            Ok(receipt) => {
                return Err(FaultTestError::UnexpectedReceipt {
                    expected: ExpectedFault::Poisoned,
                    observed: receipt.durable_end,
                });
            }
        }
        drop(journal);
        let mut reopened = FileJournal::open(fixture.path())?;
        assert_eq!(
            reopened.replay()?,
            Recovery {
                state: WorkflowState::New,
                pending_effect: None,
            }
        );
        assert_eq!(fs::metadata(fixture.path())?.len(), 32);
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
        match result {
            Err(CommitError::OutcomeUnknown {
                attempted,
                sequence: FrameSequence::FIRST,
                step: CommitIoStep::SyncFrame,
                source,
            }) if attempted == WorkflowRecord::from(event)
                && observed_fault(&source) == Some(&InjectedFault::FrameSync) => {}
            Err(observed) => {
                return Err(FaultTestError::UnexpectedCommit {
                    expected: ExpectedFault::FrameSync,
                    observed,
                });
            }
            Ok(receipt) => {
                return Err(FaultTestError::UnexpectedReceipt {
                    expected: ExpectedFault::FrameSync,
                    observed: receipt.durable_end,
                });
            }
        }
        match journal.replay() {
            Err(JournalError::Poisoned) => {}
            Err(observed) => {
                return Err(FaultTestError::UnexpectedJournal {
                    expected: ExpectedFault::Poisoned,
                    observed,
                });
            }
            Ok(_) => {
                return Err(FaultTestError::UnexpectedReceipt {
                    expected: ExpectedFault::Poisoned,
                    observed: JournalOffset(0),
                });
            }
        }
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
fn legal_short_writes_can_complete_one_frame_without_an_intermediate_receipt()
-> Result<(), FaultTestError> {
    const CHUNKS: [usize; 7] = [1, 2, 3, 5, 8, 13, 60];
    let fixture = Fixture::new("short-success");
    let mut journal = FileJournal::create(fixture.path())?;
    let receipt = journal.append_using(requested(17), |file, frame| {
        file.seek(SeekFrom::End(0))
            .map_err(|source| persist_failure(CommitIoStep::Position, source))?;
        let mut written = 0;
        for bytes in CHUNKS {
            let end = written + bytes;
            file.write_all(&frame.as_bytes()[written..end])
                .map_err(|source| persist_failure(CommitIoStep::WriteFrame, source))?;
            written = end;
        }
        file.sync_all().map_err(|source| PersistFailure {
            step: CommitIoStep::SyncFrame,
            source,
        })
    })?;
    assert_eq!(receipt.sequence, FrameSequence::FIRST);
    assert_eq!(receipt.durable_end, JournalOffset(124));
    assert_eq!(fs::metadata(fixture.path())?.len(), 124);
    fixture.remove()?;
    Ok(())
}
