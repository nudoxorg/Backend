//! Unpacking a downloaded registry artifact into a package root.
//!
//! This is the Rust half of `nix build .#checks.corpus`'s `extract-to` /
//! `deepest-sole-dir` / `finalize-package` trio, and it keeps that script's one
//! hard-won rule: **the wrapper-directory strip is ecosystem-scoped, and that
//! is load-bearing rather than tidiness.**
//!
//! Quoting the reason from `fetch.nu`, because it cost a day and is not
//! rediscoverable from the code:
//!
//! > `deepest-sole-dir` cannot tell an archive *wrapper* from a real leading
//! > *package* directory — both look like "a directory with one child". For
//! > maven and nuget those leading directories ARE the package path, so
//! > stripping them silently corrupts the layout. `javax.inject-1-sources.jar`
//! > contains `javax/ -> inject/ -> 7 .java files`. Recursive stripping
//! > descended twice and produced flat files at the package root, each still
//! > declaring `package javax.inject;`. javadoc resolves `-sourcepath` by
//! > directory structure, so the artifact was present, hash-verified, and
//! > unusable.
//!
//! `jsr305` survived only because its `javax/annotation/` has several
//! subdirectories — the tell that this was never a maven-safe transformation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::packages::purl::PurlType;

// ---------------------------------------------------------------------------
// ArchiveKind
// ---------------------------------------------------------------------------

/// The container format a registry serves its source artifact in.
///
/// Decided from the ecosystem, not sniffed from the URL suffix: every registry
/// here serves exactly one shape, and a URL that ends in something unexpected
/// means the resolver built the wrong URL — a bug to surface, not a format to
/// guess at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveKind {
    /// gzip-compressed tar: `.crate`, npm `.tgz`, PyPI sdist `.tar.gz`.
    TarGz,
    /// zip family: Go module `.zip`, Maven sources `.jar`, NuGet `.nupkg`.
    Zip,
}

impl ArchiveKind {
    /// The container this ecosystem's source artifact arrives in.
    pub(crate) fn of(ty: PurlType) -> Self {
        match ty {
            PurlType::Cargo | PurlType::Npm | PurlType::PyPi => Self::TarGz,
            PurlType::Golang | PurlType::Maven | PurlType::NuGet => Self::Zip,
        }
    }
}

/// Whether this ecosystem's archive wraps its contents in nested single-child
/// directories that must be stripped to reach the package root.
///
/// `nix build .#checks.corpus`'s `WRAPPED_ECOSYSTEMS`, transcribed. `true` for the
/// ecosystems whose archives carry a `<name>-<version>/` (or, for Go, a whole
/// `<module path>@<version>/`) wrapper; `false` for maven and nuget, whose
/// leading directories are the *package path* and whose stripping was the
/// `javax.inject` bug.
fn is_wrapped(ty: PurlType) -> bool {
    match ty {
        PurlType::Cargo | PurlType::Npm | PurlType::PyPi | PurlType::Golang => true,
        PurlType::Maven | PurlType::NuGet => false,
    }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Everything that can go wrong turning downloaded bytes into a directory.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("the archive is not a valid {kind:?}: {detail}")]
    Malformed {
        kind: ArchiveKind,
        detail: String,
    },
    /// An entry's path escaped the extraction directory.
    ///
    /// Registry artifacts are third-party bytes. A `../../../.ssh/authorized_keys`
    /// member in a tarball is the oldest trick there is, and "we trust
    /// crates.io" is not a property of the *bytes*, only of the *host* — the
    /// two are the same claim only when a digest was verified, which is not
    /// true for every ecosystem here (see `Integrity::TransportOnly`).
    #[error("archive member {member:?} escapes the extraction directory; refusing to unpack it")]
    PathEscape { member: String },
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> Error {
    let context = context.into();
    move |source| Error::Io { context, source }
}

/// Unpack `bytes` and leave the package root at `dest`.
///
/// `dest` must not exist; it is created. `scratch` is a directory the caller
/// owns and may hold intermediate output.
pub(crate) fn unpack(
    bytes: &[u8],
    ty: PurlType,
    scratch: &Path,
    dest: &Path,
) -> Result<(), Error> {
    let staging = scratch.join("extracted");
    fs::create_dir_all(&staging).map_err(io_err(format!("creating {}", staging.display())))?;

    match ArchiveKind::of(ty) {
        ArchiveKind::TarGz => unpack_tar_gz(bytes, &staging)?,
        ArchiveKind::Zip => unpack_zip(bytes, &staging)?,
    }

    // The ecosystem-scoped strip. See the module docs for why this is not
    // "descend while there is one child" for every ecosystem.
    let source = if is_wrapped(ty) {
        deepest_sole_dir(&staging)?
    } else {
        staging.clone()
    };

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(io_err(format!("creating {}", parent.display())))?;
    }
    // `rename` within one filesystem is atomic, so a concurrent reader either
    // sees no package root or a complete one — never a half-extracted tree
    // that a producer would happily lower into a partial package.
    fs::rename(&source, dest).map_err(io_err(format!(
        "moving {} to {}",
        source.display(),
        dest.display()
    )))?;

    Ok(())
}

fn unpack_tar_gz(bytes: &[u8], dest: &Path) -> Result<(), Error> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    // `set_overwrite(false)` is not enough on its own — `tar` resolves `..`
    // itself only when `set_preserve_permissions`/`unpack_in` are used, so the
    // check below is explicit rather than delegated.
    let entries = archive
        .entries()
        .map_err(|e| Error::Malformed {
            kind: ArchiveKind::TarGz,
            detail: e.to_string(),
        })?;

    for entry in entries {
        let mut entry = entry.map_err(|e| Error::Malformed {
            kind: ArchiveKind::TarGz,
            detail: e.to_string(),
        })?;
        let path = entry
            .path()
            .map_err(|e| Error::Malformed {
                kind: ArchiveKind::TarGz,
                detail: e.to_string(),
            })?
            .into_owned();
        let target = safe_join(dest, &path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(io_err(format!("creating {}", parent.display())))?;
        }
        // `unpack` handles directories, regular files and (on unix) symlinks.
        // A symlink whose *target* escapes is harmless here because nothing
        // follows it: the producer reads files under `dest`, and a dangling or
        // escaping symlink yields a read error, not a read of the target.
        entry
            .unpack(&target)
            .map_err(io_err(format!("unpacking {}", target.display())))?;
    }
    Ok(())
}

fn unpack_zip(bytes: &[u8], dest: &Path) -> Result<(), Error> {
    let reader = io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| Error::Malformed {
        kind: ArchiveKind::Zip,
        detail: e.to_string(),
    })?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| Error::Malformed {
            kind: ArchiveKind::Zip,
            detail: e.to_string(),
        })?;
        // `enclosed_name` is `zip`'s own path-escape check; `None` means the
        // member name is not safe to join. Handling it as an error rather than
        // skipping the member is deliberate: a package with a member we
        // refused to write is not the package the registry serves, and
        // producing IR from it would describe an API that does not exist.
        let Some(relative) = file.enclosed_name() else {
            return Err(Error::PathEscape {
                member: file.name().to_owned(),
            });
        };
        let target = safe_join(dest, &relative)?;
        if file.is_dir() {
            fs::create_dir_all(&target)
                .map_err(io_err(format!("creating {}", target.display())))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(io_err(format!("creating {}", parent.display())))?;
        }
        let mut out = fs::File::create(&target)
            .map_err(io_err(format!("creating {}", target.display())))?;
        io::copy(&mut file, &mut out)
            .map_err(io_err(format!("writing {}", target.display())))?;
    }
    Ok(())
}

/// Join `relative` under `root`, refusing anything that escapes.
fn safe_join(root: &Path, relative: &Path) -> Result<PathBuf, Error> {
    use std::path::Component;

    let mut out = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::PathEscape {
                    member: relative.display().to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Descend through directories that contain nothing but a single subdirectory.
///
/// Recursive, not one level: a Go module zip nests the whole archive as
/// `<module path>@<version>/`, and a module path itself has path separators, so
/// `github.com/pkg/errors@v0.9.1/errors.go` unpacks as several
/// one-entry directories in a row.
fn deepest_sole_dir(dir: &Path) -> Result<PathBuf, Error> {
    let mut current = dir.to_path_buf();
    loop {
        let mut entries = fs::read_dir(&current)
            .map_err(io_err(format!("reading {}", current.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_err(format!("reading {}", current.display())))?;
        if entries.len() != 1 {
            return Ok(current);
        }
        let only = entries.pop().expect("length was checked to be 1");
        let file_type = only
            .file_type()
            .map_err(io_err(format!("stat {}", only.path().display())))?;
        if !file_type.is_dir() {
            return Ok(current);
        }
        current = only.path();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The `javax.inject` regression, stated as an invariant rather than as a
    /// story: maven and nuget archives must reach the producer with their
    /// leading directories intact, because those directories *are* the package
    /// path that javadoc's `-sourcepath` resolves by.
    #[test]
    fn maven_and_nuget_archives_are_never_wrapper_stripped() {
        assert!(!is_wrapped(PurlType::Maven));
        assert!(!is_wrapped(PurlType::NuGet));
    }

    /// Everything else wraps its contents in at least one directory that is not
    /// part of the package's own layout.
    #[test]
    fn the_source_tarball_ecosystems_are_wrapper_stripped() {
        for ty in [
            PurlType::Cargo,
            PurlType::Npm,
            PurlType::PyPi,
            PurlType::Golang,
        ] {
            assert!(is_wrapped(ty), "{ty} archives carry a wrapper directory");
        }
    }

    #[test]
    fn every_ecosystem_declares_exactly_one_container_format() {
        assert_eq!(ArchiveKind::of(PurlType::Cargo), ArchiveKind::TarGz);
        assert_eq!(ArchiveKind::of(PurlType::Npm), ArchiveKind::TarGz);
        assert_eq!(ArchiveKind::of(PurlType::PyPi), ArchiveKind::TarGz);
        assert_eq!(ArchiveKind::of(PurlType::Golang), ArchiveKind::Zip);
        assert_eq!(ArchiveKind::of(PurlType::Maven), ArchiveKind::Zip);
        assert_eq!(ArchiveKind::of(PurlType::NuGet), ArchiveKind::Zip);
    }

    #[test]
    fn a_traversing_member_name_is_refused_rather_than_clamped() {
        let root = Path::new("/tmp/nudox-test-root");
        let err = safe_join(root, Path::new("../../etc/passwd"))
            .expect_err("a traversing member must not resolve");
        assert!(matches!(err, Error::PathEscape { .. }));
        // And an absolute member, which is the same attack without the dots.
        assert!(matches!(
            safe_join(root, Path::new("/etc/passwd")),
            Err(Error::PathEscape { .. })
        ));
    }

    #[test]
    fn an_ordinary_nested_member_joins_under_the_root() {
        let root = Path::new("/tmp/nudox-test-root");
        assert_eq!(
            safe_join(root, Path::new("src/./lib.rs")).unwrap(),
            root.join("src").join("lib.rs"),
        );
    }
}
