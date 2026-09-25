//! Dependency edges declared by indexed local project manifests.

use backend_engine::{PackageDependencyRecord, PackageReference};
use backend_library::{
    admit_dependency_rows, DependencyAuthority, DependencyEvidence, DependencyFacts,
    DependencyScope, PackageDependencySourceFacts, PackageDependencyTarget, RegistryEcosystem,
};
use std::path::Path;

/// Reads one local Cargo manifest and returns its outgoing dependency facts.
pub(crate) fn cargo_dependency_facts(
    project_root: &Path,
) -> Result<Option<PackageDependencySourceFacts>, String> {
    let manifest = project_root.join("Cargo.toml");
    let bytes = std::fs::read(&manifest)
        .map_err(|error| format!("read local manifest {}: {error}", manifest.display()))?;
    let root = toml::from_str::<toml::Value>(
        std::str::from_utf8(&bytes).map_err(|_| "local manifest is not UTF-8".to_owned())?,
    )
    .map_err(|error| format!("parse local manifest {}: {error}", manifest.display()))?;
    let package = root
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("local manifest {} has no [package] table", manifest.display()))?;
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("local manifest {} omits package.name", manifest.display()))?;
    let version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("local manifest {} omits package.version", manifest.display()))?;
    let source = PackageReference::parse(format!("pkg:cargo/{name}@{version}"))
        .map_err(|_| format!("local manifest {} has an invalid package identity", manifest.display()))?;
    let provenance = *blake3::hash(&bytes).as_bytes();
    let frontier = *blake3::hash(project_root.as_os_str().as_encoded_bytes()).as_bytes();
    let mut rows = Vec::new();
    for (table_name, scope, optional_default) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("dev-dependencies", DependencyScope::Development, true),
        ("build-dependencies", DependencyScope::Build, false),
    ] {
        let Some(table) = root.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (dependency_name, value) in table {
            let (requirement, optional) = match value {
                toml::Value::String(value) => (value.clone(), optional_default),
                toml::Value::Table(table) => (
                    table
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("*")
                        .to_owned(),
                    table
                        .get("optional")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(optional_default),
                ),
                _ => continue,
            };
            let target = PackageDependencyTarget::new(
                RegistryEcosystem::Cargo,
                dependency_name,
                requirement,
                None,
            )
            .map_err(|error| format!("admit local dependency {dependency_name}: {error:?}"))?;
            rows.push(PackageDependencyRecord::new(
                source.clone(),
                target,
                scope,
                optional,
                DependencyEvidence {
                    authority: DependencyAuthority::LocalManifest,
                    frontier,
                    provenance,
                },
            ));
        }
    }
    let facts = admit_dependency_rows(rows)
        .map(DependencyFacts::Known)
        .map_err(|error| format!("local manifest dependency rows are invalid: {error:?}"))?;
    Ok(Some((source, facts)))
}
