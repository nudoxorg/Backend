//! Dependency edges declared by indexed local project manifests.

use backend_engine::{PackageDependencyRecord, PackageReference, RegistryPackageRecord};
use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
use backend_library::{
    admit_dependency_rows, AdvisoryPackageDto, DependencyAuthority, DependencyEvidence,
    DependencyFacts, DependencyScope, PackageDependencySourceFacts, PackageDependencyTarget,
    ProductText, RegistryDownloadCount, RegistryEcosystem, RegistryFactAvailability,
    RegistryNativeMetadata, RegistryReleaseStanding,
};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Reads `field` from the nearest ancestor manifest's `[workspace.package]`
/// table, for a member manifest that writes `field.workspace = true`
/// instead of a literal value (cargo's own inheritance, which the raw TOML
/// read below does not otherwise follow).
fn workspace_package_field(project_root: &Path, field: &str) -> Option<String> {
    let mut dir = Some(project_root);
    while let Some(current) = dir {
        let candidate = current.join("Cargo.toml");
        if let Ok(bytes) = std::fs::read(&candidate) {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                if let Ok(root) = toml::from_str::<toml::Value>(text) {
                    if let Some(value) = root
                        .get("workspace")
                        .and_then(toml::Value::as_table)
                        .and_then(|workspace| workspace.get("package"))
                        .and_then(toml::Value::as_table)
                        .and_then(|package| package.get(field))
                        .and_then(toml::Value::as_str)
                    {
                        return Some(value.to_owned());
                    }
                }
            }
        }
        dir = current.parent();
    }
    None
}

/// Reads a string field of `[package]`, following `field.workspace = true`
/// up to the enclosing workspace's `[workspace.package]` table when the
/// member manifest inherits it instead of declaring it literally.
fn package_string_field(
    package: &toml::value::Table,
    project_root: &Path,
    manifest: &Path,
    field: &str,
) -> Result<String, String> {
    match package.get(field) {
        Some(toml::Value::String(value)) => Ok(value.clone()),
        Some(toml::Value::Table(table))
            if table.get("workspace").and_then(toml::Value::as_bool) == Some(true) =>
        {
            workspace_package_field(project_root, field).ok_or_else(|| {
                format!(
                    "local manifest {} inherits package.{field} from the workspace, but no ancestor manifest declares [workspace.package].{field}",
                    manifest.display()
                )
            })
        }
        _ => Err(format!("local manifest {} omits package.{field}", manifest.display())),
    }
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
    let name = package_string_field(package, project_root, &manifest, "name")?;
    let version = package_string_field(package, project_root, &manifest, "version")?;
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

fn resolve_staged_package_root(directory: &Path, version: &str) -> Result<PathBuf, String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("read staged archive {}: {error}", directory.display()))?
        .map(|entry| entry.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, String>>()?;
    if entries.len() != 1 {
        return Ok(directory.to_path_buf());
    }
    let only = entries.pop().expect("single staged archive entry");
    let name = only.file_name();
    let wrapper = name.to_str().is_some_and(|name| {
        name == "package" || (!version.is_empty() && name.ends_with(version))
    });
    if wrapper && only.file_type().map_err(|error| error.to_string())?.is_dir() {
        return Ok(only.path());
    }
    Ok(directory.to_path_buf())
}

/// Returns the on-disk source root for one indexed project label.
pub(crate) fn indexed_package_source_root(label: &str, workspace: &Path) -> Result<PathBuf, String> {
    if !label.starts_with("pkg:") {
        let root = Path::new(label);
        if !root.is_dir() {
            return Err(format!("indexed project {} is not a directory", label));
        }
        return Ok(root.to_path_buf());
    }
    let package = PackageReference::parse(label).map_err(|error| error.to_string())?;
    let PackageReference::Purl(coordinate) = package else {
        return Err(format!(
            "indexed registry project requires a pinned package URL: {}",
            label
        ));
    };
    let staging_root = workspace.join("registry").join("registry-staging");
    if !staging_root.is_dir() {
        return Err(format!(
            "registry staging is missing for indexed package {}",
            label
        ));
    }
    for entry in fs::read_dir(&staging_root)
        .map_err(|error| format!("read registry staging {}: {error}", staging_root.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry
            .path()
            .canonicalize()
            .map_err(|error| format!("canonical staged archive {}: {error}", entry.path().display()))?;
        if !path.is_dir() {
            continue;
        }
        let package_root = resolve_staged_package_root(&path, coordinate.version())?;
        let (_, _, manifest) = cargo_package_fields(&package_root)?;
        if manifest.as_str() == label {
            return Ok(package_root);
        }
    }
    Err(format!(
        "indexed package {} has no staged source manifest",
        label
    ))
}

/// Reads the manifest package name for one indexed project label.
pub(crate) fn indexed_package_manifest_name(label: &str, workspace: &Path) -> Result<String, String> {
    let source_root = indexed_package_source_root(label, workspace)?;
    let (name, _, _) = cargo_package_identity(&source_root)?;
    Ok(name.as_str().to_owned())
}

/// Reads the canonical package identity declared by one indexed Cargo manifest.
pub(crate) fn cargo_package_identity(
    project_root: &Path,
) -> Result<(ProductText, ProductText, PackageReference), String> {
    cargo_package_fields(project_root)
}

/// Reads the Rust language profile declared by one indexed Cargo manifest.
pub(crate) fn cargo_language_profile(project_root: &Path) -> Result<LanguageProfile, String> {
    let (_, root) = read_cargo_package_table(project_root)?;
    let manifest = project_root.join("Cargo.toml");
    let package = root
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("local manifest {} has no [package] table", manifest.display()))?;
    let edition = package_string_field(package, project_root, &manifest, "edition")?;
    match edition.as_str() {
        "2021" => Ok(LanguageProfile::Rust(RustEdition::Rust2021)),
        "2024" => Ok(LanguageProfile::Rust(RustEdition::Rust2024)),
        other => Err(format!(
            "local manifest {} declares unsupported rust edition {other}",
            manifest.display()
        )),
    }
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
    let name = package_string_field(package, project_root, &manifest, "name")?;
    let version = package_string_field(package, project_root, &manifest, "version")?;
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

#[cfg(test)]
mod tests {
    use super::{cargo_language_profile, cargo_package_fields};
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
    use std::fs;
    use std::path::PathBuf;

    /// A scratch workspace, removed and recreated fresh: a `name` unique to
    /// the calling test plus the process id, since these tests share no
    /// fixture directory and never run the same name twice.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "backend-local-service-manifest-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch workspace dir");
        dir
    }

    /// `crates/present/Cargo.toml` (a real workspace member) declares
    /// `version.workspace = true` and `edition.workspace = true`: ordinary
    /// cargo inheritance, resolved by cargo itself from the enclosing
    /// workspace's `[workspace.package]` table. Before `workspace_package_field`/
    /// `package_string_field` existed, `cargo_package_fields` read
    /// `package.version` with `toml::Value::as_str`, which returned `None`
    /// for the inherited table value `{ workspace = true }` and the read
    /// failed with "omits package.version" as if the field were absent
    /// outright — deterministically breaking every desktop-harness fixture
    /// boot that indexes a real, workspace-inheriting local project.
    ///
    /// Reverting the fix (restoring the plain
    /// `package.get("version").and_then(toml::Value::as_str)` reads) fails
    /// this test with exactly that message:
    ///
    /// ```text
    /// thread '...' panicked at crates/local-service/src/builtin/local_manifest.rs:...:
    /// workspace-inherited fields resolve: "local manifest
    /// .../member/Cargo.toml omits package.version"
    /// ```
    #[test]
    fn workspace_inherited_version_and_edition_resolve_from_the_ancestor_workspace() {
        let root = scratch("workspace-inherit");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\n\n[workspace.package]\nversion = \"9.9.9\"\nedition = \"2024\"\n",
        )
        .expect("write root manifest");
        let member = root.join("member");
        fs::create_dir_all(&member).expect("member dir");
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"inherits-fine\"\nversion.workspace = true\nedition.workspace = true\n",
        )
        .expect("write member manifest");

        let (name, version, _source) =
            cargo_package_fields(&member).expect("workspace-inherited fields resolve");
        assert_eq!(name.as_str(), "inherits-fine");
        assert_eq!(version.as_str(), "9.9.9");

        let profile =
            cargo_language_profile(&member).expect("workspace-inherited edition resolves");
        assert_eq!(profile, LanguageProfile::Rust(RustEdition::Rust2024));

        let _ = fs::remove_dir_all(&root);
    }

    /// A member that inherits a field but has no ancestor `[workspace.package]`
    /// table at all still fails, with a message that names the real problem
    /// (no ancestor declares it) rather than a generic "omits" — the literal
    /// field really is missing everywhere it could come from.
    #[test]
    fn workspace_inherited_field_with_no_declaring_ancestor_fails_by_name() {
        let root = scratch("workspace-inherit-orphan");
        // No `[workspace]` table anywhere above `member`: nothing declares
        // `[workspace.package]`.
        let member = root.join("member");
        fs::create_dir_all(&member).expect("member dir");
        fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"orphaned\"\nversion.workspace = true\n",
        )
        .expect("write member manifest");

        let error = cargo_package_fields(&member).expect_err("no workspace declares the version");
        assert!(
            error.contains("inherits package.version") && error.contains("no ancestor"),
            "{error}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
