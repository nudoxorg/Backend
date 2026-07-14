//! Rich metadata extraction — Phase 2.
//!
//! Turns a package's ingest inputs ([`ExtractionInput`]) into weighted keywords
//! and a quality score ([`RichMetadata`]).  All logic is self-contained; the only
//! workspace item used is [`crate::registry::metadata::heuristics`] (normalize_keyword,
//! Synonyms, Specifics).

use smol_str::SmolStr;
use std::borrow::Borrow;
use std::collections::HashMap;

use crate::registry::metadata::heuristics::{normalize_keyword, Specifics, Synonyms};

// ---------------------------------------------------------------------------
// Stopwords
// ---------------------------------------------------------------------------

const STOPWORDS: &[&str] = &[
	"a", "an", "the", "and", "or", "but", "nor", "so", "yet",
	"in", "on", "at", "to", "for", "of", "by", "as", "up", "is",
	"it", "be", "do", "we", "he", "she", "they", "you", "me", "us",
	"this", "that", "these", "those", "what", "which", "who",
	"all", "any", "both", "each", "few", "more", "most", "other",
	"some", "such", "than", "then", "when", "where", "how",
	"not", "no", "via", "with", "without", "about", "from",
	"into", "over", "after", "before", "will", "can", "may", "one",
	"rust", "crate", "crates", "library", "libraries", "package",
	"use", "used", "using", "usage", "useful", "usable", "uses",
	"new", "get", "set", "build", "code", "make", "run",
	"has", "have", "had", "are", "was", "were", "been", "being",
	"just", "also", "even", "only", "very", "well", "much",
	"many", "large", "small", "fast", "easy", "simple", "good",
	"todo", "wip", "readme", "example", "examples",
	"feature", "features", "option", "options",
];

const IDENT_STOPWORDS: &[&str] = &[
	"impl", "default", "debug", "clone", "copy", "hash", "eq", "ord",
	"partial", "into", "from", "as", "try", "with", "new", "get", "set",
	"is", "has", "to", "ref", "mut", "iter", "str", "self",
	"inner", "outer", "helper", "util", "utils", "misc",
	"test", "tests", "bench", "benches", "mock",
	"init", "create", "build", "make", "run", "start", "stop",
	"read", "write", "parse", "format", "convert", "check", "validate",
	"send", "recv", "open", "close", "drop",
	"add", "remove", "insert", "delete", "update", "find", "search",
	"main", "app", "core", "lib", "mod", "pub", "fn", "struct", "enum",
	"err", "ok", "none", "some", "true", "false",
	"len", "size", "count", "index", "pos", "ptr", "raw",
];

/// README section headers to skip (lowercased, trimmed).
const SKIP_SECTIONS: &[&str] = &[
	"license", "licensing", "contributing", "contribution", "contributions",
	"installation", "install", "installing", "changelog", "change log",
	"credits", "acknowledgements", "acknowledgement", "acknowledgments",
	"author", "authors", "todo", "code of conduct",
	"building", "build", "setup", "msrv",
	"minimum supported rust", "semantic versioning", "copyright",
	"sponsor", "sponsors", "sponsoring",
];

/// Well-known category slugs used for inference when none declared.
const KNOWN_CATEGORIES: &[&str] = &[
	"async-io", "parser", "web-programming", "command-line-utilities",
	"encoding", "compression", "cryptography", "data-structures",
	"algorithms", "science", "embedded", "wasm", "graphics",
	"database", "network-programming",
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
	pub fn new() -> Self { Self::default() }

	/// Add `score` points if `has_it`, else 0.
	#[inline]
	pub fn has(&mut self, for_what: &'static str, score: u32, has_it: bool) -> ScoreAdj<'_> {
		self.score_f(for_what, f64::from(score), if has_it { f64::from(score) } else { 0. })
	}

	/// Add `n` points (clamped to `max_score`).
	#[inline]
	pub fn n(&mut self, for_what: &'static str, max_score: u32, n: impl Into<i64>) -> ScoreAdj<'_> {
		self.score_f(for_what, f64::from(max_score), n.into() as f64)
	}

	/// Add `max_score * n` where `n ∈ 0..=1`.
	#[track_caller]
	pub fn frac(&mut self, for_what: &'static str, max_score: u32, n: impl Into<f64>) -> ScoreAdj<'_> {
		let n = n.into();
		assert!((0. ..=1.).contains(&n), "frac n={n} out of 0..=1");
		let max = f64::from(max_score);
		self.score_f(for_what, max, n * max)
	}

	/// Raw score entry.
	#[track_caller]
	pub fn score_f(&mut self, for_what: &'static str, max_score: f64, n: impl Into<f64>) -> ScoreAdj<'_> {
		let n = n.into();
		assert!(max_score > 0.);
		self.total += max_score;
		self.scores.push((n.max(0.), max_score, for_what));
		ScoreAdj { score: &mut self.scores.last_mut().unwrap().0 }
	}

	/// Embed a sub-score group; `max_score` is the weight of the sub-group in
	/// this accumulator.
	pub fn group(&mut self, for_what: &'static str, max_score: u32, group: impl Borrow<Self>) -> ScoreAdj<'_> {
		self.frac(for_what, max_score, group.borrow().total())
	}

	/// Compute the overall score in 0..=1.
	#[must_use]
	pub fn total(&self) -> f64 {
		if self.total == 0. { return 0.; }
		let sum: f64 = self.scores.iter().map(|&(v, limit, _)| v.max(0.).min(limit)).sum();
		sum / self.total
	}
}

impl ScoreAdj<'_> {
	/// Multiply the stored value by `by`.
	pub fn mul(&mut self, by: f64) { *self.score *= by; }
	/// Apply an arbitrary transformation to the stored value.
	pub fn adj(&mut self, f: impl FnOnce(f64) -> f64) { *self.score = f(*self.score); }
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
}

impl RichMetadata {
	/// Keyword slugs joined by spaces, weight-ordered — ready for a tantivy
	/// TEXT field.
	#[must_use]
	pub fn keyword_text(&self) -> String {
		self.keywords.iter().map(|(_, k)| k.as_str()).collect::<Vec<_>>().join(" ")
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
}

impl SearchFacets {
	/// Project the weighted [`RichMetadata`] down to its `Eq`-able facets.
	#[must_use]
	pub fn from_rich(rich: &RichMetadata) -> Self {
		Self {
			keywords:    rich.keywords.iter().map(|(_, slug)| slug.clone()).collect(),
			quality_ppm: rich.quality_ppm().min(1_000_000) as u32,
		}
	}

	/// The keyword slugs space-joined for the tantivy `keywords` TEXT field.
	#[must_use]
	pub fn keyword_text(&self) -> String {
		self.keywords.iter().map(SmolStr::as_str).collect::<Vec<_>>().join(" ")
	}

	/// Quality back as a `0.0..=1.0` float, for the ranking fusion.
	#[must_use]
	pub fn quality(&self) -> f32 { self.quality_ppm as f32 / 1_000_000.0 }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn is_stopword(w: &str) -> bool {
	STOPWORDS.contains(&w)
}

fn is_ident_stopword(w: &str) -> bool {
	IDENT_STOPWORDS.contains(&w) || STOPWORDS.contains(&w)
}

/// Split a prose string into word candidates, normalize each, and yield
/// `(normalized, raw_weight)` pairs where `raw_weight` is the caller-supplied
/// source weight.
fn prose_words(text: &str, weight: f32) -> Vec<(SmolStr, f32)> {
	text.split(|c: char| {
		c.is_ascii_whitespace()
			|| matches!(c, '.' | ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ';' | '!' | '?')
	})
	.filter(|w| w.len() >= 2)
	.map(normalize_keyword)
	.filter(|w| !w.is_empty() && w.len() >= 2)
	.filter(|w| !is_stopword(w.as_str()))
	.map(|w| (w, weight))
	.collect()
}

/// Split an identifier into its component words (snake_case split, camelCase
/// split), strip prefixes/suffixes, and normalize.
fn ident_words(ident: &str, weight: f32) -> Vec<(SmolStr, f32)> {
	// Convert camelCase → snake_case first by inserting underscores before
	// uppercase runs.
	let snake = camel_to_snake(ident);

	// Strip common method prefixes.
	let prefixes = ["get_", "set_", "is_", "as_", "to_", "try_", "into_", "from_", "with_", "new_"];
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
		.filter(|w| !is_ident_stopword(w.as_str()))
		.map(|w| (w, weight))
		.collect()
}

/// Naive camelCase → snake_case: insert `_` before uppercase letters.
fn camel_to_snake(s: &str) -> String {
	let mut out = String::with_capacity(s.len() + 4);
	for (i, c) in s.char_indices() {
		if c.is_ascii_uppercase() && i > 0 {
			out.push('_');
		}
		out.push(c.to_ascii_lowercase());
	}
	out
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

	for line in readme.lines() {
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
			section_start = pos + line.len() + 1; // +1 for '\n'
		}
		pos += line.len() + 1;
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

	if matches!(h, "overview" | "about" | "summary" | "introduction" | "description" | "synopsis" | "what is this" | "motivation") {
		return 0.45;
	}
	if h.starts_with("example") || h.starts_with("usage") || h.starts_with("feature") || h == "features" {
		return 0.35;
	}
	if matches!(h, "getting started" | "quick start" | "quickstart" | "documentation" | "how it works" | "how to use") {
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
			let code_blocks = r.lines()
				.filter(|l| l.trim_start().starts_with("```"))
				.count() as u32 / 2; // open+close = 1 block
			// Count headings.
			let sections = r.lines()
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
pub fn extract(
	input: &ExtractionInput<'_>,
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

	// 3. Crate name parts (weight 0.6, hidden — they are not stopword-filtered
	//    like prose; they're kept if len > 2 and not obviously generic).
	let name_skip = ["rs", "impl", "internal", "shared"];
	for part in input.name.split(|c: char| !c.is_ascii_alphanumeric()) {
		if part.len() <= 2 || name_skip.contains(&part) {
			continue;
		}
		let norm = normalize_keyword(part);
		if !norm.is_empty() && norm.len() >= 2 && !is_stopword(norm.as_str()) {
			add!(norm, 0.6);
		}
	}

	// 4. Description words (weight 0.6).
	if let Some(desc) = input.description {
		for (kw, w) in prose_words(desc, 0.6) {
			add!(kw, w);
		}
	}

	// 5. README words (weight 0.3, skip boilerplate sections).
	if let Some(readme) = input.readme {
		for (text, section_w) in readme_relevant_text(readme) {
			for (kw, w) in prose_words(text, 0.3 * section_w) {
				add!(kw, w);
			}
		}
	}

	// 6. Source identifiers (weight 0.25).
	for ident in input.identifiers {
		for (kw, w) in ident_words(ident.as_str(), 0.25) {
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
	bag.retain(|k, _| !k.starts_with("dep:") && !is_stopword(k.as_str()));

	// Sort descending by weight, cap at 20.
	let mut keywords: Vec<(f32, SmolStr)> = bag.into_iter().map(|(k, w)| (w, k)).collect();
	keywords.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
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

	RichMetadata { keywords, categories, quality }
}

fn derive_categories(input: &ExtractionInput<'_>, keywords: &[(f32, SmolStr)]) -> Vec<(f32, SmolStr)> {
	if !input.manifest_categories.is_empty() {
		return input.manifest_categories.iter()
			.take(3)
			.map(|c| (1.0f32, normalize_keyword(c.as_str())))
			.filter(|(_, k)| !k.is_empty())
			.collect();
	}

	// Infer from keyword overlap with known categories.
	let kw_set: std::collections::HashSet<&str> = keywords.iter().map(|(_, k)| k.as_str()).collect();
	let cats: Vec<(f32, SmolStr)> = KNOWN_CATEGORIES.iter()
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
		};
		let meta = extract(&input, None, None);
		assert!(meta.quality_ppm() <= 1_000_000);
	}
}
