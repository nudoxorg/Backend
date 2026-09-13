//! Cargo package URL parsing and offline package location.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{RustAuthorityError, RustProject, RustToolchain};
use backend_compile::RustEdition;

const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// A borrowed Cargo package URL. The input remains owned by its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RustPackageUrl<'url> {
    name: &'url str,
    version: &'url str,
}

impl<'url> RustPackageUrl<'url> {
    /// Parses `cargo:<name>@<version>` without copying either component.
    ///
    /// # Errors
    ///
    /// Returns [`RustPurlError`] when the scheme, name, or version is malformed.
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
    ///
    /// # Errors
    ///
    /// Returns [`RustPurlError`] when metadata, package validation, or offline
    /// registry lookup fails.
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
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "root is not absolute",
                    ),
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
                    let source_path =
                        package
                            .source_path
                            .ok_or(RustPurlError::MissingPackageField {
                                field: "lib target",
                            })?;
                    let project = RustProject::open_with_source(
                        &package_root,
                        source_path,
                        toolchain,
                        package.edition,
                    )
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
        let root =
            locate_root.map_or_else(|| default_cargo_home().join("registry/src"), PathBuf::from);
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
        let edition = registry_edition(&package_root)?;
        let source_path = registry_source_path(&package_root)?;
        let project = RustProject::open_with_source(&package_root, source_path, toolchain, edition)
            .map_err(|source| RustPurlError::Project {
                path: package_root,
                source,
            })?;
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
    std::env::var_os("CARGO_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".cargo")
        },
        PathBuf::from,
    )
}

struct MetadataPackage {
    name: String,
    version: String,
    manifest: PathBuf,
    edition: RustEdition,
    source_path: Option<PathBuf>,
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
            let edition = match package.get("edition") {
                None => RustEdition::Rust2015,
                Some(value) => match value.as_str() {
                    Some(spelling) => parse_edition(spelling)?,
                    None => {
                        return Err(RustPurlError::UnknownEdition {
                            spelling: value.to_string(),
                        });
                    }
                },
            };
            let source_path = package
                .get("targets")
                .and_then(serde_json::Value::as_array)
                .and_then(|targets| {
                    targets.iter().find_map(|target| {
                        let is_lib = target
                            .get("kind")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|kinds| {
                                kinds.iter().any(|kind| kind.as_str() == Some("lib"))
                            });
                        is_lib
                            .then(|| target.get("src_path"))
                            .flatten()
                            .and_then(serde_json::Value::as_str)
                            .map(PathBuf::from)
                    })
                });
            Ok(MetadataPackage {
                name: name.to_owned(),
                version: version.to_owned(),
                manifest: PathBuf::from(manifest),
                edition,
                source_path,
            })
        })
        .collect()
}

fn registry_source_path<'url>(package_root: &Path) -> Result<PathBuf, RustPurlError<'url>> {
    let manifest = package_root.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest).map_err(|source| RustPurlError::ManifestIo {
        path: manifest.clone(),
        source,
    })?;
    let mut in_lib = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_lib = line == "[lib]";
            continue;
        }
        if !in_lib {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "path" {
            continue;
        }
        let value = value.trim();
        let Some(path) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            return Err(RustPurlError::UnknownEdition {
                spelling: value.to_owned(),
            });
        };
        return Ok(package_root.join(path));
    }
    Ok(package_root.join("src/lib.rs"))
}

fn parse_edition<'url>(spelling: &str) -> Result<RustEdition, RustPurlError<'url>> {
    match spelling {
        "2015" => Ok(RustEdition::Rust2015),
        "2018" => Ok(RustEdition::Rust2018),
        "2021" => Ok(RustEdition::Rust2021),
        "2024" => Ok(RustEdition::Rust2024),
        _ => Err(RustPurlError::UnknownEdition {
            spelling: spelling.to_owned(),
        }),
    }
}

fn registry_edition<'url>(package_root: &Path) -> Result<RustEdition, RustPurlError<'url>> {
    let manifest = package_root.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest).map_err(|source| RustPurlError::ManifestIo {
        path: manifest.clone(),
        source,
    })?;
    let mut in_package = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key != "edition" && key != "edition.workspace" {
            continue;
        }
        let value = value.trim();
        let Some(spelling) = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        else {
            return Err(RustPurlError::UnknownEdition {
                spelling: value.to_owned(),
            });
        };
        return parse_edition(spelling);
    }
    Ok(RustEdition::Rust2015)
}

/// Exact parse or location failures; rejected PURLs are retained on parse errors.
#[derive(Debug, thiserror::Error)]
pub enum RustPurlError<'url> {
    /// The input did not use the `cargo:` scheme.
    #[error("wrong Cargo PURL scheme: {purl}")]
    WrongScheme {
        /// Rejected input spelling.
        purl: &'url str,
    },
    /// The input omitted the `@version` component.
    #[error("Cargo PURL has no version: {purl}")]
    MissingVersion {
        /// Rejected input spelling.
        purl: &'url str,
    },
    /// The package name component was empty.
    #[error("Cargo PURL has an empty name: {purl}")]
    EmptyName {
        /// Rejected input spelling.
        purl: &'url str,
    },
    /// The version did not satisfy the admitted Cargo version grammar.
    #[error("malformed Cargo version {version} in {purl}")]
    MalformedVersion {
        /// Rejected input spelling.
        purl: &'url str,
        /// Rejected version component.
        version: &'url str,
    },
    /// The workspace root could not be validated.
    #[error("workspace root error at {path}: {source}")]
    WorkspaceRoot {
        /// Workspace path used for metadata lookup.
        path: PathBuf,
        #[source]
        /// Original project validation failure.
        source: RustAuthorityError,
    },
    /// The requested package name exists, but under different versions.
    #[error("package {requested_name}@{requested_version} observed versions {observed_versions:?}")]
    VersionMismatch {
        /// Name supplied by the caller.
        requested_name: &'url str,
        /// Version supplied by the caller.
        requested_version: &'url str,
        /// Versions observed in the workspace metadata.
        observed_versions: Vec<String>,
    },
    /// No matching package was present in the offline registry cache.
    #[error("offline registry package was not found under {searched_root}")]
    RegistryAbsent {
        /// Registry root searched by the locator.
        searched_root: PathBuf,
    },
    /// A metadata package contained a malformed manifest path.
    #[error("invalid package manifest path: {path}")]
    InvalidManifestPath {
        /// Malformed manifest path.
        path: PathBuf,
    },
    /// The selected package could not be opened as a Rust project.
    #[error("package project error at {path}: {source}")]
    Project {
        /// Package root that failed to open.
        path: PathBuf,
        #[source]
        /// Original project validation failure.
        source: RustAuthorityError,
    },
    /// The caller cancelled metadata or registry access.
    #[error("PURL location cancelled")]
    Cancelled,
    /// Cargo metadata could not be started.
    #[error("cargo metadata failed to start: {0}")]
    MetadataIo(#[source] std::io::Error),
    /// Cargo metadata output exceeded the bounded buffer.
    #[error("cargo metadata output exceeded bound: {bytes} bytes")]
    MetadataTooLarge {
        /// Number of bytes produced by Cargo.
        bytes: usize,
    },
    /// Cargo metadata exited unsuccessfully.
    #[error("cargo metadata failed with status {status:?}: {stderr:?}")]
    MetadataFailed {
        /// Exit status returned by Cargo, when it had one.
        status: Option<i32>,
        /// Bounded Cargo stderr bytes.
        stderr: Vec<u8>,
    },
    /// Cargo emitted invalid JSON metadata.
    #[error("cargo metadata JSON is invalid: {0}")]
    MetadataJson(#[source] serde_json::Error),
    /// Cargo metadata contained no packages.
    #[error("cargo metadata omitted packages")]
    MissingPackages,
    /// A package omitted a required metadata field.
    #[error("cargo metadata package omitted {field}")]
    MissingPackageField {
        /// Name of the omitted metadata field.
        field: &'static str,
    },
    /// A package declared an edition this authority does not understand.
    #[error("package declares unsupported Rust edition {spelling}")]
    UnknownEdition {
        /// Edition spelling emitted by Cargo metadata or the manifest.
        spelling: String,
    },
    /// The package manifest could not be read.
    #[error("cannot read package manifest at {path}: {source}")]
    ManifestIo {
        /// Manifest path that failed to read.
        path: PathBuf,
        #[source]
        /// Original filesystem failure.
        source: std::io::Error,
    },
}
