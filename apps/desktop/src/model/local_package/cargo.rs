//! `cargo metadata --no-deps --offline` reader.
//!
//! Cargo is the authority for workspace expansion and inherited package
//! fields. It runs through the platform's bounded child capture: stdin
//! closed, stdout and stderr capped, the whole process tree (a Unix process
//! group, a Windows Job) retired at the wall-clock bound or on cancellation.

use super::{
    CargoFailure, DependencyKind, Facts, LocalPackage, LocalPackageSource, dependencies, features,
    readme,
};
use crate::core::LocalProjectId;
use backend_platform::child_output::{
    self, CaptureCommand, CaptureEnvironment, CaptureError, CaptureLimits,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Cargo's diagnostics are read only to keep its stderr pipe drained; the
/// metadata document is on stdout.
const STDERR_BYTES: usize = 64 * 1024;

/// The stable subset of `cargo metadata --format-version 1` the dossier uses.
#[derive(Deserialize)]
pub(super) struct CargoMetadata {
    pub(super) packages: Vec<CargoPackage>,
    pub(super) workspace_members: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct CargoPackage {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) description: Option<String>,
    pub(super) license: Option<String>,
    pub(super) repository: Option<String>,
    pub(super) homepage: Option<String>,
    pub(super) documentation: Option<String>,
    #[serde(default)]
    pub(super) keywords: Vec<String>,
    #[serde(default)]
    pub(super) categories: Vec<String>,
    pub(super) readme: Option<String>,
    pub(super) rust_version: Option<String>,
    pub(super) manifest_path: String,
    #[serde(default)]
    pub(super) dependencies: Vec<CargoDependency>,
    #[serde(default)]
    pub(super) features: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct CargoDependency {
    /// Resolved package name; the manifest alias is in `rename`.
    pub(super) name: String,
    pub(super) req: String,
    pub(super) kind: Option<String>,
    #[serde(default)]
    pub(super) rename: Option<String>,
    #[serde(default)]
    pub(super) optional: bool,
    #[serde(default = "default_true")]
    pub(super) uses_default_features: bool,
    #[serde(default)]
    pub(super) features: Vec<String>,
    #[serde(default)]
    pub(super) target: Option<String>,
    #[serde(default)]
    pub(super) source: Option<String>,
    #[serde(default)]
    pub(super) registry: Option<String>,
    #[serde(default)]
    pub(super) path: Option<String>,
}

const fn default_true() -> bool {
    true
}

/// Runs Cargo on one root manifest and decodes its metadata document.
pub(super) fn metadata(
    program: &Path,
    manifest: &Path,
    timeout: Duration,
    max_output: usize,
    cancelled: &AtomicBool,
) -> Result<CargoMetadata, CargoFailure> {
    let args = ["metadata", "--no-deps", "--format-version", "1", "--offline", "--manifest-path"]
        .into_iter()
        .map(OsString::from)
        .chain([manifest.as_os_str().to_os_string()])
        .collect();
    let command = CaptureCommand {
        program: program.as_os_str().to_os_string(),
        args,
        cwd: manifest.parent().map(Path::to_path_buf),
        environment: CaptureEnvironment::Inherit,
        overrides: [
            ("CARGO_NET_OFFLINE", "true"),
            ("CARGO_TERM_COLOR", "never"),
            // A `rust-toolchain.toml` must not make rustup download a toolchain.
            ("RUSTUP_AUTO_INSTALL", "0"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), Some(OsString::from(value))))
        .collect(),
    };
    let bytes = bounded_output_cancelled(&command, timeout, max_output, cancelled)?;
    serde_json::from_slice(&bytes).map_err(|_| CargoFailure::Decode)
}

/// Runs `command` to completion, returning stdout within both bounds.
pub(super) fn bounded_output(
    command: &CaptureCommand,
    timeout: Duration,
    max_output: usize,
) -> Result<Vec<u8>, CargoFailure> {
    bounded_output_cancelled(command, timeout, max_output, &AtomicBool::new(false))
}

/// [`bounded_output`] that also stops when `cancelled` is set.
pub(super) fn bounded_output_cancelled(
    command: &CaptureCommand,
    timeout: Duration,
    max_output: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, CargoFailure> {
    if cancelled.load(Ordering::Acquire) {
        return Err(CargoFailure::Cancelled);
    }
    // An arbitrary public timeout must never panic after the child starts.
    // Reject an unrepresentable deadline before creating a process or pipe.
    let deadline = Instant::now().checked_add(timeout).ok_or(CargoFailure::Timeout)?;
    let limits = CaptureLimits { deadline, stdout_bytes: max_output, stderr_bytes: STDERR_BYTES };
    let output = child_output::capture(command, limits, cancelled).map_err(CargoFailure::from)?;
    if output.status.success() { Ok(output.stdout) } else { Err(CargoFailure::Status) }
}

impl From<CaptureError> for CargoFailure {
    /// The loader's view of a capture failure. Cleanup keeps its primary cause.
    fn from(error: CaptureError) -> Self {
        match error {
            CaptureError::Unsupported => Self::UnsupportedCapture,
            CaptureError::Capacity { .. } => Self::Busy,
            CaptureError::Cancelled => Self::Cancelled,
            CaptureError::Deadline => Self::Timeout,
            CaptureError::OutputLimit { .. } => Self::OutputLimit,
            CaptureError::Spawn(_)
            | CaptureError::MissingPipe(_)
            | CaptureError::Configure { .. }
            | CaptureError::Wait(_) => Self::Spawn,
            CaptureError::Read { .. } => Self::Decode,
            CaptureError::Cleanup { primary, .. } => Self::from(*primary),
        }
    }
}

/// Projects Cargo's workspace metadata into local package facts.
pub(super) fn project(
    project: LocalProjectId,
    root: &Path,
    metadata: CargoMetadata,
) -> LocalPackage {
    let CargoMetadata {
        packages,
        workspace_members,
    } = metadata;
    let mut members = packages
        .into_iter()
        .filter(|package| workspace_members.contains(&package.id))
        .collect::<Vec<_>>();
    members.sort_by(|left, right| left.manifest_path.cmp(&right.manifest_path));
    let selected = root_package(root, &members);
    let selected_manifest = selected.map(|package| package.manifest_path.clone());
    let readme_path = selected
        .and_then(|package| {
            let file = package.readme.as_ref()?;
            let directory = Path::new(&package.manifest_path).parent().unwrap_or(root);
            Some(directory.join(file))
        })
        .unwrap_or_else(|| root.join("README.md"));
    let readme = readme::read(root, &readme_path);
    let facts = Facts {
        name: selected.map(|package| package.name.clone()),
        version: selected.map(|package| package.version.clone()),
        description: selected.and_then(|package| package.description.clone()),
        license: selected.and_then(|package| package.license.clone()),
        rust_version: selected.and_then(|package| package.rust_version.clone()),
        repository: selected.and_then(|package| package.repository.clone()),
        homepage: selected.and_then(|package| package.homepage.clone()),
        documentation: selected.and_then(|package| package.documentation.clone()),
        keywords: selected.map_or_else(Vec::new, |package| package.keywords.clone()),
        categories: selected.map_or_else(Vec::new, |package| package.categories.clone()),
        readme,
        readme_path: Some(readme_path),
    };
    let members = scope_to_member(root, members, selected_manifest.as_deref());
    let dependencies = dependencies(members.iter().flat_map(|package| {
        package.dependencies.iter().map(|dependency| {
            let name = dependency
                .rename
                .as_ref()
                .unwrap_or(&dependency.name)
                .clone();
            (
                kind(dependency.kind.as_deref()),
                name,
                requirement(dependency),
            )
        })
    }));
    let features = features(members.iter().flat_map(|package| {
        package
            .features
            .iter()
            .map(|(name, values)| (name.clone(), values.clone()))
    }));
    let count = members.len();
    facts.into_package(
        project,
        LocalPackageSource::Cargo,
        dependencies,
        features,
        count,
    )
}

/// Keeps only the selected package when the project folder is one member of
/// a larger Cargo workspace: its dossier must not list every sibling's
/// dependencies. A folder that is the workspace root keeps every member.
fn scope_to_member(
    root: &Path,
    mut members: Vec<CargoPackage>,
    selected: Option<&str>,
) -> Vec<CargoPackage> {
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let enclosed = |package: &CargoPackage| {
        let manifest = Path::new(&package.manifest_path);
        std::fs::canonicalize(manifest)
            .unwrap_or_else(|_| manifest.to_path_buf())
            .starts_with(&canonical_root)
    };
    if let Some(selected) = selected
        && !members.iter().all(enclosed)
    {
        members.retain(|package| package.manifest_path == selected);
    }
    members
}

/// Finds the member whose manifest is the project root's `Cargo.toml`.
///
/// Cargo reports canonical absolute paths while the project may have been
/// admitted through a symlink, so both sides are canonicalized.
fn root_package<'members>(
    root: &Path,
    members: &'members [CargoPackage],
) -> Option<&'members CargoPackage> {
    let manifest = root.join("Cargo.toml");
    let manifest = std::fs::canonicalize(&manifest).unwrap_or(manifest);
    members.iter().find(|package| {
        let path = Path::new(&package.manifest_path);
        std::fs::canonicalize(path).map_or_else(|_| path == manifest, |path| path == manifest)
    })
}

fn kind(kind: Option<&str>) -> DependencyKind {
    match kind {
        Some("dev") => DependencyKind::Development,
        Some("build") => DependencyKind::Build,
        _ => DependencyKind::Normal,
    }
}

/// Spells the requirement plus every resolution dimension Cargo reported.
pub(super) fn requirement(dependency: &CargoDependency) -> String {
    let mut details = Vec::new();
    if dependency.req != "*" {
        details.push(dependency.req.clone());
    }
    if let Some(path) = &dependency.path {
        details.push(format!("path: {path}"));
    } else if let Some(source) = &dependency.source {
        if source.starts_with("registry+") || source.starts_with("sparse+") {
            details.push(dependency.registry.as_deref().map_or_else(
                || "registry".to_owned(),
                |registry| format!("registry: {registry}"),
            ));
        } else if let Some(git) = source.strip_prefix("git+") {
            details.push(format!("git: {git}"));
        } else {
            details.push(source.clone());
        }
    }
    if dependency.rename.is_some() {
        // The alias is the displayed key; keep the real package visible.
        details.push(format!("package: {}", dependency.name));
    }
    if dependency.optional {
        details.push("optional".to_owned());
    }
    if !dependency.uses_default_features {
        details.push("default-features: false".to_owned());
    }
    if !dependency.features.is_empty() {
        details.push(format!("features: {}", dependency.features.join(", ")));
    }
    if let Some(target) = &dependency.target {
        details.push(format!("target: {target}"));
    }
    if details.is_empty() {
        "workspace".to_owned()
    } else {
        details.join(" · ")
    }
}
