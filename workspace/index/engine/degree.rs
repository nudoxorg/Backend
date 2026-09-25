//! In-degree of the versioned edge tips.
//!
//! Each edge sync replaces one version's counting names and diffs that
//! package's union into a histogram. [`VersionedCatalog::dependents`] reads
//! the histogram. [`VersionedCatalog::dependents_from_tips`] walks every tip
//! and is the differential oracle. Runtime and optional edges in the
//! depender's own ecosystem count. Dev, build, and peer do not. A self-edge
//! does not. A dropped version leaves the union.

use std::collections::{BTreeSet, HashMap};

use heart::Language;
use smol_str::SmolStr;

use turso_versioning::orm::OrmResult;

use crate::{
    enums::TextEnum,
    record::DepEdge,
    search::ranking::dependents::{DependencyRow, count_dependents},
};

use super::{
    edge_fact::EdgeFact,
    turso_vc::{EdgeTip, VersionedCatalog},
};

/// Per-package version unions and the corpus histogram they imply.
pub(super) struct InDegree {
    versions: HashMap<(Language, SmolStr), HashMap<SmolStr, BTreeSet<SmolStr>>>,
    counts: HashMap<(Language, SmolStr), u32>,
}

impl InDegree {
    pub(super) fn new() -> Self {
        Self {
            versions: HashMap::new(),
            counts: HashMap::new(),
        }
    }

    fn observe(
        &mut self,
        ecosystem: Language,
        package: &str,
        version: &str,
        names: BTreeSet<SmolStr>,
    ) {
        let package_key = (ecosystem, SmolStr::new(package));
        let versions = self.versions.entry(package_key.clone()).or_default();
        let version_key = SmolStr::new(version);
        if versions
            .get(&version_key)
            .is_some_and(|current| current == &names)
        {
            return;
        }
        let old_union = union_of(versions);
        if names.is_empty() {
            versions.remove(&version_key);
        } else {
            versions.insert(version_key, names);
        }
        let new_union = union_of(versions);
        if versions.is_empty() {
            self.versions.remove(&package_key);
        }
        apply_diff(&mut self.counts, ecosystem, &old_union, &new_union);
    }
}

fn union_of(versions: &HashMap<SmolStr, BTreeSet<SmolStr>>) -> BTreeSet<SmolStr> {
    let mut union = BTreeSet::new();
    for names in versions.values() {
        union.extend(names.iter().cloned());
    }
    union
}

fn apply_diff(
    counts: &mut HashMap<(Language, SmolStr), u32>,
    ecosystem: Language,
    old: &BTreeSet<SmolStr>,
    new: &BTreeSet<SmolStr>,
) {
    for name in old {
        if new.contains(name) {
            continue;
        }
        let key = (ecosystem, name.clone());
        let next = counts.get(&key).copied().unwrap_or(0).saturating_sub(1);
        if next == 0 {
            counts.remove(&key);
        } else {
            counts.insert(key, next);
        }
    }
    for name in new {
        if old.contains(name) {
            continue;
        }
        *counts.entry((ecosystem, name.clone())).or_default() += 1;
    }
}

fn counting_names(tips: &[EdgeTip], ecosystem: &str, package: &str) -> BTreeSet<SmolStr> {
    tips.iter()
        .filter(|tip| tip_counts(tip, ecosystem))
        .filter_map(|tip| {
            crate::record::counted_dependency_name(package, tip.name.as_str()).map(SmolStr::new)
        })
        .collect()
}

fn tip_counts(tip: &EdgeTip, ecosystem: &str) -> bool {
    (tip.class == "runtime" || tip.class == "optional")
        && (tip.dep_ecosystem.is_empty() || tip.dep_ecosystem.as_str() == ecosystem)
}

impl VersionedCatalog {
    pub(super) fn tip_edges(
        &mut self,
        ecosystem: &str,
        name: &str,
        version: &str,
    ) -> OrmResult<Vec<DepEdge>> {
        let Some(pid) = self.pid_owned(ecosystem, name, version) else {
            return Ok(Vec::new());
        };
        let Some(tips) = self.edge_tips.get(pid.as_str()).cloned() else {
            return Ok(Vec::new());
        };
        let mut edges = Vec::with_capacity(tips.len());
        for tip in tips {
            let Some(row) = self.db.table::<EdgeFact>().get(&super::edge_fact::edge_pk(
                &pid,
                tip.dep_ecosystem.as_str(),
                tip.name.as_str(),
                tip.kind.as_str(),
            )) else {
                continue;
            };
            edges.push(row.to_edge()?);
        }
        Ok(edges)
    }

    pub fn get_edge(
        &mut self,
        ecosystem: &str,
        package: &str,
        version: &str,
        name: &str,
    ) -> Option<EdgeFact> {
        let pid = self.pid_owned(ecosystem, package, version)?;
        for dep in ["", ecosystem] {
            for kind in crate::enums::EdgeKind::all_variants() {
                if let Some(row) = self.db.table::<EdgeFact>().get(&super::edge_fact::edge_pk(
                    &pid,
                    dep,
                    name,
                    kind.as_token(),
                )) {
                    return Some(row);
                }
            }
        }
        None
    }

    /// Packages depended on, counted once per depending package name.
    ///
    /// The histogram is updated when a version's tips change. Versions of one
    /// name are one union. A self-edge does not increment.
    pub fn dependents(&self) -> HashMap<(Language, SmolStr), u32> {
        self.degree.counts.clone()
    }

    /// Full tip walk. Tests use this to show [`Self::dependents`] matches the
    /// union [`count_dependents`] would compute from the same tips.
    pub fn dependents_from_tips(&self) -> HashMap<(Language, SmolStr), u32> {
        count_dependents(self.dependency_rows())
    }

    pub(super) fn note_degree(
        &mut self,
        ecosystem: &str,
        package: &str,
        version: &str,
        tips: &[EdgeTip],
    ) {
        let Some(language) = Language::from_token(ecosystem) else {
            return;
        };
        self.degree.observe(
            language,
            package,
            version,
            counting_names(tips, ecosystem, package),
        );
    }

    fn dependency_rows(&self) -> Vec<DependencyRow> {
        let mut rows = Vec::new();
        for (key, pid) in &self.coords {
            let Some(ecosystem) = Language::from_token(key.ecosystem.as_str()) else {
                continue;
            };
            let Some(tips) = self.edge_tips.get(pid) else {
                continue;
            };
            let dependencies: Vec<SmolStr> =
                counting_names(tips, key.ecosystem.as_str(), key.package.as_str())
                    .into_iter()
                    .collect();
            if dependencies.is_empty() {
                continue;
            }
            rows.push(DependencyRow {
                ecosystem,
                name: key.package.clone(),
                dependencies,
            });
        }
        rows
    }
}
