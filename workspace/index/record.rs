//! One package fact every ecosystem ingest path emits.
//!
//! Rust, npm, PyPI, Go, Maven, NuGet, C++, and Homebrew each used to invent a
//! parallel fact shape. [`PackageRecord`] is the shared model those paths fill
//! instead: identity, descriptive metadata, and the dependency edges the
//! dependents sweep and the facet consistency check both read.
//!
//! The ecosystem is [`heart::Language`]. This module does not parse manifests,
//! talk to a registry, or touch the catalog.

use std::collections::HashSet;

use heart::Language;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// How a dependency was declared.
///
/// Membership in [`PackageRecord::runtime_names`] and [`edge_names_agree`] is
/// decided by this class, not by [`DepEdge::optional`]. A new variant has to
/// take a side in [`DepClass::is_runtime_or_optional`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepClass {
    /// Installed with the package (`dependencies`, `install_requires`,
    /// `require`).
    Runtime,
    /// Development-only (`dev-dependencies`, `devDependencies`, test scope).
    Dev,
    /// Used to build the package, not installed with it (`build-dependencies`,
    /// a Homebrew `:build` dependency).
    Build,
    /// Optional at install time (`optionalDependencies`, an extra).
    Optional,
    /// Provided by the consumer (`peerDependencies`).
    Peer,
}

impl DepClass {
    /// Whether edges of this class are runtime or optional dependencies.
    ///
    /// `Runtime` and `Optional` count. `Dev`, `Build`, and `Peer` do not.
    #[must_use]
    pub const fn is_runtime_or_optional(self) -> bool {
        // Exhaustive on purpose: a new class must choose whether it counts,
        // rather than silently dropping out of the dependents sweep.
        #[allow(
            clippy::match_like_matches_macro,
            reason = "exhaustive so a new DepClass must choose whether it counts"
        )]
        match self {
            Self::Runtime | Self::Optional => true,
            Self::Dev | Self::Build | Self::Peer => false,
        }
    }
}

/// One directed dependency as an ingest path saw it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DepEdge {
    /// Dependency name, as published. Callers compare it trimmed and
    /// case-sensitively.
    pub name: SmolStr,
    /// Version requirement expression (`^1.0`, `>=2`, a Maven range), when the
    /// manifest declared one.
    pub requirement: Option<SmolStr>,
    /// Which table or scope the edge came from.
    pub class: DepClass,
    /// Catalog kind this edge replaces. Recipe and runtime share a class and
    /// stay distinct rows because the kind is part of the identity.
    pub kind: crate::enums::EdgeKind,
    /// The manifest marked this edge optional without moving it to
    /// [`DepClass::Optional`] (Cargo `optional = true` inside
    /// `[dependencies]`).
    ///
    /// The flag does not change membership in [`PackageRecord::runtime_names`].
    /// A [`DepClass::Runtime`] edge stays in that set when the flag is set, and
    /// a [`DepClass::Dev`] edge stays out.
    pub optional: bool,
    /// Ecosystem of the dependency. `None` means the depending package's own
    /// ecosystem. A foreign ecosystem is a real edge and is not an in-degree
    /// in the depender's language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dep_ecosystem: Option<Language>,
}

/// The package record every ecosystem ingest path emits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageRecord {
    /// Ecosystem this package was published to.
    pub ecosystem: Language,
    /// Canonical package name for that ecosystem.
    pub canonical_name: SmolStr,
    /// Version string, already in the ecosystem's canonical spelling.
    pub version: SmolStr,
    /// Manifest summary, when one was declared.
    pub description: Option<SmolStr>,
    /// SPDX expression or license name, when one was declared.
    pub license: Option<SmolStr>,
    /// Manifest keywords or tags, in source order.
    pub keywords: Vec<SmolStr>,
    /// Repository or SCM URL, verbatim from the manifest.
    pub repository: Option<SmolStr>,
    /// Registry download count, when that registry publishes one.
    pub downloads: Option<u64>,
    /// This version is yanked or withdrawn on its registry.
    pub yanked: bool,
    /// Bytes this version currently names, when ingest observed a digest.
    ///
    /// Absent from the JSON when unknown, so a record with no digest keeps
    /// the same payload hash it had before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<crate::pid::ContentDigest>,
    /// Direct dependency edges, in manifest order.
    pub edges: Vec<DepEdge>,
}

impl DepEdge {
    /// A runtime edge with no version requirement.
    ///
    /// Catalog feeds that publish a name and not a requirement use this.
    #[must_use]
    pub fn runtime(name: impl Into<SmolStr>) -> Self {
        Self {
            name: name.into(),
            requirement: None,
            class: DepClass::Runtime,
            kind: crate::enums::EdgeKind::Runtime,
            optional: false,
            dep_ecosystem: None,
        }
    }
}

/// One runtime-edge set, de-duplicated by name, then sorted.
///
/// Followers that read a manifest into [`DepEdge`] share this fold so the
/// first-row rule and the name order live in one place.
/// [`Self::prefer_required`] replaces an optional row when a later row of the
/// same name is required.
#[derive(Debug, Default)]
pub struct RuntimeEdgeFold {
    edges: Vec<DepEdge>,
    prefer_required: bool,
}

impl RuntimeEdgeFold {
    /// A repeated name keeps the first row.
    #[must_use]
    pub fn keep_first() -> Self {
        Self::default()
    }

    /// A repeated name keeps the first row, unless that row is optional and
    /// the new row is required.
    #[must_use]
    pub fn prefer_required() -> Self {
        Self {
            edges: Vec::new(),
            prefer_required: true,
        }
    }

    /// Record one runtime edge. An empty name is ignored.
    pub fn observe(&mut self, name: impl Into<SmolStr>, requirement: Option<&str>, optional: bool) {
        let name = name.into();
        if name.is_empty() {
            return;
        }
        let mut edge = DepEdge::runtime(name);
        if let Some(requirement) = requirement.map(str::trim).filter(|text| !text.is_empty()) {
            edge.requirement = Some(requirement.into());
        }
        edge.optional = optional;
        self.observe_edge(edge);
    }

    /// Record an edge that a parser already built. Kind and class stay as the
    /// parser set them. An empty name is ignored.
    pub fn observe_edge(&mut self, edge: DepEdge) {
        if edge.name.is_empty() {
            return;
        }
        if let Some(existing) = self
            .edges
            .iter_mut()
            .find(|stored| stored.name == edge.name)
        {
            if self.prefer_required && existing.optional && !edge.optional {
                *existing = edge;
            }
            return;
        }
        self.edges.push(edge);
    }

    /// Sort by name and return the rows.
    #[must_use]
    pub fn finish(mut self) -> Vec<DepEdge> {
        self.edges.sort_by(|left, right| left.name.cmp(&right.name));
        self.edges
    }
}

/// Runtime edges from catalog dependency names.
///
/// Blank names are dropped. Surviving names are trimmed and de-duplicated in
/// first-seen order. Each edge is [`DepClass::Runtime`].
#[must_use]
pub fn runtime_edges_from_names(names: &[impl AsRef<str>]) -> Vec<DepEdge> {
    let mut seen = HashSet::<String>::new();
    let mut edges = Vec::new();
    for name in names {
        let trimmed = name.as_ref().trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_owned()) {
            continue;
        }
        edges.push(DepEdge::runtime(trimmed));
    }
    edges
}

impl PackageRecord {
    /// A published version whose only known facts are its dependency names.
    ///
    /// Names go through [`runtime_edges_from_names`], so the stored edges and
    /// any facet list derived from [`Self::runtime_names`] are the same set.
    #[must_use]
    pub fn published(
        ecosystem: Language,
        canonical_name: impl Into<SmolStr>,
        version: impl Into<SmolStr>,
        dependencies: &[impl AsRef<str>],
    ) -> Self {
        Self::from_parts(
            ecosystem,
            canonical_name,
            version,
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            runtime_edges_from_names(dependencies),
        )
    }

    /// Assemble a record from fields an ingest path already extracted.
    ///
    /// `canonical_name` and `version` accept anything that converts into
    /// [`SmolStr`] (`&str`, `String`). The remaining strings are already
    /// [`SmolStr`] so a missing description stays a typed `None` rather than an
    /// empty string.
    #[must_use]
    pub fn from_parts(
        ecosystem: Language,
        canonical_name: impl Into<SmolStr>,
        version: impl Into<SmolStr>,
        description: Option<SmolStr>,
        license: Option<SmolStr>,
        keywords: Vec<SmolStr>,
        repository: Option<SmolStr>,
        downloads: Option<u64>,
        yanked: bool,
        edges: Vec<DepEdge>,
    ) -> Self {
        Self {
            ecosystem,
            canonical_name: canonical_name.into(),
            version: version.into(),
            description,
            license,
            keywords,
            repository,
            downloads,
            yanked,
            content: None,
            edges,
        }
    }

    /// Bind a content digest. The version coordinate stays the same.
    #[must_use]
    pub fn with_content(mut self, content: crate::pid::ContentDigest) -> Self {
        self.content = Some(content);
        self
    }

    /// Dependency names the dependents sweep should count.
    ///
    /// Includes [`DepClass::Runtime`] and [`DepClass::Optional`] only, so a
    /// `Dev` edge never contributes in-degree. Names are trimmed
    /// (`str::trim`), compared case-sensitively, de-duplicated, and returned
    /// in first-seen edge order. An empty name is not a dependency.
    /// [`DepEdge::optional`] does not add or remove a name.
    #[must_use]
    pub fn runtime_names(&self) -> Vec<&str> {
        let mut seen = HashSet::<&str>::new();
        let mut names = Vec::new();
        for edge in &self.edges {
            if !edge.class.is_runtime_or_optional() {
                continue;
            }
            let name = edge.name.as_str().trim();
            if name.is_empty() || !seen.insert(name) {
                continue;
            }
            names.push(name);
        }
        names
    }
}

/// The name the dependents sweep counts, or nothing for a blank or a self-edge.
///
/// Both the SQL sweep and the ledger histogram call this, so padding and an
/// empty token cannot become a second key.
#[must_use]
pub fn counted_dependency_name<'a>(package: &str, dependency: &'a str) -> Option<&'a str> {
    let dependency = dependency.trim();
    if dependency.is_empty() || dependency == package.trim() {
        None
    } else {
        Some(dependency)
    }
}

/// Runtime and optional dependency names, trimmed, sorted, and de-duplicated.
///
/// This is the facet list. Build, dev, and peer edges are absent, so the list
/// agrees with [`PackageRecord::runtime_names`].
#[must_use]
pub fn runtime_facet_names(edges: &[DepEdge]) -> Vec<SmolStr> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for edge in edges {
        if !edge.class.is_runtime_or_optional() {
            continue;
        }
        let name = edge.name.as_str().trim();
        if name.is_empty() || !seen.insert(name) {
            continue;
        }
        names.push(SmolStr::new(name));
    }
    names.sort();
    names
}

/// Whether `record`'s runtime and optional dependency names equal `facet_deps`.
///
/// Both sides are trimmed (`str::trim`) and compared as sets: order and
/// repeated names do not matter, and the comparison is case-sensitive. `Dev`,
/// `Build`, and `Peer` edges are not part of the record side, so a facet list
/// that still contains one of those names does not agree.
#[must_use]
pub fn edge_names_agree(record: &PackageRecord, facet_deps: &[impl AsRef<str>]) -> bool {
    let record_names: HashSet<&str> = record.runtime_names().into_iter().collect();
    let facet_names: HashSet<&str> = facet_deps.iter().map(|dep| dep.as_ref().trim()).collect();
    record_names == facet_names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smol(text: &str) -> SmolStr {
        SmolStr::new(text)
    }

    fn edge(name: &str, class: DepClass, optional: bool) -> DepEdge {
        DepEdge {
            name: smol(name),
            requirement: None,
            class,
            kind: match class {
                DepClass::Runtime | DepClass::Optional => crate::enums::EdgeKind::Runtime,
                DepClass::Dev | DepClass::Build | DepClass::Peer => crate::enums::EdgeKind::Build,
            },
            optional,
            dep_ecosystem: None,
        }
    }

    fn sample(edges: Vec<DepEdge>) -> PackageRecord {
        PackageRecord::from_parts(
            Language::Rust,
            "example",
            "1.2.3",
            Some(smol("An example package")),
            Some(smol("MIT OR Apache-2.0")),
            vec![smol("example"), smol("demo")],
            Some(smol("https://github.com/example/example")),
            Some(42),
            false,
            edges,
        )
    }

    fn mixed_edges() -> Vec<DepEdge> {
        vec![
            edge("serde", DepClass::Runtime, false),
            edge("  tokio  ", DepClass::Runtime, true),
            edge("criterion", DepClass::Dev, false),
            edge("cc", DepClass::Build, false),
            edge("react", DepClass::Peer, false),
            edge("serde_json", DepClass::Optional, false),
            // A dev edge marked optional is still dev.
            edge("tempfile", DepClass::Dev, true),
            // Same runtime name, different spelling of whitespace: one entry.
            edge(" serde ", DepClass::Runtime, false),
        ]
    }

    #[test]
    fn runtime_facet_names_drop_build_dev_and_peer() {
        let edges = mixed_edges();
        let names = runtime_facet_names(&edges);
        assert_eq!(names, vec![
            smol("serde"),
            smol("serde_json"),
            smol("tokio")
        ]);
        assert!(edge_names_agree(&sample(edges), &names));
    }

    #[test]
    fn edge_names_agree_is_true_for_the_same_trimmed_set() {
        let record = sample(mixed_edges());
        let facet = ["serde_json", "  serde  ", "tokio", "serde", "tokio"];
        assert!(edge_names_agree(&record, &facet));
        assert!(edge_names_agree(&sample(vec![]), &[] as &[&str]));
    }

    #[test]
    fn edge_names_agree_is_false_when_the_sets_differ() {
        let record = sample(mixed_edges());

        assert!(
            !edge_names_agree(&record, &["serde", "tokio"]),
            "missing an optional runtime name"
        );
        assert!(
            !edge_names_agree(&record, &["serde", "tokio", "serde_json", "criterion"]),
            "a dev-only name on the facet side is not a runtime name"
        );
        assert!(
            !edge_names_agree(&record, &["Serde", "tokio", "serde_json"]),
            "comparison is case-sensitive"
        );
        assert!(
            !edge_names_agree(&record, &["serde", "tokio", "serde_json", "extra"]),
            "an extra facet name is a mismatch"
        );
    }

    #[test]
    fn dev_edges_are_excluded_from_runtime_names() {
        let record = sample(mixed_edges());
        let names = record.runtime_names();
        assert_eq!(names, vec!["serde", "tokio", "serde_json"]);
        assert!(!names.contains(&"criterion"));
        assert!(!names.contains(&"tempfile"));
        assert!(!names.contains(&"cc"));
        assert!(!names.contains(&"react"));
    }

    #[test]
    fn yanked_flag_round_trips_via_serde() {
        let yanked = PackageRecord::from_parts(
            Language::Python,
            "left-pad",
            "1.0.0",
            None,
            Some(smol("MIT")),
            vec![],
            None,
            None,
            true,
            vec![DepEdge {
                name: smol("something"),
                requirement: Some(smol(">=1")),
                class: DepClass::Runtime,
                kind: crate::enums::EdgeKind::Runtime,
                optional: false,
                dep_ecosystem: None,
            }],
        );
        let encoded = serde_json::to_string(&yanked).expect("serialize yanked record");
        let decoded: PackageRecord =
            serde_json::from_str(&encoded).expect("deserialize yanked record");
        assert!(decoded.yanked);
        assert_eq!(decoded, yanked);

        let listed = sample(vec![edge("serde", DepClass::Runtime, false)]);
        let encoded = serde_json::to_string(&listed).expect("serialize listed record");
        let decoded: PackageRecord =
            serde_json::from_str(&encoded).expect("deserialize listed record");
        assert!(!decoded.yanked);
        assert_eq!(decoded, listed);
    }

    /// The fold and a second implementation of the same rules agree.
    #[test]
    fn runtime_edge_fold_matches_a_second_implementation() {
        let rows = [
            ("b", Some("^1"), true),
            ("a", Some("1.0"), false),
            ("b", Some("2"), false),
            ("", Some("x"), false),
            ("a", Some("9"), true),
            ("c", None, false),
        ];
        let folded = finish(RuntimeEdgeFold::prefer_required(), &rows);
        let oracle = oracle_prefer_required(&rows);
        assert_eq!(folded, oracle);
        let first = finish(RuntimeEdgeFold::keep_first(), &rows);
        assert_eq!(first[0].name.as_str(), "a");
        assert_eq!(first[1].requirement.as_deref(), Some("^1"));
        assert!(first[1].optional);
        assert_eq!(first.len(), 3);
    }

    fn finish(mut fold: RuntimeEdgeFold, rows: &[(&str, Option<&str>, bool)]) -> Vec<DepEdge> {
        for (name, requirement, optional) in rows {
            fold.observe(*name, *requirement, *optional);
        }
        fold.finish()
    }

    fn oracle_prefer_required(rows: &[(&str, Option<&str>, bool)]) -> Vec<DepEdge> {
        let mut edges: Vec<DepEdge> = Vec::new();
        for (name, requirement, optional) in rows {
            if name.is_empty() {
                continue;
            }
            let mut edge = DepEdge::runtime(*name);
            if let Some(requirement) = requirement.filter(|text| !text.is_empty()) {
                edge.requirement = Some((*requirement).into());
            }
            edge.optional = *optional;
            if let Some(existing) = edges.iter_mut().find(|stored| stored.name == edge.name) {
                if existing.optional && !edge.optional {
                    *existing = edge;
                }
                continue;
            }
            edges.push(edge);
        }
        edges.sort_by(|left, right| left.name.cmp(&right.name));
        edges
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]

        #[test]
        fn runtime_facet_names_match_the_record_for_any_edge(
            rows in proptest::collection::vec(
                ("[A-Za-z]{0,6}", 0usize..5, proptest::bool::ANY),
                0..12,
            )
        ) {
            let classes = [
                DepClass::Runtime,
                DepClass::Dev,
                DepClass::Build,
                DepClass::Optional,
                DepClass::Peer,
            ];
            let edges: Vec<DepEdge> = rows
                .iter()
                .map(|(name, class, optional)| edge(name, classes[*class], *optional))
                .collect();
            let names = runtime_facet_names(&edges);
            let record = sample(edges);
            proptest::prop_assert!(edge_names_agree(&record, &names));
        }
    }
}
