//! Cargo package URL parsing and offline package location.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{RustAuthorityError, RustProject, RustToolchain};
use compiler_vocabulary::RustEdition;

const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// A borrowed Cargo package URL. The input remains owned by its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RustPackageUrl<'url> {
    name: &'url str,
    version: &'url str,
}

impl<'url> RustPackageUrl<'url> {
    /// Parses `cargo:<name>@<version>` without copying either component.
    pub fn parse(input: &'url str) -> Result<Self, RustPurlError<'url>> {
        let rejected = input;
        let Some(body) = input.strip_prefix("cargo:") else {
            return Err(RustPurlError::WrongScheme { purl: rejected });
        };
        let Some((name, version)) = body.rsplit_once('@') else {
            return Err(RustPurlError::MissingVersion { purl: rejected });
        };
        if name.is_empty() {
            return Err(RustPurlError::EmptyName { purl: rejected });
        }
        if version.is_empty() {
            return Err(RustPurlError::MissingVersion { purl: rejected });
        }
        if !valid_version(version) {
            return Err(RustPurlError::MalformedVersion {
                purl: rejected,
                version,
            });
        }
        Ok(Self { name, version })
    }

    /// Returns the exact borrowed package name.
    #[must_use]
    pub const fn name(self) -> &'url str {
        self.name
    }
    /// Returns the exact borrowed version spelling.
    #[must_use]
    pub const fn version(self) -> &'url str {
        self.version
    }

    /// Locates a workspace member, then the caller-selected offline registry root.
    pub fn locate(
        self,
        workspace_root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        locate_root: Option<&Path>,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<RustLocatedPackage, RustPurlError<'url>> {
        let workspace_root = workspace_root.as_ref();
        if !workspace_root.is_absolute() {
            return Err(RustPurlError::WorkspaceRoot {
                path: workspace_root.to_path_buf(),
                source: RustAuthorityError::ProjectRoot {
                    path: workspace_root.to_path_buf(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "root is not absolute"),
                },
            });
        }
        let root = RustProject::validate_root(workspace_root).map_err(|source| {
            RustPurlError::WorkspaceRoot {
                path: workspace_root.to_path_buf(),
                source,
            }
        })?;
        let metadata = metadata(&root, toolchain, cancelled)?;
        let mut candidates = Vec::new();
        for package in metadata {
            if package.name == self.name {
                candidates.push(package.version.clone());
                if package.version == self.version {
                    let package_root = package
                        .manifest
                        .parent()
                        .map(Path::to_path_buf)
                        .ok_or_else(|| RustPurlError::InvalidManifestPath {
                            path: package.manifest.clone(),
                        })?;
                    let project =
                        RustProject::open(&package_root, toolchain, RustEdition::Rust2024)
                            .map_err(|source| RustPurlError::Project {
                                path: package_root,
                                source,
                            })?;
                    return Ok(RustLocatedPackage {
                        project,
                        from_workspace: true,
                    });
                }
            }
        }
        if !candidates.is_empty() {
            return Err(RustPurlError::VersionMismatch {
                requested_name: self.name,
                requested_version: self.version,
                observed_versions: candidates,
            });
        }
        let root = locate_root
            .map(PathBuf::from)
            .unwrap_or_else(|| default_cargo_home().join("registry/src"));
        let mut matches = fs::read_dir(&root)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join(format!("{}-{}", self.name, self.version)))
            .filter(|path| path.is_dir());
        let Some(package_root) = matches.find(|_| true) else {
            return Err(RustPurlError::RegistryAbsent {
                searched_root: root,
            });
        };
        let project = RustProject::open(&package_root, toolchain, RustEdition::Rust2024).map_err(
            |source| RustPurlError::Project {
                path: package_root,
                source,
            },
        )?;
        Ok(RustLocatedPackage {
            project,
            from_workspace: false,
        })
    }
}

/// A located package with a validated source authority.
#[derive(Debug)]
pub struct RustLocatedPackage {
    project: RustProject,
    from_workspace: bool,
}

impl RustLocatedPackage {
    /// Borrows the validated project.
    #[must_use]
    pub const fn project(&self) -> &RustProject {
        &self.project
    }
    /// Whether location came from workspace metadata rather than the cache.
    #[must_use]
    pub const fn from_workspace(&self) -> bool {
        self.from_workspace
    }
}

fn valid_version(version: &str) -> bool {
    let (core, suffix) = version.split_once(['-', '+']).unwrap_or((version, ""));
    let mut parts = core.split('.');
    let valid_number = |part: Option<&str>| {
        part.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    let core_valid = valid_number(parts.next())
        && valid_number(parts.next())
        && valid_number(parts.next())
        && parts.next().is_none();
    core_valid
        && (suffix.is_empty()
            || suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
}

fn default_cargo_home() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".cargo")
        })
}

struct MetadataPackage {
    name: String,
    version: String,
    manifest: PathBuf,
}

fn metadata<'url>(
    root: &Path,
    toolchain: &RustToolchain,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Vec<MetadataPackage>, RustPurlError<'url>> {
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(RustPurlError::Cancelled);
    }
    let cargo = toolchain
        .tool
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("cargo");
    let output = Command::new(cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .map_err(RustPurlError::MetadataIo)?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(RustPurlError::Cancelled);
    }
    if output.stdout.len() > MAX_METADATA_BYTES {
        return Err(RustPurlError::MetadataTooLarge {
            bytes: output.stdout.len(),
        });
    }
    if !output.status.success() {
        return Err(RustPurlError::MetadataFailed {
            status: output.status.code(),
            stderr: output.stderr,
        });
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(RustPurlError::MetadataJson)?;
    value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or(RustPurlError::MissingPackages)?
        .iter()
        .map(|package| {
            let name = package
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(RustPurlError::MissingPackageField { field: "name" })?;
            let version = package
                .get("version")
                .and_then(serde_json::Value::as_str)
                .ok_or(RustPurlError::MissingPackageField { field: "version" })?;
            let manifest = package
                .get("manifest_path")
                .and_then(serde_json::Value::as_str)
                .ok_or(RustPurlError::MissingPackageField {
                    field: "manifest_path",
                })?;
            Ok(MetadataPackage {
                name: name.to_owned(),
                version: version.to_owned(),
                manifest: PathBuf::from(manifest),
            })
        })
        .collect()
}

/// Exact parse or location failures; rejected PURLs are retained on parse errors.
#[derive(Debug, thiserror::Error)]
pub enum RustPurlError<'url> {
    #[error("wrong Cargo PURL scheme: {purl}")]
    WrongScheme { purl: &'url str },
    #[error("Cargo PURL has no version: {purl}")]
    MissingVersion { purl: &'url str },
    #[error("Cargo PURL has an empty name: {purl}")]
    EmptyName { purl: &'url str },
    #[error("malformed Cargo version {version} in {purl}")]
    MalformedVersion { purl: &'url str, version: &'url str },
    #[error("workspace root error at {path}: {source}")]
    WorkspaceRoot {
        path: PathBuf,
        #[source]
        source: RustAuthorityError,
    },
    #[error("package {requested_name}@{requested_version} observed versions {observed_versions:?}")]
    VersionMismatch {
        requested_name: &'url str,
        requested_version: &'url str,
        observed_versions: Vec<String>,
    },
    #[error("offline registry package was not found under {searched_root}")]
    RegistryAbsent { searched_root: PathBuf },
    #[error("invalid package manifest path: {path}")]
    InvalidManifestPath { path: PathBuf },
    #[error("package project error at {path}: {source}")]
    Project {
        path: PathBuf,
        #[source]
        source: RustAuthorityError,
    },
    #[error("PURL location cancelled")]
    Cancelled,
    #[error("cargo metadata failed to start: {0}")]
    MetadataIo(#[source] std::io::Error),
    #[error("cargo metadata output exceeded bound: {bytes} bytes")]
    MetadataTooLarge { bytes: usize },
    #[error("cargo metadata failed with status {status:?}: {stderr:?}")]
    MetadataFailed {
        status: Option<i32>,
        stderr: Vec<u8>,
    },
    #[error("cargo metadata JSON is invalid: {0}")]
    MetadataJson(#[source] serde_json::Error),
    #[error("cargo metadata omitted packages")]
    MissingPackages,
    #[error("cargo metadata package omitted {field}")]
    MissingPackageField { field: &'static str },
}
