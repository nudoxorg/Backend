//! Defines epoch behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the epoch invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The cross-process change signal: a monotone counter in one file, bumped by writers, polled by readers.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Monotone library generation. Equal epochs mean an unchanged shelf.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LibraryEpoch(pub u64);

impl LibraryEpoch {
    /// The next epoch.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Exact epoch file failure.
#[derive(Debug)]
pub enum EpochError {
    /// The epoch file could not be read or written.
    Io {
        /// Failed operation.
        phase: EpochPhase,
        /// Exact path.
        path: Box<Path>,
        /// Underlying error.
        source: io::Error,
    },
    /// The file did not contain one decimal counter.
    Malformed {
        /// Exact path.
        path: Box<Path>,
    },
}

/// Which epoch operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpochPhase {
    /// Reading the counter.
    Read,
    /// Writing the staged counter.
    Write,
    /// Atomically renaming the staged counter into place.
    Rename,
}

/// Reader-side handle that answers "did anything change since I last looked?".
#[derive(Clone, Debug)]
pub struct LibraryWatcher {
    path: PathBuf,
    observed: LibraryEpoch,
}

impl LibraryWatcher {
    pub(crate) fn new(library_dir: &Path, observed: LibraryEpoch) -> Self {
        Self {
            path: library_dir.join("epoch"),
            observed,
        }
    }

    /// The epoch this watcher last acknowledged.
    #[must_use]
    pub const fn observed(&self) -> LibraryEpoch {
        self.observed
    }

    /// Returns the new epoch when the shelf changed since the last acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns the exact file failure; a missing file reads as epoch zero.
    pub fn poll(&mut self) -> Result<Option<LibraryEpoch>, EpochError> {
        let current = read_epoch(&self.path)?;
        if current > self.observed {
            self.observed = current;
            Ok(Some(current))
        } else {
            Ok(None)
        }
    }
}

pub(crate) fn read_epoch(path: &Path) -> Result<LibraryEpoch, EpochError> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map(LibraryEpoch)
            .map_err(|_| EpochError::Malformed { path: path.into() }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(LibraryEpoch::default()),
        Err(source) => Err(EpochError::Io {
            phase: EpochPhase::Read,
            path: path.into(),
            source,
        }),
    }
}

pub(crate) fn bump_epoch(path: &Path) -> Result<LibraryEpoch, EpochError> {
    let next = read_epoch(path)?.next();
    let staged = path.with_extension("staged");
    fs::write(&staged, next.0.to_string()).map_err(|source| EpochError::Io {
        phase: EpochPhase::Write,
        path: staged.clone().into_boxed_path(),
        source,
    })?;
    fs::rename(&staged, path).map_err(|source| EpochError::Io {
        phase: EpochPhase::Rename,
        path: path.into(),
        source,
    })?;
    Ok(next)
}
