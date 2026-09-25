//! In-degree of the versioned edge tips.
//!
//! The SQL sweep groups catalog `edges` rows. This walk reads the tip index
//! the ledger already keeps, then hands the same [`DependencyRow`]s to
//! [`count_dependents`]. Runtime and optional edges count. Dev, build, and
//! peer do not. A dropped version has no tips, so it leaves the graph.

use std::collections::HashMap;

use heart::Language;
use smol_str::SmolStr;

use crate::search::ranking::dependents::{DependencyRow, count_dependents};

use super::turso_vc::VersionedCatalog;

impl VersionedCatalog {
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
                .filter(|tip| tip.class == "runtime" || tip.class == "optional")
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
