//! Offline package facts for a local project, read from its own manifests.
//!
//! A registry package gets its dossier from a live producer round-trip. A
//! local project has no registry record, yet its own `Cargo.toml` already
//! states its name, license, dependencies, features, and README. This module
//! is the read-model source for those facts. It never touches the network:
//! Cargo is asked for `metadata --no-deps --offline` under a time and output
//! bound, and a hand-written manifest reader is the fallback when Cargo is
//! absent, slow, or refuses the workspace.
//!
//! Loading performs filesystem and subprocess work, so it runs only on the
//! runtime's local-read worker (see `runtime::actor`), never on the UI thread.
//! The loaded value is immutable and enters [`crate::model::AppSnapshot`]
//! through the ordinary engine-event mapping.

mod cargo;
mod manifest;
mod readme;
#[cfg(test)]
mod tests;

use crate::core::LocalProjectId;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use readme::ReadmeBlock;

/// Every package fact the local dossier renders.
///
/// Optional facts stay `None` when the manifest does not state them; the view
/// decides how to spell absence and never receives a fabricated placeholder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalPackage {
    /// Local project the facts were read from.
    pub project: LocalProjectId,
    /// Which reader produced the facts.
    pub source: LocalPackageSource,
    /// Root package name, else README title, else the folder name.
    pub name: Arc<str>,
    /// Root package version, including a `version.workspace` inheritance.
    pub version: Option<Arc<str>>,
    /// Manifest description, else the README's first paragraph.
    pub description: Option<Arc<str>>,
    /// SPDX license expression.
    pub license: Option<Arc<str>>,
    /// Minimum supported Rust version.
    pub rust_version: Option<Arc<str>>,
    /// Manifest repository, else the `origin` remote from `.git/config`.
    pub repository: Option<Arc<str>>,
    /// Manifest homepage.
    pub homepage: Option<Arc<str>>,
    /// Manifest documentation link.
    pub documentation: Option<Arc<str>>,
    /// Manifest keywords, in manifest order.
    pub keywords: Arc<[Arc<str>]>,
    /// Manifest categories, in manifest order.
    pub categories: Arc<[Arc<str>]>,
    /// README projected into structural blocks.
    pub readme: Arc<[ReadmeBlock]>,
    /// Unique dependency requirements across workspace members.
    pub dependencies: Arc<[LocalDependency]>,
    /// Unique feature definitions across workspace members.
    pub features: Arc<[LocalFeature]>,
    /// Number of workspace member packages that contributed facts.
    pub members: usize,
}

/// The reader that produced a [`LocalPackage`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalPackageSource {
    /// Cargo resolved the workspace and its inherited fields.
    Cargo,
    /// Cargo could not answer; facts come from reading `Cargo.toml` files.
    Manifest(CargoFailure),
    /// The project root has no readable Cargo manifest.
    NoManifest,
}

/// Why the Cargo reader did not produce the facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CargoFailure {
    /// The loader was configured without a Cargo program.
    Disabled,
    /// The Cargo program could not be started.
    Spawn,
    /// Cargo did not finish within the loader's time bound.
    Timeout,
    /// Cargo wrote more than the loader's output bound.
    OutputLimit,
    /// Cargo exited unsuccessfully (no manifest, invalid manifest, …).
    Status,
    /// Cargo's output was not the expected metadata document.
    Decode,
}

/// One dependency requirement declared by one or more workspace members.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalDependency {
    /// Manifest key: the rename when one is declared, else the package name.
    pub name: Arc<str>,
    /// Requirement and resolution detail, e.g. `^1.0 · registry · optional`.
    pub requirement: Arc<str>,
    /// Dependency table the requirement was declared in.
    pub kind: DependencyKind,
    /// Number of workspace members declaring this exact requirement.
    pub users: usize,
}

/// Cargo dependency table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DependencyKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Development,
    /// `[build-dependencies]`.
    Build,
}

impl DependencyKind {
    /// Returns Cargo's own spelling for this table.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Development => "dev",
            Self::Build => "build",
        }
    }
}

/// One feature declared by one or more workspace members.
///
/// Feature values are a graph of optional-dependency and feature references;
/// keeping them lets the dossier explain why an optional dependency exists
/// without reparsing a manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFeature {
    /// Feature name.
    pub name: Arc<str>,
    /// Features and `dep:` references this feature enables.
    pub members: Arc<[Arc<str>]>,
    /// Number of workspace members declaring this exact feature.
    pub users: usize,
}

/// Bounded, offline loader for [`LocalPackage`] facts.
#[derive(Clone, Debug)]
pub struct LocalPackageLoader {
    cargo: Option<PathBuf>,
    timeout: Duration,
    max_output: usize,
}

impl Default for LocalPackageLoader {
    /// Uses the Cargo that launched this process when there is one (so a
    /// `cargo run` desktop agrees with its toolchain), else `cargo` on PATH.
    fn default() -> Self {
        let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
        Self {
            cargo: Some(cargo),
            timeout: Duration::from_secs(10),
            max_output: 32 * 1024 * 1024,
        }
    }
}

impl LocalPackageLoader {
    /// Returns a loader that reads manifests without running Cargo.
    #[must_use]
    pub fn without_cargo() -> Self {
        Self {
            cargo: None,
            ..Self::default()
        }
    }

    /// Replaces the Cargo program.
    #[must_use]
    pub fn with_cargo(mut self, program: impl Into<PathBuf>) -> Self {
        self.cargo = Some(program.into());
        self
    }

    /// Replaces the wall-clock bound on the Cargo subprocess.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replaces the byte bound on Cargo's standard output.
    #[must_use]
    pub const fn with_max_output(mut self, bytes: usize) -> Self {
        self.max_output = bytes;
        self
    }

    /// Loads the facts for one local project.
    ///
    /// This blocks for at most the configured Cargo bound plus manifest
    /// reads; call it from a worker thread.
    #[must_use]
    pub fn load(&self, project: &LocalProjectId) -> LocalPackage {
        let root = project.path();
        let manifest = root.join("Cargo.toml");
        // Without a root manifest Cargo would search parent directories and
        // could describe an unrelated enclosing workspace.
        if !manifest.is_file() {
            return manifest::project(project.clone(), &root, CargoFailure::Status);
        }
        let failure = match self.cargo.as_deref() {
            None => CargoFailure::Disabled,
            Some(program) => {
                match cargo::metadata(program, &manifest, self.timeout, self.max_output) {
                    Ok(metadata) => return cargo::project(project.clone(), &root, metadata),
                    Err(failure) => failure,
                }
            }
        };
        manifest::project(project.clone(), &root, failure)
    }
}

/// Facts shared by both readers once the package fields are known.
struct Facts {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    license: Option<String>,
    rust_version: Option<String>,
    repository: Option<String>,
    homepage: Option<String>,
    documentation: Option<String>,
    keywords: Vec<String>,
    categories: Vec<String>,
    readme: String,
}

impl Facts {
    fn into_package(
        self,
        project: LocalProjectId,
        source: LocalPackageSource,
        dependencies: Vec<LocalDependency>,
        features: Vec<LocalFeature>,
        members: usize,
    ) -> LocalPackage {
        let root = project.path();
        let name = self
            .name
            .or_else(|| readme::title(&self.readme))
            .unwrap_or_else(|| folder_name(&root));
        let description = present(self.description).or_else(|| {
            let paragraph = readme::first_paragraph(&self.readme);
            (!paragraph.is_empty()).then_some(paragraph)
        });
        LocalPackage {
            source,
            name: Arc::from(name),
            version: present(self.version).map(Arc::from),
            description: description.map(Arc::from),
            license: present(self.license).map(Arc::from),
            rust_version: present(self.rust_version).map(Arc::from),
            repository: present(self.repository)
                .or_else(|| discover_repository(&root))
                .map(Arc::from),
            homepage: present(self.homepage).map(Arc::from),
            documentation: present(self.documentation).map(Arc::from),
            keywords: shared(self.keywords),
            categories: shared(self.categories),
            readme: readme::parse(&self.readme).into(),
            dependencies: dependencies.into(),
            features: features.into(),
            members,
            project,
        }
    }
}

fn present(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn shared(values: Vec<String>) -> Arc<[Arc<str>]> {
    values.into_iter().map(Arc::from).collect()
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map_or_else(|| "workspace".to_owned(), ToOwned::to_owned)
}

/// Resolves the forge URL from the checkout's `origin` remote without
/// contacting the forge, so an unmanifested repository still links home.
fn discover_repository(project: &Path) -> Option<String> {
    let git = project.join(".git");
    let config = if git.is_dir() {
        git.join("config")
    } else {
        // A worktree or submodule has a `.git` file naming its real gitdir.
        let gitfile = std::fs::read_to_string(&git).ok()?;
        let gitdir = gitfile.trim().strip_prefix("gitdir:")?.trim();
        let gitdir = project.join(gitdir);
        let common = std::fs::read_to_string(gitdir.join("commondir"))
            .ok()
            .map(|common| gitdir.join(common.trim()));
        common.unwrap_or(gitdir).join("config")
    };
    origin_url(&std::fs::read_to_string(config).ok()?)
}

fn origin_url(config: &str) -> Option<String> {
    let mut origin = false;
    for line in config.lines().map(str::trim) {
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            origin = section.trim() == "remote \"origin\"";
        } else if origin {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "url" && !value.trim().is_empty() {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

/// Counts identical keys across workspace members, ordered by key.
fn tally<Key: Ord>(keys: impl IntoIterator<Item = Key>) -> Vec<(Key, usize)> {
    let mut counts = std::collections::BTreeMap::<Key, usize>::new();
    for key in keys {
        *counts.entry(key).or_default() += 1;
    }
    counts.into_iter().collect()
}

fn dependencies(
    keys: impl IntoIterator<Item = (DependencyKind, String, String)>,
) -> Vec<LocalDependency> {
    tally(keys)
        .into_iter()
        .map(|((kind, name, requirement), users)| LocalDependency {
            name: Arc::from(name),
            requirement: Arc::from(requirement),
            kind,
            users,
        })
        .collect()
}

fn features(keys: impl IntoIterator<Item = (String, Vec<String>)>) -> Vec<LocalFeature> {
    tally(keys)
        .into_iter()
        .map(|((name, members), users)| LocalFeature {
            name: Arc::from(name),
            members: shared(members),
            users,
        })
        .collect()
}
