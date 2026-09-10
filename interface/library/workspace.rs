//! Defines workspace behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the workspace invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Resolution of the one data root every process shares with the compiler.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The resolved absolute data root.
///
/// It is the same root the compiler host resolves, so publications, journal, and library state
/// sit side by side and one environment override moves all of them together.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    /// Resolves the root from `NUDOX_DATA_ROOT` or the documented platform default.
    ///
    /// # Errors
    ///
    /// Returns the exact variable that was missing or relative.
    pub fn resolve() -> Result<Self, WorkspaceRootError> {
        Self::resolve_with(|name| std::env::var_os(name))
    }

    /// Resolves the root from an explicit environment reader, for tests and embedded hosts.
    ///
    /// # Errors
    ///
    /// Returns the exact variable that was missing or relative.
    pub fn resolve_with(
        environment: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, WorkspaceRootError> {
        if let Some(root) = environment("NUDOX_DATA_ROOT") {
            return Self::absolute(PathBuf::from(root), "NUDOX_DATA_ROOT");
        }
        #[cfg(target_os = "macos")]
        {
            let home = environment("HOME").ok_or(WorkspaceRootError::Missing { variable: "HOME" })?;
            return Self::absolute(
                PathBuf::from(home).join("Library/Application Support/Nudox"),
                "HOME",
            );
        }
        #[cfg(target_os = "windows")]
        {
            let local = environment("LOCALAPPDATA")
                .ok_or(WorkspaceRootError::Missing { variable: "LOCALAPPDATA" })?;
            return Self::absolute(PathBuf::from(local).join("Nudox"), "LOCALAPPDATA");
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(data) = environment("XDG_DATA_HOME") {
                return Self::absolute(PathBuf::from(data).join("nudox"), "XDG_DATA_HOME");
            }
            let home = environment("HOME").ok_or(WorkspaceRootError::Missing { variable: "HOME" })?;
            return Self::absolute(PathBuf::from(home).join(".local/share/nudox"), "HOME");
        }
        #[allow(unreachable_code, reason = "every supported platform returned above")]
        Err(WorkspaceRootError::Missing {
            variable: "NUDOX_DATA_ROOT",
        })
    }

    /// Wraps an already-absolute path.
    ///
    /// # Errors
    ///
    /// Rejects a relative path.
    pub fn absolute(path: PathBuf, variable: &'static str) -> Result<Self, WorkspaceRootError> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(WorkspaceRootError::Relative {
                variable,
                path: path.into_boxed_path(),
            })
        }
    }

    /// The root directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The library state directory beneath the root.
    #[must_use]
    pub fn library_dir(&self) -> PathBuf {
        self.0.join("library")
    }

    /// The compiler artifact directory beneath the root.
    #[must_use]
    pub fn artifacts_dir(&self) -> PathBuf {
        self.0.join("artifacts")
    }

    /// The compiler journal directory beneath the root.
    #[must_use]
    pub fn journal_dir(&self) -> PathBuf {
        self.0.join("journal")
    }

    /// The durable shelf file.
    #[must_use]
    pub fn shelf_path(&self) -> PathBuf {
        self.library_dir().join("shelf")
    }

    /// The cross-process mutation lock file.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.library_dir().join("lock")
    }

    /// The epoch counter file readers poll.
    #[must_use]
    pub fn epoch_path(&self) -> PathBuf {
        self.library_dir().join("epoch")
    }

    /// The durable lexical projection directory.
    #[must_use]
    pub fn tantivy_dir(&self) -> PathBuf {
        self.library_dir().join("tantivy")
    }

    /// The parent of every per-compile scratch root.
    ///
    /// A compiler host takes an exclusive lock on the publication journal beneath the data root it
    /// is given and creates that journal with `create_new`, so two hosts cannot share one root and
    /// even one host cannot reopen a root it already used. The library therefore hands each compile
    /// its own scratch root here and promotes the resulting image into the shared artifact store.
    #[must_use]
    pub fn compiles_dir(&self) -> PathBuf {
        self.library_dir().join("compiles")
    }
}

/// Exact root resolution failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceRootError {
    /// A required variable was absent.
    Missing {
        /// Variable name.
        variable: &'static str,
    },
    /// A supplied path was not absolute.
    Relative {
        /// Variable that supplied the path.
        variable: &'static str,
        /// Exact rejected path.
        path: Box<Path>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_root_wins_and_relative_roots_are_rejected() -> Result<(), WorkspaceRootError> {
        let root = WorkspaceRoot::resolve_with(|name| {
            (name == "NUDOX_DATA_ROOT").then(|| OsString::from("/tmp/nudox-test"))
        })?;
        assert_eq!(root.library_dir(), PathBuf::from("/tmp/nudox-test/library"));
        assert!(matches!(
            WorkspaceRoot::resolve_with(|name| {
                (name == "NUDOX_DATA_ROOT").then(|| OsString::from("relative"))
            }),
            Err(WorkspaceRootError::Relative { .. })
        ));
        Ok(())
    }
}
