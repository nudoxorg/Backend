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
        let result = FileJournal::create_using(fixture.path(), |file, header| {
            file.write_all(&header.as_bytes()[..prefix])
                .map_err(|source| JournalError::io(JournalIoStep::WriteHeader, source))?;
            Err(JournalError::io(
                JournalIoStep::WriteHeader,
                injected(InjectedFault::HeaderWrite),
            ))
        });
        assert!(matches!(result, Err(JournalError::Io {
            step: JournalIoStep::WriteHeader, source,
        }) if observed_fault(&source) == Some(&InjectedFault::HeaderWrite)));
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
    assert!(matches!(result, Err(JournalError::Io {
        step: JournalIoStep::SyncHeader, source,
    }) if observed_fault(&source) == Some(&InjectedFault::HeaderSync)));
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
        assert!(matches!(result, Err(CommitError::OutcomeUnknown {
            attempted: observed,
            sequence: FrameSequence::FIRST,
            step: CommitIoStep::WriteFrame,
            source,
        }) if observed == attempted && observed_fault(&source) == Some(&InjectedFault::FrameWrite)));
        assert!(matches!(journal.append(event), Err(CommitError::Poisoned)));
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
        assert!(matches!(result, Err(CommitError::OutcomeUnknown {
            attempted,
            sequence: FrameSequence::FIRST,
            step: CommitIoStep::SyncFrame,
            source,
        }) if attempted == WorkflowRecord::from(event)
            && observed_fault(&source) == Some(&InjectedFault::FrameSync)));
        assert!(matches!(journal.replay(), Err(JournalError::Poisoned)));
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
