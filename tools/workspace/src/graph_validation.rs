//! Cargo metadata ownership, dependency, and cycle validation.

use super::dag::{DagPackage, PackageDag};
use super::{
    Dependency, LEGACY_OWNER_DIRECTORIES, MAX_METADATA_MEMBERS, MAX_METADATA_PACKAGES,
    ManifestIndex, Metadata, NameIndex, PRODUCT_OWNER_DIRECTORIES, Package, Violation,
    canonical_dag,
};
use std::collections::{BTreeMap, BTreeSet};

/// Validates already-decoded Cargo metadata.
///
/// # Errors
///
/// Returns every package, ownership, dependency, cycle, and legacy-member
/// violation found in the metadata.
pub fn validate(metadata: &Metadata, require_complete: bool) -> Result<(), Vec<Violation>> {
    if metadata.packages.len() > MAX_METADATA_PACKAGES {
        return Err(vec![Violation::InvalidMetadata(format!(
            "metadata has more than {MAX_METADATA_PACKAGES} packages"
        ))]);
    }
    if metadata.workspace_members.len() > MAX_METADATA_MEMBERS {
        return Err(vec![Violation::InvalidMetadata(format!(
            "metadata has more than {MAX_METADATA_MEMBERS} workspace members"
        ))]);
    }
    let dag = match canonical_dag() {
        Ok(dag) => dag,
        Err(error) => return Err(vec![Violation::InvalidDag(error)]),
    };
    let mut violations = member_violations(metadata);
    let member_ids = workspace_member_ids(metadata);
    let (by_name, by_manifest) =
        selected_package_indexes(metadata, &member_ids, dag, &mut violations);
    validate_ownership(
        &by_name,
        &by_manifest,
        dag,
        require_complete,
        &mut violations,
    );
    let graph = validate_dependencies(&by_name, dag, &mut violations);

    if let Some(cycle) = find_cycle(&graph) {
        violations.push(Violation::DependencyCycle { cycle });
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn member_violations(metadata: &Metadata) -> Vec<Violation> {
    let package_ids: BTreeSet<&str> = metadata
        .packages
        .iter()
        .map(|package| package.id.as_str())
        .collect();
    metadata
        .workspace_members
        .iter()
        .filter(|member| !package_ids.contains(member.as_str()))
        .cloned()
        .map(Violation::UnknownWorkspaceMember)
        .collect()
}

pub(crate) fn workspace_member_ids(metadata: &Metadata) -> BTreeSet<&str> {
    metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect()
}

fn selected_package_indexes<'a>(
    metadata: &'a Metadata,
    member_ids: &BTreeSet<&str>,
    dag: &PackageDag,
    violations: &mut Vec<Violation>,
) -> (NameIndex<'a>, ManifestIndex<'a>) {
    let mut by_name = BTreeMap::new();
    let mut by_manifest = BTreeMap::new();
    let mut seen_members = BTreeSet::new();

    for member in &metadata.workspace_members {
        if !seen_members.insert(member.as_str()) {
            violations.push(Violation::DuplicateWorkspaceMember(member.clone()));
        }
    }

    for package in metadata
        .packages
        .iter()
        .filter(|package| member_ids.contains(package.id.as_str()))
    {
        by_name
            .entry(package.name.as_str())
            .or_insert_with(Vec::new)
            .push(package);
        by_manifest
            .entry(normalized_path(&package.manifest_path))
            .or_insert_with(Vec::new)
            .push(package);
        validate_package_path(package, metadata.workspace_root.as_deref(), dag, violations);
    }

    (by_name, by_manifest)
}

fn validate_package_path(
    package: &Package,
    workspace_root: Option<&str>,
    dag: &PackageDag,
    violations: &mut Vec<Violation>,
) {
    if is_legacy(&workspace_relative_components(
        &package.manifest_path,
        workspace_root,
    )) {
        violations.push(Violation::LegacyWorkspaceMember {
            package: package.name.clone(),
            path: package.manifest_path.clone(),
        });
    }

    let inside_workspace = workspace_root.is_none_or(|root| {
        normalized_components(&package.manifest_path).starts_with(&normalized_components(root))
    });
    if let Some(spec) = package_spec(dag, &package.name)
        && (!inside_workspace
            || !matches_manifest_path(&package.manifest_path, workspace_root, &spec.manifest_path))
    {
        violations.push(Violation::InvalidOwnerPath {
            package: package.name.clone(),
            path: package.manifest_path.clone(),
        });
    }

    if let Some(expected) = spec_for_manifest(&package.manifest_path, workspace_root, dag)
        && expected.name != package.name
    {
        violations.push(Violation::OwnerPathCollision {
            package: package.name.clone(),
            expected: expected.name.clone(),
            path: package.manifest_path.clone(),
        });
    }

    if package_spec(dag, &package.name).is_none()
        && is_product_owner_path(&package.manifest_path, workspace_root)
    {
        violations.push(Violation::UnexpectedProductPackage {
            package: package.name.clone(),
            path: package.manifest_path.clone(),
        });
    }
}

fn validate_ownership(
    by_name: &BTreeMap<&str, Vec<&Package>>,
    by_manifest: &BTreeMap<String, Vec<&Package>>,
    dag: &PackageDag,
    require_complete: bool,
    violations: &mut Vec<Violation>,
) {
    for (path, packages) in by_manifest {
        if packages.len() > 1 {
            violations.push(Violation::DuplicateManifestPath {
                path: path.clone(),
                packages: packages
                    .iter()
                    .map(|package| package.name.clone())
                    .collect(),
            });
        }
    }

    for spec in &dag.packages {
        match by_name.get(spec.name.as_str()) {
            None if require_complete => {
                violations.push(Violation::MissingPackage(spec.name.clone()));
            }
            Some(matches) if matches.len() > 1 => {
                violations.push(Violation::DuplicatePackage(spec.name.clone()));
            }
            _ => {}
        }
    }
}

fn validate_dependencies(
    by_name: &BTreeMap<&str, Vec<&Package>>,
    dag: &PackageDag,
    violations: &mut Vec<Violation>,
) -> BTreeMap<String, BTreeSet<String>> {
    let product_names: BTreeSet<&str> = dag
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let mut graph: BTreeMap<String, BTreeSet<String>> = dag
        .packages
        .iter()
        .map(|package| (package.name.clone(), BTreeSet::new()))
        .collect();

    for spec in &dag.packages {
        let Some(packages) = by_name.get(spec.name.as_str()) else {
            continue;
        };
        let permitted: BTreeSet<&str> = spec.dependencies.iter().map(String::as_str).collect();
        for package in packages {
            let actual: BTreeSet<&str> = package
                .dependencies
                .iter()
                .map(|dependency| dependency_package_name(dependency, &product_names))
                .filter(|dependency| dependency.starts_with("backend-"))
                .collect();
            validate_dependency_set(&spec.name, &permitted, &actual, &dag.core_names, violations);
            validate_layer_edges(&spec.name, &actual, dag, violations);
            add_product_edges(&spec.name, &actual, &product_names, &mut graph);
        }
    }
    graph
}

fn validate_layer_edges(
    package: &str,
    actual: &BTreeSet<&str>,
    dag: &PackageDag,
    violations: &mut Vec<Violation>,
) {
    let Some(from) = package_spec(dag, package) else {
        return;
    };
    for dependency in actual {
        let Some(to) = package_spec(dag, dependency) else {
            continue;
        };
        if to.order >= from.order || layer_rank(&to.layer) > layer_rank(&from.layer) {
            violations.push(Violation::LayerViolation {
                from: package.to_owned(),
                to: (*dependency).to_owned(),
            });
        }
    }
}

fn layer_rank(layer: &str) -> u8 {
    match layer {
        "core" => 0,
        "frontend" | "extension" => 1,
        "application" => 2,
        _ => u8::MAX,
    }
}

fn validate_dependency_set(
    package: &str,
    permitted: &BTreeSet<&str>,
    actual: &BTreeSet<&str>,
    core_names: &BTreeSet<String>,
    violations: &mut Vec<Violation>,
) {
    for dependency in actual {
        if !permitted.contains(dependency) {
            if core_names.contains(*dependency) && core_names.contains(package) {
                violations.push(Violation::ForbiddenCoreEdge {
                    from: package.to_owned(),
                    to: (*dependency).to_owned(),
                });
            } else {
                violations.push(Violation::ForbiddenBackendEdge {
                    from: package.to_owned(),
                    to: (*dependency).to_owned(),
                });
            }
        }
    }

    for dependency in permitted.difference(actual) {
        if core_names.contains(package) && core_names.contains(*dependency) {
            violations.push(Violation::MissingCoreEdge {
                from: package.to_owned(),
                to: (*dependency).to_owned(),
            });
        } else {
            violations.push(Violation::MissingBackendEdge {
                from: package.to_owned(),
                to: (*dependency).to_owned(),
            });
        }
    }
}

fn add_product_edges(
    package: &str,
    actual: &BTreeSet<&str>,
    product_names: &BTreeSet<&str>,
    graph: &mut BTreeMap<String, BTreeSet<String>>,
) {
    if let Some(edges) = graph.get_mut(package) {
        for dependency in actual {
            if product_names.contains(dependency) {
                edges.insert((*dependency).to_owned());
            }
        }
    }
}

fn package_spec<'a>(dag: &'a PackageDag, name: &str) -> Option<&'a DagPackage> {
    dag.packages.iter().find(|spec| spec.name == name)
}

fn spec_for_manifest<'a>(
    path: &str,
    workspace_root: Option<&str>,
    dag: &'a PackageDag,
) -> Option<&'a DagPackage> {
    let inside_workspace = workspace_root
        .is_none_or(|root| normalized_components(path).starts_with(&normalized_components(root)));
    if !inside_workspace {
        return None;
    }
    dag.packages
        .iter()
        .find(|spec| matches_manifest_path(path, workspace_root, &spec.manifest_path))
}

fn dependency_package_name<'a>(
    dependency: &'a Dependency,
    product_names: &BTreeSet<&str>,
) -> &'a str {
    // In Cargo metadata v1, `name` is the resolved package and `rename` is
    // the dependency alias.  A few hand-authored fixtures use the alternate
    // `package` spelling, so honor it first; retain the fixture compatibility
    // fallback only when the alias is itself the known product name.
    if let Some(package) = dependency
        .package
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        return package;
    }
    if product_names.contains(dependency.name.as_str()) {
        return dependency.name.as_str();
    }
    if let Some(rename) = dependency
        .rename
        .as_deref()
        .filter(|name| product_names.contains(name))
    {
        return rename;
    }
    dependency.name.as_str()
}

pub(crate) fn normalized_path(path: &str) -> String {
    normalized_components(path).join("/")
}

fn normalized_components(path: &str) -> Vec<String> {
    // Cargo normally emits host-native absolute paths.  Treat backslashes as
    // separators as well so a metadata fixture made on another host cannot
    // evade the owner or legacy checks.
    let mut components = Vec::new();
    for raw in path.replace('\\', "/").split('/') {
        match raw {
            "" | "." => {}
            ".." => {
                if components.last().is_some_and(|part| part != "..") {
                    components.pop();
                } else {
                    components.push("..".to_owned());
                }
            }
            value => components.push(value.to_owned()),
        }
    }
    components
}

fn workspace_relative_components(path: &str, workspace_root: Option<&str>) -> Vec<String> {
    let path = normalized_components(path);
    let Some(root) = workspace_root.map(normalized_components) else {
        return path;
    };
    if path.starts_with(&root) {
        path[root.len()..].to_vec()
    } else {
        // A manifest outside the reported root is already suspicious, but the
        // owner-path check will provide the precise error.  Keep the full path
        // here so a legacy member cannot hide behind a mismatched root.
        path
    }
}

fn matches_manifest_suffix(path: &str, expected: &str) -> bool {
    let parts = normalized_components(path);
    let expected = normalized_components(expected);
    parts.len() >= expected.len()
        && parts[parts.len() - expected.len()..]
            .iter()
            .map(String::as_str)
            .eq(expected.iter().map(String::as_str))
}

fn matches_manifest_path(path: &str, workspace_root: Option<&str>, expected: &str) -> bool {
    if let Some(root) = workspace_root {
        workspace_relative_components(path, Some(root))
            .iter()
            .map(String::as_str)
            .eq(normalized_components(expected).iter().map(String::as_str))
    } else {
        matches_manifest_suffix(path, expected)
    }
}

fn is_legacy(parts: &[String]) -> bool {
    parts
        .iter()
        .any(|part| LEGACY_OWNER_DIRECTORIES.iter().any(|legacy| part == legacy))
}

fn is_product_owner_path(path: &str, workspace_root: Option<&str>) -> bool {
    let parts = workspace_relative_components(path, workspace_root);
    if workspace_root.is_some() {
        parts.first().is_some_and(|part| {
            PRODUCT_OWNER_DIRECTORIES
                .iter()
                .any(|product_root| part == product_root)
        })
    } else {
        parts.iter().any(|part| {
            PRODUCT_OWNER_DIRECTORIES
                .iter()
                .any(|product_root| part == product_root)
        })
    }
}

pub(crate) fn find_cycle(graph: &BTreeMap<String, BTreeSet<String>>) -> Option<Vec<String>> {
    let mut state = BTreeMap::new();
    let mut stack = Vec::new();
    for node in graph.keys() {
        if state.get(node).copied().unwrap_or_default() == 0
            && let Some(cycle) = find_cycle_from(node, graph, &mut state, &mut stack)
        {
            return Some(cycle);
        }
    }
    None
}

fn find_cycle_from(
    node: &str,
    graph: &BTreeMap<String, BTreeSet<String>>,
    state: &mut BTreeMap<String, u8>,
    stack: &mut Vec<String>,
) -> Option<Vec<String>> {
    state.insert(node.to_owned(), 1);
    stack.push(node.to_owned());

    if let Some(dependencies) = graph.get(node) {
        for dependency in dependencies {
            match state.get(dependency).copied().unwrap_or_default() {
                0 => {
                    if let Some(cycle) = find_cycle_from(dependency, graph, state, stack) {
                        return Some(cycle);
                    }
                }
                1 => {
                    let start = stack.iter().position(|entry| entry == dependency)?;
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(dependency.clone());
                    return Some(cycle);
                }
                _ => {}
            }
        }
    }

    stack.pop();
    state.insert(node.to_owned(), 2);
    None
}
