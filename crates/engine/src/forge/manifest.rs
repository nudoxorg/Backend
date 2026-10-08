use super::archive::ArchiveFile;
use super::*;

pub(super) fn discover_manifests(
    files: &[ArchiveFile],
    subdir: Option<&str>,
) -> Result<Vec<ForgePackageManifest>, ForgeRejectReason> {
    let mut results = Vec::new();
    let mut selected = BTreeMap::new();
    for file in files {
        let path = match subdir {
            Some(prefix) => {
                let prefix = prefix.trim_end_matches('/');
                let Some(path) = file
                    .path
                    .strip_prefix(prefix)
                    .and_then(|path| path.strip_prefix('/'))
                else {
                    continue;
                };
                path
            }
            None => file.path.as_ref(),
        };
        selected.insert(path, file);
    }
    let mut python_roots = std::collections::BTreeSet::new();
    for (path, file) in &selected {
        let filename = path.rsplit('/').next().unwrap_or(path);
        if crate::python_project::is_python_manifest(filename) {
            if file.bytes_len > MAX_MANIFEST_BYTES {
                return Err(ForgeRejectReason::Bounds);
            }
            python_roots.insert(path.rsplit_once('/').map_or("", |(parent, _)| parent));
            continue;
        }
        let Some(kind) = manifest_kind(path) else {
            continue;
        };
        if file.bytes_len > MAX_MANIFEST_BYTES {
            return Err(ForgeRejectReason::Bounds);
        }
        results.push(parse_manifest(kind, path, &file.bytes)?);
    }
    for root in python_roots {
        let metadata = crate::python_project::extract_python_project(|relative| {
            let path = if root.is_empty() {
                relative.to_owned()
            } else {
                format!("{root}/{relative}")
            };
            Ok(selected.get(path.as_str()).map(|file| file.bytes.clone()))
        })
        .map_err(|_| ForgeRejectReason::Manifest)?;
        let Some(metadata) = metadata else {
            continue;
        };
        let name = metadata.name.recorded().map(String::as_str);
        let version = metadata.version.recorded().map(String::as_str);
        let source = name.zip(version).and_then(|(name, version)| {
            PackageReference::parse(format!("pkg:pypi/{name}@{version}")).ok()
        });
        let dependencies = source.map_or_else(
            || {
                DependencyFacts::Unavailable(ProductText::from_static(
                    "Python manifest has no statically evidenced immutable package version",
                ))
            },
            |source| {
                crate::python_project::python_dependency_facts(
                    &metadata,
                    &source,
                    backend_library::PackageGraphSourceAuthority::Unattributed,
                    DependencyAuthority::ForgeManifest,
                    [0; 32],
                )
            },
        );
        let path = if root.is_empty() {
            metadata.manifest_path.clone()
        } else {
            format!("{root}/{}", metadata.manifest_path)
        };
        results.push(ForgePackageManifest {
            path: Arc::from(path),
            ecosystem: RegistryEcosystem::Pypi,
            name: product(name),
            version: product(version),
            dependencies,
            python_metadata: Some(metadata),
        });
    }
    results.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(results)
}

#[derive(Clone, Copy)]
enum ManifestKind {
    Cargo,
    Npm,
    Golang,
    Maven,
}

fn manifest_kind(path: &str) -> Option<ManifestKind> {
    match path.rsplit('/').next()? {
        "Cargo.toml" => Some(ManifestKind::Cargo),
        "package.json" => Some(ManifestKind::Npm),
        "go.mod" => Some(ManifestKind::Golang),
        "pom.xml" => Some(ManifestKind::Maven),
        _ => None,
    }
}

fn parse_manifest(
    kind: ManifestKind,
    path: &str,
    bytes: &[u8],
) -> Result<ForgePackageManifest, ForgeRejectReason> {
    let name_and_version = match kind {
        ManifestKind::Cargo => parse_cargo(bytes),
        ManifestKind::Npm => parse_npm(bytes),
        ManifestKind::Golang => parse_go(bytes),
        ManifestKind::Maven => parse_pom(bytes),
    }?;
    Ok(ForgePackageManifest {
        path: Arc::from(path),
        ecosystem: name_and_version.0,
        name: name_and_version.1,
        version: name_and_version.2,
        dependencies: name_and_version.3,
        python_metadata: None,
    })
}

type ManifestParts = (
    RegistryEcosystem,
    ForgeFact<ProductText>,
    ForgeFact<ProductText>,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
);

fn product(value: Option<&str>) -> ForgeFact<ProductText> {
    value.and_then(|value| ProductText::new(value).ok()).map_or(
        ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
        ForgeFact::Recorded,
    )
}

fn parse_toml(value: &[u8]) -> Result<toml::Value, ForgeRejectReason> {
    toml::from_str(std::str::from_utf8(value).map_err(|_| ForgeRejectReason::Manifest)?)
        .map_err(|_| ForgeRejectReason::Manifest)
}

fn parse_cargo(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let root = parse_toml(bytes)?;
    let package = root.get("package").and_then(toml::Value::as_table);
    let name = package
        .and_then(|table| table.get("name"))
        .and_then(toml::Value::as_str);
    let version = package
        .and_then(|table| table.get("version"))
        .and_then(toml::Value::as_str);
    let source = name.zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:cargo/{name}@{version}")).ok()
    });
    let dependencies = parse_toml_dependencies(&root, source, RegistryEcosystem::Cargo);
    Ok((
        RegistryEcosystem::Cargo,
        product(name),
        product(version),
        dependencies,
    ))
}

fn parse_toml_dependencies(
    root: &toml::Value,
    source: Option<PackageReference>,
    ecosystem: RegistryEcosystem,
) -> DependencyFacts<Box<[PackageDependencyRecord]>> {
    let Some(source) = source else {
        return DependencyFacts::Unavailable(ProductText::from_static(
            "manifest has no immutable package version",
        ));
    };
    let mut rows = Vec::new();
    for (table_name, scope) in [
        ("dependencies", DependencyScope::Runtime),
        ("dev-dependencies", DependencyScope::Development),
        ("build-dependencies", DependencyScope::Build),
    ] {
        let Some(table) = root.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, value) in table {
            let (requirement, declared_optional) = match value {
                toml::Value::String(value) => (value.clone(), false),
                toml::Value::Table(table) => (
                    table
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("*")
                        .to_owned(),
                    table
                        .get("optional")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(false),
                ),
                _ => continue,
            };
            if let Ok(target) = PackageDependencyTarget::new(ecosystem, name, requirement, None) {
                rows.push(PackageDependencyRecord::new(
                    source.clone(),
                    target,
                    scope,
                    backend_library::dependency_optional(scope, declared_optional),
                    DependencyEvidence {
                        authority: DependencyAuthority::ForgeManifest,
                        frontier: [0; 32],
                        provenance: *blake3::hash(root.to_string().as_bytes()).as_bytes(),
                    },
                ));
            }
        }
    }
    backend_library::admit_dependency_rows(
        crate::registry::coalesce_runtime_development_dependency_rows(rows),
    )
    .map_or_else(
        |_| {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest dependency rows exceed bounds",
            ))
        },
        DependencyFacts::Known,
    )
}

fn parse_npm(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ForgeRejectReason::Manifest)?;
    let name = value.get("name").and_then(Value::as_str);
    let version = value.get("version").and_then(Value::as_str);
    let source = name.zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:npm/{name}@{version}")).ok()
    });
    let mut rows = Vec::new();
    for (field, scope) in [
        ("dependencies", DependencyScope::Runtime),
        ("devDependencies", DependencyScope::Development),
        ("peerDependencies", DependencyScope::Peer),
        ("optionalDependencies", DependencyScope::Optional),
    ] {
        if let (Some(source), Some(table)) =
            (source.clone(), value.get(field).and_then(Value::as_object))
        {
            let declared_optional = matches!(scope, DependencyScope::Optional);
            for (name, requirement) in table {
                if let Some(requirement) = requirement
                    .as_str()
                    .and_then(|value| ProductText::new(value).ok())
                {
                    if let Ok(target) = PackageDependencyTarget::new(
                        RegistryEcosystem::Npm,
                        name,
                        requirement.as_str(),
                        None,
                    ) {
                        rows.push(PackageDependencyRecord::new(
                            source.clone(),
                            target,
                            scope,
                            backend_library::dependency_optional(scope, declared_optional),
                            DependencyEvidence {
                                authority: DependencyAuthority::ForgeManifest,
                                frontier: [0; 32],
                                provenance: *blake3::hash(bytes).as_bytes(),
                            },
                        ));
                    }
                }
            }
        }
    }
    let facts = source.map_or_else(
        || {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest has no immutable package version",
            ))
        },
        |_| {
            backend_library::admit_dependency_rows(
                crate::registry::coalesce_runtime_development_dependency_rows(rows),
            )
            .map_or_else(
                |_| {
                    DependencyFacts::Unavailable(ProductText::from_static(
                        "manifest dependency rows exceed bounds",
                    ))
                },
                DependencyFacts::Known,
            )
        },
    );
    Ok((
        RegistryEcosystem::Npm,
        product(name),
        product(version),
        facts,
    ))
}

fn parse_go(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let text = std::str::from_utf8(bytes).map_err(|_| ForgeRejectReason::Manifest)?;
    let module = text
        .lines()
        .find_map(|line| line.strip_prefix("module ").map(str::trim));
    let name = module;
    let facts = DependencyFacts::Unknown(ProductText::from_static(
        "go.mod dependencies retain module requirements after resolver selection",
    ));
    Ok((
        RegistryEcosystem::Golang,
        product(name),
        ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
        facts,
    ))
}

fn parse_pom(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let text = std::str::from_utf8(bytes).map_err(|_| ForgeRejectReason::Manifest)?;
    let value = |tag: &str| {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        text.split_once(&open)
            .and_then(|(_, rest)| rest.split_once(&close).map(|(value, _)| value.trim()))
    };
    let group = value("groupId");
    let artifact = value("artifactId");
    let name = group
        .zip(artifact)
        .map(|(group, artifact)| format!("{group}:{artifact}"));
    let version = value("version");
    let source = name.as_deref().zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:maven/{name}@{version}")).ok()
    });
    let facts = source.map_or_else(
        || {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest has no immutable package version",
            ))
        },
        |_| {
            DependencyFacts::Unknown(ProductText::from_static(
                "pom dependency scope is available after XML authority parsing",
            ))
        },
    );
    Ok((
        RegistryEcosystem::Maven,
        product(name.as_deref()),
        product(version),
        facts,
    ))
}

#[cfg(test)]
mod tests {
    use super::{ArchiveFile, ForgeFact, MAX_MANIFEST_BYTES, discover_manifests, parse_cargo, parse_npm};
    use backend_library::{DependencyFacts, DependencyScope, ProductText};
    use std::sync::Arc;

    #[test]
    fn forge_cargo_dev_and_normal_same_name_stays_runtime() {
        let manifest = r#"
            [package]
            name = "demo"
            version = "1.0.0"

            [dependencies]
            serde = "1"

            [dev-dependencies]
            serde = "1"
        "#;
        let (_, _, _, facts) = parse_cargo(manifest.as_bytes()).expect("parse");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "serde");
        assert_eq!(rows[0].scope, DependencyScope::Runtime);
        assert!(!rows[0].optional);
    }

    #[test]
    fn forge_cargo_dev_only_stays_development_and_optional_false() {
        let manifest = r#"
            [package]
            name = "demo"
            version = "1.0.0"

            [dev-dependencies]
            serde = "1"
        "#;
        let (_, _, _, facts) = parse_cargo(manifest.as_bytes()).expect("parse");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scope, DependencyScope::Development);
        assert!(!rows[0].optional);
    }

    #[test]
    fn forge_npm_dev_dependencies_vitest_is_development_optional_false() {
        let manifest = br#"{
            "name": "demo",
            "version": "1.0.0",
            "devDependencies": {"vitest": "^1.0.0"}
        }"#;
        let (_, _, _, facts) = parse_npm(manifest).expect("parse");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "vitest");
        assert_eq!(rows[0].scope, DependencyScope::Development);
        assert!(!rows[0].optional);
    }

    #[test]
    fn forge_npm_name_in_dependencies_and_dev_dependencies_stays_runtime() {
        let manifest = br#"{
            "name": "demo",
            "version": "1.0.0",
            "dependencies": {"lodash": "^4.0.0"},
            "devDependencies": {"lodash": "^4.0.0"}
        }"#;
        let (_, _, _, facts) = parse_npm(manifest).expect("parse");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "lodash");
        assert_eq!(rows[0].scope, DependencyScope::Runtime);
        assert!(!rows[0].optional);
    }

    #[test]
    fn forge_setup_cfg_is_parsed_as_ini_and_retains_runtime_requirements()
    -> Result<(), &'static str> {
        let manifest = br#"[metadata]
name = legacy-demo
version = 2.1.0

[options]
install_requires =
    requests>=2.31
"#;
        let files = [super::super::archive::ArchiveFile {
            path: Arc::from("setup.cfg"),
            bytes: manifest.to_vec(),
            bytes_len: manifest.len() as u64,
            mode: 0,
        }];
        let manifests = discover_manifests(&files, None).expect("valid setup.cfg");
        assert_eq!(manifests.len(), 1);
        assert_eq!(
            manifests[0].ecosystem,
            backend_library::RegistryEcosystem::Pypi
        );
        assert_eq!(
            manifests[0].name,
            super::ForgeFact::Recorded(
                backend_library::ProductText::new("legacy-demo").expect("name")
            )
        );
        assert_eq!(
            manifests[0].version,
            super::ForgeFact::Recorded(
                backend_library::ProductText::new("2.1.0").expect("version")
            )
        );
        let rows = match &manifests[0].dependencies {
            DependencyFacts::Known(rows) => rows,
            _ => return Err("valid setup.cfg PEP 508 requirement should produce graph facts"),
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "requests");
        assert_eq!(rows[0].target.requirement.as_str(), "requests>=2.31");
        let declarations = manifests[0]
            .python_metadata
            .as_ref()
            .expect("source metadata")
            .dependencies
            .recorded()
            .expect("original declarations");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].requirement, "requests>=2.31");
        assert_eq!(declarations[0].scope, DependencyScope::Runtime);
        Ok(())
    }

    #[test]
    fn forge_httpie_cfg_setup_and_literal_attribute_use_one_metadata_path()
    -> Result<(), &'static str> {
        let files = [
            (
                "setup.cfg",
                include_bytes!("../../tests/fixtures/httpie-5b604c37/setup.cfg").as_slice(),
            ),
            (
                "setup.py",
                include_bytes!("../../tests/fixtures/httpie-5b604c37/setup.py").as_slice(),
            ),
            (
                "httpie/__init__.py",
                include_bytes!("../../tests/fixtures/httpie-5b604c37/httpie/__init__.py")
                    .as_slice(),
            ),
        ]
        .into_iter()
        .map(|(path, bytes)| ArchiveFile {
            path: Arc::from(path),
            bytes: bytes.to_vec(),
            bytes_len: bytes.len() as u64,
            mode: 0,
        })
        .collect::<Vec<_>>();
        let manifests = discover_manifests(&files, None).expect("static HTTPie metadata");
        assert_eq!(
            manifests.len(),
            1,
            "setup.py and setup.cfg are one package declaration"
        );
        assert_eq!(manifests[0].path.as_ref(), "setup.cfg");
        assert_eq!(
            manifests[0].version,
            ForgeFact::Recorded(ProductText::new("3.2.4").expect("version"))
        );
        assert!(
            manifests[0]
                .python_metadata
                .as_ref()
                .expect("typed metadata")
                .documentation
                .recorded()
                .is_some()
        );
        let declarations = manifests[0]
            .python_metadata
            .as_ref()
            .expect("source metadata")
            .dependencies
            .recorded()
            .expect("original declarations");
        assert_eq!(
            declarations
                .iter()
                .filter(|row| row.scope == DependencyScope::Runtime)
                .count(),
            11
        );
        let rows = match &manifests[0].dependencies {
            DependencyFacts::Known(rows) => rows,
            _ => return Err("valid HTTPie requirements should produce graph facts"),
        };
        assert_eq!(rows.len(), declarations.len());
        assert_eq!(
            rows.iter()
                .filter(|row| row.scope == DependencyScope::Runtime)
                .count(),
            11
        );
        Ok(())
    }

    #[test]
    fn forge_setup_cfg_oversized_manifest_is_rejected_before_parsing() {
        let files = [super::super::archive::ArchiveFile {
            path: Arc::from("setup.cfg"),
            bytes: Vec::new(),
            bytes_len: MAX_MANIFEST_BYTES + 1,
            mode: 0,
        }];
        assert_eq!(
            discover_manifests(&files, None).expect_err("oversized setup.cfg"),
            super::ForgeRejectReason::Bounds
        );
    }
}
