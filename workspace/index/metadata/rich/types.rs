//! Public rich-metadata types: extraction input and outputs.

use smol_str::SmolStr;

/// Everything the extractor needs, already pulled from the archive by the
/// caller (manifest parse, README bytes, public identifiers from the compiler
/// surface).
#[derive(Debug, Default, Clone)]
pub struct ExtractionInput<'a> {
    /// Package name (crate name slug).
    pub name: &'a str,
    /// Short description from `[package] description`.
    pub description: Option<&'a str>,
    /// Explicit `[package] keywords` (up to 5 per Cargo rules).
    pub manifest_keywords: &'a [String],
    /// Explicit `[package] categories` slugs.
    pub manifest_categories: &'a [String],
    /// Full README text, if available.
    pub readme: Option<&'a str>,
    /// Public fn/type/const/… identifiers extracted from the compiler surface.
    pub identifiers: &'a [String],
    /// Direct dependency names (used to emit `dep:*` invisible keywords).
    pub dependencies: &'a [String],
    /// Whether the manifest declares a repository link.
    pub has_repository: bool,
    /// Whether the manifest declares a documentation link.
    pub has_documentation: bool,
    /// Whether the manifest declares a license.
    pub has_license: bool,
    /// Lines of code (Rust source).
    pub loc: u32,
    /// Published release count observed at resolve time (`None` before resolve).
    pub release_count: Option<u32>,
    /// Withdrawn/yanked releases among them.
    pub withdrawn_count: Option<u32>,
    /// Days since the most recent published release (`None` when unknown).
    /// Used by the temporal quality group (freshness / deadness); missing →
    /// that group contributes nothing.
    pub last_release_days_ago: Option<u32>,
}

/// The derived, searchable metadata.  Serde so it can ride inside a stored
/// record.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RichMetadata {
    /// Weighted keywords in descending weight order, weight ∈ 0.0..=1.0.
    pub keywords: Vec<(f32, SmolStr)>,
    /// Derived categories (slug, relevance), may be empty.
    pub categories: Vec<(f32, SmolStr)>,
    /// Overall quality score in 0.0..=1.0.
    pub quality: f32,
    /// Normalized direct-dependency name slugs (lowercase, trimmed, deduped, sorted).
    pub dependencies: Vec<SmolStr>,
}

impl RichMetadata {
    /// Keyword slugs joined by spaces, weight-ordered — ready for a tantivy
    /// TEXT field.
    #[must_use]
    pub fn keyword_text(&self) -> String {
        self.keywords
            .iter()
            .map(|(_, k)| k.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Quality mapped to the 0..=1_000_000 integer lib.rs uses for a FAST
    /// field.
    #[must_use]
    pub fn quality_ppm(&self) -> u64 {
        (self.quality.clamp(0., 1.) * 1_000_000.) as u64
    }
}

/// The `Eq`-able projection of [`RichMetadata`] that actually rides inside a
/// stored [`crate::GlobalPackage`] record.
///
/// [`RichMetadata`] carries `f32` weights, so it can't be `Eq` — but
/// `GlobalPackage` is (and callers rely on that). This projection keeps only what
/// indexing and ranking need — the keyword *slugs* (weight order preserved,
/// weights dropped) and quality as an integer parts-per-million — all of which
/// are `Eq`. Build it once at ingest with [`SearchFacets::from_rich`].
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchFacets {
    /// Normalized keyword slugs, most-relevant first.
    pub keywords: Vec<SmolStr>,
    /// Quality in parts-per-million (0..=1_000_000) — integer so the enclosing
    /// record stays `Eq`.
    pub quality_ppm: u32,
    /// The manifest description, carried through to the tantivy `description`
    /// field (S2). `#[serde(default)]` so pre-existing stored facets decode.
    #[serde(default)]
    pub description: Option<SmolStr>,
    /// Monthly downloads from the ecosystem's `DownloadEndpoint` (S4); `None`
    /// when the ecosystem has no source — downloads-driven ranking stages then
    /// never fire for it.
    #[serde(default)]
    pub downloads: Option<u64>,
    /// Normalized direct-dependency name slugs (lowercase), for `dep:` search
    /// filters and the corpus-wide reverse-dependency sweep.
    #[serde(default)]
    pub dependencies: Vec<SmolStr>,
    /// Reverse-dependency in-degree computed by the periodic corpus sweep —
    /// the ecosystem-fair popularity signal. `None` until the first sweep.
    #[serde(default)]
    pub dependents: Option<u32>,
    /// Normalized repository slug (`host/owner/repo`, lowercase) for
    /// cross-ecosystem entity resolution. `None` when no repository is declared.
    #[serde(default)]
    pub repo_slug: Option<SmolStr>,
    /// Lowercased license expression (e.g. `mit`, `apache-2.0`, `mit or apache-2.0`)
    /// for `license:` filters. `None` when the manifest declares none.
    #[serde(default)]
    pub license: Option<SmolStr>,
    /// Published (listed + withdrawn) release count observed at resolve time.
    #[serde(default)]
    pub release_count: Option<u32>,
    /// How many of those releases are withdrawn/yanked.
    #[serde(default)]
    pub withdrawn_count: Option<u32>,
    /// Whether THIS version is currently withdrawn on its registry — ranking
    /// applies a graded demotion (never a binary visibility cut).
    #[serde(default)]
    pub withdrawn: bool,
    /// Per-ecosystem popularity percentile in parts-per-10_000
    /// (`0..=10_000` ≡ `0.0..=1.0`). `None` until the offline CDF job fills it.
    /// Integer so the enclosing record stays `Eq`.
    #[serde(default)]
    pub popularity_pct: Option<u16>,
    /// Typosquat / name land-grab suspect. Ranking skips the exact-name bonus
    /// and multiplies the fused score by the squat gate factor.
    #[serde(default)]
    pub squat_suspect: bool,
    /// Known or flagged malware. Ranking multiplies the fused score by the
    /// malware gate factor (still visible, but buried).
    #[serde(default)]
    pub malware: bool,
    /// Declared repository path contains the package name (soft trust signal).
    #[serde(default)]
    pub verified_repo: bool,
}

impl SearchFacets {
    /// Project the weighted [`RichMetadata`] down to its `Eq`-able facets.
    #[must_use]
    pub fn from_rich(rich: &RichMetadata) -> Self {
        Self {
            keywords: rich.keywords.iter().map(|(_, slug)| slug.clone()).collect(),
            quality_ppm: rich.quality_ppm().min(1_000_000) as u32,
            description: None,
            downloads: None,
            dependencies: rich.dependencies.clone(),
            dependents: None,
            repo_slug: None,
            license: None,
            release_count: None,
            withdrawn_count: None,
            withdrawn: false,
            popularity_pct: None,
            squat_suspect: false,
            malware: false,
            verified_repo: false,
        }
    }

    /// Decode `popularity_pct` (parts-per-10_000) as a float in `0.0..=1.0`.
    #[must_use]
    pub fn popularity_pct_f32(&self) -> Option<f32> {
        self.popularity_pct
            .map(|ppm| (f32::from(ppm.min(10_000)) / 10_000.0).clamp(0.0, 1.0))
    }

    /// Encode a float percentile in `0.0..=1.0` as parts-per-10_000 (`0..=10_000`).
    #[must_use]
    pub fn encode_popularity_pct(pct: f32) -> u16 {
        (pct.clamp(0.0, 1.0) * 10_000.0).round() as u16
    }

    /// The keyword slugs space-joined for the tantivy `keywords` TEXT field.
    #[must_use]
    pub fn keyword_text(&self) -> String {
        self.keywords
            .iter()
            .map(SmolStr::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Quality back as a `0.0..=1.0` float, for the ranking fusion.
    #[must_use]
    pub fn quality(&self) -> f32 {
        self.quality_ppm as f32 / 1_000_000.0
    }
}
