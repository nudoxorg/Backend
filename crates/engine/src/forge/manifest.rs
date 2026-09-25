use super::archive::ArchiveFile;
use super::*;

pub(super) fn discover_manifests(
    files: &[ArchiveFile],
    subdir: Option<&str>,
) -> Result<Vec<ForgePackageManifest>, ForgeRejectReason> {
    let mut results = Vec::new();
    for file in files {
        if file.bytes_len > MAX_MANIFEST_BYTES {
            return Err(ForgeRejectReason::Bounds);
        }
        let path = match subdir {
            Some(prefix) => {
                let prefix = prefix.trim_end_matches('/');
                let Some(path) = file.path.strip_prefix(prefix) else {
                    continue;
                };
                let Some(path) = path.strip_prefix('/') else {
                    continue;
                };
                path
            }
            None => file.path.as_ref(),
        };
        let Some(kind) = manifest_kind(path) else {
            continue;
        };
        results.push(parse_manifest(kind, path, &file.bytes)?);
    }
    results.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(results)
}

#[derive(Clone, Copy)]
enum ManifestKind {
    Cargo,
    Npm,
    Pypi,
    Golang,
    Maven,
}

fn manifest_kind(path: &str) -> Option<ManifestKind> {
    match path.rsplit('/').next()? {
        "Cargo.toml" => Some(ManifestKind::Cargo),
        "package.json" => Some(ManifestKind::Npm),
        "pyproject.toml" | "setup.cfg" => Some(ManifestKind::Pypi),
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
        ManifestKind::Pypi => parse_pyproject(bytes),
        ManifestKind::Golang => parse_go(bytes),
        ManifestKind::Maven => parse_pom(bytes),
    }?;
    Ok(ForgePackageManifest {
        path: Arc::from(path),
        ecosystem: name_and_version.0,
        name: name_and_version.1,
        version: name_and_version.2,
        dependencies: name_and_version.3,
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
    backend_library::admit_dependency_rows(backend_library::collapse_dependency_rows(rows))
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
            backend_library::admit_dependency_rows(backend_library::collapse_dependency_rows(rows))
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

fn parse_pyproject(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let root = parse_toml(bytes)?;
    let project = root.get("project").and_then(toml::Value::as_table);
    let name = project
        .and_then(|table| table.get("name"))
        .and_then(toml::Value::as_str);
    let version = project
        .and_then(|table| table.get("version"))
        .and_then(toml::Value::as_str);
    let source = name.zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:pypi/{name}@{version}")).ok()
    });
    let mut rows = Vec::new();
    if let (Some(source), Some(requirements)) = (
        source.clone(),
        project
            .and_then(|table| table.get("dependencies"))
            .and_then(toml::Value::as_array),
    ) {
        for requirement in requirements.iter().filter_map(toml::Value::as_str) {
            let name = requirement
                .split(['=', '<', '>', '!', '~', ';'])
                .next()
                .unwrap_or(requirement)
                .trim();
            if let Ok(target) =
                PackageDependencyTarget::new(RegistryEcosystem::Pypi, name, requirement, None)
            {
                rows.push(PackageDependencyRecord::new(
                    source.clone(),
                    target,
                    DependencyScope::Runtime,
                    false,
                    DependencyEvidence {
                        authority: DependencyAuthority::ForgeManifest,
                        frontier: [0; 32],
                        provenance: *blake3::hash(bytes).as_bytes(),
                    },
                ));
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
            backend_library::admit_dependency_rows(rows).map_or_else(
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
        RegistryEcosystem::Pypi,
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
    use super::{parse_cargo, parse_npm};
    use backend_library::{DependencyFacts, DependencyScope};

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
}
