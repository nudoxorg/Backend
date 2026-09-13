//! Cargo package facts and README blocks loaded from the local package boundary.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

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
}

impl PackageMetadata {
    pub(super) fn load(project: &Path) -> Self {
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
        let manifests = member_manifests(project);
        let dependencies = collect_dependencies(&manifests);
        let description = package
            .and_then(|package| package.description.clone())
            .unwrap_or_else(|| first_paragraph(&readme_source));
        Self {
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
                .unwrap_or_default(),
            homepage: package.and_then(|p| p.homepage.clone()).unwrap_or_default(),
            documentation: package
                .and_then(|p| p.documentation.clone())
                .unwrap_or_default(),
            keywords: package.and_then(|p| p.keywords.clone()).unwrap_or_default(),
            categories: package
                .and_then(|p| p.categories.clone())
                .unwrap_or_default(),
            readme: parse_readme(&readme_source),
            dependencies,
            members: manifests.len(),
        }
    }
}

fn read_manifest(path: &Path) -> Option<Manifest> {
    toml::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn member_manifests(project: &Path) -> Vec<Manifest> {
    let mut paths = vec![project.join("Cargo.toml")];
    for group in [
        "crates",
        "apps",
        "frontends",
        "extensions",
        "tests",
        "tools",
    ] {
        let directory = project.join(group);
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() {
                paths.push(manifest);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|path| read_manifest(&path))
        .collect()
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

fn dependency_requirement(value: &toml::Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| {
            let table = value.as_table()?;
            table
                .get("version")
                .and_then(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    table
                        .get("path")
                        .and_then(toml::Value::as_str)
                        .map(|path| format!("path: {path}"))
                })
                .or_else(|| {
                    table
                        .get("git")
                        .and_then(toml::Value::as_str)
                        .map(|_| "git".to_owned())
                })
        })
        .unwrap_or_else(|| "workspace".to_owned())
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
    use super::{ReadmeBlock, parse_readme};

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
}
