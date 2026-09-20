//! Cargo package facts and README blocks loaded from the local package boundary.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub(super) struct PackageMetadata {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) description: String,
    pub(super) license: String,
    pub(super) rust_version: String,
    pub(super) repository: String,
    pub(super) homepage: String,
    pub(super) documentation: String,
    pub(super) keywords: Vec<String>,
    pub(super) categories: Vec<String>,
    pub(super) readme: Vec<ReadmeBlock>,
    pub(super) dependencies: Vec<PackageDependency>,
    pub(super) features: Vec<PackageFeature>,
    pub(super) members: usize,
}

#[derive(Clone, Debug)]
pub(super) enum ReadmeBlock {
    Heading { level: u8, text: String },
    Paragraph(String),
    Bullet(String),
    Code { language: String, text: String },
}

#[derive(Clone, Debug)]
pub(super) struct PackageDependency {
    pub(super) name: String,
    pub(super) requirement: String,
    pub(super) kind: DependencyKind,
    pub(super) users: usize,
}

/// One feature declared by a package in the selected workspace.
///
/// Cargo exposes feature values as a graph of optional dependency and feature
/// references.  Keeping those references in the desktop catalog means a
/// package page can explain why a dependency is present without reparsing a
/// manifest or making a network request.
#[derive(Clone, Debug)]
pub(super) struct PackageFeature {
    pub(super) name: String,
    pub(super) members: Vec<String>,
    pub(super) users: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum DependencyKind {
    Runtime,
    Development,
    Build,
}

impl DependencyKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Runtime => "normal",
            Self::Development => "dev",
            Self::Build => "build",
        }
    }
}

#[derive(Deserialize, Default)]
struct Manifest {
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
    description: Option<String>,
    license: Option<toml::Value>,
    #[serde(rename = "rust-version")]
    rust_version: Option<toml::Value>,
    repository: Option<String>,
    homepage: Option<String>,
    documentation: Option<String>,
    keywords: Option<Vec<String>>,
    categories: Option<Vec<String>>,
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

/// The stable subset of `cargo metadata --format-version 1` that the package
/// surface needs.  Cargo is the authority for workspace expansion and
/// inherited package fields; the TOML parser below remains a deliberate
/// fallback for projects without an executable Cargo installation.
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    version: String,
    description: Option<String>,
    license: Option<String>,
    repository: Option<String>,
    homepage: Option<String>,
    documentation: Option<String>,
    keywords: Vec<String>,
    categories: Vec<String>,
    readme: Option<String>,
    rust_version: Option<String>,
    manifest_path: String,
    dependencies: Vec<CargoDependency>,
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct CargoDependency {
    name: String,
    req: String,
    kind: Option<String>,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    optional: bool,
    #[serde(default = "default_true")]
    uses_default_features: bool,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    registry: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

const fn default_true() -> bool {
    true
}

impl PackageMetadata {
    pub(super) fn load(project: &Path) -> Self {
        if let Some(metadata) = cargo_metadata(project) {
            return from_cargo_metadata(project, metadata);
        }
        from_manifests(project)
    }
}

fn from_cargo_metadata(project: &Path, metadata: CargoMetadata) -> PackageMetadata {
    let CargoMetadata {
        packages: all_packages,
        workspace_members,
    } = metadata;
    let mut packages = all_packages
        .into_iter()
        .filter(|package| workspace_members.iter().any(|id| id == &package.id))
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.manifest_path.cmp(&right.manifest_path));
    // Cargo emits canonical absolute manifest paths.  The desktop can be
    // launched with a relative project argument, so compare canonical paths
    // before selecting the package represented by the root page.
    let root_manifest =
        fs::canonicalize(project.join("Cargo.toml")).unwrap_or_else(|_| project.join("Cargo.toml"));
    let selected = packages.iter().find(|package| {
        fs::canonicalize(&package.manifest_path).map_or_else(
            |_| Path::new(&package.manifest_path) == root_manifest,
            |path| path == root_manifest,
        )
    });
    let fallback_name = project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_owned();
    let readme_source = selected
        .and_then(|package| {
            package.readme.as_ref().map(|readme| {
                let manifest_directory = Path::new(&package.manifest_path)
                    .parent()
                    .unwrap_or(project);
                fs::read_to_string(manifest_directory.join(readme)).unwrap_or_default()
            })
        })
        .unwrap_or_else(|| fs::read_to_string(project.join("README.md")).unwrap_or_default());
    let dependencies = collect_cargo_dependencies(&packages);
    let features = collect_cargo_features(&packages);
    PackageMetadata {
        name: selected
            .map(|package| package.name.clone())
            .unwrap_or_else(|| readme_title(&readme_source).unwrap_or(fallback_name)),
        version: selected
            .map(|package| package.version.clone())
            .unwrap_or_else(|| "local".to_owned()),
        description: selected
            .and_then(|package| package.description.clone())
            .filter(|description| !description.trim().is_empty())
            .unwrap_or_else(|| first_paragraph(&readme_source)),
        license: selected
            .and_then(|package| package.license.clone())
            .unwrap_or_else(|| "Unspecified".to_owned()),
        rust_version: selected
            .and_then(|package| package.rust_version.clone())
            .unwrap_or_else(|| "Unspecified".to_owned()),
        repository: selected
            .and_then(|package| package.repository.clone())
            .or_else(|| discover_repository(project))
            .unwrap_or_default(),
        homepage: selected
            .and_then(|package| package.homepage.clone())
            .unwrap_or_default(),
        documentation: selected
            .and_then(|package| package.documentation.clone())
            .unwrap_or_default(),
        keywords: selected
            .map(|package| package.keywords.clone())
            .unwrap_or_default(),
        categories: selected
            .map(|package| package.categories.clone())
            .unwrap_or_default(),
        readme: parse_readme(&readme_source),
        dependencies,
        features,
        members: packages.len(),
    }
}

fn from_manifests(project: &Path) -> PackageMetadata {
    let root = read_manifest(&project.join("Cargo.toml")).unwrap_or_default();
    let workspace = root
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.package.as_ref());
    let package = root.package.as_ref();
    let fallback_name = project
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_owned();
    let readme_path = package
        .and_then(|package| package.readme.as_ref())
        .and_then(value_string)
        .map_or_else(|| project.join("README.md"), |path| project.join(path));
    let readme_source = fs::read_to_string(readme_path).unwrap_or_default();
    let manifests = member_manifests(project, root.workspace.as_ref());
    let dependencies = collect_dependencies(&manifests);
    let description = package
        .and_then(|package| package.description.clone())
        .unwrap_or_else(|| first_paragraph(&readme_source));
    PackageMetadata {
        name: package
            .and_then(|package| package.name.clone())
            .unwrap_or_else(|| readme_title(&readme_source).unwrap_or(fallback_name)),
        version: inherited_string(
            package.and_then(|p| p.version.as_ref()),
            workspace.and_then(|p| p.version.as_ref()),
        )
        .unwrap_or_else(|| "local".to_owned()),
        description,
        license: inherited_string(
            package.and_then(|p| p.license.as_ref()),
            workspace.and_then(|p| p.license.as_ref()),
        )
        .unwrap_or_else(|| "Unspecified".to_owned()),
        rust_version: inherited_string(
            package.and_then(|p| p.rust_version.as_ref()),
            workspace.and_then(|p| p.rust_version.as_ref()),
        )
        .unwrap_or_else(|| "Unspecified".to_owned()),
        repository: package
            .and_then(|p| p.repository.clone())
            .or_else(|| workspace.and_then(|p| p.repository.clone()))
            .or_else(|| discover_repository(project))
            .unwrap_or_default(),
        homepage: package
            .and_then(|p| p.homepage.clone())
            .or_else(|| workspace.and_then(|p| p.homepage.clone()))
            .unwrap_or_default(),
        documentation: package
            .and_then(|p| p.documentation.clone())
            .or_else(|| workspace.and_then(|p| p.documentation.clone()))
            .unwrap_or_default(),
        keywords: package
            .and_then(|p| p.keywords.clone())
            .or_else(|| workspace.and_then(|p| p.keywords.clone()))
            .unwrap_or_default(),
        categories: package
            .and_then(|p| p.categories.clone())
            .or_else(|| workspace.and_then(|p| p.categories.clone()))
            .unwrap_or_default(),
        readme: parse_readme(&readme_source),
        dependencies,
        features: collect_features(&manifests),
        members: manifests.len(),
    }
}

fn cargo_metadata(project: &Path) -> Option<CargoMetadata> {
    let output = Command::new("cargo")
        .current_dir(project)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Resolves a forge URL without contacting the forge. This keeps an
/// unmanifested checkout useful in the package view and works when the
/// project was cloned from GitHub, GitLab, Codeberg, or a private forge.
fn discover_repository(project: &Path) -> Option<String> {
    let git = project.join(".git");
    let config = if git.is_dir() {
        git.join("config")
    } else {
        let gitfile = fs::read_to_string(git).ok()?;
        let gitdir = gitfile.trim().strip_prefix("gitdir: ")?;
        project.join(gitdir).join("config")
    };
    let source = fs::read_to_string(config).ok()?;
    let mut origin = false;
    for line in source.lines().map(str::trim) {
        if let Some(remote) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            origin = remote == "remote \"origin\"";
        } else if origin {
            if let Some(url) = line.strip_prefix("url = ") {
                return Some(url.to_owned());
            }
        }
    }
    None
}

fn read_manifest(path: &Path) -> Option<Manifest> {
    toml::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn member_manifests(project: &Path, workspace: Option<&Workspace>) -> Vec<Manifest> {
    let mut paths = vec![project.join("Cargo.toml")];
    if let Some(workspace) = workspace {
        for pattern in &workspace.members {
            paths.extend(expand_member_pattern(project, pattern));
        }
        paths.retain(|path| {
            let relative = path
                .parent()
                .and_then(|parent| parent.strip_prefix(project).ok())
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
    } else {
        // A non-workspace project has only its root package.  The old
        // hard-coded group scan accidentally pulled unrelated manifests from
        // monorepos into a standalone package's dependency counts.
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|path| read_manifest(&path))
        .collect()
}

fn expand_member_pattern(project: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut candidates = vec![project.to_path_buf()];
    for component in pattern.split('/').filter(|component| !component.is_empty()) {
        let mut next = Vec::new();
        for candidate in candidates {
            match component {
                "*" => {
                    if let Ok(entries) = fs::read_dir(candidate) {
                        next.extend(entries.flatten().filter_map(|entry| {
                            let file_type = entry.file_type().ok()?;
                            if file_type.is_symlink() {
                                return None;
                            }
                            if file_type.is_dir()
                                && entry
                                    .file_name()
                                    .to_str()
                                    .is_some_and(ignored_manifest_directory)
                            {
                                return None;
                            }
                            Some(entry.path())
                        }));
                    }
                }
                "**" => collect_descendants(&candidate, &mut next),
                literal => {
                    let path = candidate.join(literal);
                    if !fs::symlink_metadata(&path)
                        .is_ok_and(|metadata| metadata.file_type().is_symlink())
                    {
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
        .filter(|path| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .collect()
}

fn collect_descendants(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir()
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(ignored_manifest_directory)
        {
            output.push(path.clone());
            collect_descendants(&path, output);
        }
    }
}

fn ignored_manifest_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".backend"
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

fn member_pattern_matches(pattern: &str, relative: &str) -> bool {
    let pattern = pattern.trim_matches('/');
    if pattern == relative {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return relative
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/') && !suffix[1..].contains('/'));
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return relative
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'));
    }
    false
}

fn collect_dependencies(manifests: &[Manifest]) -> Vec<PackageDependency> {
    let mut values: BTreeMap<(DependencyKind, String, String), usize> = BTreeMap::new();
    for manifest in manifests {
        for (kind, dependencies) in [
            (DependencyKind::Runtime, &manifest.dependencies),
            (DependencyKind::Development, &manifest.dev_dependencies),
            (DependencyKind::Build, &manifest.build_dependencies),
        ] {
            for (name, value) in dependencies {
                let requirement = dependency_requirement(value);
                *values.entry((kind, name.clone(), requirement)).or_default() += 1;
            }
        }
    }
    values
        .into_iter()
        .map(|((kind, name, requirement), users)| PackageDependency {
            name,
            requirement,
            kind,
            users,
        })
        .collect()
}

fn collect_cargo_dependencies(packages: &[CargoPackage]) -> Vec<PackageDependency> {
    let mut values: BTreeMap<(DependencyKind, String, String), usize> = BTreeMap::new();
    for package in packages {
        for dependency in &package.dependencies {
            let kind = dependency_kind(dependency.kind.as_deref());
            let requirement = cargo_dependency_requirement(dependency);
            let name = dependency
                .rename
                .as_deref()
                .unwrap_or(&dependency.name)
                .to_owned();
            *values.entry((kind, name, requirement)).or_default() += 1;
        }
    }
    values
        .into_iter()
        .map(|((kind, name, requirement), users)| PackageDependency {
            name,
            requirement,
            kind,
            users,
        })
        .collect()
}

fn collect_features(manifests: &[Manifest]) -> Vec<PackageFeature> {
    let mut values: BTreeMap<(String, Vec<String>), usize> = BTreeMap::new();
    for manifest in manifests {
        for (name, members) in &manifest.features {
            *values.entry((name.clone(), members.clone())).or_default() += 1;
        }
    }
    values
        .into_iter()
        .map(|((name, members), users)| PackageFeature {
            name,
            members,
            users,
        })
        .collect()
}

fn collect_cargo_features(packages: &[CargoPackage]) -> Vec<PackageFeature> {
    let mut values: BTreeMap<(String, Vec<String>), usize> = BTreeMap::new();
    for package in packages {
        for (name, members) in &package.features {
            *values.entry((name.clone(), members.clone())).or_default() += 1;
        }
    }
    values
        .into_iter()
        .map(|((name, members), users)| PackageFeature {
            name,
            members,
            users,
        })
        .collect()
}

fn dependency_kind(kind: Option<&str>) -> DependencyKind {
    match kind {
        Some("dev") => DependencyKind::Development,
        Some("build") => DependencyKind::Build,
        _ => DependencyKind::Runtime,
    }
}

fn cargo_dependency_requirement(dependency: &CargoDependency) -> String {
    let mut details = Vec::new();
    if dependency.req != "*" {
        details.push(dependency.req.clone());
    }
    if let Some(path) = &dependency.path {
        details.push(format!("path: {path}"));
    } else if let Some(source) = &dependency.source {
        if source.starts_with("registry+") {
            details.push(dependency.registry.as_deref().map_or_else(
                || "registry".to_owned(),
                |registry| format!("registry: {registry}"),
            ));
        } else if source.starts_with("git+") {
            details.push(format!("git: {}", source.trim_start_matches("git+")));
        } else {
            details.push(source.clone());
        }
    }
    if dependency.rename.is_some() {
        // `name` is Cargo's resolved package name and `rename` is the local
        // manifest alias.  The alias is already the displayed dependency key;
        // preserve the real package name in the requirement details.
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

fn dependency_requirement(value: &toml::Value) -> String {
    if let Some(version) = value.as_str() {
        return version.to_owned();
    }
    let Some(table) = value.as_table() else {
        return "workspace".to_owned();
    };
    let mut details = Vec::new();
    if let Some(version) = table.get("version").and_then(toml::Value::as_str) {
        details.push(version.to_owned());
    }
    if table
        .get("workspace")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
    {
        details.push("workspace".to_owned());
    }
    if let Some(path) = table.get("path").and_then(toml::Value::as_str) {
        details.push(format!("path: {path}"));
    }
    if let Some(git) = table.get("git").and_then(toml::Value::as_str) {
        details.push(format!("git: {git}"));
    }
    if let Some(registry) = table.get("registry").and_then(toml::Value::as_str) {
        details.push(format!("registry: {registry}"));
    }
    if let Some(package) = table.get("package").and_then(toml::Value::as_str) {
        details.push(format!("package: {package}"));
    }
    if table
        .get("optional")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
    {
        details.push("optional".to_owned());
    }
    if table.get("default-features").and_then(toml::Value::as_bool) == Some(false) {
        details.push("default-features: false".to_owned());
    }
    if let Some(features) = table.get("features").and_then(toml::Value::as_array) {
        let features = features
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>();
        if !features.is_empty() {
            details.push(format!("features: {}", features.join(", ")));
        }
    }
    if details.is_empty() {
        "workspace".to_owned()
    } else {
        details.join(" · ")
    }
}

fn inherited_string(
    value: Option<&toml::Value>,
    inherited: Option<&toml::Value>,
) -> Option<String> {
    value
        .and_then(value_string)
        .or_else(|| {
            value
                .and_then(toml::Value::as_table)
                .and_then(|table| table.get("workspace"))
                .and_then(toml::Value::as_bool)
                .filter(|enabled| *enabled)
                .and_then(|_| inherited.and_then(value_string))
        })
        .or_else(|| {
            value
                .is_none()
                .then(|| inherited.and_then(value_string))
                .flatten()
        })
}

fn value_string(value: &toml::Value) -> Option<String> {
    value.as_str().map(ToOwned::to_owned)
}

fn first_paragraph(readme: &str) -> String {
    let mut paragraph = Vec::new();
    for line in readme.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with("```") {
            continue;
        }
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
        } else {
            paragraph.push(line);
        }
    }
    paragraph.join(" ")
}

fn readme_title(readme: &str) -> Option<String> {
    let title = readme
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))?;
    Some(
        title
            .split_whitespace()
            .take_while(|part| !part.starts_with('v') || part.len() == 1)
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
            .replace(' ', "-"),
    )
}

fn parse_readme(readme: &str) -> Vec<ReadmeBlock> {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut code = Vec::new();
    let mut code_language = String::new();
    let mut in_code = false;
    let flush_paragraph = |blocks: &mut Vec<ReadmeBlock>, paragraph: &mut Vec<&str>| {
        if !paragraph.is_empty() {
            blocks.push(ReadmeBlock::Paragraph(paragraph.join(" ")));
            paragraph.clear();
        }
    };
    for line in readme.lines() {
        let trimmed = line.trim();
        if let Some(language) = trimmed.strip_prefix("```") {
            flush_paragraph(&mut blocks, &mut paragraph);
            if in_code {
                blocks.push(ReadmeBlock::Code {
                    language: code_language.clone(),
                    text: code.join("\n"),
                });
                code.clear();
                code_language.clear();
            } else {
                language.clone_into(&mut code_language);
            }
            in_code = !in_code;
        } else if in_code {
            code.push(line);
        } else if let Some(heading) = trimmed.strip_prefix('#') {
            flush_paragraph(&mut blocks, &mut paragraph);
            let level = u8::try_from(
                trimmed
                    .len()
                    .saturating_sub(heading.trim_start_matches('#').len())
                    .min(6),
            )
            .unwrap_or(6);
            blocks.push(ReadmeBlock::Heading {
                level,
                text: heading.trim_start_matches('#').trim().to_owned(),
            });
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(ReadmeBlock::Bullet(item.to_owned()));
        } else if trimmed.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
        } else {
            paragraph.push(trimmed);
        }
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    if !code.is_empty() {
        blocks.push(ReadmeBlock::Code {
            language: code_language,
            text: code.join("\n"),
        });
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::{
        CargoDependency, CargoMetadata, CargoPackage, DependencyKind, PackageMetadata, ReadmeBlock,
        from_cargo_metadata, from_manifests, parse_readme,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Result<Self, String> {
            let suffix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos();
            let path = std::env::temp_dir().join(format!("backend-metadata-{label}-{suffix}"));
            fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn readme_projection_retains_document_structure() {
        let blocks =
            parse_readme("# Name\n\nA useful crate.\n\n- fast\n\n```rust\nfn main() {}\n```\n");
        assert!(matches!(
            blocks.as_slice(),
            [
                ReadmeBlock::Heading { .. },
                ReadmeBlock::Paragraph(_),
                ReadmeBlock::Bullet(_),
                ReadmeBlock::Code { .. }
            ]
        ));
    }

    #[test]
    fn cargo_dependency_requirement_keeps_resolution_dimensions() {
        let dependency = CargoDependency {
            // Cargo metadata reports the resolved package in `name` and the
            // manifest alias in `rename`.
            name: "serde".to_owned(),
            req: "^1.0".to_owned(),
            kind: Some("dev".to_owned()),
            rename: Some("serde_alias".to_owned()),
            optional: true,
            uses_default_features: false,
            features: vec!["derive".to_owned()],
            target: Some("cfg(unix)".to_owned()),
            source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
            registry: None,
            path: None,
        };
        let metadata = super::cargo_dependency_requirement(&dependency);
        assert!(metadata.contains("^1.0"));
        assert!(metadata.contains("registry"));
        assert!(metadata.contains("package: serde"));
        assert!(metadata.contains("optional"));
        assert!(metadata.contains("default-features: false"));
        assert!(metadata.contains("features: derive"));
        assert!(metadata.contains("target: cfg(unix)"));
    }

    #[test]
    fn cargo_projection_uses_workspace_members_and_feature_graph() {
        let package =
            |id: &str, name: &str, manifest_path: &str, features: BTreeMap<String, Vec<String>>| {
                CargoPackage {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    version: "1.2.3".to_owned(),
                    description: Some(format!("{name} description")),
                    license: Some("MIT".to_owned()),
                    repository: None,
                    homepage: None,
                    documentation: None,
                    keywords: vec![],
                    categories: vec![],
                    readme: None,
                    rust_version: None,
                    manifest_path: manifest_path.to_owned(),
                    dependencies: vec![],
                    features,
                }
            };
        let first = package(
            "first",
            "first",
            "/tmp/workspace/first/Cargo.toml",
            BTreeMap::from([("default".to_owned(), vec!["serde".to_owned()])]),
        );
        let second = package(
            "second",
            "second",
            "/tmp/workspace/second/Cargo.toml",
            BTreeMap::from([("runtime".to_owned(), vec!["dep:serde".to_owned()])]),
        );
        let metadata = from_cargo_metadata(
            &PathBuf::from("/tmp/workspace"),
            CargoMetadata {
                packages: vec![second, first],
                workspace_members: vec!["first".to_owned(), "second".to_owned()],
            },
        );
        assert_eq!(metadata.name, "workspace");
        assert_eq!(metadata.version, "local");
        assert_eq!(metadata.members, 2);
        assert_eq!(metadata.features.len(), 2);
        assert!(
            metadata
                .features
                .iter()
                .any(|feature| feature.name == "default")
        );
    }

    #[test]
    fn manifest_fallback_expands_custom_workspace_members_and_forge_remote() -> Result<(), String> {
        let scratch = Scratch::new("fallback")?;
        fs::create_dir_all(scratch.0.join("components/alpha"))
            .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("nested/beta")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join(".git/ignored")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("Cargo.toml"),
            "[workspace]\nmembers = [\"components/*\", \"nested/**\"]\nexclude = [\"nested/ignored\"]\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("components/alpha/Cargo.toml"),
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n[features]\ndefault = []\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("nested/beta/Cargo.toml"),
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n[dependencies]\nserde = { version = \"1\", optional = true }\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join(".git/ignored/Cargo.toml"),
            "[package]\nname = \"must-not-be-discovered\"\nversion = \"99.0.0\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("README.md"),
            "# Forge Project\n\nA project without a package root.\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@codeberg.org:owner/project.git\n",
        )
        .map_err(|error| error.to_string())?;
        let metadata = from_manifests(&scratch.0);
        assert_eq!(metadata.members, 3);
        assert_eq!(metadata.name, "forge-project");
        assert_eq!(metadata.repository, "git@codeberg.org:owner/project.git");
        assert_eq!(metadata.dependencies.len(), 1);
        assert_eq!(metadata.dependencies[0].kind, DependencyKind::Runtime);
        assert!(metadata.dependencies[0].requirement.contains("optional"));
        Ok(())
    }

    #[test]
    fn load_uses_cargo_without_fetching_dependencies() -> Result<(), String> {
        let scratch = Scratch::new("cargo")?;
        fs::create_dir_all(scratch.0.join("crates/one/src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"7.0.0\"\nlicense = \"Apache-2.0\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("crates/one/Cargo.toml"),
            "[package]\nname = \"one\"\nversion.workspace = true\nlicense.workspace = true\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("crates/one/src/lib.rs"), "pub fn one() {}\n")
            .map_err(|error| error.to_string())?;
        let metadata = PackageMetadata::load(&scratch.0);
        assert_eq!(
            metadata.name,
            scratch
                .0
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("scratch directory has no UTF-8 name")?
        );
        assert_eq!(metadata.version, "local");
        assert_eq!(metadata.license, "Unspecified");
        assert_eq!(metadata.members, 1);
        Ok(())
    }
}
