//! Rich metadata extraction — Phase 2.
//!
//! Turns a package's ingest inputs ([`ExtractionInput`]) into weighted keywords
//! and a quality score ([`RichMetadata`]).  All logic is self-contained; the only
//! workspace items used are [`crate::metadata::heuristics`] (normalize_keyword,
//! Synonyms, Specifics) and [`ecosystem::search::SearchNorms`] (per-ecosystem
//! stopwords — R3).
//!
//! The shared English stopword list now lives in `ecosystem::search::ENGLISH_STOPWORDS`
//! (sorted, binary-searched). Per-ecosystem convention words (`crate`, `rust`,
//! `npm`, `py`, …) live on each impl's `SearchNorms::stopwords`. The old
//! `STOPWORDS` array is gone; `is_stopword` delegates to `norms.is_stopword`.

use smol_str::SmolStr;
use std::borrow::Borrow;
use std::collections::HashMap;

use crate::metadata::heuristics::{Specifics, Synonyms, normalize_keyword};
use ecosystem::search::SearchNorms;

/// Identifier-specific stopwords (code structure words with no semantic meaning
/// in search context). These are LOCAL to rich.rs: they supplement the shared
/// English + ecosystem stopwords for the identifier extraction path only.
const IDENT_STOPWORDS: &[&str] = &[
    "add", "app", "as", "bench", "benches", "build", "check", "clone", "close", "convert", "copy",
    "core", "count", "create", "debug", "default", "delete", "drop", "enum", "eq", "err", "false",
    "find", "fn", "format", "from", "get", "has", "hash", "helper", "impl", "index", "init",
    "inner", "insert", "into", "is", "iter", "len", "lib", "main", "make", "misc", "mock", "mod",
    "mut", "new", "none", "ok", "open", "ord", "outer", "parse", "partial", "pos", "ptr", "pub",
    "raw", "read", "recv", "ref", "remove", "run", "search", "self", "send", "set", "size", "some",
    "start", "stop", "str", "struct", "test", "tests", "to", "true", "try", "update", "util",
    "utils", "validate", "with", "write",
];

/// README section headers to skip (lowercased, trimmed).
const SKIP_SECTIONS: &[&str] = &[
    "license",
    "licensing",
    "contributing",
    "contribution",
    "contributions",
    "installation",
    "install",
    "installing",
    "changelog",
    "change log",
    "credits",
    "acknowledgements",
    "acknowledgement",
    "acknowledgments",
    "author",
    "authors",
    "todo",
    "code of conduct",
    "building",
    "build",
    "setup",
    "msrv",
    "minimum supported rust",
    "semantic versioning",
    "copyright",
    "sponsor",
    "sponsors",
    "sponsoring",
];

/// Well-known category slugs used for inference when none declared.
const KNOWN_CATEGORIES: &[&str] = &[
    "async-io",
    "parser",
    "web-programming",
    "command-line-utilities",
    "encoding",
    "compression",
    "cryptography",
    "data-structures",
    "algorithms",
    "science",
    "embedded",
    "wasm",
    "graphics",
    "database",
    "network-programming",
];

// ---------------------------------------------------------------------------
// Score accumulator (ported from ranking_src_scorer.rs)
// ---------------------------------------------------------------------------

/// Weighted quality score accumulator.  Each component contributes a clamped
/// value and a maximum; `total()` = Σclamp(v,0,max) / Σmax → 0..=1.
#[derive(Debug, Clone, Default)]
pub struct Score {
    scores: Vec<(f64, f64, &'static str)>,
    total: f64,
}

/// Handle returned by score-adding methods; allows post-hoc adjustment.
pub struct ScoreAdj<'a> {
    score: &'a mut f64,
}

impl Score {
    /// Create a new empty accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `score` points if `has_it`, else 0.
    #[inline]
    pub fn has(&mut self, for_what: &'static str, score: u32, has_it: bool) -> ScoreAdj<'_> {
        self.score_f(
            for_what,
            f64::from(score),
            if has_it { f64::from(score) } else { 0. },
        )
    }

    /// Add `n` points (clamped to `max_score`).
    #[inline]
    pub fn n(&mut self, for_what: &'static str, max_score: u32, n: impl Into<i64>) -> ScoreAdj<'_> {
        self.score_f(for_what, f64::from(max_score), n.into() as f64)
    }

    /// Add `max_score * n` where `n ∈ 0..=1`.
    #[track_caller]
    pub fn frac(
        &mut self,
        for_what: &'static str,
        max_score: u32,
        n: impl Into<f64>,
    ) -> ScoreAdj<'_> {
        let n = n.into();
        assert!((0. ..=1.).contains(&n), "frac n={n} out of 0..=1");
        let max = f64::from(max_score);
        self.score_f(for_what, max, n * max)
    }

    /// Raw score entry.
    #[track_caller]
    pub fn score_f(
        &mut self,
        for_what: &'static str,
        max_score: f64,
        n: impl Into<f64>,
    ) -> ScoreAdj<'_> {
        let n = n.into();
        assert!(max_score > 0.);
        self.total += max_score;
        self.scores.push((n.max(0.), max_score, for_what));
        ScoreAdj {
            score: &mut self.scores.last_mut().unwrap().0,
        }
    }

    /// Embed a sub-score group; `max_score` is the weight of the sub-group in
    /// this accumulator.
    pub fn group(
        &mut self,
        for_what: &'static str,
        max_score: u32,
        group: impl Borrow<Self>,
    ) -> ScoreAdj<'_> {
        self.frac(for_what, max_score, group.borrow().total())
    }

    /// Compute the overall score in 0..=1.
    #[must_use]
    pub fn total(&self) -> f64 {
        if self.total == 0. {
            return 0.;
        }
        let sum: f64 = self
            .scores
            .iter()
            .map(|&(v, limit, _)| v.max(0.).min(limit))
            .sum();
        sum / self.total
    }
}

impl ScoreAdj<'_> {
    /// Multiply the stored value by `by`.
    pub fn mul(&mut self, by: f64) {
        *self.score *= by;
    }
    /// Apply an arbitrary transformation to the stored value.
    pub fn adj(&mut self, f: impl FnOnce(f64) -> f64) {
        *self.score = f(*self.score);
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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
        }
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

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn is_stopword(w: &str, norms: &SearchNorms) -> bool {
    norms.is_stopword(w)
}

fn is_ident_stopword(w: &str, norms: &SearchNorms) -> bool {
    IDENT_STOPWORDS.binary_search(&w).is_ok() || norms.is_stopword(w)
}

/// Split a prose string into word candidates, normalize each, and yield
/// `(normalized, raw_weight)` pairs where `raw_weight` is the caller-supplied
/// source weight. Stopword filtering is delegated to `norms` (R3).
fn prose_words<'a>(
    text: &'a str,
    weight: f32,
    norms: &'a SearchNorms,
) -> impl Iterator<Item = (SmolStr, f32)> + 'a {
    text.split(|c: char| {
        c.is_ascii_whitespace()
            || matches!(
                c,
                '.' | ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ';' | '!' | '?'
            )
    })
    .filter(|w| w.len() >= 2)
    .map(normalize_keyword)
    .filter(|w| !w.is_empty() && w.len() >= 2)
    .filter(move |w| !is_stopword(w.as_str(), norms))
    .map(move |w| (w, weight))
}

/// Split an identifier into its component words (snake_case split, camelCase
/// split), strip prefixes/suffixes, and normalize. Stopword filtering uses
/// `norms` (R3).
fn ident_words(ident: &str, weight: f32, norms: &SearchNorms) -> Vec<(SmolStr, f32)> {
    // Convert camelCase → snake_case first by inserting underscores before
    // uppercase runs.
    let snake = camel_to_snake(ident);

    // Strip common method prefixes.
    let prefixes = [
        "get_", "set_", "is_", "as_", "to_", "try_", "into_", "from_", "with_", "new_",
    ];
    let mut s: &str = snake.as_str();
    for p in &prefixes {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest;
            break;
        }
    }

    // Strip common suffixes.
    let suffixes = ["_ref", "_mut", "_iter", "_t", "_str"];
    for suf in &suffixes {
        if let Some(rest) = s.strip_suffix(suf) {
            s = rest;
            break;
        }
    }

    s.split('_')
        .filter(|w| w.len() >= 2)
        .map(normalize_keyword)
        .filter(|w| !w.is_empty() && w.len() >= 2)
        .filter(|w| !is_ident_stopword(w.as_str(), norms))
        .map(|w| (w, weight))
        .collect()
}

/// Convert camelCase to snake_case using heck.
fn camel_to_snake(s: &str) -> String {
    heck::AsSnakeCase(s).to_string()
}

/// Combine weight from a new source into an existing entry: prefer the larger,
/// add 30 % of the smaller (multi-source agreement bonus).
fn combine_weights(existing: f32, new: f32) -> f32 {
    let hi = existing.max(new);
    let lo = existing.min(new);
    hi + lo * 0.3
}

/// Parse README into sections and return `(text, weight)` pairs for relevant
/// sections only.
fn readme_relevant_text(readme: &str) -> Vec<(&str, f32)> {
    let mut sections: Vec<(&str, f32)> = Vec::new();
    let mut current_header: Option<&str> = None;
    let mut section_start = 0usize;
    let mut pos = 0usize;

    for line in readme.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            // Emit the previous section body.
            let body = readme.get(section_start..pos).unwrap_or("").trim();
            let weight = section_weight(current_header);
            if weight > 0.0 && !body.is_empty() {
                sections.push((body, weight));
            }
            // Advance to new section.
            let header = trimmed.trim_start_matches('#').trim();
            current_header = Some(header);
            section_start = pos + line.len(); // No need to + 1, \n is included
        }
        pos += line.len();
    }
    // Emit the last section.
    let body = readme.get(section_start..).unwrap_or("").trim();
    let weight = section_weight(current_header);
    if weight > 0.0 && !body.is_empty() {
        sections.push((body, weight));
    }

    sections
}

/// Return the prose weight for a README section with the given header.
/// Returns 0.0 for sections that should be skipped entirely.
fn section_weight(header: Option<&str>) -> f32 {
    let h = match header {
        None => return 0.3, // preamble before any heading
        Some(h) => h.to_lowercase(),
    };
    let h = h.trim();

    // Skip boilerplate sections.
    for skip in SKIP_SECTIONS {
        if h == *skip || h.contains(skip) {
            return 0.0;
        }
    }

    if matches!(
        h,
        "overview"
            | "about"
            | "summary"
            | "introduction"
            | "description"
            | "synopsis"
            | "what is this"
            | "motivation"
    ) {
        return 0.45;
    }
    if h.starts_with("example")
        || h.starts_with("usage")
        || h.starts_with("feature")
        || h == "features"
    {
        return 0.35;
    }
    if matches!(
        h,
        "getting started"
            | "quick start"
            | "quickstart"
            | "documentation"
            | "how it works"
            | "how to use"
    ) {
        return 0.25;
    }

    0.3
}

// ---------------------------------------------------------------------------
// Quality scoring
// ---------------------------------------------------------------------------

fn readme_score(readme: Option<&str>) -> Score {
    let mut s = Score::new();
    let (text_len, code_blocks, sections) = match readme {
        None => (0usize, 0u32, 0u32),
        Some(r) => {
            let text_len = r.len();
            // Count fenced code blocks: lines starting with ```
            let code_blocks = r
                .lines()
                .filter(|l| l.trim_start().starts_with("```"))
                .count() as u32
                / 2; // open+close = 1 block
            // Count headings.
            let sections = r
                .lines()
                .filter(|l| l.trim_start().starts_with('#'))
                .count() as u32;
            (text_len, code_blocks, sections)
        }
    };
    s.frac("readme_text", 75, (text_len as f64 / 3000.).min(1.0));
    s.n("readme_code_blocks", 25, (code_blocks * 5).min(25));
    s.has("readme_has_code", 30, code_blocks > 0);
    s.n("readme_sections", 30, (sections * 4).min(30));
    s
}

fn compute_quality(input: &ExtractionInput<'_>) -> f32 {
    let mut score = Score::new();

    // Description length.
    let desc_len = input.description.map(|d| d.len()).unwrap_or(0);
    score.frac("description_len", 30, (desc_len as f64 / 300.).min(1.0));

    // Manifest completeness.
    score.has("repository", 10, input.has_repository);
    score.has("documentation", 20, input.has_documentation);
    score.has("license", 10, input.has_license);
    score.has("keywords", 7, !input.manifest_keywords.is_empty());
    score.has("categories", 5, !input.manifest_categories.is_empty());

    // README richness as a group.
    let rs = readme_score(input.readme);
    score.group("README", 5, rs);

    // Code size.
    score.has("non_trivial", 2, input.loc > 700);
    score.has("non_giant", 1, input.loc < 80_000);
    score.frac("loc", 3, (input.loc as f64 / 10_000.).min(1.0));

    // Release-history group — only contributes when release_count is known.
    if let Some(release_count) = input.release_count {
        let withdrawn_count = input.withdrawn_count.unwrap_or(0);
        let mut rh = Score::new();
        rh.n("release_maturity", 20, i64::from(release_count.min(20)));
        let ratio = if release_count == 0 {
            0.0f64
        } else {
            f64::from(withdrawn_count) / f64::from(release_count)
        };
        rh.has("low_withdrawn_ratio", 3, ratio < 0.15);
        score.group("release_history", 23, rh);
    }

    score.total() as f32
}

// ---------------------------------------------------------------------------
// Keyword extraction
// ---------------------------------------------------------------------------

fn apply_synonyms_and_specifics(
    bag: &mut HashMap<SmolStr, f32>,
    synonyms: Option<&Synonyms>,
    specifics: Option<&Specifics>,
) {
    if synonyms.is_none() && specifics.is_none() {
        return;
    }

    // Collect (old_key, new_key, new_weight) to avoid borrow issues.
    let mut remap: Vec<(SmolStr, SmolStr, f32)> = Vec::new();
    for (kw, &w) in bag.iter() {
        let mut current = kw.clone();
        let mut current_w = w;

        if let Some(syn) = synonyms {
            let (canonical, syn_w) = syn.normalize(current.as_str(), 3);
            if canonical != current.as_str() {
                current = SmolStr::from(canonical);
                current_w *= syn_w;
            }
        }

        if let Some(sp) = specifics
            && sp.is_bland(current.as_str()).is_some() {
                current_w *= 0.3;
            }

        if current != *kw || (current_w - w).abs() > 1e-6 {
            remap.push((kw.clone(), current, current_w));
        }
    }

    for (old, new, new_w) in remap {
        bag.remove(&old);
        let entry = bag.entry(new).or_insert(0.0);
        *entry = combine_weights(*entry, new_w);
    }
}

/// Extract rich metadata from the given input.
///
/// `norms` is the per-ecosystem [`SearchNorms`] whose `is_stopword` governs
/// prose/identifier filtering. Pass `ecosystem::spec(lang).search_norms()` at
/// the call site (R3).
pub fn extract(
    input: &ExtractionInput<'_>,
    norms: &'static SearchNorms,
    synonyms: Option<&Synonyms>,
    specifics: Option<&Specifics>,
) -> RichMetadata {
    let mut bag: HashMap<SmolStr, f32> = HashMap::new();

    /// Insert or combine into `bag`.
    macro_rules! add {
        ($kw:expr, $w:expr) => {{
            let kw: SmolStr = $kw;
            let w: f32 = $w;
            if !kw.is_empty() && kw.len() >= 2 {
                let entry = bag.entry(kw).or_insert(0.0);
                *entry = combine_weights(*entry, w);
            }
        }};
    }

    // 1. Manifest keywords (weight 1.0).
    for kw in input.manifest_keywords {
        let norm = normalize_keyword(kw.as_str());
        if !norm.is_empty() {
            add!(norm, 1.0);
        }
    }

    // 2. Manifest categories (weight 0.7).
    for cat in input.manifest_categories {
        let norm = normalize_keyword(cat.as_str());
        if !norm.is_empty() {
            add!(norm, 0.7);
        }
    }

    // 3. Package name parts (weight 0.6, hidden — kept if len > 2 and not obviously generic).
    let name_skip = ["rs", "impl", "internal", "shared"];
    for part in input.name.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.len() <= 2 || name_skip.contains(&part) {
            continue;
        }
        let norm = normalize_keyword(part);
        if !norm.is_empty() && norm.len() >= 2 && !is_stopword(norm.as_str(), norms) {
            add!(norm, 0.6);
        }
    }

    // 4. Description words (weight 0.6).
    if let Some(desc) = input.description {
        for (kw, w) in prose_words(desc, 0.6, norms) {
            add!(kw, w);
        }
    }

    // 5. README words (weight 0.3, skip boilerplate sections).
    if let Some(readme) = input.readme {
        for (text, section_w) in readme_relevant_text(readme) {
            for (kw, w) in prose_words(text, 0.3 * section_w, norms) {
                add!(kw, w);
            }
        }
    }

    // 6. Source identifiers (weight 0.25).
    for ident in input.identifiers {
        for (kw, w) in ident_words(ident.as_str(), 0.25, norms) {
            add!(kw, w);
        }
    }

    // 7. Dependency keywords as `dep:name` (weight 0.2, invisible).
    for dep in input.dependencies {
        let dep_kw = SmolStr::from(format!("dep:{dep}"));
        add!(dep_kw, 0.2);
    }

    // Apply synonyms + specifics.
    apply_synonyms_and_specifics(&mut bag, synonyms, specifics);

    // Remove dep: keywords from visible output and any remaining stopwords.
    bag.retain(|k, _| !k.starts_with("dep:") && !is_stopword(k.as_str(), norms));

    // Sort descending by weight, cap at 20.
    let mut keywords: Vec<(f32, SmolStr)> = bag.into_iter().map(|(k, w)| (w, k)).collect();
    keywords.sort_by(|a, b| b.0.total_cmp(&a.0));
    keywords.truncate(20);

    // Normalize so the top keyword = 1.0.
    if let Some(&(top_w, _)) = keywords.first()
        && top_w > 0.0 {
            for (w, _) in &mut keywords {
                *w = (*w / top_w).clamp(0.0, 1.0);
            }
        }

    // Categories: from manifest, else infer from keywords.
    let categories = derive_categories(input, &keywords);

    let quality = compute_quality(input);

    // Dependency slugs: lowercase + trim, deduplicated, sorted for determinism.
    // No `normalize_keyword` — names must round-trip exactly for the reverse-dep join.
    let mut dependencies: Vec<SmolStr> = input
        .dependencies
        .iter()
        .map(|d| SmolStr::from(d.trim().to_ascii_lowercase()))
        .filter(|d| !d.is_empty())
        .collect();
    dependencies.sort_unstable();
    dependencies.dedup();

    RichMetadata {
        keywords,
        categories,
        quality,
        dependencies,
    }
}

fn derive_categories(
    input: &ExtractionInput<'_>,
    keywords: &[(f32, SmolStr)],
) -> Vec<(f32, SmolStr)> {
    if !input.manifest_categories.is_empty() {
        return input
            .manifest_categories
            .iter()
            .take(3)
            .map(|c| (1.0f32, normalize_keyword(c.as_str())))
            .filter(|(_, k)| !k.is_empty())
            .collect();
    }

    // Infer from keyword overlap with known categories.
    let kw_set: std::collections::HashSet<&str> =
        keywords.iter().map(|(_, k)| k.as_str()).collect();
    let cats: Vec<(f32, SmolStr)> = KNOWN_CATEGORIES
        .iter()
        .filter(|&&cat| kw_set.contains(cat))
        .map(|&cat| (0.5f32, SmolStr::from(cat)))
        .take(3)
        .collect();
    cats
}

// ---------------------------------------------------------------------------
// Tests (internal)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Rust norms: suitable for tests that just need a valid `&'static SearchNorms`.
    fn rust_norms() -> &'static SearchNorms {
        ecosystem::spec(ecosystem::Language::Rust).search_norms()
    }

    #[test]
    fn score_basic() {
        let mut s = Score::new();
        s.has("x", 10, true);
        assert!((s.total() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn score_partial() {
        let mut s = Score::new();
        s.has("x", 5, true);
        s.has("y", 15, false);
        let t = s.total();
        assert!(t > 0.2 && t < 0.3, "got {t}");
    }

    #[test]
    fn quality_ppm_bounded() {
        let input = ExtractionInput {
            name: "test",
            has_repository: true,
            has_documentation: true,
            has_license: true,
            description: Some("A well-documented crate"),
            manifest_keywords: &["parsing".to_string()],
            manifest_categories: &[],
            readme: Some("# Test\n\nExample crate.\n\n```rust\nfn main() {}\n```"),
            identifiers: &[],
            dependencies: &[],
            loc: 2000,
            ..Default::default()
        };
        let meta = extract(&input, rust_norms(), None, None);
        assert!(meta.quality_ppm() <= 1_000_000);
    }

    #[test]
    fn rust_ecosystem_words_stopword_filtered() {
        // "rust"/"crate"/"crates" must be stopwords when using Rust norms.
        let norms = rust_norms();
        assert!(norms.is_stopword("rust"), "'rust' must be stopped by Rust norms");
        assert!(norms.is_stopword("crate"), "'crate' must be stopped by Rust norms");
        assert!(norms.is_stopword("crates"), "'crates' must be stopped by Rust norms");
    }

    #[test]
    fn npm_ecosystem_words_not_stopped_by_rust_norms() {
        let norms = rust_norms();
        assert!(!norms.is_stopword("node"), "'node' NOT stopped by Rust norms");
        assert!(!norms.is_stopword("npm"), "'npm' NOT stopped by Rust norms");
    }

    #[test]
    fn extract_uses_ecosystem_norms_for_stopwords() {
        // "rust" is a stopword for Rust norms → must not appear in keywords.
        let rust_norms = ecosystem::spec(ecosystem::Language::Rust).search_norms();
        let input = ExtractionInput {
            name: "mylib",
            description: Some("A rust library for parsing"),
            manifest_keywords: &[],
            manifest_categories: &[],
            readme: None,
            identifiers: &[],
            dependencies: &[],
            has_repository: false,
            has_documentation: false,
            has_license: false,
            loc: 0,
            ..Default::default()
        };
        let meta = extract(&input, rust_norms, None, None);
        let kw_slugs: Vec<&str> = meta.keywords.iter().map(|(_, k)| k.as_str()).collect();
        assert!(!kw_slugs.contains(&"rust"), "'rust' must be filtered by Rust norms");

        // "node" is a stopword for npm norms → must not appear.
        let npm_norms = ecosystem::spec(ecosystem::Language::Typescript).search_norms();
        let input_npm = ExtractionInput {
            name: "axios",
            description: Some("Promise based http client for node"),
            ..input
        };
        let meta_npm = extract(&input_npm, npm_norms, None, None);
        let kw_npm: Vec<&str> = meta_npm.keywords.iter().map(|(_, k)| k.as_str()).collect();
        assert!(!kw_npm.contains(&"node"), "'node' must be filtered by npm norms");
    }

    // -----------------------------------------------------------------------
    // Task 2: dependency slug extraction
    // -----------------------------------------------------------------------

    #[test]
    fn dependencies_dedup_sort_lowercase() {
        let deps = vec![
            "Serde".to_string(),
            "tokio".to_string(),
            "SERDE".to_string(),
            "Anyhow".to_string(),
            "tokio".to_string(),
        ];
        let input = ExtractionInput {
            name: "mylib",
            dependencies: &deps,
            ..Default::default()
        };
        let meta = extract(&input, rust_norms(), None, None);
        // Expect sorted, deduplicated, lowercased.
        assert_eq!(
            meta.dependencies,
            vec![
                SmolStr::from("anyhow"),
                SmolStr::from("serde"),
                SmolStr::from("tokio"),
            ],
            "dependencies must be sorted, deduped, and lowercased"
        );
    }

    #[test]
    fn dep_keywords_never_in_visible_keywords() {
        let deps = vec!["tokio".to_string(), "serde".to_string()];
        let input = ExtractionInput {
            name: "mylib",
            description: Some("async runtime wrapper"),
            dependencies: &deps,
            ..Default::default()
        };
        let meta = extract(&input, rust_norms(), None, None);
        for (_, kw) in &meta.keywords {
            assert!(
                !kw.starts_with("dep:"),
                "dep: keyword leaked into visible keywords: {kw}"
            );
        }
    }

    #[test]
    fn search_facets_copies_dependencies() {
        let deps = vec!["serde".to_string(), "anyhow".to_string()];
        let input = ExtractionInput {
            name: "mylib",
            dependencies: &deps,
            ..Default::default()
        };
        let meta = extract(&input, rust_norms(), None, None);
        let facets = SearchFacets::from_rich(&meta);
        assert_eq!(facets.dependencies, meta.dependencies);
    }

    // -----------------------------------------------------------------------
    // Task 3: release-maturity quality signals
    // -----------------------------------------------------------------------

    #[test]
    fn release_maturity_raises_quality() {
        let base = ExtractionInput {
            name: "mylib",
            description: Some("A useful library"),
            has_license: true,
            loc: 1000,
            ..Default::default()
        };
        let with_releases = ExtractionInput {
            release_count: Some(20),
            withdrawn_count: Some(0),
            ..base.clone()
        };

        let q_base = compute_quality(&base);
        let q_with = compute_quality(&with_releases);
        assert!(
            q_with > q_base,
            "quality with release_count=20 ({q_with}) should exceed quality without ({q_base})"
        );
    }

    #[test]
    fn none_release_count_leaves_quality_identical() {
        let base = ExtractionInput {
            name: "mylib",
            description: Some("A useful library"),
            has_license: true,
            loc: 1000,
            ..Default::default()
        };
        // Explicitly None fields — same as Default.
        let with_none = ExtractionInput {
            release_count: None,
            withdrawn_count: None,
            ..base.clone()
        };

        let q_base = compute_quality(&base);
        let q_none = compute_quality(&with_none);
        assert!(
            (q_base - q_none).abs() < 1e-7,
            "None release_count must not change quality: {q_base} vs {q_none}"
        );
    }

    #[test]
    fn high_withdrawn_ratio_penalizes_low_withdrawn_ratio_signal() {
        let good = ExtractionInput {
            name: "mylib",
            release_count: Some(10),
            withdrawn_count: Some(0),
            ..Default::default()
        };
        let bad = ExtractionInput {
            name: "mylib",
            release_count: Some(10),
            withdrawn_count: Some(2), // 20% > 15% threshold
            ..Default::default()
        };
        let q_good = compute_quality(&good);
        let q_bad = compute_quality(&bad);
        assert!(
            q_good > q_bad,
            "low withdrawn ratio should score higher than high: {q_good} vs {q_bad}"
        );
    }

    // -----------------------------------------------------------------------
    // Task 4: SearchFacets serde round-trip + legacy decode
    // -----------------------------------------------------------------------

    #[test]
    fn search_facets_serde_roundtrip_with_new_fields() {
        let facets = SearchFacets {
            keywords: vec![SmolStr::from("async"), SmolStr::from("runtime")],
            quality_ppm: 750_000,
            description: Some(SmolStr::from("An async runtime")),
            downloads: Some(100_000),
            dependencies: vec![SmolStr::from("tokio"), SmolStr::from("serde")],
            dependents: Some(42),
            repo_slug: Some(SmolStr::from("github/tokio-rs/tokio")),
            license: Some(SmolStr::from("mit")),
            release_count: Some(15),
            withdrawn_count: Some(1),
            withdrawn: false,
        };

        let json = serde_json::to_string(&facets).expect("serialize");
        let decoded: SearchFacets = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(facets, decoded, "round-trip must be identity");
    }

    #[test]
    fn search_facets_legacy_json_decodes_with_defaults() {
        // Simulate a stored facet that predates all new fields.
        let legacy = r#"{"keywords":["async","runtime"],"quality_ppm":500000}"#;
        let decoded: SearchFacets = serde_json::from_str(legacy).expect("legacy decode");
        assert_eq!(decoded.keywords, vec![SmolStr::from("async"), SmolStr::from("runtime")]);
        assert_eq!(decoded.quality_ppm, 500_000);
        assert!(decoded.dependencies.is_empty());
        assert!(decoded.dependents.is_none());
        assert!(decoded.repo_slug.is_none());
        assert!(decoded.license.is_none());
        assert!(decoded.release_count.is_none());
        assert!(decoded.withdrawn_count.is_none());
        assert!(!decoded.withdrawn);
        assert!(decoded.description.is_none());
        assert!(decoded.downloads.is_none());
    }
}
