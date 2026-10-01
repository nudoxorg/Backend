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
pub(crate) mod files;
mod manifest;
mod readme;
#[cfg(test)]
mod tests;

/// Maximum exact destination retained for a local Markdown link action.
pub(crate) const MAX_README_LINK_DESTINATION_BYTES: usize = 4 * 1024;

use crate::core::LocalProjectId;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use readme::{ReadmeBlock, ReadmeHeading, ReadmeLink};
pub(crate) use readme::rustdoc_link;

/// Normalizes a Markdown heading fragment using the README index's spelling.
pub(crate) fn readme_fragment_slug(value: &str) -> String {
    readme::heading_slug(value)
}

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
    /// The bounded Markdown source, retained verbatim for the rich reader.
    /// `None` means no README source was read.
    pub readme_markdown: Option<Arc<str>>,
    /// Bounded Markdown links with only worker-resolved local files admitted.
    pub readme_links: Arc<[ReadmeLink]>,
    /// Bounded heading targets matching the README's rendered anchors.
    pub readme_headings: Arc<[ReadmeHeading]>,
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
    /// README projection only. Package facts came from the canonical graph.
    Readme,
}

/// Why the Cargo reader did not produce the facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CargoFailure {
    /// The loader was configured without a Cargo program.
    Disabled,
    /// Bounded child capture is not yet available on this platform.
    UnsupportedCapture,
    /// The requesting worker withdrew while Cargo was running.
    Cancelled,
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

    /// Projects the project's README without reading package facts.
    ///
    /// The canonical graph owns name, version, and dependencies. But it never
    /// owns description or license: `RegistryPackageRecord` (the DTO behind
    /// every "package" surface reply, registry release or synthesized local
    /// record alike) has no field for either, so a caller that trusts the
    /// canonical graph for those two facts renders them as unknown forever,
    /// even when the manifest states both (this was toml's bug: its own
    /// `Cargo.toml` states `description` and `license`, but the record built
    /// from the canonical graph could never carry them). This reader
    /// recovers exactly those two fields with one extra cheap, cargo-free
    /// manifest read (`manifest::package_facts`, no subprocess, no
    /// workspace-member glob walk), alongside the document.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn readme(&self, project: &LocalProjectId) -> Option<LocalPackage> {
        let root = project.path();
        let readme_file = readme::project_readme_file(&root);
        let markdown = readme_file.as_ref().map(|(_, source)| Arc::clone(source));
        let readme: Arc<[ReadmeBlock]> = markdown
            .as_deref()
            .map_or_else(|| Arc::from([]), |source| readme::parse(source).into());
        let (readme_links, readme_headings): (Arc<[ReadmeLink]>, Arc<[ReadmeHeading]>) = markdown.as_deref().map_or_else(
            || (Arc::from([]), Arc::from([])),
            |source| {
                readme::navigation_index(
                    source,
                    &root,
                    readme_file.as_ref().map_or(&root, |(path, _)| path),
                )
            },
        );
        let facts = manifest::read_manifest(&root.join("Cargo.toml"))
            .map(|manifest| manifest::package_facts(&root, &manifest));
        let description = facts.as_ref().and_then(|facts| present(facts.description.clone()));
        let license = facts.as_ref().and_then(|facts| present(facts.license.clone()));
        // A project with neither a README to project nor manifest facts to
        // recover has nothing for this reader to contribute; `None` lets the
        // caller fall through to its own gap. But a missing README must not
        // discard manifest facts that were actually read (this was crate
        // `present`'s bug: no `README.md`, yet its `Cargo.toml` states a
        // workspace-inherited license that a bare emptiness check threw away).
        if readme.is_empty() && description.is_none() && license.is_none() {
            return None;
        }
        Some(LocalPackage {
            project: project.clone(),
            source: LocalPackageSource::Readme,
            name: Arc::from(folder_name(&root)),
            version: None,
            description: description.map(Arc::from),
            license: license.map(Arc::from),
            rust_version: None,
            repository: None,
            homepage: None,
            documentation: None,
            keywords: Arc::from([]),
            categories: Arc::from([]),
            readme,
            readme_markdown: markdown,
            readme_links,
            readme_headings,
            dependencies: Arc::from([]),
            features: Arc::from([]),
            members: 0,
        })
    }

    /// Loads the facts for one local project.
    ///
    /// This blocks for at most the configured Cargo bound plus manifest
    /// reads; call it from a worker thread. Callers that already hold a
    /// canonical package record use [`Self::readme`] instead.
    #[must_use]
    pub fn load(&self, project: &LocalProjectId) -> LocalPackage {
        self.load_with_cancel(project, &|| false).unwrap_or_else(|| {
            // The closure above never cancels; keep the ordinary loader's
            // return type total if the cancellation implementation changes.
            manifest::project(project.clone(), &project.path(), CargoFailure::Cancelled)
        })
    }

    /// Loads on a worker and stops its subprocess when that worker closes.
    /// `None` is a withdrawn read and must never be published as package data.
    pub(crate) fn load_with_cancel(
        &self,
        project: &LocalProjectId,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<LocalPackage> {
        if cancelled() { return None; }
        let root = project.path();
        let manifest = root.join("Cargo.toml");
        // Without a root manifest Cargo would search parent directories and
        // could describe an unrelated enclosing workspace.
        if !manifest.is_file() {
            return (!cancelled()).then(|| manifest::project(project.clone(), &root, CargoFailure::Status));
        }
        let failure = match self.cargo.as_deref() {
            None => CargoFailure::Disabled,
            Some(program) => {
                match cargo::metadata(program, &manifest, self.timeout, self.max_output, cancelled) {
                    Ok(metadata) => return (!cancelled()).then(|| cargo::project(project.clone(), &root, metadata)),
                    Err(failure) => failure,
                }
            }
        };
        (!cancelled()).then(|| manifest::project(project.clone(), &root, failure))
    }
}

/// What a mark needs to know about the *active* workspace project — not
/// necessarily the package whose page is open, but the one the reader is
/// working in (`WorkspaceState::active`). Read the same cheap, cargo-free
/// way as [`LocalPackageLoader::readme`]'s recovered fields: one manifest
/// parse, no subprocess, no full dependency resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActiveProject {
    /// Its own name, when its manifest states one.
    pub name: Option<Arc<str>>,
    /// Its own declared license; `None` when not known. Comparing a
    /// package's license against itself says nothing, so this must only
    /// ever come from the reader's own active project, never from the
    /// package whose page happens to be open.
    pub license: Option<Arc<str>>,
    /// Every package name this workspace builds itself: the root and every
    /// `[workspace] members` match. A dependency whose name is in this set
    /// is truly "yours"; merely resolving to a path on this machine is not
    /// enough (vendored and registry sources do that too).
    pub members: BTreeSet<Arc<str>>,
}

/// Reads `root`'s [`ActiveProject`] facts (an empty, unknown result when
/// there is no manifest to read).
#[must_use]
pub fn active_project(root: &Path) -> ActiveProject {
    let Some(manifest) = manifest::read_manifest(&root.join("Cargo.toml")) else {
        return ActiveProject::default();
    };
    let facts = manifest::package_facts(root, &manifest);
    ActiveProject {
        name: present(facts.name).map(Arc::from),
        license: present(facts.license).map(Arc::from),
        members: manifest::workspace_member_names(root, &manifest).into_iter().map(Arc::from).collect(),
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
    readme_path: Option<PathBuf>,
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
        let fallback_readme_path = root.join("README.md");
        let readme_path = self.readme_path.as_deref().unwrap_or(&fallback_readme_path);
        let (readme_links, readme_headings) =
            readme::navigation_index(&self.readme, &root, readme_path);
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
            readme_links,
            readme_headings,
            readme_markdown: (!self.readme.is_empty()).then(|| Arc::from(self.readme)),
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
