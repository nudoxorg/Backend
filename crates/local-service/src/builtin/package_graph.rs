//! Canonical Cargo package graph extraction for the versioned product relation.
//!
//! Cargo remains the authority for workspace inheritance and target-specific
//! dependency normalization. The retained output is a compact set of stable,
//! independently keyed facts rather than Cargo's large environment-specific
//! JSON document.

use backend_engine::{
    ProductDependencyKind, ProductSourceRecord, product_dependency_key,
    product_dependency_source_key, product_package_key,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    manifest_path: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    req: String,
    #[serde(default)]
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

pub(super) struct PackageGraphSnapshot {
    pub(super) version: [u8; 32],
    pub(super) facts: Vec<([u8; 32], ProductSourceRecord)>,
}

pub(super) fn scan(
    root: &Path,
    project: [u8; 32],
    prior_version: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<PackageGraphSnapshot, String> {
    let manifest = root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(PackageGraphSnapshot {
            version: [0; 32],
            facts: Vec::new(),
        });
    }
    let version = manifest_graph_version(root)?;
    if version == prior_version {
        return Ok(PackageGraphSnapshot {
            version,
            facts: reusable
                .iter()
                .map(|(key, record)| (*key, record.clone()))
                .collect(),
        });
    }
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version=1")
        .arg("--no-deps")
        .arg("--offline")
        .arg("--manifest-path")
        .arg(&manifest)
        .output();
    let facts = match output {
        Ok(output) if output.status.success() => {
            let metadata = serde_json::from_slice::<Metadata>(&output.stdout)
                .map_err(|error| format!("decode cargo metadata: {error}"))?;
            facts_from_metadata(root, project, metadata)?
        }
        _ => fallback_manifest(root, project)?,
    };
    Ok(PackageGraphSnapshot { version, facts })
}

fn manifest_graph_version(root: &Path) -> Result<[u8; 32], String> {
    const MAX_MANIFESTS: usize = 10_000;
    const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
    const MAX_GRAPH_BYTES: usize = 32 * 1024 * 1024;
    let mut pending = vec![root.to_path_buf()];
    let mut manifests = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| format!("read {}: {error}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read {}: {error}", directory.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !matches!(
                    name.as_ref(),
                    ".git" | ".backend" | "target" | "node_modules" | ".venv" | "venv"
                ) {
                    pending.push(entry.path());
                }
            } else if file_type.is_file() && entry.file_name() == "Cargo.toml" {
                manifests.push(entry.path());
            }
        }
    }
    manifests.sort();
    if manifests.len() > MAX_MANIFESTS {
        return Err("package graph contains too many manifests".to_owned());
    }
    let mut hasher = backend_engine::blake3::Hasher::new();
    hasher.update(b"backend.package-manifest-graph.v1\0");
    let mut total = 0_usize;
    for path in manifests {
        let bytes = fs::read(&path)
            .map_err(|error| format!("read manifest {}: {error}", path.display()))?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!("manifest {} is oversized", path.display()));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| "manifest byte count overflow".to_owned())?;
        if total > MAX_GRAPH_BYTES {
            return Err("package manifest graph exceeds its byte budget".to_owned());
        }
        let relative = relative_path(root, &path)?;
        hasher.update(&(relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn facts_from_metadata(
    root: &Path,
    project: [u8; 32],
    metadata: Metadata,
) -> Result<Vec<([u8; 32], ProductSourceRecord)>, String> {
    let members = metadata
        .workspace_members
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut packages = metadata
        .packages
        .into_iter()
        .filter(|package| members.contains(&package.id))
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.manifest_path.cmp(&right.manifest_path));
    let mut facts = Vec::new();
    let mut sources = BTreeMap::new();
    for package in packages {
        let manifest_path = relative_path(root, Path::new(&package.manifest_path))?;
        let package_key = product_package_key(project, &manifest_path);
        facts.push((
            package_key,
            ProductSourceRecord::package(
                project,
                manifest_path,
                package.name,
                package.version,
                package.description.unwrap_or_default(),
                package.license.unwrap_or_default(),
                package.repository.unwrap_or_default(),
            )?,
        ));
        for dependency in package.dependencies {
            let source = dependency_source(root, &dependency);
            let source = intern_source(project, source, &mut sources, &mut facts)?;
            let kind = dependency_kind(dependency.kind.as_deref())?;
            let alias = dependency.rename.unwrap_or_else(|| dependency.name.clone());
            let target = dependency.target.unwrap_or_default();
            let key = product_dependency_key(project, package_key, &alias, kind, &target);
            facts.push((
                key,
                ProductSourceRecord::dependency(
                    project,
                    package_key,
                    alias,
                    dependency.name,
                    dependency.req,
                    kind,
                    target,
                    source,
                    dependency.optional,
                    dependency.uses_default_features,
                    dependency.features,
                )?,
            ));
        }
    }
    canonicalize(facts)
}

fn dependency_kind(value: Option<&str>) -> Result<ProductDependencyKind, String> {
    match value.unwrap_or("normal") {
        "normal" => Ok(ProductDependencyKind::Normal),
        "build" => Ok(ProductDependencyKind::Build),
        "dev" => Ok(ProductDependencyKind::Development),
        value => Err(format!("unknown cargo dependency kind {value}")),
    }
}

fn dependency_source(root: &Path, dependency: &Dependency) -> String {
    if let Some(source) = dependency.source.as_ref().or(dependency.registry.as_ref()) {
        return source.clone();
    }
    dependency.path.as_ref().map_or_else(String::new, |path| {
        let path = Path::new(path);
        path.strip_prefix(root).map_or_else(
            |_| format!("path:external/{}", dependency.name),
            |relative| format!("path:{}", normalized(relative)),
        )
    })
}

fn relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    canonical
        .strip_prefix(root)
        .map(normalized)
        .map_err(|_| format!("workspace manifest {} escapes project root", path.display()))
}

fn normalized(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn canonicalize(
    mut facts: Vec<([u8; 32], ProductSourceRecord)>,
) -> Result<Vec<([u8; 32], ProductSourceRecord)>, String> {
    facts.sort_by_key(|(key, _)| *key);
    if facts.windows(2).any(|window| window[0].0 == window[1].0) {
        return Err("package graph contains duplicate canonical identities".to_owned());
    }
    if facts.len() > ProductSourceRecord::MAX_PROJECT_FACTS {
        return Err("package graph exceeds the bounded fact budget".to_owned());
    }
    Ok(facts)
}

fn fallback_manifest(
    root: &Path,
    project: [u8; 32],
) -> Result<Vec<([u8; 32], ProductSourceRecord)>, String> {
    let manifest_path = root.join("Cargo.toml");
    let bytes = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest = bytes
        .parse::<toml::Value>()
        .map_err(|error| format!("parse {}: {error}", manifest_path.display()))?;
    let Some(package) = manifest.get("package").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "Cargo package has no name".to_owned())?;
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .unwrap_or("workspace");
    let package_key = product_package_key(project, "Cargo.toml");
    let mut facts = vec![(
        package_key,
        ProductSourceRecord::package(
            project,
            "Cargo.toml",
            name,
            version,
            package
                .get("description")
                .and_then(toml::Value::as_str)
                .unwrap_or_default(),
            package
                .get("license")
                .and_then(toml::Value::as_str)
                .unwrap_or_default(),
            package
                .get("repository")
                .and_then(toml::Value::as_str)
                .unwrap_or_default(),
        )?,
    )];
    let mut sources = BTreeMap::new();
    append_manifest_dependencies(
        root,
        project,
        package_key,
        &manifest,
        "",
        &mut sources,
        &mut facts,
    )?;
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, value) in targets {
            append_manifest_dependencies(
                root,
                project,
                package_key,
                value,
                target,
                &mut sources,
                &mut facts,
            )?;
        }
    }
    canonicalize(facts)
}

fn append_manifest_dependencies(
    root: &Path,
    project: [u8; 32],
    package: [u8; 32],
    manifest: &toml::Value,
    target: &str,
    sources: &mut BTreeMap<String, [u8; 32]>,
    facts: &mut Vec<([u8; 32], ProductSourceRecord)>,
) -> Result<(), String> {
    for (table_name, kind) in [
        ("dependencies", ProductDependencyKind::Normal),
        ("build-dependencies", ProductDependencyKind::Build),
        ("dev-dependencies", ProductDependencyKind::Development),
    ] {
        let Some(table) = manifest.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (alias, value) in table {
            let (name, requirement, source, optional, default_features, features) = match value {
                toml::Value::String(requirement) => (
                    alias.clone(),
                    requirement.clone(),
                    String::new(),
                    false,
                    true,
                    Vec::new(),
                ),
                toml::Value::Table(detail) => {
                    let name = detail
                        .get("package")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(alias)
                        .to_owned();
                    let requirement = detail
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("*")
                        .to_owned();
                    let source = if let Some(git) = detail.get("git").and_then(toml::Value::as_str)
                    {
                        format!("git+{git}")
                    } else if let Some(path) = detail.get("path").and_then(toml::Value::as_str) {
                        let path = root.join(path);
                        path.strip_prefix(root).map_or_else(
                            |_| format!("path:external/{name}"),
                            |relative| format!("path:{}", normalized(relative)),
                        )
                    } else {
                        detail
                            .get("registry")
                            .and_then(toml::Value::as_str)
                            .map_or_else(String::new, |registry| format!("registry+{registry}"))
                    };
                    let features = detail
                        .get("features")
                        .and_then(toml::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(toml::Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    (
                        name,
                        requirement,
                        source,
                        detail
                            .get("optional")
                            .and_then(toml::Value::as_bool)
                            .unwrap_or(false),
                        detail
                            .get("default-features")
                            .and_then(toml::Value::as_bool)
                            .unwrap_or(true),
                        features,
                    )
                }
                _ => return Err(format!("unsupported dependency declaration for {alias}")),
            };
            let source = intern_source(project, source, sources, facts)?;
            let key = product_dependency_key(project, package, alias, kind, target);
            facts.push((
                key,
                ProductSourceRecord::dependency(
                    project,
                    package,
                    alias,
                    name,
                    requirement,
                    kind,
                    target,
                    source,
                    optional,
                    default_features,
                    features,
                )?,
            ));
        }
    }
    Ok(())
}

fn intern_source(
    project: [u8; 32],
    coordinate: String,
    sources: &mut BTreeMap<String, [u8; 32]>,
    facts: &mut Vec<([u8; 32], ProductSourceRecord)>,
) -> Result<Option<[u8; 32]>, String> {
    if coordinate.is_empty() {
        return Ok(None);
    }
    if let Some(key) = sources.get(&coordinate) {
        return Ok(Some(*key));
    }
    let key = product_dependency_source_key(project, &coordinate);
    facts.push((
        key,
        ProductSourceRecord::dependency_source(project, coordinate.clone())?,
    ));
    sources.insert(coordinate, key);
    Ok(Some(key))
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::expect_used)]
mod tests {
    use super::*;
    use backend_engine::{ProductSourceRelation, Relation};
    use std::time::Instant;

    #[test]
    fn metadata_becomes_independently_keyed_package_and_edge_facts() {
        let metadata: Metadata = serde_json::from_str(
            r#"{"packages":[{"id":"p","name":"demo","version":"1.0.0","manifest_path":"/tmp/demo/Cargo.toml","dependencies":[{"name":"serde","req":"^1","rename":"wire","optional":true,"uses_default_features":false,"features":["derive"],"target":"cfg(unix)","source":"registry+https://github.com/rust-lang/crates.io-index"}]}],"workspace_members":["p"]}"#,
        )
        .expect("metadata");
        let facts = facts_from_metadata(Path::new("/tmp/demo"), [7; 32], metadata).expect("facts");
        assert_eq!(facts.len(), 3);
        assert!(facts.iter().any(|(_, value)| matches!(value, ProductSourceRecord::Package { name, .. } if name.as_ref() == "demo")));
        assert!(facts.iter().any(|(_, value)| matches!(value, ProductSourceRecord::Dependency { alias, optional: true, default_features: false, .. } if alias.as_ref() == "wire")));
    }

    #[test]
    #[ignore = "bounded package graph stress probe"]
    fn stress_package_graph_reports_canonicalization_and_storage_costs() {
        const DEPENDENCIES: usize = 50_000;
        let dependencies = (0..DEPENDENCIES)
            .map(|index| Dependency {
                name: format!("package-{index:05}"),
                req: format!("^{}.{}", index % 10, index % 100),
                kind: Some(if index % 17 == 0 { "dev" } else { "normal" }.to_owned()),
                rename: (index % 7 == 0).then(|| format!("alias-{index:05}")),
                optional: index % 11 == 0,
                uses_default_features: index % 13 != 0,
                features: vec![format!("feature-{}", index % 32)],
                target: (index % 19 == 0).then(|| "cfg(unix)".to_owned()),
                source: Some("registry+https://github.com/rust-lang/crates.io-index".to_owned()),
                registry: None,
                path: None,
            })
            .collect();
        let metadata = Metadata {
            packages: vec![Package {
                id: "stress".to_owned(),
                name: "stress".to_owned(),
                version: "1.0.0".to_owned(),
                manifest_path: "/tmp/stress/Cargo.toml".to_owned(),
                description: Some("large graph".to_owned()),
                license: Some("MIT".to_owned()),
                repository: Some("https://example.invalid/stress".to_owned()),
                dependencies,
            }],
            workspace_members: vec!["stress".to_owned()],
        };
        let started = Instant::now();
        let facts =
            facts_from_metadata(Path::new("/tmp/stress"), [0x77; 32], metadata).expect("facts");
        let build_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(facts.len(), DEPENDENCIES + 2);
        let encoded_bytes = facts.iter().fold(0_u64, |total, (_, record)| {
            let mut encoded = Vec::new();
            ProductSourceRelation::encode_value(record, &mut encoded);
            total.saturating_add(encoded.len() as u64)
        });
        eprintln!(
            "package_graph_stress dependencies={DEPENDENCIES} build_ms={build_ms:.2} encoded_bytes={encoded_bytes} bytes_per_fact={:.1}",
            encoded_bytes as f64 / facts.len() as f64
        );
    }
}
