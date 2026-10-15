//! Filesystem access used by the TypeScript configuration and module resolvers.
//!
//! The default resolver uses [`StdResolutionFileSystem`]. Hosts which need to
//! capture or restrict filesystem access can pass a borrowed implementation to
//! the config loader or [`crate::module_resolver::ModuleResolver`]. A missing
//! path is represented by `Ok(None)` / `Ok(false)`; any other access failure is
//! returned as an error and must not be treated as a negative lookup.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Filesystem entry kind, including links which may need host-specific
/// admission before a resolver follows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionFileKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// A direct child returned by [`ResolutionFileSystem::read_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionDirEntry {
    pub name: OsString,
    pub kind: ResolutionFileKind,
}

/// Filesystem operations required by TypeScript configuration and module
/// resolution.
///
/// Implementations should keep a stable view for the lifetime of a resolver,
/// or advance [`Self::cache_epoch`] when that view changes. The resolver keeps
/// positive and negative probes only for that instance and epoch.
pub trait ResolutionFileSystem {
    /// Cache generation for the provider's current view. Increment this when
    /// file, directory, or package metadata answers may have changed.
    fn cache_epoch(&self) -> u64 {
        0
    }

    /// Optional root above which resolution must not search. This is useful
    /// for captured hosts that admit one workspace and its dependency roots.
    fn resolution_boundary(&self) -> Option<&Path> {
        None
    }

    /// Whether a package or module lookup may continue to `directory`'s
    /// parent. The default enforces the optional single-root boundary above;
    /// providers with several admitted roots can override this without
    /// widening them to one common ancestor.
    fn may_walk_parent(&self, directory: &Path) -> bool {
        self.resolution_boundary()
            .is_none_or(|boundary| directory != boundary && directory.starts_with(boundary))
    }

    /// Read a file. Return `Ok(None)` only for an ordinary missing path;
    /// capability refusals and other IO failures must be `Err`.
    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;

    /// Whether `path` is a file, following links as the host's resolver policy
    /// permits. A missing path is `Ok(false)`.
    fn is_file(&self, path: &Path) -> io::Result<bool>;

    /// Whether `path` is a directory, following links as the host's resolver
    /// policy permits. A missing path is `Ok(false)`.
    fn is_dir(&self, path: &Path) -> io::Result<bool>;

    /// List direct children. Return `Ok(None)` only when the directory is
    /// missing; an unreadable or denied directory is an error.
    fn read_dir(&self, path: &Path) -> io::Result<Option<Vec<ResolutionDirEntry>>>;

    /// Canonicalize an existing path. Return `Ok(None)` only for a missing
    /// path. Hosts can use this hook to record and validate realpath access.
    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>>;

    /// Return the no-follow entry kind for an existing path. Return `Ok(None)`
    /// only when the path is missing.
    fn symlink_metadata(&self, path: &Path) -> io::Result<Option<ResolutionFileKind>>;
}

/// Filesystem provider used by the existing CLI and LSP resolver APIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdResolutionFileSystem;

impl ResolutionFileSystem for StdResolutionFileSystem {
    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn is_file(&self, path: &Path) -> io::Result<bool> {
        Ok(path.is_file())
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        Ok(path.is_dir())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Option<Vec<ResolutionDirEntry>>> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        entries
            .map(|entry| {
                let entry = entry?;
                let file_type = entry.file_type()?;
                let kind = if file_type.is_symlink() {
                    ResolutionFileKind::Symlink
                } else if file_type.is_dir() {
                    ResolutionFileKind::Directory
                } else if file_type.is_file() {
                    ResolutionFileKind::File
                } else {
                    ResolutionFileKind::Other
                };
                Ok(ResolutionDirEntry {
                    name: entry.file_name(),
                    kind,
                })
            })
            .collect::<io::Result<Vec<_>>>()
            .map(Some)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        Ok(std::fs::canonicalize(path).ok())
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Option<ResolutionFileKind>> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                let kind = if file_type.is_symlink() {
                    ResolutionFileKind::Symlink
                } else if file_type.is_dir() {
                    ResolutionFileKind::Directory
                } else if file_type.is_file() {
                    ResolutionFileKind::File
                } else {
                    ResolutionFileKind::Other
                };
                Ok(Some(kind))
            }
            Err(_) => Ok(None),
        }
    }
}

pub(crate) static STD_RESOLUTION_FILE_SYSTEM: StdResolutionFileSystem = StdResolutionFileSystem;
