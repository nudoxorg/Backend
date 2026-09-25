//! C/C++ project identity and compilation-argument authority.
//!
//! This module owns the one project identity surface for the real C/C++
//! native fact lane: it checks an immutable project root and entry
//! translation unit, then supplies the exact libclang argument vector the
//! entry must parse under. Fact collection itself lives in [`crate::legacy`];
//! tree-sitter remains available through [`crate::syntax_frontend`] as a
//! syntax baseline and is never used to manufacture a semantic claim here.

use crate::{compile_commands::CompileCommands, system_includes};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A checked C/C++ semantic authority rooted at one immutable project path and
/// bound to the one entry translation unit being compiled.
///
/// The entry is retained so the cross-file reference plane can be keyed
/// relative to the package root: libclang resolves project-local `#include`s
/// against their real on-disk paths, and only paths beneath this root yield a
/// stable cross-fragment identity (see `cursor_file_identity`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClangProject {
    root: PathBuf,
    entry: PathBuf,
}

impl ClangProject {
    /// Opens a project root and its exact entry translation unit.
    ///
    /// The root is not followed through a symlink, and the entry must be a
    /// regular file canonically contained beneath it, so no foreign fragment
    /// key can be minted from an out-of-project path.
    ///
    /// # Errors
    /// Returns [`ClangAuthorityError`] when `root` is not an absolute regular
    /// directory, when `entry` is not an absolute regular file, when either
    /// cannot be canonicalized, or when `entry` escapes `root`.
    pub fn open(
        root: impl AsRef<Path>,
        entry: impl AsRef<Path>,
    ) -> Result<Self, ClangAuthorityError> {
        let requested = root.as_ref();
        if !requested.is_absolute() {
            return Err(ClangAuthorityError::RelativePath {
                path: requested.to_path_buf(),
            });
        }
        let metadata =
            fs::symlink_metadata(requested).map_err(|source| ClangAuthorityError::RootIo {
                path: requested.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ClangAuthorityError::RootNotDirectory {
                path: requested.to_path_buf(),
            });
        }
        let root = requested
            .canonicalize()
            .map_err(|source| ClangAuthorityError::RootIo {
                path: requested.to_path_buf(),
                source,
            })?;
        let entry = checked_file(entry.as_ref())?;
        if !entry.starts_with(&root) {
            return Err(ClangAuthorityError::EntryOutsideRoot {
                root,
                entry,
            });
        }
        Ok(Self { root, entry })
    }

    /// Returns the canonical project root retained by this authority.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the canonical entry translation unit retained by this authority.
    #[must_use]
    pub fn entry(&self) -> &Path {
        &self.entry
    }

    /// Returns the exact libclang argument vector for the entry translation
    /// unit: the host system include search path followed by the entry's
    /// compilation-database arguments, or the per-extension defaults when the
    /// database is absent or does not list the entry.
    #[must_use]
    pub fn arguments(&self) -> Vec<String> {
        let database = CompileCommands::load(&self.root);
        let selected = database
            .as_ref()
            .and_then(|commands| commands.args_for(&self.entry));
        let cpp_package = package_prefers_cpp(&self.root);
        let defaults = default_arguments(&self.entry, cpp_package);
        let selected = selected.as_deref().unwrap_or(&defaults);
        system_includes::args()
            .into_iter()
            .chain(selected.iter().cloned())
            .collect()
    }
}

/// Typed failure from the executable Clang authority.
#[derive(Debug, thiserror::Error)]
pub enum ClangAuthorityError {
    /// A caller supplied a relative path.
    #[error("Clang authority path is relative: {path}", path = path.display())]
    RelativePath {
        /// Caller-selected path.
        path: PathBuf,
    },
    /// The project root could not be inspected.
    #[error("cannot inspect Clang project root {path}: {source}", path = path.display())]
    RootIo {
        /// Path involved in the filesystem operation.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The project root was not a real directory.
    #[error("Clang project root is not a directory: {path}", path = path.display())]
    RootNotDirectory {
        /// Caller-selected root path.
        path: PathBuf,
    },
    /// The entry translation unit was not contained beneath the project root.
    #[error(
        "Clang entry {entry} is outside project root {root}",
        entry = entry.display(),
        root = root.display()
    )]
    EntryOutsideRoot {
        /// Canonical project root.
        root: PathBuf,
        /// Canonical entry path that escaped the root.
        entry: PathBuf,
    },
    /// A source file disappeared or changed type during discovery.
    #[error("Clang source path is not a regular file: {path}", path = path.display())]
    InvalidSource {
        /// Source path that failed the regular-file check.
        path: PathBuf,
    },
}

fn checked_file(path: &Path) -> Result<PathBuf, ClangAuthorityError> {
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| ClangAuthorityError::RootIo {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ClangAuthorityError::InvalidSource {
            path: path.to_path_buf(),
        });
    }
    path.canonicalize()
        .map_err(|source| ClangAuthorityError::RootIo {
            path: path.to_path_buf(),
            source,
        })
}

/// The per-extension parse arguments for a file absent from the compilation
/// database.
///
/// C++ header extensions are classified as C++ deliberately: libclang infers
/// the language from the file name alone, and `.hpp`/`.hh`/`.hxx`/`.h++` are
/// unambiguously C++ there, so a C default contradicts the file's own
/// language. Measured against the real-package corpus: the amalgamated
/// single-header entries `nlohmann-json/single_include/nlohmann/json.hpp`
/// (919,975 bytes) and `catch2/single_include/catch2/catch.hpp` (657,411
/// bytes) were handed `-std=c11`, which the driver rejects with the fatal
/// `invalid argument '-std=c11' not allowed with 'C++'` before a single
/// declaration is visited, and `clang_parseTranslationUnit2` reports that
/// rejection as `CXError_ASTReadError`. `.h` stays in the C arm on purpose
/// when the package is C-only: it is genuinely ambiguous (C projects and C++
/// projects both use it), and the C default matches libclang's own extension
/// inference for it. A `.h` next to C++ sources, or in a header-only tree whose
/// headers look like C++, uses the C++ arm instead.
fn default_arguments(path: &Path, cpp_package: bool) -> Vec<String> {
    if prefers_cpp(path, cpp_package) {
        vec!["-std=c++17".to_owned(), "-x".to_owned(), "c++".to_owned()]
    } else {
        vec!["-std=c11".to_owned()]
    }
}

fn prefers_cpp(path: &Path, cpp_package: bool) -> bool {
    is_cpp_source(path) || is_cpp_header(path) || (is_c_header(path) && cpp_package)
}

fn package_prefers_cpp(root: &Path) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if is_cpp_source(&path)
                || is_cpp_header(&path)
                || (is_c_header(&path) && header_looks_like_cpp(&path))
            {
                return true;
            }
        }
    }
    false
}

fn is_cpp_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("cc" | "cpp" | "cxx" | "C" | "c++" | "mm")
    )
}

fn is_cpp_header(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("hpp" | "hxx" | "hh" | "h++" | "H")
    )
}

fn is_c_header(path: &Path) -> bool {
    matches!(path.extension().and_then(|extension| extension.to_str()), Some("h"))
}

/// C++ tokens that are not C. A comment that mentions `class` can false-trigger;
/// a header-only C library that only uses `struct` does not.
fn header_looks_like_cpp(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let head: String = text.chars().take(64 * 1024).collect();
    ["namespace ", "namespace\t", "template<", "template <", "constexpr", "nullptr"]
        .iter()
        .any(|token| head.contains(token))
        || head.contains("class ")
}
