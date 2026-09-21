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
    for (table_name, scope, optional_default) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("dev-dependencies", DependencyScope::Development, true),
        ("build-dependencies", DependencyScope::Build, false),
    ] {
        let Some(table) = root.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, value) in table {
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
            if let Ok(target) = PackageDependencyTarget::new(ecosystem, name, requirement, None) {
                rows.push(PackageDependencyRecord::new(
                    source.clone(),
                    target,
                    scope,
                    optional,
                    DependencyEvidence {
                        authority: DependencyAuthority::ForgeManifest,
                        frontier: [0; 32],
                        provenance: *blake3::hash(root.to_string().as_bytes()).as_bytes(),
                    },
                ));
            }
        }
    }
    backend_library::admit_dependency_rows(rows).map_or_else(
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
    for (field, scope, optional) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("devDependencies", DependencyScope::Development, true),
        ("peerDependencies", DependencyScope::Peer, false),
        ("optionalDependencies", DependencyScope::Optional, true),
    ] {
        if let (Some(source), Some(table)) =
            (source.clone(), value.get(field).and_then(Value::as_object))
        {
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
                            optional,
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
