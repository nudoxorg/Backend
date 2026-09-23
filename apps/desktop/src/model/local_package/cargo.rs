//! `cargo metadata --no-deps --offline` reader.
//!
//! Cargo is the authority for workspace expansion and inherited package
//! fields. It runs as a bounded subprocess: stdin closed, stderr discarded,
//! stdout capped, and the process killed at the wall-clock bound.

use super::{
    CargoFailure, DependencyKind, Facts, LocalPackage, LocalPackageSource, dependencies, features,
    readme,
};
use crate::core::LocalProjectId;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
) -> Result<CargoMetadata, CargoFailure> {
    let mut command = Command::new(program);
    if let Some(directory) = manifest.parent() {
        command.current_dir(directory);
    }
    command
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
            "--manifest-path",
        ])
        .arg(manifest)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TERM_COLOR", "never")
        // A `rust-toolchain.toml` must not make rustup download a toolchain.
        .env("RUSTUP_AUTO_INSTALL", "0");
    let bytes = bounded_output(command, timeout, max_output)?;
    serde_json::from_slice(&bytes).map_err(|_| CargoFailure::Decode)
}

/// Runs `command` to completion, returning stdout within both bounds.
pub(super) fn bounded_output(
    mut command: Command,
    timeout: Duration,
    max_output: usize,
) -> Result<Vec<u8>, CargoFailure> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CargoFailure::Spawn)?;
    let Some(stdout) = child.stdout.take() else {
        stop(&mut child);
        return Err(CargoFailure::Spawn);
    };
    // One byte past the bound proves overflow without buffering the excess.
    let cap = u64::try_from(max_output)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let overflowed = Arc::new(AtomicBool::new(false));
    let reader_overflowed = Arc::clone(&overflowed);
    let reader = std::thread::Builder::new()
        .name("nudox-cargo-metadata".to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let read = stdout.take(cap).read_to_end(&mut bytes);
            if bytes.len() > max_output {
                reader_overflowed.store(true, Ordering::Release);
            }
            read.map(|_| bytes)
        });
    let Ok(reader) = reader else {
        stop(&mut child);
        return Err(CargoFailure::Spawn);
    };
    // On timeout or overflow the reader is detached, not joined: a
    // grandchild that inherited the pipe could otherwise hold this worker.
    let status = wait(&mut child, &overflowed, timeout)?;
    let bytes = reader
        .join()
        .map_err(|_| CargoFailure::Decode)?
        .map_err(|_| CargoFailure::Decode)?;
    if bytes.len() > max_output {
        return Err(CargoFailure::OutputLimit);
    }
    if !status.success() {
        return Err(CargoFailure::Status);
    }
    Ok(bytes)
}

/// Polls the child until it exits, the deadline passes, or the reader has
/// seen more than the output bound (a child blocked writing into a pipe
/// nobody drains would otherwise only be stopped by the deadline).
fn wait(
    child: &mut Child,
    overflowed: &AtomicBool,
    timeout: Duration,
) -> Result<ExitStatus, CargoFailure> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(_) => {
                stop(child);
                return Err(CargoFailure::Spawn);
            }
        }
        if overflowed.load(Ordering::Acquire) {
            stop(child);
            return Err(CargoFailure::OutputLimit);
        }
        if Instant::now() >= deadline {
            stop(child);
            return Err(CargoFailure::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop(child: &mut Child) {
    // Kill fails only when the child was already reaped; then there is
    // nothing left to wait for.
    if child.kill().is_ok() {
        let _reaped = child.wait();
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
    let readme = selected
        .and_then(|package| {
            let file = package.readme.as_ref()?;
            let directory = Path::new(&package.manifest_path).parent().unwrap_or(root);
            Some(readme::read(&directory.join(file)))
        })
        .unwrap_or_else(|| readme::read(&root.join("README.md")));
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
    };
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
