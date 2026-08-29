use std::{io, path::PathBuf};

use allocation_counter::{AllocationInfo, measure};
use nudox_durable_journal::{CommitError, FileJournal, JournalError};
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

fn fixture_path() -> PathBuf {
    std::env::temp_dir().join(format!("nudox-journal-allocation-{}", std::process::id()))
}

#[test]
fn replay_retains_no_record_collection_and_allocates_nothing() -> Result<(), AllocationTestError> {
    let fixture = fixture_path();
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
