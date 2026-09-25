//! Dependency edges declared by indexed local project manifests.

use backend_engine::{PackageDependencyRecord, PackageReference, RegistryPackageRecord};
use backend_library::{
    admit_dependency_rows, AdvisoryPackageDto, DependencyAuthority, DependencyEvidence,
    DependencyFacts, DependencyScope, PackageDependencySourceFacts, PackageDependencyTarget,
    ProductText, RegistryDownloadCount, RegistryEcosystem, RegistryFactAvailability,
    RegistryNativeMetadata, RegistryReleaseStanding,
};
use std::path::Path;

fn read_cargo_package_table(project_root: &Path) -> Result<(Vec<u8>, toml::Value), String> {
    let manifest = project_root.join("Cargo.toml");
    let bytes = std::fs::read(&manifest)
        .map_err(|error| format!("read local manifest {}: {error}", manifest.display()))?;
    let root = toml::from_str::<toml::Value>(
        std::str::from_utf8(&bytes).map_err(|_| "local manifest is not UTF-8".to_owned())?,
    )
    .map_err(|error| format!("parse local manifest {}: {error}", manifest.display()))?;
    Ok((bytes, root))
}

fn cargo_package_fields(
    project_root: &Path,
) -> Result<(ProductText, ProductText, PackageReference), String> {
    let (_, root) = read_cargo_package_table(project_root)?;
    let manifest = project_root.join("Cargo.toml");
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
    let source = PackageReference::parse(format!("pkg:cargo/{name}@{version}")).map_err(|_| {
        format!(
            "local manifest {} has an invalid package identity",
            manifest.display()
        )
    })?;
    Ok((
        ProductText::new(name).map_err(|error| error.to_string())?,
        ProductText::new(version).map_err(|error| error.to_string())?,
        source,
    ))
}

/// Reads the canonical package identity declared by one indexed Cargo manifest.
pub(crate) fn cargo_package_identity(
    project_root: &Path,
) -> Result<(ProductText, ProductText, PackageReference), String> {
    cargo_package_fields(project_root)
}

/// Builds one registry package record from an indexed local Cargo manifest.
pub(crate) fn cargo_registry_record(
    project_root: &Path,
) -> Result<RegistryPackageRecord, String> {
    let (bytes, _) = read_cargo_package_table(project_root)?;
    let (name, version, source) = cargo_package_fields(project_root)?;
    let native_metadata = RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "local manifest");
    Ok(RegistryPackageRecord {
        coordinate: source,
        ecosystem: RegistryEcosystem::Cargo,
        name,
        version,
        bytes: u64::try_from(bytes.len()).map_err(|_| "local manifest byte count overflow".to_owned())?,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
        facts_version: *blake3::hash(&bytes).as_bytes(),
        native_metadata_version: native_metadata
            .identity()
            .map_err(|error| error.to_string())?,
        native_metadata,
        forge_sources: Box::new([]),
        advisory: AdvisoryPackageDto::unknown(),
    })
}

/// Reads one local Cargo manifest and returns its outgoing dependency facts.
pub(crate) fn cargo_dependency_facts(
    project_root: &Path,
) -> Result<Option<PackageDependencySourceFacts>, String> {
    let (bytes, root) = read_cargo_package_table(project_root)?;
    let manifest = project_root.join("Cargo.toml");
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
