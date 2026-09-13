//! Exercises the `server-journal` tests harness contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use server_journal::{CommitError, JournalError};
use thiserror::Error;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpectedFailure {
    Header,
    FrameChecksum,
    ExclusiveOwnership,
    Reduction,
}

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("test filesystem I/O failed")]
    Io(#[from] io::Error),
    #[error("journal operation failed")]
    Journal(#[from] JournalError),
    #[error("journal append failed")]
    Commit(#[from] CommitError),
    #[error("fixture integer conversion failed")]
    Integer(#[from] core::num::TryFromIntError),
    #[error("fixture receipt arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("expected {expected:?}, but the journal operation succeeded")]
    JournalSuccess { expected: ExpectedFailure },
    #[error("expected {expected:?}, observed {observed}")]
    JournalMismatch {
        expected: ExpectedFailure,
        #[source]
        observed: JournalError,
    },
    #[error("expected {expected:?}, but append returned a stable receipt")]
    CommitSuccess { expected: ExpectedFailure },
    #[error("expected {expected:?}, observed {observed}")]
    CommitMismatch {
        expected: ExpectedFailure,
        #[source]
        observed: CommitError,
    },
}

pub fn expect_journal<Value>(
    result: Result<Value, JournalError>,
    expected: ExpectedFailure,
    accepts: impl FnOnce(&JournalError) -> bool,
) -> Result<(), ScenarioError> {
    match result {
        Err(observed) if accepts(&observed) => Ok(()),
        Err(observed) => Err(ScenarioError::JournalMismatch { expected, observed }),
        Ok(_) => Err(ScenarioError::JournalSuccess { expected }),
    }
}

pub fn expect_commit<Value>(
    result: Result<Value, CommitError>,
    expected: ExpectedFailure,
    accepts: impl FnOnce(&CommitError) -> bool,
) -> Result<(), ScenarioError> {
    match result {
        Err(observed) if accepts(&observed) => Ok(()),
        Err(observed) => Err(ScenarioError::CommitMismatch { expected, observed }),
        Ok(_) => Err(ScenarioError::CommitSuccess { expected }),
    }
}

pub struct Fixture {
    path: PathBuf,
}

impl Fixture {
    pub fn new(label: &str) -> Self {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir().join(format!(
                "server-journal-public-{label}-{}-{ordinal}",
                std::process::id()
            )),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> Result<Vec<u8>, io::Error> {
        fs::read(&self.path)
    }

    pub fn replace(&self, bytes: &[u8]) -> Result<(), io::Error> {
        fs::write(&self.path, bytes)
    }

    pub fn remove(self) -> Result<(), io::Error> {
        fs::remove_file(self.path)
    }
}

pub fn receipt_end(sequence: usize) -> Result<u64, ScenarioError> {
    let count = u64::try_from(sequence)?
        .checked_add(1)
        .ok_or(ScenarioError::ArithmeticOverflow)?;
    let frames = count
        .checked_mul(u64::try_from(server_journal::JOURNAL_FRAME_BYTES)?)
        .ok_or(ScenarioError::ArithmeticOverflow)?;
    u64::try_from(server_journal::JOURNAL_HEADER_BYTES)?
        .checked_add(frames)
        .ok_or(ScenarioError::ArithmeticOverflow)
}
