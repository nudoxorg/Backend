//! Bounded package membership from explicitly selected lockfile formats.
//!
//! These are package tuples, not dependency edges, registry metadata, active
//! environment resolution or authority to execute a package manager. Sources
//! without a representable package/local identity stay explicitly unresolved.
//! Coverage describes the selected file's package rows. In particular, go.mod
//! contributes declared minimum requirements, and requirements.txt contributes
//! pinned declarations without selecting markers for an active environment.
//! Neither is a solved transitive dependency closure or Go MVS build list.

use backend_library::{PackageReference, ProductText, ProjectUnresolvedLockMember};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum bytes read before parsing any supported project lockfile.
pub const MAX_PROJECT_LOCKFILE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum unique imported and unresolved package identities combined.
pub const MAX_PROJECT_LOCKFILE_MEMBERS: usize = backend_library::MAX_PROJECT_MEMBERS;
const MAX_ENTRIES: usize = 20_000;
const MAX_FIELD_BYTES: usize = 2048;

/// The format selected by an exact admitted basename, never by line contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockfileFormat {
    /// Cargo.lock, parsed by the shared Cargo browsing authority.
    Cargo,
    /// npm package-lock.json versions 1, 2 and 3.
    Npm,
    /// uv.lock schema 1.
    Uv,
    /// Poetry lock schemas 1.1, 2.0 and 2.1.
    Poetry,
    /// Pinned PEP 508 declarations in requirements.txt.
    Requirements,
    /// go.mod module requirements, including block syntax and replacements.
    GoMod,
}

/// Why an entire document cannot be admitted; no partial parse is published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockfileError {
    /// The basename has no implemented parser.
    UnsupportedFormat,
    /// The document advertises a schema outside this parser's contract.
    UnsupportedVersion,
    /// A document, entry count, identity or field exceeded its bound.
    Bound,
    /// The selected format is malformed or has unsupported syntax.
    Document(&'static str),
}

impl std::fmt::Display for LockfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFormat => f.write_str("unsupported project lockfile format; supported: Cargo.lock, package-lock.json, uv.lock, poetry.lock, pinned requirements.txt, go.mod"),
            Self::UnsupportedVersion => f.write_str("unsupported project lockfile schema version"),
            Self::Bound => f.write_str("project lockfile exceeds its byte, field, entry or member bound"),
            Self::Document(reason) => write!(f, "invalid project lockfile: {reason}"),
        }
    }
}
impl std::error::Error for LockfileError {}

/// A package coordinate or an actual format-provided local path witness.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LockedMember {
    /// Exact package ecosystem, name and release.
    Package(PackageReference),
    /// Contained relative path, interpreted against the selected lockfile parent.
    Workspace(String),
}

/// Whole-file inventory, including package rows that cannot be imported.
///
/// This covers the selected file's rows, not the installed environment or the
/// transitive dependency closure that a package manager might later resolve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockfileInventory {
    /// Deduplicated typed package identities in canonical order.
    pub members: Box<[LockedMember]>,
    /// Unavailable membership with original name/version/source, never fake PURLs.
    pub unresolved: Box<[ProjectUnresolvedLockMember]>,
}

impl LockfileFormat {
    /// Selects only formats backed by an actual parser.
    pub fn from_basename(name: &str) -> Result<Self, LockfileError> {
        match name {
            "Cargo.lock" => Ok(Self::Cargo),
            "package-lock.json" => Ok(Self::Npm),
            "uv.lock" => Ok(Self::Uv),
            "poetry.lock" => Ok(Self::Poetry),
            "requirements.txt" => Ok(Self::Requirements),
            "go.mod" => Ok(Self::GoMod),
            _ => Err(LockfileError::UnsupportedFormat),
        }
    }

    /// Parses one whole document without filesystem reads or package execution.
    pub fn parse(self, text: &str) -> Result<LockfileInventory, LockfileError> {
        if text.len() > MAX_PROJECT_LOCKFILE_BYTES {
            return Err(LockfileError::Bound);
        }
        let mut inventory = Inventory::default();
        match self {
            Self::Cargo => cargo(text, &mut inventory)?,
            Self::Npm => npm(text, &mut inventory)?,
            Self::Uv | Self::Poetry => python_lock(self, text, &mut inventory)?,
            Self::Requirements => requirements(text, &mut inventory)?,
            Self::GoMod => go_mod(text, &mut inventory)?,
        }
        Ok(LockfileInventory {
            members: inventory.members.into_iter().collect(),
            unresolved: inventory.unresolved.into_values().collect(),
        })
    }
}

#[derive(Default)]
struct Inventory {
    members: BTreeSet<LockedMember>,
    unresolved: BTreeMap<(String, Option<String>, Option<String>), ProjectUnresolvedLockMember>,
    entries: usize,
    payload_bytes: usize,
}
impl Inventory {
    fn entry(&mut self) -> Result<(), LockfileError> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(LockfileError::Bound);
        }
        Ok(())
    }
    fn bounded(&self) -> Result<(), LockfileError> {
        if self.members.len() + self.unresolved.len() > MAX_PROJECT_LOCKFILE_MEMBERS
            || self.payload_bytes > backend_library::MAX_PROJECT_MEMBER_PAYLOAD_BYTES
        {
            return Err(LockfileError::Bound);
        }
        Ok(())
    }
    fn package(&mut self, ecosystem: &str, name: &str, version: &str) -> Result<(), LockfileError> {
        let coordinate = package_coordinate(ecosystem, name, version)?;
        self.insert_package(coordinate, name)
    }
    fn insert_package(
        &mut self,
        coordinate: PackageReference,
        name: &str,
    ) -> Result<(), LockfileError> {
        let payload = coordinate.as_str().len().saturating_add(name.len());
        if self.members.insert(LockedMember::Package(coordinate)) {
            self.payload_bytes = self.payload_bytes.saturating_add(payload);
        }
        self.bounded()
    }
    fn workspace(&mut self, path: &str) -> Result<(), LockfileError> {
        field(path)?;
        if !contained_path(path) {
            return Err(LockfileError::Document(
                "local package path escapes the lockfile root",
            ));
        }
        if self
            .members
            .insert(LockedMember::Workspace(path.to_owned()))
        {
            self.payload_bytes = self
                .payload_bytes
                .saturating_add(path.len().saturating_mul(2));
        }
        self.bounded()
    }
    fn unresolved(
        &mut self,
        name: &str,
        version: Option<&str>,
        source: Option<&str>,
        reason: &'static str,
    ) -> Result<(), LockfileError> {
        field(name)?;
        if let Some(version) = version {
            field(version)?;
        }
        // Source evidence may be a structured, multiline TOML value. Admit
        // its bounded raw bytes separately from the public display text.
        let public_source = source.map(source_display).transpose()?;
        let source_payload = source.map_or(0, str::len).max(
            public_source
                .as_ref()
                .map_or(0, |value| value.as_str().len()),
        );
        let key = (
            name.to_owned(),
            version.map(str::to_owned),
            source.map(str::to_owned),
        );
        let record = ProjectUnresolvedLockMember {
            name: product(name)?,
            version: version.map(product).transpose()?,
            source: public_source,
            reason: ProductText::from_static(reason),
        };
        let payload = name.len() + version.map_or(0, str::len) + source_payload + reason.len();
        if let std::collections::btree_map::Entry::Vacant(entry) = self.unresolved.entry(key) {
            entry.insert(record);
            self.payload_bytes = self.payload_bytes.saturating_add(payload);
        }
        self.bounded()
    }
}
fn package_coordinate(
    ecosystem: &str,
    name: &str,
    version: &str,
) -> Result<PackageReference, LockfileError> {
    field(name)?;
    field(version)?;
    let valid_name = match ecosystem {
        "npm" => crate::registry::valid_npm_name(name),
        "pypi" => crate::registry::valid_pypi_name(name),
        "cargo" => {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        }
        "golang" => module_path(name),
        _ => false,
    };
    if !valid_name {
        return Err(LockfileError::Document("invalid package name"));
    }
    let version_syntax = if ecosystem == "pypi" {
        // Reuse the full pinned PEP 508/440 parser and its complexity bounds.
        let parsed =
            crate::python_requirement::parse_python_requirement(&format!("{name}=={version}"))
                .map_err(|_| LockfileError::Document("invalid pinned Python version"))?;
        if parsed.pinned_version.is_none() {
            return Err(LockfileError::Document("Python version is not pinned"));
        }
        None
    } else {
        Some(if ecosystem == "golang" {
            backend_advisory::VersionSyntax::Go
        } else {
            backend_advisory::VersionSyntax::Semver
        })
    };
    if let Some(syntax) = version_syntax {
        if !exact_release(version) {
            return Err(LockfileError::Document(
                "package version is not an exact release",
            ));
        }
        backend_advisory::normalize_version(syntax, version)
            .map_err(|_| LockfileError::Document("invalid pinned package version"))?;
    }
    // Preserve exact locked version spelling, including build/local metadata.
    PackageReference::parse(format!("pkg:{ecosystem}/{name}@{version}"))
        .map_err(|_| LockfileError::Document("invalid package coordinate"))
}

fn field(value: &str) -> Result<(), LockfileError> {
    if value.is_empty() || value.len() > MAX_FIELD_BYTES || value.chars().any(char::is_control) {
        Err(LockfileError::Bound)
    } else {
        Ok(())
    }
}
fn product(value: &str) -> Result<ProductText, LockfileError> {
    field(value)?;
    ProductText::new(value).map_err(|_| LockfileError::Bound)
}

// Unknown-source evidence is display data, never registry authority. Keep
// ordinary exact source spellings, but do not publish authentication material.
fn source_display(source: &str) -> Result<ProductText, LockfileError> {
    if source.is_empty() || source.len() > MAX_FIELD_BYTES {
        return Err(LockfileError::Bound);
    }
    let lower = source.to_ascii_lowercase();
    let sensitive = source.chars().any(char::is_control)
        || source.contains('?')
        || ["password", "credential", "token", "authorization"]
            .iter()
            .any(|word| lower.contains(word))
        || source.split("://").skip(1).any(|rest| {
            rest.split(['/', '"', '\'', ' '])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        });
    if sensitive {
        use sha2::{Digest, Sha256};
        product(&format!(
            "redacted-source-sha256:{:x}",
            Sha256::digest(source.as_bytes())
        ))
    } else {
        product(source)
    }
}
fn contained_path(value: &str) -> bool {
    value == "."
        || (!value.starts_with('/')
            && !value.contains(['\\', ':'])
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."))
}

fn cargo(text: &str, out: &mut Inventory) -> Result<(), LockfileError> {
    let packages = backend_library::browse::cargo_locked_packages(text)
        .map_err(|_| LockfileError::Document("unreadable Cargo.lock package inventory"))?;
    for package in packages {
        out.entry()?;
        package_coordinate("cargo", &package.name, &package.version)?;
        match package.source.as_deref() {
            Some("registry+https://github.com/rust-lang/crates.io-index" | "sparse+https://index.crates.io/") =>
                out.package("cargo", &package.name, &package.version)?,
            source => out.unresolved(&package.name, Some(&package.version), source,
                "Cargo.lock does not prove a default-registry identity or a local manifest path; add the local package explicitly")?,
        }
    }
    Ok(())
}

// Only semantically used fields are decoded. Derive rejects repeated fields;
// unique maps reject duplicate package locations rather than losing inventory.
#[derive(Deserialize)]
struct NpmLock {
    #[serde(rename = "lockfileVersion")]
    version: u64,
    packages: Option<UniqueMap<NpmPackage>>,
    #[serde(default)]
    dependencies: UniqueMap<NpmLegacyPackage>,
}
#[derive(Deserialize)]
struct NpmPackage {
    name: Option<String>,
    version: Option<String>,
    resolved: Option<String>,
    #[serde(default)]
    link: bool,
}
#[derive(Deserialize)]
struct NpmLegacyPackage {
    #[serde(flatten)]
    package: NpmPackage,
    dependencies: Option<UniqueMap<NpmLegacyPackage>>,
}
struct UniqueMap<T>(BTreeMap<String, T>);
impl<T> Default for UniqueMap<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for UniqueMap<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
            type Value = UniqueMap<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a unique bounded package map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, T>()? {
                    if entries.len() >= MAX_ENTRIES || key.len() > MAX_FIELD_BYTES {
                        return Err(serde::de::Error::custom("package map exceeds bound"));
                    }
                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate package map key"));
                    }
                }
                Ok(UniqueMap(entries))
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}

fn npm(text: &str, out: &mut Inventory) -> Result<(), LockfileError> {
    json_inventory_bound(text)?;
    let lock: NpmLock = serde_json::from_str(text)
        .map_err(|_| LockfileError::Document("unreadable package-lock.json"))?;
    match lock.version {
        1 => {
            if lock.packages.is_some() {
                return Err(LockfileError::Document(
                    "npm v1 has an unexpected packages table",
                ));
            }
            // npm v1 omits this table when its install tree has no children.
            // A present value still has to deserialize as a package map.
            let mut pending = vec![lock.dependencies];
            while let Some(entries) = pending.pop() {
                for (name, mut entry) in entries.0 {
                    out.entry()?;
                    npm_package(&name, &entry.package, out)?;
                    if let Some(children) = entry.dependencies.take() {
                        pending.push(children);
                    }
                }
            }
        }
        2 | 3 => {
            let entries = lock
                .packages
                .ok_or(LockfileError::Document("npm v2/v3 has no packages table"))?;
            for (location, package) in &entries.0 {
                out.entry()?;
                if !location.is_empty() && !relative_path(location) {
                    return Err(LockfileError::Document("invalid npm package location"));
                }
                if location.is_empty() {
                    continue;
                } // Project root, not an installed dependency.
                if package.link {
                    let target = package
                        .resolved
                        .as_deref()
                        .ok_or(LockfileError::Document("npm link has no target"))?;
                    let target_package = entries
                        .0
                        .get(target)
                        .ok_or(LockfileError::Document("npm link target is not recorded"))?;
                    if target_package.link || npm_install_name(target).is_some() {
                        return Err(LockfileError::Document(
                            "npm link target is not a workspace package",
                        ));
                    }
                    npm_workspace(target, target_package, out)?;
                    continue;
                }
                let Some(installed_name) = npm_install_name(location) else {
                    if location.split('/').any(|part| part == "node_modules") {
                        return Err(LockfileError::Document(
                            "invalid installed npm package name",
                        ));
                    }
                    // Real workspace manifests have no registry identity. Their
                    // path is sufficient even when their package version is absent.
                    npm_workspace(location, package, out)?;
                    continue;
                };
                npm_package(
                    package.name.as_deref().unwrap_or(installed_name),
                    package,
                    out,
                )?;
            }
            // npm v2's legacy dependencies tree is not a second inventory.
        }
        _ => return Err(LockfileError::UnsupportedVersion),
    }
    Ok(())
}
fn npm_install_name(location: &str) -> Option<&str> {
    if !contained_path(location) {
        return None;
    }
    location
        .strip_prefix("node_modules/")
        .or_else(|| location.rsplit_once("/node_modules/").map(|(_, name)| name))
        .map(|rest| {
            rest.rsplit_once("/node_modules/")
                .map_or(rest, |(_, name)| name)
        })
        .filter(|name| crate::registry::valid_npm_name(name))
}
fn relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FIELD_BYTES
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && !value.chars().any(char::is_control)
        && value.split('/').all(|part| !part.is_empty())
}
fn npm_workspace(
    path: &str,
    package: &NpmPackage,
    out: &mut Inventory,
) -> Result<(), LockfileError> {
    if contained_path(path) {
        return out.workspace(path);
    }
    out.unresolved(
        package.name.as_deref().unwrap_or(path),
        package.version.as_deref(),
        Some(path),
        "npm local package is outside the lockfile root; add its manifest path explicitly",
    )
}
fn npm_package(name: &str, package: &NpmPackage, out: &mut Inventory) -> Result<(), LockfileError> {
    if !crate::registry::valid_npm_name(name) {
        return Err(LockfileError::Document("invalid npm package name"));
    }
    let Some(version) = package.version.as_deref() else {
        return out.unresolved(name, None, package.resolved.as_deref(),
            "npm package has no locked release; optional or uninstalled peer membership is unavailable");
    };
    field(version)?;
    if version.starts_with("file:") || version.contains("://") || version.starts_with("git+") {
        return out.unresolved(
            name,
            Some(version),
            package.resolved.as_deref(),
            "npm non-registry dependency has no admitted package or local path identity",
        );
    }
    let (actual_name, actual_version) = match version.strip_prefix("npm:") {
        Some(alias) => alias
            .rsplit_once('@')
            .ok_or(LockfileError::Document("invalid npm alias"))?,
        None => (name, version),
    };
    let coordinate = package_coordinate("npm", actual_name, actual_version)?;
    match package.resolved.as_deref() {
        Some(source) => {
            field(source)?;
            let basename = actual_name
                .rsplit('/')
                .next()
                .ok_or(LockfileError::Document("invalid npm package name"))?;
            let expected = format!(
                "https://registry.npmjs.org/{actual_name}/-/{basename}-{actual_version}.tgz"
            );
            if source != expected {
                return out.unresolved(
                    name,
                    Some(version),
                    Some(source),
                    "npm dependency source does not prove a default npm registry release",
                );
            }
        }
        None => return out.unresolved(
            name,
            Some(version),
            None,
            "npm lock omits the resolved origin; a default npm registry identity is unavailable",
        ),
    }
    out.insert_package(coordinate, actual_name)
}

fn toml_string<'a>(value: &'a toml::Value, key: &str) -> Result<&'a str, LockfileError> {
    let value = value
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or(LockfileError::Document(
            "package field is missing or not a string",
        ))?;
    field(value)?;
    Ok(value)
}

// Count raw JSON syntax nodes before serde allocates package/metadata maps.
// This is only a memory preflight; serde remains the complete grammar authority.
fn json_inventory_bound(text: &str) -> Result<(), LockfileError> {
    let mut quoted = false;
    let mut escaped = false;
    let mut nodes = 0_usize;
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
            nodes += 1;
        } else if matches!(byte, b'{' | b'[' | b',' | b':') {
            nodes += 1;
        }
        if nodes > MAX_ENTRIES * 64 {
            return Err(LockfileError::Bound);
        }
    }
    Ok(())
}
fn python_lock(
    format: LockfileFormat,
    text: &str,
    out: &mut Inventory,
) -> Result<(), LockfileError> {
    let root: toml::Value = toml::from_str(text)
        .map_err(|_| LockfileError::Document("unreadable Python TOML lockfile"))?;
    match format {
        LockfileFormat::Uv => {
            if root.get("version").and_then(toml::Value::as_integer) != Some(1)
                || root
                    .get("revision")
                    .is_some_and(|value| !matches!(value.as_integer(), Some(1..=3)))
            {
                return Err(LockfileError::UnsupportedVersion);
            }
        }
        LockfileFormat::Poetry => {
            let metadata = root
                .get("metadata")
                .ok_or(LockfileError::Document("Poetry lock has no metadata"))?;
            if !matches!(
                toml_string(metadata, "lock-version")?,
                "1.1" | "2.0" | "2.1"
            ) {
                return Err(LockfileError::UnsupportedVersion);
            }
        }
        _ => return Err(LockfileError::UnsupportedFormat),
    }
    let packages =
        root.get("package")
            .and_then(toml::Value::as_array)
            .ok_or(LockfileError::Document(
                "Python lock has no package inventory",
            ))?;
    for package in packages {
        out.entry()?;
        let name = toml_string(package, "name")?;
        let source = package.get("source");
        if format == LockfileFormat::Uv {
            let source = source
                .and_then(toml::Value::as_table)
                .filter(|source| source.len() == 1)
                .ok_or(LockfileError::Document("uv package has no unique source"))?;
            if let Some(path) = source.get("editable").or_else(|| source.get("directory")) {
                python_workspace(
                    out,
                    name,
                    package.get("version").and_then(toml::Value::as_str),
                    path.as_str()
                        .ok_or(LockfileError::Document("uv local source is not a path"))?,
                )?;
                continue;
            }
            if let Some(path) = source.get("virtual") {
                let path = path
                    .as_str()
                    .ok_or(LockfileError::Document("uv virtual source is not a path"))?;
                if !contained_path(path) {
                    return Err(LockfileError::Document(
                        "uv virtual root escapes lockfile parent",
                    ));
                }
                // A virtual project groups dependencies; it is not a package.
                continue;
            }
            let version = toml_string(package, "version")?;
            if source
                .get("registry")
                .and_then(toml::Value::as_str)
                .is_some_and(|url| {
                    matches!(url, "https://pypi.org/simple" | "https://pypi.org/simple/")
                })
            {
                out.package("pypi", &python_name(name)?, version)?;
            } else {
                out.unresolved(
                    name,
                    Some(version),
                    Some(&toml::Value::Table(source.clone()).to_string()),
                    "uv package source has no default PyPI or proven workspace identity",
                )?;
            }
        } else {
            let version = toml_string(package, "version")?;
            if let Some(source) = source {
                let kind = toml_string(source, "type")?;
                if kind == "directory" {
                    python_workspace(out, name, Some(version), toml_string(source, "url")?)?;
                } else {
                    out.unresolved(
                        name,
                        Some(version),
                        Some(&source.to_string()),
                        "Poetry package source has no default PyPI or proven workspace identity",
                    )?;
                }
            } else {
                out.package("pypi", &python_name(name)?, version)?;
            }
        }
    }
    Ok(())
}
fn python_workspace(
    out: &mut Inventory,
    name: &str,
    version: Option<&str>,
    path: &str,
) -> Result<(), LockfileError> {
    if contained_path(path) {
        out.workspace(path)
    } else {
        out.unresolved(
            name,
            version,
            Some(path),
            "Python local package is outside the lockfile root; add its manifest path explicitly",
        )
    }
}
fn python_name(name: &str) -> Result<String, LockfileError> {
    if !crate::registry::valid_pypi_name(name) {
        return Err(LockfileError::Document("invalid Python package name"));
    }
    let parsed = crate::python_requirement::parse_python_requirement(name)
        .map_err(|_| LockfileError::Document("invalid Python package name"))?;
    Ok(parsed.package_name)
}
fn requirements(text: &str, out: &mut Inventory) -> Result<(), LockfileError> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.entry()?;
        // Requirements options, includes, URLs and continuations need their
        // own source/authority contract; they never become pretend pins.
        let line = line
            .split_once(" #")
            .map_or(line, |(requirement, _)| requirement)
            .trim();
        let parsed = crate::python_requirement::parse_python_requirement(line).map_err(|_| {
            LockfileError::Document("unsupported or invalid pinned requirements.txt entry")
        })?;
        let version = parsed.pinned_version.ok_or(LockfileError::Document(
            "requirements.txt entry is not exactly version-pinned",
        ))?;
        out.package("pypi", &parsed.package_name, &version)?;
    }
    Ok(())
}

fn module_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FIELD_BYTES
        && !value.starts_with('/')
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~'))
        })
}

fn go_version(value: &str) -> Result<(), LockfileError> {
    let release = value.strip_prefix('v').ok_or(LockfileError::Document(
        "Go requirement is not version-pinned",
    ))?;
    if !exact_release(release) {
        return Err(LockfileError::Document("invalid Go module version"));
    }
    backend_advisory::normalize_version(backend_advisory::VersionSyntax::Go, value)
        .map_err(|_| LockfileError::Document("invalid Go module version"))?;
    Ok(())
}

// The advisory comparison parser intentionally accepts abbreviated releases
// and ignores build suffixes. Lock membership needs a complete pinned spelling.
fn exact_release(value: &str) -> bool {
    let value = value.strip_prefix('v').unwrap_or(value);
    let (release, build) = value
        .split_once('+')
        .map_or((value, None), |(release, build)| (release, Some(build)));
    let (core, pre) = release
        .split_once('-')
        .map_or((release, None), |(core, pre)| (core, Some(pre)));
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let identifier = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    };
    core.split('.').count() == 3
        && core.split('.').all(numeric)
        && pre.is_none_or(|pre| {
            pre.split('.').all(|part| {
                identifier(part)
                    && (!part.bytes().all(|byte| byte.is_ascii_digit()) || numeric(part))
            })
        })
        && build.is_none_or(|build| build.split('.').all(identifier))
}

// go.mod has a line/block grammar. This lexer preserves quoted tokens and
// recognizes comments only outside strings; it never applies to other formats.
fn go_lines(text: &str) -> Result<Vec<Vec<String>>, LockfileError> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut chars = text.chars().peekable();
    let mut total = 0;
    while let Some(c) = chars.next() {
        if c == '\n' {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            continue;
        }
        let token = if c == '"' || c == '`' {
            let mut token = String::new();
            let mut closed = false;
            while let Some(next) = chars.next() {
                if next == c {
                    closed = true;
                    break;
                }
                if next == '\n' || next == '\r' {
                    return Err(LockfileError::Document("multiline Go string"));
                }
                if next == '\\' && c == '"' {
                    return Err(LockfileError::Document(
                        "escaped Go module tokens are unsupported",
                    ));
                }
                token.push(next);
                if token.len() > MAX_FIELD_BYTES {
                    return Err(LockfileError::Bound);
                }
            }
            if !closed {
                return Err(LockfileError::Document("unterminated Go string"));
            }
            token
        } else if matches!(c, '(' | ')') {
            c.to_string()
        } else {
            let mut token = c.to_string();
            while let Some(next) = chars.peek() {
                if next.is_whitespace() || matches!(next, '(' | ')') {
                    break;
                }
                token.push(
                    chars
                        .next()
                        .ok_or(LockfileError::Document("incomplete Go token"))?,
                );
                if token.len() > MAX_FIELD_BYTES {
                    return Err(LockfileError::Bound);
                }
            }
            token
        };
        total += 1;
        if total > MAX_ENTRIES * 8 {
            return Err(LockfileError::Bound);
        }
        line.push(token);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    Ok(lines)
}

fn go_mod(text: &str, out: &mut Inventory) -> Result<(), LockfileError> {
    let mut block: Option<String> = None;
    let mut module = None;
    let mut saw_go = false;
    let mut saw_toolchain = false;
    let mut requirements = BTreeMap::new();
    let mut replacements = BTreeMap::new();
    let mut exclusions = BTreeSet::new();
    for mut line in go_lines(text)? {
        out.entry()?;
        if line.as_slice() == [")"] {
            if block.take().is_none() {
                return Err(LockfileError::Document("unmatched Go block terminator"));
            }
            continue;
        }
        let directive = match &block {
            Some(directive) => directive.clone(),
            None => line.remove(0),
        };
        if line.as_slice() == ["("] {
            if block.is_some()
                || !matches!(
                    directive.as_str(),
                    "require" | "replace" | "exclude" | "retract" | "tool" | "godebug"
                )
            {
                return Err(LockfileError::Document("invalid Go directive block"));
            }
            block = Some(directive);
            continue;
        }
        if line.iter().any(|word| matches!(word.as_str(), "(" | ")")) {
            return Err(LockfileError::Document("invalid Go block syntax"));
        }
        let words: Vec<&str> = line.iter().map(String::as_str).collect();
        match (directive.as_str(), words.as_slice()) {
            ("module", [name]) if module.is_none() && module_path(name) => {
                module = Some((*name).to_owned());
            }
            ("go", [version]) if !saw_go && go_language_version(version) => {
                saw_go = true;
            }
            ("toolchain", [name])
                if !saw_toolchain
                    && (*name == "default"
                        || name.strip_prefix("go").is_some_and(go_language_version)) =>
            {
                saw_toolchain = true;
            }
            ("godebug", [setting])
                if setting.split_once('=').is_some_and(|(key, value)| {
                    let valid = |text: &str| {
                        !text.is_empty()
                            && !text.contains([',', '"', '`', '\''])
                            && !text.chars().any(char::is_whitespace)
                    };
                    valid(key) && valid(value)
                }) => {}
            ("tool", [name]) if module_path(name) => {}
            ("retract", _) if valid_go_retraction(&words) => {} // Own versions, not dependencies.
            ("require" | "exclude", [name, version]) if module_path(name) => {
                go_version(version)?;
                if directive == "exclude" {
                    exclusions.insert(((*name).to_owned(), (*version).to_owned()));
                } else if requirements
                    .insert((*name).to_owned(), (*version).to_owned())
                    .is_some()
                {
                    return Err(LockfileError::Document("duplicate Go requirement"));
                }
            }
            ("replace", _) => {
                let arrow = words
                    .iter()
                    .position(|word| *word == "=>")
                    .ok_or(LockfileError::Document("Go replace has no arrow"))?;
                let (old, old_version) = match &words[..arrow] {
                    [old] if module_path(old) => (*old, None),
                    [old, version] if module_path(old) => {
                        go_version(version)?;
                        (*old, Some((*version).to_owned()))
                    }
                    _ => return Err(LockfileError::Document("invalid Go replacement source")),
                };
                let target = match &words[arrow + 1..] {
                    [path] if path.starts_with('.') || path.starts_with('/') => {
                        ((*path).to_owned(), None)
                    }
                    [name, version] if module_path(name) => {
                        go_version(version)?;
                        ((*name).to_owned(), Some((*version).to_owned()))
                    }
                    _ => return Err(LockfileError::Document("invalid Go replacement target")),
                };
                if replacements
                    .insert((old.to_owned(), old_version), target)
                    .is_some()
                {
                    return Err(LockfileError::Document("duplicate Go replacement"));
                }
            }
            _ => {
                return Err(LockfileError::Document(
                    "invalid or unsupported Go directive",
                ));
            }
        }
    }
    if module.is_none() || block.is_some() {
        return Err(LockfileError::Document(
            "Go module declaration or block is incomplete",
        ));
    }
    for (name, version) in requirements {
        if exclusions.contains(&(name.clone(), version.clone())) {
            out.unresolved(&name, Some(&version), None, "go.mod excludes this required release; actual Go resolver selection is unavailable")?;
            continue;
        }
        let replacement = replacements
            .get(&(name.clone(), Some(version.clone())))
            .or_else(|| replacements.get(&(name.clone(), None)));
        match replacement {
            Some((target, Some(version))) => out.package("golang", target, version)?,
            Some((path, None)) => out.unresolved(
                &name,
                Some(&version),
                Some(path),
                "go.mod local replacement needs an explicit local manifest member",
            )?,
            None => out.package("golang", &name, &version)?,
        }
    }
    Ok(())
}

fn go_language_version(value: &str) -> bool {
    let (core, prerelease) = value
        .find(|c: char| c.is_ascii_alphabetic())
        .map_or((value, None), |at| (&value[..at], Some(&value[at..])));
    let number = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|c| c.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<&str> = core.split('.').collect();
    matches!(parts.len(), 2 | 3)
        && parts[0] != "0"
        && parts.iter().all(|part| number(part))
        && prerelease.is_none_or(|pre| {
            parts.len() == 2
                && pre
                    .strip_prefix("rc")
                    .or_else(|| pre.strip_prefix("beta"))
                    .is_some_and(|serial| serial != "0" && number(serial))
        })
}

fn valid_go_retraction(words: &[&str]) -> bool {
    let joined = words.join("");
    match joined
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        Some(range) => range
            .split_once(',')
            .is_some_and(|(low, high)| go_version(low).is_ok() && go_version(high).is_ok()),
        None => words.len() == 1 && go_version(&joined).is_ok(),
    }
}

#[cfg(test)]
#[path = "project_lockfile/tests.rs"]
mod tests;
