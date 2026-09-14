//! Local Maven repository layout lookup for validated coordinates.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::legacy::purl::MavenCoordinates;

/// A caller-selected local Maven repository root.
pub struct Repository<'repo> {
    root: &'repo Path,
}

impl<'repo> Repository<'repo> {
    /// Uses `root` exactly; this type does not discover roots from the environment or home.
    pub fn new(root: &'repo Path) -> Self {
        Self { root }
    }

    /// Locates the main artifact JAR.
    pub fn jar(&self, coords: &MavenCoordinates<'_>) -> Result<PathBuf, LocateError> {
        let path = self.artifact_path(coords, "");
        if path.exists() {
            Ok(path)
        } else {
            Err(LocateError::Missing { path })
        }
    }

    /// Locates the sources JAR.
    pub fn sources_jar(&self, coords: &MavenCoordinates<'_>) -> Result<PathBuf, LocateError> {
        let path = self.artifact_path(coords, "-sources");
        if path.exists() {
            Ok(path)
        } else {
            Err(LocateError::MissingSources { path })
        }
    }

    fn artifact_path(&self, coords: &MavenCoordinates<'_>, suffix: &str) -> PathBuf {
        // One owned PathBuf per lookup: filesystem operands must outlive this call's borrows;
        // its bound is the root plus coordinate text, owned by the Result caller. Borrowing the
        // root or using a caller scratch buffer would not return an owned path; string formatting
        // and group replacement would add a second allocation, so they are rejected.
        let mut path = self.root.to_path_buf();
        for group_part in coords.group.split('.') {
            path.push(group_part);
        }
        path.push(coords.artifact);
        path.push(coords.version);
        path.push(format!(
            "{}-{}{}.jar",
            coords.artifact, coords.version, suffix
        ));
        path
    }
}

/// Failure to find one kind of repository artifact.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum LocateError {
    /// The main artifact JAR does not exist at `path`.
    #[error("Maven artifact JAR is missing: {path:?}")]
    Missing {
        /// The exact attempted path.
        path: PathBuf,
    },
    /// The sources artifact does not exist at `path`.
    #[error("Maven sources JAR is missing: {path:?}")]
    MissingSources {
        /// The exact attempted path.
        path: PathBuf,
    },
}
