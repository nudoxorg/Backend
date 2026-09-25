//! Corpus-wide reverse-dependency counting — the ecosystem-fair popularity
//! signal (in-degree over the mirror's own manifests; no upstream API).
//!
//! # How the signal reaches ranking
//!
//! 1. **Ingest** stores each package's direct deps as catalog `edges` and, as
//!    a projection, on [`SearchFacets::dependencies`](crate::metadata::SearchFacets::dependencies).
//! 2. **Sweep** ([`crate::catalog::GlobalStore::refresh_dependents`]) builds
//!    [`DependencyRow`]s with [`names_for_sweep`]: runtime edges in the
//!    package's own ecosystem when any exist, otherwise the facet names.
//!    [`count_dependents`] then writes the in-degree back to
//!    [`SearchFacets::dependents`](crate::metadata::SearchFacets::dependents).
//! 3. **Search candidate build** (`search/mod.rs`) copies `facets.dependents`
//!    onto [`ranking::Candidate::dependents`]. When downloads are absent,
//!    [`ranking::Candidate::popularity_weight`] uses dependents alone
//!    (converted via `DEPENDENT_DOWNLOAD_EQUIV`).

use std::collections::HashMap;

use crate::ecosystem::Language;
use smol_str::SmolStr;

/// One package's contribution to the sweep: who it is and what it depends on.
#[derive(Clone)]
pub struct DependencyRow {
    pub ecosystem: Language,
    /// Canonical lowercase package name.
    pub name: SmolStr,
    /// Lowercase direct-dependency names the sweep counts.
    pub dependencies: Vec<SmolStr>,
}

/// Names one version contributes to the in-degree sweep.
///
/// Runtime edges in the version's own ecosystem are the graph. Facet names
/// are used only when that version stored no such edge, so a row written
/// before feed edges existed still counts, and a leftover facet name cannot
/// increment beside a real edge.
pub fn names_for_sweep(runtime_edges: &[SmolStr], facet_names: &[SmolStr]) -> Vec<SmolStr> {
    if runtime_edges.is_empty() {
        facet_names.to_vec()
    } else {
        runtime_edges.to_vec()
    }
}

/// Count direct dependents per `(ecosystem, name)`. Each depending package
/// counts once per target even if it appears with multiple versions — pass one
/// row per package (latest generation); duplicate `(ecosystem, name)` rows are
/// collapsed, self-dependencies ignored.
///
/// The resulting counts are the values persisted into `SearchFacets.dependents`
/// and later consumed by ranking popularity when downloads are `None`.
pub fn count_dependents(
    rows: impl IntoIterator<Item = DependencyRow>,
) -> HashMap<(Language, SmolStr), u32> {
    crate::lane::count_dependents(rows)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn row(eco: Language, name: &str, deps: &[&str]) -> DependencyRow {
        DependencyRow {
            ecosystem: eco,
            name: SmolStr::new(name),
            dependencies: deps.iter().map(|&d| SmolStr::new(d)).collect(),
        }
    }

    #[test]
    fn edges_replace_facets_and_an_empty_edge_list_keeps_facets() {
        let serde = SmolStr::new("serde");
        let leftover = SmolStr::new("leftover");
        let tokio = SmolStr::new("tokio");
        let with_edges = names_for_sweep(&[serde.clone()], &[leftover.clone(), serde.clone()]);
        let facet_only = names_for_sweep(&[], &[tokio.clone()]);
        assert_eq!(with_edges, vec![serde]);
        assert_eq!(facet_only, vec![tokio]);
        assert!(!with_edges.contains(&leftover));
    }

    #[test]
    fn basic_in_degree() {
        let rows = vec![
            row(Language::Rust, "a", &["serde", "tokio"]),
            row(Language::Rust, "b", &["serde"]),
            row(Language::Rust, "c", &["tokio"]),
        ];
        let counts = count_dependents(rows);
        assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 2);
        assert_eq!(counts[&(Language::Rust, SmolStr::new("tokio"))], 2);
    }

    #[test]
    fn duplicate_depender_rows_collapse() {
        // Same (ecosystem, name) depender appears twice — should count only once.
        let rows = vec![
            row(Language::Rust, "a", &["serde"]),
            row(Language::Rust, "a", &["serde"]),
        ];
        let counts = count_dependents(rows);
        assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 1);
    }

    #[test]
    fn self_dep_ignored() {
        let rows = vec![row(Language::Rust, "mylib", &["mylib", "serde"])];
        let counts = count_dependents(rows);
        assert!(
            !counts.contains_key(&(Language::Rust, SmolStr::new("mylib"))),
            "self-dependency must not count"
        );
        assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 1);
    }

    #[test]
    fn cross_ecosystem_names_dont_collide() {
        // "serde" in Rust vs "serde" in npm are separate targets.
        let rows = vec![
            row(Language::Rust, "a", &["serde"]),
            row(Language::Typescript, "b", &["serde"]),
        ];
        let counts = count_dependents(rows);
        assert_eq!(counts[&(Language::Rust, SmolStr::new("serde"))], 1);
        assert_eq!(counts[&(Language::Typescript, SmolStr::new("serde"))], 1);
    }

    #[test]
    fn empty_input_returns_empty() {
        let counts = count_dependents(std::iter::empty());
        assert!(counts.is_empty());
    }
}
