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
	pub const fn new(path_suffix: &'static str) -> Self { Self { path_suffix } }
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
	/// `None` when absent. Use [`crate::repo::normalize_repo_url`] to
	/// obtain a cross-ecosystem comparable slug.
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
	/// Direct dependency names, for the `dep:` invisible-keyword feature.
	pub dependencies: Vec<String>,
}

/// The per-ecosystem manifest type's one obligation: fold into the erased
/// facts.
pub trait ManifestFacts {
	fn into_facts(self) -> ExtractedFacts;
}

/// The degenerate manifest for ecosystems (or parse failures) yielding no
/// facts — facet extraction degrades to name-only, never fails.
impl ManifestFacts for ExtractedFacts {
	fn into_facts(self) -> ExtractedFacts { self }
}
