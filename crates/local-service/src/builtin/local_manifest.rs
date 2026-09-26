//! Dependency edges declared by indexed local project manifests.

use backend_engine::{PackageDependencyRecord, PackageReference, RegistryPackageRecord};
use backend_library::{
    AdvisoryPackageDto, DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencySourceFacts, PackageDependencyTarget, ProductText, RegistryDownloadCount,
    RegistryEcosystem, RegistryFactAvailability, RegistryNativeMetadata, RegistryReleaseStanding,
    admit_dependency_rows,
};
use backend_semantic::vocabulary::{
    CSharpVersion, GoVersion, JavaRelease, LanguageProfile, PythonVersion, RustEdition,
    TypeScriptSource,
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
    let wrapper = name
        .to_str()
        .is_some_and(|name| name == "package" || (!version.is_empty() && name.ends_with(version)));
    if wrapper
        && only
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
    {
        return Ok(only.path());
    }
    Ok(directory.to_path_buf())
}

/// Returns the on-disk source root for one indexed project label.
pub(crate) fn indexed_package_source_root(
    label: &str,
    workspace: &Path,
) -> Result<PathBuf, String> {
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
        let path = entry.path().canonicalize().map_err(|error| {
            format!(
                "canonical staged archive {}: {error}",
                entry.path().display()
            )
        })?;
        if !path.is_dir() {
            continue;
        }
        if let Some(root) = staged_manifest_root(&path, coordinate.version(), label)? {
            return Ok(root);
        }
    }
    Err(format!(
        "indexed package {} has no staged source manifest",
        label
    ))
}

/// Reads the manifest package name for one indexed project label.
pub(crate) fn indexed_package_manifest_name(
    label: &str,
    workspace: &Path,
) -> Result<String, String> {
    let source_root = indexed_package_source_root(label, workspace)?;
    let manifest = require_local_manifest(&source_root)?;
    Ok(manifest.record.name.as_str().to_owned())
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
pub(crate) fn cargo_registry_record(project_root: &Path) -> Result<RegistryPackageRecord, String> {
    let (bytes, _) = read_cargo_package_table(project_root)?;
    let (name, version, source) = cargo_package_identity(project_root)?;
    manifest_record(
        RegistryEcosystem::Cargo,
        name.as_str(),
        version.as_str(),
        source,
        &bytes,
    )
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

/// Identity and language profile proved by one indexed local manifest.
#[derive(Debug)]
pub(crate) struct LocalPackageManifest {
    /// Closed compiler profile selected from the manifest.
    pub(crate) profile: LanguageProfile,
    /// Registry record whose coordinate is the manifest's own identity.
    pub(crate) record: RegistryPackageRecord,
}

/// Reads one supported manifest when the directory has a provable identity.
///
/// `Cargo.toml` stays authoritative, including a hard error for a virtual
/// workspace or a missing edition. Other ecosystems are read only when Cargo
/// is absent. A missing manifest or an unprovable version is `Ok(None)`.
///
/// # Errors
///
/// Returns an error when a present manifest is malformed or declares a
/// language release this profile cannot compile.
pub(crate) fn read_local_manifest(
    project_root: &Path,
) -> Result<Option<LocalPackageManifest>, String> {
    if project_root.join("Cargo.toml").is_file() {
        return Ok(Some(LocalPackageManifest {
            profile: cargo_language_profile(project_root)?,
            record: cargo_registry_record(project_root)?,
        }));
    }
    if project_root.join("package.json").is_file() {
        return read_npm_manifest(project_root);
    }
    if project_root.join("pyproject.toml").is_file() {
        return read_python_manifest(project_root);
    }
    if project_root.join("go.mod").is_file() {
        return read_go_manifest(project_root);
    }
    if project_root.join("pom.xml").is_file() {
        return read_maven_manifest(project_root);
    }
    read_nuget_root(project_root)
}

/// Reads one supported manifest, rejecting a directory that has none.
///
/// # Errors
///
/// Returns an error when the manifest is missing, malformed, or unprovable.
pub(crate) fn require_local_manifest(project_root: &Path) -> Result<LocalPackageManifest, String> {
    read_local_manifest(project_root)?.ok_or_else(|| {
        format!(
            "indexed project {} has no supported manifest",
            project_root.display()
        )
    })
}

/// Builds the registry record proved by one indexed local manifest.
///
/// # Errors
///
/// Returns an error when the manifest is missing, malformed, or unprovable.
pub(crate) fn local_registry_record(project_root: &Path) -> Result<RegistryPackageRecord, String> {
    Ok(require_local_manifest(project_root)?.record)
}

/// Returns the staged directory whose manifest coordinate equals `label`.
///
/// The hash directory, one archive wrapper, and a `package` child are the
/// only candidates. A versionless manifest does not borrow `version` from the
/// caller.
///
/// # Errors
///
/// Returns an error when a candidate manifest is malformed.
pub(crate) fn staged_manifest_root(
    directory: &Path,
    version: &str,
    label: &str,
) -> Result<Option<PathBuf>, String> {
    let mut candidates = vec![directory.to_path_buf()];
    let descended = resolve_staged_package_root(directory, version)?;
    if descended != directory {
        candidates.push(descended);
    }
    let package_dir = directory.join("package");
    if package_dir.is_dir() && !candidates.iter().any(|candidate| candidate == &package_dir) {
        candidates.push(package_dir);
    }
    for root in candidates {
        if let Some(manifest) = read_local_manifest(&root)?
            && manifest.record.coordinate.as_str() == label
        {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

fn manifest_record(
    ecosystem: RegistryEcosystem,
    name: &str,
    version: &str,
    coordinate: PackageReference,
    bytes: &[u8],
) -> Result<RegistryPackageRecord, String> {
    let native_metadata = RegistryNativeMetadata::unavailable(ecosystem, "local manifest");
    Ok(RegistryPackageRecord {
        coordinate,
        ecosystem,
        name: ProductText::new(name).map_err(|error| error.to_string())?,
        version: ProductText::new(version).map_err(|error| error.to_string())?,
        bytes: u64::try_from(bytes.len())
            .map_err(|_| "local manifest byte count overflow".to_owned())?,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::Unavailable(RegistryFactAvailability::Unsupported),
        facts_version: *blake3::hash(bytes).as_bytes(),
        native_metadata_version: native_metadata
            .identity()
            .map_err(|error| error.to_string())?,
        native_metadata,
        forge_sources: Box::new([]),
        advisory: AdvisoryPackageDto::unknown(),
    })
}

fn coordinate_for(
    ecosystem: RegistryEcosystem,
    path: &str,
    version: &str,
) -> Result<PackageReference, String> {
    let purl = format!("pkg:{}/{path}@{version}", ecosystem.package_type().as_str());
    PackageReference::parse(purl.clone())
        .map_err(|error| format!("local manifest identity {purl} is invalid: {error}"))
}

fn read_manifest_bytes(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("read local manifest {}: {error}", path.display()))
}

fn read_npm_manifest(project_root: &Path) -> Result<Option<LocalPackageManifest>, String> {
    let path = project_root.join("package.json");
    let bytes = read_manifest_bytes(&path)?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|error| format!("parse local manifest {}: {error}", path.display()))?;
    let Some(object) = value.as_object() else {
        return Err(format!(
            "local manifest {} is not a JSON object",
            path.display()
        ));
    };
    let name = match object.get("name") {
        Some(serde_json::Value::String(name)) if !name.is_empty() => name.as_str(),
        Some(_) => {
            return Err(format!(
                "local manifest {} has a non-string name",
                path.display()
            ));
        }
        None => {
            return Err(format!("local manifest {} omits name", path.display()));
        }
    };
    let version = match object.get("version") {
        Some(serde_json::Value::String(version))
            if !version.is_empty() && !version.contains("${") =>
        {
            version.as_str()
        }
        Some(serde_json::Value::String(_)) | Some(serde_json::Value::Null) | None => {
            return Ok(None);
        }
        Some(_) => {
            return Err(format!(
                "local manifest {} has a non-string version",
                path.display()
            ));
        }
    };
    let coordinate = coordinate_for(RegistryEcosystem::Npm, name, version)?;
    let record = manifest_record(RegistryEcosystem::Npm, name, version, coordinate, &bytes)?;
    Ok(Some(LocalPackageManifest {
        profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        record,
    }))
}

fn read_python_manifest(project_root: &Path) -> Result<Option<LocalPackageManifest>, String> {
    let path = project_root.join("pyproject.toml");
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let root = toml::from_str::<toml::Value>(text)
        .map_err(|error| format!("parse local manifest {}: {error}", path.display()))?;
    let Some(project) = root.get("project").and_then(toml::Value::as_table) else {
        return Ok(None);
    };
    let name = match project.get("name") {
        Some(toml::Value::String(name)) if !name.is_empty() => name.as_str(),
        Some(_) => {
            return Err(format!(
                "local manifest {} has a non-string project name",
                path.display()
            ));
        }
        None => {
            return Err(format!(
                "local manifest {} omits project.name",
                path.display()
            ));
        }
    };
    let Some(version) = python_version(project) else {
        return Ok(None);
    };
    let coordinate = coordinate_for(RegistryEcosystem::Pypi, name, version)?;
    let record = manifest_record(RegistryEcosystem::Pypi, name, version, coordinate, &bytes)?;
    Ok(Some(LocalPackageManifest {
        profile: python_profile(project)?,
        record,
    }))
}

fn python_version(project: &toml::Table) -> Option<&str> {
    match project.get("version") {
        Some(toml::Value::String(version)) if !version.is_empty() && !version.contains("${") => {
            Some(version.as_str())
        }
        _ => None,
    }
}

fn python_profile(project: &toml::Table) -> Result<LanguageProfile, String> {
    let Some(value) = project.get("requires-python") else {
        return Ok(LanguageProfile::Python(PythonVersion::Python314));
    };
    let Some(text) = value.as_str() else {
        return Err("local manifest requires-python is not a string".to_owned());
    };
    Ok(LanguageProfile::Python(python_version_token(text)))
}

fn python_version_token(text: &str) -> PythonVersion {
    for (token, version) in [
        ("3.10", PythonVersion::Python310),
        ("3.11", PythonVersion::Python311),
        ("3.12", PythonVersion::Python312),
        ("3.13", PythonVersion::Python313),
        ("3.14", PythonVersion::Python314),
    ] {
        if text.contains(token) {
            return version;
        }
    }
    PythonVersion::Python314
}

fn read_go_manifest(project_root: &Path) -> Result<Option<LocalPackageManifest>, String> {
    let path = project_root.join("go.mod");
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let module_path = go_module_path(text)?;
    let Some(version) = directory_version(project_root) else {
        return Ok(None);
    };
    let coordinate = coordinate_for(RegistryEcosystem::Golang, &module_path, &version)?;
    let PackageReference::Purl(url) = &coordinate else {
        return Err("local go manifest did not produce a package URL".to_owned());
    };
    if url.lineage_name() != module_path {
        return Err(format!(
            "local go manifest module {module_path} does not match {}",
            url.lineage_name()
        ));
    }
    let record = manifest_record(
        RegistryEcosystem::Golang,
        &module_path,
        &version,
        coordinate,
        &bytes,
    )?;
    Ok(Some(LocalPackageManifest {
        profile: LanguageProfile::Go(go_release(text)),
        record,
    }))
}

fn go_module_path(text: &str) -> Result<String, String> {
    for raw in text.lines() {
        let line = strip_line_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("module") else {
            return Err("go.mod does not begin with a module path".to_owned());
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            return Err("go.mod does not begin with a module path".to_owned());
        }
        let token = rest.trim();
        if token.split_whitespace().nth(1).is_some() {
            return Err("go.mod module path is not a single token".to_owned());
        }
        let token = token.trim_matches('"');
        if token.is_empty() || token.contains('"') || token.contains('@') {
            return Err("go.mod module path is not a literal".to_owned());
        }
        return Ok(token.to_owned());
    }
    Err("go.mod omits a module path".to_owned())
}

fn directory_version(directory: &Path) -> Option<String> {
    for candidate in directory.ancestors().take(2) {
        let Some(name) = candidate.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((_, version)) = name.rsplit_once('@') else {
            continue;
        };
        if !version.is_empty() && !version.contains('/') && !version.contains('@') {
            return Some(version.to_owned());
        }
    }
    None
}

fn go_release(text: &str) -> GoVersion {
    for raw in text.lines() {
        let line = strip_line_comment(raw).trim();
        let Some(rest) = line.strip_prefix("go ") else {
            continue;
        };
        let Some(token) = rest.split_whitespace().next() else {
            continue;
        };
        return classify_go(token);
    }
    GoVersion::Go125
}

fn classify_go(token: &str) -> GoVersion {
    let mut parts = token.split('.');
    let major = decimal_prefix(parts.next().unwrap_or(""));
    let minor = decimal_prefix(parts.next().unwrap_or(""));
    if major != 1 {
        return if major > 1 {
            GoVersion::Go125
        } else {
            GoVersion::Go122
        };
    }
    match minor {
        23 => GoVersion::Go123,
        24 => GoVersion::Go124,
        25.. => GoVersion::Go125,
        _ => GoVersion::Go122,
    }
}

fn decimal_prefix(text: &str) -> u32 {
    text.chars()
        .map_while(|digit| digit.to_digit(10))
        .fold(0, |value, digit| {
            value.saturating_mul(10).saturating_add(digit)
        })
}

fn strip_line_comment(line: &str) -> &str {
    line.split_once("//").map_or(line, |(code, _)| code)
}

fn read_maven_manifest(project_root: &Path) -> Result<Option<LocalPackageManifest>, String> {
    let path = project_root.join("pom.xml");
    let bytes = read_manifest_bytes(&path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let document = strip_xml_comments(text)?;
    let release = java_release(&document)?;
    let preamble = xml_identity_region(&document);
    let (parent, body) = match preamble.split_once("</parent>") {
        Some((parent, body)) => (Some(parent), body),
        None => (None, preamble),
    };
    let Some(artifact) = required_xml_literal(body, "artifactId", &path)? else {
        return Ok(None);
    };
    let Some(group) = fallback_xml_literal(body, parent, "groupId", &path)? else {
        return Ok(None);
    };
    let Some(version) = xml_version(body, parent)? else {
        return Ok(None);
    };
    let coordinate = coordinate_for(
        RegistryEcosystem::Maven,
        &format!("{group}/{artifact}"),
        version,
    )?;
    let record = manifest_record(
        RegistryEcosystem::Maven,
        &format!("{group}/{artifact}"),
        version,
        coordinate,
        &bytes,
    )?;
    Ok(Some(LocalPackageManifest {
        profile: LanguageProfile::Java(release),
        record,
    }))
}

fn java_release(document: &str) -> Result<JavaRelease, String> {
    for tag in ["maven.compiler.release", "maven.compiler.source"] {
        match element_text(document, tag)? {
            ManifestText::Missing => continue,
            ManifestText::Dynamic => {
                return Err(format!(
                    "local manifest {tag} is not a literal java release"
                ));
            }
            ManifestText::Literal(value) => {
                let normalized = if value == "1.8" { "8" } else { value };
                return JavaRelease::try_from(normalized).map_err(|rejected| {
                    format!("local manifest declares unsupported java release {rejected}")
                });
            }
        }
    }
    Ok(JavaRelease::Java25)
}

fn read_nuget_root(project_root: &Path) -> Result<Option<LocalPackageManifest>, String> {
    let mut specs = Vec::new();
    for entry in fs::read_dir(project_root)
        .map_err(|error| format!("read indexed project {}: {error}", project_root.display()))?
    {
        let path = entry.map_err(|error| error.to_string())?.path();
        let is_nuspec = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("nuspec"));
        if is_nuspec && path.is_file() {
            specs.push(path);
        }
    }
    specs.sort();
    match specs.as_slice() {
        [] => Ok(None),
        [path] => read_nuget_manifest(path),
        _ => Err(format!(
            "indexed project {} has more than one nuspec",
            project_root.display()
        )),
    }
}

fn read_nuget_manifest(path: &Path) -> Result<Option<LocalPackageManifest>, String> {
    let bytes = read_manifest_bytes(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("local manifest {} is not UTF-8", path.display()))?;
    let document = strip_xml_comments(text)?;
    let region = xml_identity_region(&document);
    let Some(name) = required_xml_literal(region, "id", path)? else {
        return Ok(None);
    };
    let ManifestText::Literal(version) = element_text(region, "version")? else {
        return Ok(None);
    };
    let coordinate = coordinate_for(RegistryEcosystem::Nuget, name, version)?;
    let record = manifest_record(RegistryEcosystem::Nuget, name, version, coordinate, &bytes)?;
    Ok(Some(LocalPackageManifest {
        profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
        record,
    }))
}

enum ManifestText<'text> {
    Missing,
    Dynamic,
    Literal(&'text str),
}

fn element_text<'text>(document: &'text str, tag: &str) -> Result<ManifestText<'text>, String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = document.find(&open) else {
        return Ok(ManifestText::Missing);
    };
    let Some(body) = document.get(start + open.len()..) else {
        return Err(format!("local manifest {tag} element is truncated"));
    };
    let Some(end) = body.find(&close) else {
        return Err(format!("local manifest {tag} element is not closed"));
    };
    let Some(text) = body.get(..end) else {
        return Err(format!("local manifest {tag} element is truncated"));
    };
    let text = text.trim();
    if text.contains('<') || text.contains('&') {
        return Err(format!("local manifest {tag} element is not plain text"));
    }
    if text.is_empty() || text.contains("${") {
        return Ok(ManifestText::Dynamic);
    }
    Ok(ManifestText::Literal(text))
}

fn required_xml_literal<'text>(
    document: &'text str,
    tag: &str,
    path: &Path,
) -> Result<Option<&'text str>, String> {
    match element_text(document, tag)? {
        ManifestText::Literal(value) => Ok(Some(value)),
        ManifestText::Dynamic => Ok(None),
        ManifestText::Missing => Err(format!("local manifest {} omits {tag}", path.display())),
    }
}

fn xml_version<'text>(
    body: &'text str,
    parent: Option<&'text str>,
) -> Result<Option<&'text str>, String> {
    match element_text(body, "version")? {
        ManifestText::Literal(value) => Ok(Some(value)),
        ManifestText::Dynamic => Ok(None),
        ManifestText::Missing => match parent {
            Some(parent) => match element_text(parent, "version")? {
                ManifestText::Literal(value) => Ok(Some(value)),
                ManifestText::Dynamic | ManifestText::Missing => Ok(None),
            },
            None => Ok(None),
        },
    }
}

fn fallback_xml_literal<'text>(
    body: &'text str,
    parent: Option<&'text str>,
    tag: &str,
    path: &Path,
) -> Result<Option<&'text str>, String> {
    match element_text(body, tag)? {
        ManifestText::Literal(value) => Ok(Some(value)),
        ManifestText::Dynamic => Ok(None),
        ManifestText::Missing => match parent {
            Some(parent) => required_xml_literal(parent, tag, path),
            None => required_xml_literal(body, tag, path),
        },
    }
}

fn xml_identity_region(document: &str) -> &str {
    const MARKERS: [&str; 4] = [
        "<dependencies",
        "<dependencyManagement",
        "<build",
        "<profiles",
    ];
    let end = MARKERS
        .iter()
        .filter_map(|marker| document.find(marker))
        .min()
        .unwrap_or(document.len());
    document.get(..end).unwrap_or(document)
}

fn strip_xml_comments(text: &str) -> Result<String, String> {
    let mut stripped = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        let Some(prefix) = rest.get(..start) else {
            return Err("local manifest has an unclosed XML comment".to_owned());
        };
        stripped.push_str(prefix);
        let Some(after) = rest.get(start + 4..) else {
            return Err("local manifest has an unclosed XML comment".to_owned());
        };
        let Some(end) = after.find("-->") else {
            return Err("local manifest has an unclosed XML comment".to_owned());
        };
        rest = after.get(end + 3..).unwrap_or("");
    }
    stripped.push_str(rest);
    Ok(stripped)
}

#[path = "local_manifest_dependencies.rs"]
mod dependencies;

/// Reads outgoing dependency facts from one indexed local manifest.
///
/// `Cargo.toml` stays authoritative. Other ecosystems are read only when Cargo
/// is absent. A directory with no provable package identity contributes no facts.
///
/// # Errors
///
/// Returns an error when a present manifest declares a malformed dependency.
pub(crate) fn local_dependency_facts(
    project_root: &Path,
) -> Result<Option<PackageDependencySourceFacts>, String> {
    dependencies::local_dependency_facts(project_root)
}

#[cfg(test)]
mod tests {
    use super::{
        LanguageProfile, PythonVersion, RustEdition, TypeScriptSource, cargo_language_profile,
        cargo_package_fields, directory_version, indexed_package_source_root, read_local_manifest,
    };
    use backend_semantic::vocabulary::{GoVersion, JavaRelease};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-local-manifest-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    fn write(path: &Path, bytes: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, bytes).expect("fixture manifest");
    }

    #[test]
    fn local_manifest_npm_python_and_nuget_identity() {
        let root = fixture("npm-python-nuget");
        let npm = root.join("npm");
        write(
            &npm.join("package.json"),
            r#"{"name":"left-pad","version":"1.3.0"}"#,
        );
        let npm_manifest = read_local_manifest(&npm).expect("npm").expect("identity");
        assert_eq!(
            npm_manifest.record.coordinate.as_str(),
            "pkg:npm/left-pad@1.3.0"
        );
        assert_eq!(
            npm_manifest.profile,
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript)
        );

        let scoped = root.join("scoped");
        write(
            &scoped.join("package.json"),
            r#"{"name":"@types/node","version":"20.0.0"}"#,
        );
        let scoped_manifest = read_local_manifest(&scoped)
            .expect("scoped")
            .expect("identity");
        assert_eq!(
            scoped_manifest.record.coordinate.as_str(),
            "pkg:npm/@types/node@20.0.0"
        );

        let python = root.join("python");
        write(
            &python.join("pyproject.toml"),
            "[project]\nname = \"attrs\"\nversion = \"24.2.0\"\nrequires-python = \">=3.12\"\n",
        );
        let python_manifest = read_local_manifest(&python)
            .expect("python")
            .expect("identity");
        assert_eq!(
            python_manifest.record.coordinate.as_str(),
            "pkg:pypi/attrs@24.2.0"
        );
        assert_eq!(
            python_manifest.profile,
            LanguageProfile::Python(PythonVersion::Python312)
        );

        let omitted = root.join("python-omitted");
        write(
            &omitted.join("pyproject.toml"),
            "[project]\nname = \"attrs\"\nversion = \"24.2.0\"\n",
        );
        let omitted_manifest = read_local_manifest(&omitted)
            .expect("omitted")
            .expect("identity");
        assert_eq!(
            omitted_manifest.profile,
            LanguageProfile::Python(PythonVersion::Python314)
        );

        let nuget = root.join("nuget");
        write(
            &nuget.join("AutoMapper.nuspec"),
            "<package><metadata><id>AutoMapper</id><version>13.0.0</version></metadata></package>",
        );
        let nuget_manifest = read_local_manifest(&nuget)
            .expect("nuget")
            .expect("identity");
        assert_eq!(
            nuget_manifest.record.coordinate.as_str(),
            "pkg:nuget/AutoMapper@13.0.0"
        );
        assert_eq!(
            nuget_manifest.profile,
            LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14)
        );

        write(&root.join("broken").join("package.json"), "{");
        let broken = read_local_manifest(&root.join("broken"));
        assert!(broken.is_err(), "malformed package.json must fail closed");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_manifest_go_version_comes_from_the_directory_name() {
        let root = fixture("go");
        let versioned = root.join("errors@v0.9.1");
        write(
            &versioned.join("go.mod"),
            "module github.com/pkg/errors\n\ngo 1.22\n",
        );
        let manifest = read_local_manifest(&versioned)
            .expect("go")
            .expect("identity");
        assert_eq!(
            manifest.record.coordinate.as_str(),
            "pkg:golang/github.com/pkg/errors@v0.9.1"
        );
        assert_eq!(manifest.profile, LanguageProfile::Go(GoVersion::Go122));

        let bare = root.join("content-hash");
        write(
            &bare.join("go.mod"),
            "module github.com/pkg/errors\n\ngo 1.21\n",
        );
        assert!(
            read_local_manifest(&bare).expect("bare go").is_none(),
            "a content-hash directory must not invent a module version"
        );

        let newer = root.join("mod@v1.2.3");
        write(&newer.join("go.mod"), "module example.com/mod\n\ngo 1.26\n");
        let newer_manifest = read_local_manifest(&newer)
            .expect("newer")
            .expect("identity");
        assert_eq!(
            newer_manifest.profile,
            LanguageProfile::Go(GoVersion::Go125)
        );
        assert!(directory_version(&bare).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_manifest_maven_parent_and_dynamic_version() {
        let root = fixture("maven");
        let parented = root.join("parented");
        write(
            &parented.join("pom.xml"),
            r#"<project>
              <parent>
                <groupId>com.google.guava</groupId>
                <artifactId>guava-parent</artifactId>
                <version>33.0.0-jre</version>
              </parent>
              <artifactId>guava</artifactId>
              <properties><maven.compiler.release>17</maven.compiler.release></properties>
            </project>"#,
        );
        let manifest = read_local_manifest(&parented)
            .expect("maven")
            .expect("identity");
        assert_eq!(
            manifest.record.coordinate.as_str(),
            "pkg:maven/com.google.guava/guava@33.0.0-jre"
        );
        assert_eq!(manifest.profile, LanguageProfile::Java(JavaRelease::Java17));

        let java8 = root.join("java8");
        write(
            &java8.join("pom.xml"),
            r#"<project><groupId>com.example</groupId><artifactId>demo</artifactId><version>1.0.0</version><maven.compiler.source>1.8</maven.compiler.source></project>"#,
        );
        let java8_manifest = read_local_manifest(&java8)
            .expect("java8")
            .expect("identity");
        assert_eq!(
            java8_manifest.profile,
            LanguageProfile::Java(JavaRelease::Java8)
        );

        let omitted = root.join("omitted-release");
        write(
            &omitted.join("pom.xml"),
            r#"<project><groupId>com.example</groupId><artifactId>demo</artifactId><version>1.0.0</version></project>"#,
        );
        let omitted_manifest = read_local_manifest(&omitted)
            .expect("omitted")
            .expect("identity");
        assert_eq!(
            omitted_manifest.profile,
            LanguageProfile::Java(JavaRelease::Java25)
        );

        let dynamic = root.join("dynamic");
        write(
            &dynamic.join("pom.xml"),
            r#"<project><groupId>com.example</groupId><artifactId>demo</artifactId><version>${revision}</version></project>"#,
        );
        assert!(read_local_manifest(&dynamic).expect("dynamic").is_none());
        let missing_version = root.join("missing-version");
        write(
            &missing_version.join("pom.xml"),
            r#"<project><groupId>com.example</groupId><artifactId>demo</artifactId></project>"#,
        );
        assert!(
            read_local_manifest(&missing_version)
                .expect("missing version")
                .is_none()
        );

        let unknown = root.join("unknown");
        write(
            &unknown.join("pom.xml"),
            r#"<project><groupId>com.example</groupId><artifactId>demo</artifactId><version>1.0.0</version><maven.compiler.release>9</maven.compiler.release></project>"#,
        );
        assert!(read_local_manifest(&unknown).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_manifest_cargo_edition_stays_authoritative() {
        let root = fixture("cargo");
        write(
            &root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
        );
        write(
            &root.join("package.json"),
            r#"{"name":"not-cargo","version":"9.9.9"}"#,
        );
        let missing_edition = read_local_manifest(&root);
        assert!(
            missing_edition
                .expect_err("missing edition")
                .contains("edition")
        );

        write(
            &root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        let manifest = read_local_manifest(&root)
            .expect("cargo")
            .expect("identity");
        assert_eq!(manifest.record.coordinate.as_str(), "pkg:cargo/demo@1.0.0");
        assert_eq!(
            manifest.profile,
            LanguageProfile::Rust(RustEdition::Rust2021)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_manifest_staged_scan_skips_foreign_and_unprovable_trees() {
        let workspace = fixture("staging");
        let staging = workspace.join("registry").join("registry-staging");
        write(
            &staging.join("hash-npm").join("package.json"),
            r#"{"name":"left-pad","version":"1.3.0"}"#,
        );
        write(
            &staging.join("hash-cargo").join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        write(
            &staging.join("hash-pom").join("pom.xml"),
            r#"<project><groupId>com.example</groupId><artifactId>demo</artifactId><version>${revision}</version></project>"#,
        );
        write(
            &staging.join("hash-go").join("errors@v0.9.1").join("go.mod"),
            "module github.com/pkg/errors\n\ngo 1.23\n",
        );
        write(
            &staging.join("hash-bare-go").join("go.mod"),
            "module github.com/pkg/errors\n\ngo 1.23\n",
        );

        let cargo = indexed_package_source_root("pkg:cargo/demo@1.0.0", &workspace)
            .expect("cargo survives a leading non-cargo tree");
        assert_eq!(
            cargo,
            staging
                .join("hash-cargo")
                .canonicalize()
                .expect("cargo root")
        );
        let npm = indexed_package_source_root("pkg:npm/left-pad@1.3.0", &workspace).expect("npm");
        assert_eq!(
            npm,
            staging.join("hash-npm").canonicalize().expect("npm root")
        );
        let golang =
            indexed_package_source_root("pkg:golang/github.com/pkg/errors@v0.9.1", &workspace)
                .expect("golang");
        assert_eq!(
            golang,
            staging
                .join("hash-go")
                .join("errors@v0.9.1")
                .canonicalize()
                .expect("go root")
        );
        assert!(
            indexed_package_source_root("pkg:golang/github.com/pkg/errors@v0.9.2", &workspace)
                .is_err()
        );
        let _ = fs::remove_dir_all(workspace);
    }

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
