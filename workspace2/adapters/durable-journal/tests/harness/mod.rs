use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use nudox_durable_journal::{CommitError, JournalError};
use thiserror::Error;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

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
}

pub struct Fixture {
    path: PathBuf,
}

impl Fixture {
    pub fn new(label: &str) -> Self {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        Self {
            path: std::env::temp_dir().join(format!(
                "nudox-journal-public-{label}-{}-{ordinal}",
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
        .checked_mul(u64::try_from(nudox_durable_journal::JOURNAL_FRAME_BYTES)?)
        .ok_or(ScenarioError::ArithmeticOverflow)?;
    u64::try_from(nudox_durable_journal::JOURNAL_HEADER_BYTES)?
        .checked_add(frames)
        .ok_or(ScenarioError::ArithmeticOverflow)
}
