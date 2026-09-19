//! Canonical package DAG parsing and internal consistency checks.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::{find_cycle, normalized_path};

pub(super) const PACKAGE_DAG_JSON: &str =
    include_str!("../../../docs/architecture/package-dag.json");
const MAX_DAG_BYTES: usize = 64 * 1024;
const MAX_DAG_PACKAGES: usize = 128;
const MAX_DAG_EDGES: usize = 512;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DagPackage {
    pub(super) name: String,
    pub(super) manifest_path: String,
    pub(super) dependencies: Vec<String>,
    pub(super) layer: String,
    pub(super) order: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DagDocument {
    target_packages: usize,
    core_packages: usize,
    layer_counts: BTreeMap<String, usize>,
    pub(super) packages: Vec<DagPackage>,
    topological_order: Vec<String>,
    #[serde(default, rename = "note")]
    _note: String,
}

#[derive(Debug)]
pub(super) struct PackageDag {
    pub(super) packages: Vec<DagPackage>,
    pub(super) core_names: BTreeSet<String>,
}

type DagNameIndex = BTreeMap<String, usize>;
type DagOrderIndex = BTreeMap<usize, String>;
type DagGraph = BTreeMap<String, BTreeSet<String>>;

pub(super) fn canonical_dag() -> Result<&'static PackageDag, String> {
    static DAG: OnceLock<Result<PackageDag, String>> = OnceLock::new();
    DAG.get_or_init(|| parse_dag(PACKAGE_DAG_JSON))
        .as_ref()
        .map_err(Clone::clone)
}

pub(super) fn parse_dag(json: &str) -> Result<PackageDag, String> {
    if json.len() > MAX_DAG_BYTES {
        return Err(format!("document exceeds {MAX_DAG_BYTES} bytes"));
    }
    let document: DagDocument =
        serde_json::from_str(json).map_err(|error| format!("malformed package DAG: {error}"))?;
    validate_document_shape(&document)?;
    let (by_name, by_order) = index_dag_packages(&document)?;
    validate_topological_order(&document, &by_name, &by_order)?;
    let (core_names, graph) = dag_graph(&document, &by_name)?;
    validate_dag_counts(&document, &core_names)?;
    if let Some(cycle) = find_cycle(&graph) {
        return Err(format!("cycle in package DAG: {cycle:?}"));
    }
    validate_dependency_order(&document, &by_name)?;
    let mut packages = document.packages;
    packages.sort_by_key(|package| package.order);
    Ok(PackageDag {
        packages,
        core_names,
    })
}

fn validate_document_shape(document: &DagDocument) -> Result<(), String> {
    if document.target_packages == 0 || document.target_packages > MAX_DAG_PACKAGES {
        return Err(format!(
            "target_packages must be between 1 and {MAX_DAG_PACKAGES}"
        ));
    }
    if document.core_packages > document.target_packages {
        return Err("core_packages exceeds target_packages".to_owned());
    }
    if document.packages.len() != document.target_packages {
        return Err(format!(
            "target_packages is {}, but packages has {} entries",
            document.target_packages,
            document.packages.len()
        ));
    }
    Ok(())
}

fn index_dag_packages(document: &DagDocument) -> Result<(DagNameIndex, DagOrderIndex), String> {
    let mut by_name = BTreeMap::new();
    let mut by_order = BTreeMap::new();
    let mut manifest_paths = BTreeSet::new();
    let mut edge_count = 0;
    for (index, package) in document.packages.iter().enumerate() {
        if package.name.is_empty() || package.manifest_path.is_empty() || package.layer.is_empty() {
            return Err(format!("package entry {index} has an empty required field"));
        }
        if by_name.insert(package.name.clone(), index).is_some() {
            return Err(format!("duplicate package `{}`", package.name));
        }
        let manifest_path = normalized_path(&package.manifest_path);
        if !manifest_paths.insert(manifest_path.clone()) {
            return Err(format!("duplicate manifest path `{manifest_path}`"));
        }
        if package.order >= document.target_packages {
            return Err(format!(
                "package `{}` has out-of-range order {}",
                package.name, package.order
            ));
        }
        if by_order
            .insert(package.order, package.name.clone())
            .is_some()
        {
            return Err(format!("duplicate package order {}", package.order));
        }
        if !matches!(
            package.layer.as_str(),
            "core" | "frontend" | "extension" | "application"
        ) {
            return Err(format!(
                "package `{}` has unknown layer `{}`",
                package.name, package.layer
            ));
        }
        edge_count += package.dependencies.len();
        if edge_count > MAX_DAG_EDGES {
            return Err(format!("dependency edge count exceeds {MAX_DAG_EDGES}"));
        }
        let mut seen_dependencies = BTreeSet::new();
        for dependency in &package.dependencies {
            if !seen_dependencies.insert(dependency) {
                return Err(format!(
                    "package `{}` repeats dependency `{dependency}`",
                    package.name
                ));
            }
        }
    }
    if by_order.len() != document.target_packages {
        return Err("package orders must cover every order exactly once".to_owned());
    }
    Ok((by_name, by_order))
}

fn validate_topological_order(
    document: &DagDocument,
    by_name: &DagNameIndex,
    by_order: &DagOrderIndex,
) -> Result<(), String> {
    if document.topological_order.len() != document.target_packages {
        return Err(format!(
            "topological_order has {}, expected {} entries",
            document.topological_order.len(),
            document.target_packages
        ));
    }
    let mut seen_order_names = BTreeSet::new();
    for (order, name) in document.topological_order.iter().enumerate() {
        if !by_name.contains_key(name) {
            return Err(format!(
                "topological_order references unknown package `{name}`"
            ));
        }
        if !seen_order_names.insert(name) {
            return Err(format!("topological_order repeats package `{name}`"));
        }
        if by_order.get(&order) != Some(name) {
            return Err(format!(
                "topological_order position {order} does not contain package `{}`",
                by_order.get(&order).map_or("<missing>", String::as_str)
            ));
        }
    }
    Ok(())
}

fn dag_graph(
    document: &DagDocument,
    by_name: &DagNameIndex,
) -> Result<(BTreeSet<String>, DagGraph), String> {
    let mut core_names = BTreeSet::new();
    let mut graph = DagGraph::new();
    for package in &document.packages {
        if package.layer == "core" {
            core_names.insert(package.name.clone());
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &package.dependencies {
            if !by_name.contains_key(dependency) {
                return Err(format!(
                    "package `{}` references unknown dependency `{dependency}`",
                    package.name
                ));
            }
            if dependency == &package.name {
                return Err(format!("package `{}` depends on itself", package.name));
            }
            dependencies.insert(dependency.clone());
        }
        graph.insert(package.name.clone(), dependencies);
    }
    Ok((core_names, graph))
}

fn validate_dag_counts(
    document: &DagDocument,
    core_names: &BTreeSet<String>,
) -> Result<(), String> {
    if document.core_packages != core_names.len() {
        return Err(format!(
            "core_packages is {}, but {} packages are in the core layer",
            document.core_packages,
            core_names.len()
        ));
    }
    let mut observed_layers = BTreeMap::<String, usize>::new();
    for package in &document.packages {
        *observed_layers.entry(package.layer.clone()).or_default() += 1;
    }
    if document.layer_counts != observed_layers {
        return Err(format!(
            "layer_counts metadata drift: expected {observed_layers:?}, got {:?}",
            document.layer_counts
        ));
    }
    Ok(())
}

fn validate_dependency_order(document: &DagDocument, by_name: &DagNameIndex) -> Result<(), String> {
    for package in &document.packages {
        for dependency in &package.dependencies {
            let dependency_order = document.packages[by_name[dependency]].order;
            if dependency_order >= package.order {
                return Err(format!(
                    "dependency `{dependency}` is not before package `{}`",
                    package.name
                ));
            }
        }
    }
    Ok(())
}
