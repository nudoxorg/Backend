use std::{io, path::PathBuf};

use allocation_counter::{AllocationInfo, measure};
use nudox_durable_journal::{
    CommitError, FileJournal, FrameSequence, JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES,
    JournalError, JournalOffset,
};
use nudox_workflow::{EventKind, StageKey, WorkflowEvent, WorkflowVersion};
use thiserror::Error;

const REPLAY_RECORDS: usize = 64;

#[derive(Debug, Error)]
enum AllocationTestError {
    #[error("test filesystem I/O failed")]
    Io(#[from] io::Error),
    #[error("journal operation failed")]
    Journal(#[from] JournalError),
    #[error("journal append failed")]
    Commit(#[from] CommitError),
    #[error("allocation measurement did not execute its closure")]
    MeasurementDidNotRun,
}

fn fixture_path(law: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "nudox-journal-allocation-{law}-{}",
        std::process::id()
    ))
}

#[test]
fn replay_retains_no_record_collection_and_allocates_nothing() -> Result<(), AllocationTestError> {
    let fixture = fixture_path("replay");
    let event = WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([21; 32]),
        kind: EventKind::Requested,
    };
    let mut journal = FileJournal::create(&fixture)?;
    for _ in 0..REPLAY_RECORDS {
        let _receipt = journal.append(event)?;
    }
    let _warm = journal.replay()?;

    let mut recovery = None;
    let allocations = measure(|| recovery = Some(journal.replay()));
    let _recovery = match recovery {
        Some(Ok(recovery)) => recovery,
        Some(Err(error)) => return Err(AllocationTestError::Journal(error)),
        None => return Err(AllocationTestError::MeasurementDidNotRun),
    };
    assert_eq!(
        allocations,
        AllocationInfo {
            count_total: 0,
            count_current: 0,
            count_max: 0,
            bytes_total: 0,
            bytes_current: 0,
            bytes_max: 0,
        }
    );
    std::fs::remove_file(fixture)?;
    Ok(())
}

#[test]
fn warmed_nonempty_append_has_no_heap_allocation_and_exact_control_receipt()
-> Result<(), AllocationTestError> {
    let fixture = fixture_path("append-control");
    let event = WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: StageKey::from([22; 32]),
        kind: EventKind::Requested,
    };
    let mut journal = FileJournal::create(&fixture)?;
    let _warm_receipt = journal.append(event)?;

    let mut receipt = None;
    let allocations = measure(|| receipt = Some(journal.append(event)));
    let receipt = match receipt {
        Some(Ok(receipt)) => receipt,
        Some(Err(error)) => return Err(AllocationTestError::Commit(error)),
        None => return Err(AllocationTestError::MeasurementDidNotRun),
    };
    assert_eq!(
        allocations,
        AllocationInfo {
            count_total: 0,
            count_current: 0,
            count_max: 0,
            bytes_total: 0,
            bytes_current: 0,
            bytes_max: 0,
        }
    );
    assert_eq!(receipt.sequence, FrameSequence::from(1));
    assert_eq!(
        receipt.durable_end,
        JournalOffset::from((JOURNAL_HEADER_BYTES + 2 * JOURNAL_FRAME_BYTES) as u64)
    );
    std::fs::remove_file(fixture)?;
    Ok(())
}
