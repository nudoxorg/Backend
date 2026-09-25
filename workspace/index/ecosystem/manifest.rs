//! Manifest parsing currency — what a per-ecosystem manifest parse yields,
//! ecosystem-erased.

/// A manifest file to look for inside an extracted archive, matched
/// case-insensitively as a path *suffix* against snapshot entries (so
/// `pkg-1.0/Cargo.toml` under a tarball wrapper dir still matches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestCandidate {
    pub path_suffix: &'static str,
}

impl ManifestCandidate {
    pub const fn new(path_suffix: &'static str) -> Self {
        Self { path_suffix }
    }
}

/// What a manifest parse yields — a field-for-field mirror of the Rust-only
/// `ExtractionInput` population the audit found in `indexing.rs` (S1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedFacts {
    pub description: Option<String>,
    /// npm keywords / PyPI keywords / NuGet tags / Cargo keywords.
    pub keywords: Vec<String>,
    /// Cargo categories / PyPI trove classifiers, already mapped into the
    /// shared internal taxonomy via `SearchNorms::map_category`.
    pub categories: Vec<String>,
    /// Manifest-declared readme path, if any.
    pub readme_hint: Option<String>,
    /// Verbatim repository/SCM URL from the manifest when one is declared;
    /// `None` when absent. Use [`crate::ecosystem::repo::normalize_repo_url`]
    /// to obtain a cross-ecosystem comparable slug.
    pub repository: Option<String>,
    pub documentation: bool,
    /// Verbatim SPDX license expression or license name declared in the
    /// manifest (e.g. `"MIT OR Apache-2.0"`). `None` when no expression is
    /// present. For packages where only a license *file* is referenced with
    /// no expression, this is `None` and [`Self::has_license_file`] is
    /// `true`.
    pub license: Option<String>,
    /// `true` when the manifest references a license file but carries no
    /// parseable license expression.
    pub has_license_file: bool,
    /// Direct dependency edges. Facet keywords use [`Self::dependency_names`].
    pub dependencies: Vec<crate::record::DepEdge>,
}

impl ExtractedFacts {
    /// Dependency names in manifest order.
    #[must_use]
    pub fn dependency_names(&self) -> Vec<String> {
        self.dependencies
            .iter()
            .map(|edge| edge.name.to_string())
            .collect()
    }
}

impl ExtractedFacts {
    /// Fold `other` (parsed from a *lower-priority* manifest candidate) into
    /// `self` (the accumulator, seeded from higher-priority candidates so
    /// far). Called once per successfully-parsed candidate, in the
    /// ecosystem's declared priority order
    /// (`EcosystemSpec::manifest_candidates`) —
    /// see `server::coordination::indexing::facets::extract_facets`, the only
    /// caller.
    ///
    /// Merge policy, decided per field kind:
    ///
    /// - **Scalars** (`description`, `repository`, `readme_hint`, `license`):
    ///   first-non-`None` wins, i.e. `self` is left untouched if it's already
    ///   `Some`. The candidate priority order already encodes which manifest
    ///   the ecosystem trusts most for these fields (e.g. `vcpkg.json` before
    ///   `CMakeLists.txt`), so a lower-priority manifest may only fill a gap,
    ///   never override a value a higher-priority one already supplied.
    /// - **Booleans** (`documentation`, `has_license_file`): logical OR. Both
    ///   are *presence* signals ("a homepage/license was declared somewhere"),
    ///   `false` is "no evidence yet" rather than a claim of absence, so
    ///   evidence from any manifest should only ever turn a signal on, never
    ///   off — order-independent by construction, which also keeps the result
    ///   stable regardless of how many candidates are added later.
    /// - **Collections** (`keywords`, `categories`, `dependencies`): union,
    ///   deduped by exact string equality, higher-priority manifest's items
    ///   first. Each of these lists is a set of independent facts (a keyword, a
    ///   category, a dependency token) rather than one competing value, so
    ///   dropping a lower-priority manifest's items (as first-non-empty would)
    ///   would silently discard real search/dependency signal — e.g. `cpp`'s
    ///   dependency records span 7 distinct declaration mechanisms
    ///   (`find_package`, `pkg_config`, submodule, `FetchContent`, meson wrap,
    ///   vcpkg/conan recipe, `bazel_dep`) precisely because a single package
    ///   legitimately declares dependencies through more than one of its
    ///   manifests at once. The dedup guards against the same token surfacing
    ///   from two manifests (e.g. `zlib` as both a vcpkg recipe dep and a CMake
    ///   `find_package` name).
    pub fn merge(&mut self, other: ExtractedFacts) {
        if self.description.is_none() {
            self.description = other.description;
        }
        if self.repository.is_none() {
            self.repository = other.repository;
        }
        if self.readme_hint.is_none() {
            self.readme_hint = other.readme_hint;
        }
        if self.license.is_none() {
            self.license = other.license;
        }
        self.documentation = self.documentation || other.documentation;
        self.has_license_file = self.has_license_file || other.has_license_file;
        merge_union(&mut self.keywords, other.keywords);
        merge_union(&mut self.categories, other.categories);
        merge_edges(&mut self.dependencies, other.dependencies);
    }
}

/// Keep the first edge for a name. Later manifests only append new names.
fn merge_edges(acc: &mut Vec<crate::record::DepEdge>, new: Vec<crate::record::DepEdge>) {
    for edge in new {
        if edge.name.is_empty() || acc.iter().any(|kept| kept.name == edge.name) {
            continue;
        }
        acc.push(edge);
    }
}

/// Append items from `new` onto `acc` that aren't already present (exact
/// string equality), preserving `acc`'s existing order and `new`'s relative
/// order for the appended tail.
fn merge_union(acc: &mut Vec<String>, new: Vec<String>) {
    for item in new {
        if !acc.contains(&item) {
            acc.push(item);
        }
    }
}

/// The per-ecosystem manifest type's one obligation: fold into the erased
/// facts.
pub trait ManifestFacts {
    fn into_facts(self) -> ExtractedFacts;
}

/// The degenerate manifest for ecosystems (or parse failures) yielding no
/// facts — facet extraction degrades to name-only, never fails.
impl ManifestFacts for ExtractedFacts {
    fn into_facts(self) -> ExtractedFacts {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(f: impl FnOnce(&mut ExtractedFacts)) -> ExtractedFacts {
        let mut facts = ExtractedFacts::default();
        f(&mut facts);
        facts
    }

    #[test]
    fn merge_scalars_first_non_none_wins() {
        let mut acc = facts(|f| f.description = Some("high-priority".into()));
        acc.merge(facts(|f| f.description = Some("low-priority".into())));
        assert_eq!(acc.description.as_deref(), Some("high-priority"));
    }

    #[test]
    fn merge_scalars_lower_priority_fills_a_gap() {
        let mut acc = ExtractedFacts::default();
        acc.merge(facts(|f| {
            f.repository = Some("https://example.com/repo".into());
            f.readme_hint = Some("README.rst".into());
            f.license = Some("MIT".into());
        }));
        assert_eq!(acc.repository.as_deref(), Some("https://example.com/repo"));
        assert_eq!(acc.readme_hint.as_deref(), Some("README.rst"));
        assert_eq!(acc.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn merge_booleans_are_logical_or() {
        let mut acc = facts(|f| f.has_license_file = false);
        acc.merge(facts(|f| f.has_license_file = true));
        assert!(acc.has_license_file, "OR: any manifest setting it wins");

        let mut acc2 = facts(|f| f.documentation = true);
        acc2.merge(facts(|f| f.documentation = false));
        assert!(acc2.documentation, "OR: true is sticky regardless of order");
    }

    #[test]
    fn merge_collections_union_with_dedup() {
        let mut acc = facts(|f| {
            f.dependencies = vec![
                crate::record::DepEdge::runtime("zlib"),
                crate::record::DepEdge::runtime("openssl"),
            ];
        });
        acc.merge(facts(|f| {
            f.dependencies = vec![
                crate::record::DepEdge::runtime("zlib"),
                crate::record::DepEdge::runtime("libpng"),
            ];
        }));
        assert_eq!(
            acc.dependency_names(),
            vec!["zlib".to_owned(), "openssl".to_owned(), "libpng".to_owned()],
            "higher-priority items first, duplicates dropped, new items appended in order"
        );
    }

    #[test]
    fn merge_keywords_and_categories_also_union() {
        let mut acc = facts(|f| f.keywords = vec!["http".into()]);
        acc.merge(facts(|f| {
            f.keywords = vec!["http".into(), "networking".into()];
        }));
        assert_eq!(acc.keywords, vec![
            "http".to_owned(),
            "networking".to_owned()
        ]);

        let mut acc = facts(|f| f.categories = vec!["net".into()]);
        acc.merge(facts(|f| f.categories = vec!["compression".into()]));
        assert_eq!(acc.categories, vec![
            "net".to_owned(),
            "compression".to_owned()
        ]);
    }

    #[test]
    fn merge_of_default_into_populated_is_identity() {
        let mut acc = facts(|f| {
            f.description = Some("d".into());
            f.keywords = vec!["k".into()];
            f.has_license_file = true;
        });
        let before = acc.clone();
        acc.merge(ExtractedFacts::default());
        assert_eq!(
            acc, before,
            "merging an empty/degenerate manifest changes nothing"
        );
    }
}
