//! In-degree of the versioned edge tips.
//!
//! The SQL sweep groups catalog `edges` rows. This walk reads the tip index
//! the ledger already keeps, then hands the same [`DependencyRow`]s to
//! [`count_dependents`]. Runtime and optional edges count. Dev, build, and
//! peer do not. A dropped version has no tips, so it leaves the graph.

use std::collections::HashMap;

use heart::Language;
use smol_str::SmolStr;

use turso_versioning::orm::OrmResult;

use crate::record::DepEdge;
use crate::search::ranking::dependents::{DependencyRow, count_dependents};

use super::edge_fact::EdgeFact;
use super::turso_vc::VersionedCatalog;

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
                tip.class.as_str(),
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
            for class in super::edge_fact::class_tokens() {
                if let Some(row) = self.db.table::<EdgeFact>().get(&super::edge_fact::edge_pk(
                    &pid, dep, name, class,
                )) {
                    return Some(row);
                }
            }
        }
        None
    }

    /// Packages depended on, counted once per depending package name.
    ///
    /// Versions of one name are separate rows. [`count_dependents`] unions
    /// them. A self-edge does not increment.
    pub fn dependents(&self) -> HashMap<(Language, SmolStr), u32> {
        count_dependents(self.dependency_rows())
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
            let dependencies: Vec<SmolStr> = tips
                .iter()
                .filter(|tip| {
                    (tip.class == "runtime" || tip.class == "optional")
                        && (tip.dep_ecosystem.is_empty()
                            || tip.dep_ecosystem.as_str() == key.ecosystem.as_str())
                })
                .map(|tip| tip.name.clone())
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
