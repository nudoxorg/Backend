//! `Cargo.toml` reader used when Cargo cannot answer.
//!
//! It resolves `[workspace] members`/`exclude` globs (`*` and `**`) and
//! `field.workspace = true` inheritance from `[workspace.package]` —
//! whether that table lives in the manifest's own file (a single-file
//! workspace root) or, the ordinary case, in an ancestor's (a member
//! manifest reading the ancestor workspace root's `[workspace.package]`;
//! see `ancestor_workspace_package`). Symlinks and build/VCS directories
//! are never followed, and glob expansion is bounded so a pathological
//! `**` cannot walk an entire home directory.

use super::{
    CargoFailure, DependencyKind, Facts, LocalPackage, LocalPackageSource, dependencies, features,
    readme,
};
use crate::core::LocalProjectId;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Largest manifest read from disk.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Largest number of directories one member glob may visit.
const MAX_GLOB_DIRECTORIES: usize = 4_096;

#[derive(Deserialize, Default)]
pub(super) struct Manifest {
    package: Option<ManifestPackage>,
    workspace: Option<Workspace>,
    #[serde(default)]
    dependencies: BTreeMap<String, toml::Value>,
    #[serde(rename = "dev-dependencies", default)]
    dev_dependencies: BTreeMap<String, toml::Value>,
    #[serde(rename = "build-dependencies", default)]
    build_dependencies: BTreeMap<String, toml::Value>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize, Default)]
struct ManifestPackage {
    name: Option<String>,
    version: Option<toml::Value>,
    description: Option<toml::Value>,
    license: Option<toml::Value>,
    #[serde(rename = "rust-version")]
    rust_version: Option<toml::Value>,
    repository: Option<toml::Value>,
    homepage: Option<toml::Value>,
    documentation: Option<toml::Value>,
    keywords: Option<toml::Value>,
    categories: Option<toml::Value>,
    readme: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
struct Workspace {
    package: Option<ManifestPackage>,
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

/// Reads the root manifest and every workspace member manifest.
pub(super) fn project(project: LocalProjectId, root: &Path, failure: CargoFailure) -> LocalPackage {
    let Some(manifest) = read_manifest(&root.join("Cargo.toml")) else {
        let facts = Facts {
            readme: readme::read(root, &root.join("README.md")),
            readme_path: Some(root.join("README.md")),
            ..Facts::empty()
        };
        return facts.into_package(
            project,
            LocalPackageSource::NoManifest,
            Vec::new(),
            Vec::new(),
            0,
        );
    };
    let facts = package_facts(root, &manifest);
    let manifests = member_manifests(root, manifest.workspace.as_ref());
    let dependencies = dependencies(manifests.iter().flat_map(|manifest| {
        [
            (DependencyKind::Normal, &manifest.dependencies),
            (DependencyKind::Development, &manifest.dev_dependencies),
            (DependencyKind::Build, &manifest.build_dependencies),
        ]
        .into_iter()
        .flat_map(|(kind, table)| {
            table
                .iter()
                .map(move |(name, value)| (kind, name.clone(), requirement(value)))
        })
    }));
    let features = features(manifests.iter().flat_map(|manifest| {
        manifest
            .features
            .iter()
            .map(|(name, values)| (name.clone(), values.clone()))
    }));
    let members = manifests.len();
    facts.into_package(
        project,
        LocalPackageSource::Manifest(failure),
        dependencies,
        features,
        members,
    )
}

/// Reads the root package's fields, following workspace inheritance.
///
/// `pub(super)`: also called from the README-only projection
/// (`LocalPackageLoader::readme`), which needs `description` and `license`
/// without the cost of `Self::project`'s workspace member walk.
pub(super) fn package_facts(root: &Path, manifest: &Manifest) -> Facts {
    let package = manifest.package.as_ref();
    // `field.workspace = true` usually points at a *different* file: the
    // workspace root's `[workspace.package]`, one or more directories up
    // from a member's own manifest (this manifest's own `[workspace]`
    // table, when it has one, is tried first — a single-file workspace
    // root reading its own fields needs no walk).
    let ancestor = manifest.workspace.is_none().then(|| ancestor_workspace_package(root)).flatten();
    let inherited = manifest
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.package.as_ref())
        .or(ancestor.as_ref());
    let field = |select: fn(&ManifestPackage) -> Option<&toml::Value>| {
        inherited_string(package.and_then(select), inherited.and_then(select))
    };
    let list = |select: fn(&ManifestPackage) -> Option<&toml::Value>| {
        inherited_list(package.and_then(select), inherited.and_then(select))
    };
    let readme_path = package
        .and_then(|package| package.readme.as_ref())
        .and_then(toml::Value::as_str)
        .map_or_else(|| root.join("README.md"), |path| root.join(path));
    Facts {
        name: package.and_then(|package| package.name.clone()),
        version: field(|package| package.version.as_ref()),
        description: field(|package| package.description.as_ref()),
        license: field(|package| package.license.as_ref()),
        rust_version: field(|package| package.rust_version.as_ref()),
        repository: field(|package| package.repository.as_ref()),
        homepage: field(|package| package.homepage.as_ref()),
        documentation: field(|package| package.documentation.as_ref()),
        keywords: list(|package| package.keywords.as_ref()),
        categories: list(|package| package.categories.as_ref()),
        readme: readme::read(root, &readme_path),
        readme_path: Some(readme_path),
    }
}

/// Every package name the workspace rooted at `manifest` builds itself:
/// its own root name, plus every `[workspace] members` match. Cargo-free
/// (the member manifests are already read for `Self::project`'s
/// dependency/feature walk; this just names them), so a caller can decide
/// whether a dependency is truly *yours* — a workspace member — rather
/// than merely resolving to a path on this machine, which vendored and
/// registry sources do too.
pub(super) fn workspace_member_names(root: &Path, manifest: &Manifest) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(name) = manifest.package.as_ref().and_then(|package| package.name.clone()) {
        names.insert(name);
    }
    for member in member_manifests(root, manifest.workspace.as_ref()) {
        if let Some(name) = member.package.as_ref().and_then(|package| package.name.clone()) {
            names.insert(name);
        }
    }
    names
}

impl Facts {
    fn empty() -> Self {
        Self {
            name: None,
            version: None,
            description: None,
            license: None,
            rust_version: None,
            repository: None,
            homepage: None,
            documentation: None,
            keywords: Vec::new(),
            categories: Vec::new(),
            readme: String::new(),
            readme_path: None,
        }
    }
}

pub(super) fn read_manifest(path: &Path) -> Option<Manifest> {
    let file = fs::File::open(path).ok()?;
    let mut source = String::new();
    file.take(MAX_MANIFEST_BYTES)
        .read_to_string(&mut source)
        .ok()?;
    toml::from_str(&source).ok()
}

/// Walks from `root` up through its ancestors for the nearest manifest that
/// declares `[workspace.package]`, for a member manifest whose own file has
/// no `[workspace]` table at all — the ordinary layout, where the
/// workspace root and its members are separate files. Each step is one
/// more `Cargo.toml` read, bounded by the real directory depth (a plain
/// upward walk, never a glob: it cannot loop or fan out).
fn ancestor_workspace_package(root: &Path) -> Option<ManifestPackage> {
    let mut dir = root.parent();
    while let Some(current) = dir {
        if let Some(package) = read_manifest(&current.join("Cargo.toml"))
            .and_then(|manifest| manifest.workspace)
            .and_then(|workspace| workspace.package)
        {
            return Some(package);
        }
        dir = current.parent();
    }
    None
}

/// Returns the root manifest plus each non-excluded member manifest.
///
/// A manifest without `[workspace]` contributes only itself: scanning its
/// subdirectories would pull unrelated packages into its dependency counts.
fn member_manifests(root: &Path, workspace: Option<&Workspace>) -> Vec<Manifest> {
    let mut paths = vec![root.join("Cargo.toml")];
    if let Some(workspace) = workspace {
        for pattern in &workspace.members {
            paths.extend(expand_member_pattern(root, pattern));
        }
        paths.retain(|path| {
            let relative = path
                .parent()
                .and_then(|parent| parent.strip_prefix(root).ok())
                .map(|path| {
                    path.to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                })
                .unwrap_or_default();
            !workspace
                .exclude
                .iter()
                .any(|pattern| member_pattern_matches(pattern, &relative))
        });
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|path| read_manifest(&path))
        .collect()
}

pub(super) fn expand_member_pattern(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut budget = MAX_GLOB_DIRECTORIES;
    let mut candidates = vec![root.to_path_buf()];
    for component in pattern.split('/').filter(|component| !component.is_empty()) {
        let mut next = Vec::new();
        for candidate in candidates {
            match component {
                "*" => children(&candidate, &mut next, &mut budget),
                "**" => descendants(&candidate, &mut next, &mut budget),
                literal => {
                    let path = candidate.join(literal);
                    if !is_symlink(&path) {
                        next.push(path);
                    }
                }
            }
        }
        candidates = next;
    }
    candidates
        .into_iter()
        .map(|candidate| {
            if candidate.is_file() {
                candidate
            } else {
                candidate.join("Cargo.toml")
            }
        })
        .filter(|path| path.file_name().is_some_and(|name| name == "Cargo.toml"))
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()))
        .collect()
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// Pushes every visitable child directory of `root`.
fn children(root: &Path, output: &mut Vec<PathBuf>, budget: &mut usize) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let visitable = file_type.is_dir()
            && !entry
                .file_name()
                .to_str()
                .is_some_and(ignored_manifest_directory);
        if visitable {
            *budget -= 1;
            output.push(entry.path());
        }
    }
}

/// Pushes every visitable descendant directory of `root`, depth first.
fn descendants(root: &Path, output: &mut Vec<PathBuf>, budget: &mut usize) {
    let start = output.len();
    children(root, output, budget);
    let found = output[start..].to_vec();
    for child in found {
        descendants(&child, output, budget);
    }
}

fn ignored_manifest_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".backend"
            | ".local"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
            | ".idea"
            | ".vscode"
    )
}

pub(super) fn member_pattern_matches(pattern: &str, relative: &str) -> bool {
    let pattern = pattern.trim_matches('/');
    if pattern == relative {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return relative
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .is_some_and(|name| !name.is_empty() && !name.contains('/'));
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return relative
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .is_some_and(|rest| !rest.is_empty());
    }
    false
}

/// Spells a manifest dependency value with every resolution dimension.
pub(super) fn requirement(value: &toml::Value) -> String {
    if let Some(version) = value.as_str() {
        return version.to_owned();
    }
    let Some(table) = value.as_table() else {
        return "workspace".to_owned();
    };
    let text = |key: &str| table.get(key).and_then(toml::Value::as_str);
    let flag = |key: &str| table.get(key).and_then(toml::Value::as_bool);
    let mut details = Vec::new();
    if let Some(version) = text("version") {
        details.push(version.to_owned());
    }
    if flag("workspace") == Some(true) {
        details.push("workspace".to_owned());
    }
    for (key, label) in [
        ("path", "path"),
        ("git", "git"),
        ("registry", "registry"),
        ("package", "package"),
    ] {
        if let Some(value) = text(key) {
            details.push(format!("{label}: {value}"));
        }
    }
    if flag("optional") == Some(true) {
        details.push("optional".to_owned());
    }
    if flag("default-features") == Some(false) || flag("default_features") == Some(false) {
        details.push("default-features: false".to_owned());
    }
    let enabled = table
        .get("features")
        .and_then(toml::Value::as_array)
        .map(|features| {
            features
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !enabled.is_empty() {
        details.push(format!("features: {}", enabled.join(", ")));
    }
    if details.is_empty() {
        "workspace".to_owned()
    } else {
        details.join(" · ")
    }
}

/// Resolves a string field, following `field.workspace = true` (or an absent
/// field) to the `[workspace.package]` value.
fn inherited_string(
    value: Option<&toml::Value>,
    inherited: Option<&toml::Value>,
) -> Option<String> {
    match value {
        Some(toml::Value::String(value)) => Some(value.clone()),
        Some(value) if inherits(value) => inherited
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned),
        Some(_) => None,
        None => inherited
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned),
    }
}

fn inherited_list(value: Option<&toml::Value>, inherited: Option<&toml::Value>) -> Vec<String> {
    let strings = |value: &toml::Value| {
        value
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    match value {
        Some(value) if inherits(value) => inherited.map(strings).unwrap_or_default(),
        Some(value) => strings(value),
        None => inherited.map(strings).unwrap_or_default(),
    }
}

fn inherits(value: &toml::Value) -> bool {
    value
        .as_table()
        .and_then(|table| table.get("workspace"))
        .and_then(toml::Value::as_bool)
        == Some(true)
}
