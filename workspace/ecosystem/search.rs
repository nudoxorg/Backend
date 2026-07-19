//! Per-ecosystem search normalization: stopwords, name-convention stripping,
//! query normalization, category taxonomy, downloads calibration.
//!
//! One `&'static SearchNorms` per impl. Behavior that differs per ecosystem is
//! carried as plain function pointers so the whole table stays `const`-able
//! and the impl modules read as data.

/// The shared English stopword list (sorted; binary-searched). Per-ecosystem
/// convention words (`crate`, `rust`, `npm`, `py`, …) live on each impl's
/// [`SearchNorms::stopwords`], NOT here (R3).
pub const ENGLISH_STOPWORDS: &[&str] = &[
	"a", "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "be",
	"been", "before", "being", "both", "build", "but", "by", "can", "code", "do", "each",
	"easy", "even", "example", "examples", "fast", "feature", "features", "few", "for",
	"from", "get", "good", "had", "has", "have", "he", "how", "in", "into", "is", "it",
	"just", "large", "libraries", "library", "make", "many", "may", "me", "more", "most",
	"much", "new", "no", "nor", "not", "of", "on", "one", "only", "option", "options",
	"or", "other", "over", "package", "readme", "run", "set", "she", "simple", "small",
	"so", "some", "such", "than", "that", "the", "then", "these", "they", "this",
	"those", "to", "todo", "up", "us", "usable", "usage", "use", "used", "useful",
	"uses", "using", "very", "via", "was", "we", "well", "were", "what", "when",
	"where", "which", "who", "will", "wip", "with", "without", "yet", "you",
];

/// How a known-ecosystem query is rewritten before clause building (§8.4):
/// npm folds `@scope/name` into a namespace constraint, Go strips the
/// authority, Maven splits `group:artifact`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedQuery {
	pub terms: String,
	pub namespace: Option<String>,
}

/// Everything the search/ranking pipeline asks an ecosystem.
pub struct SearchNorms {
	/// Per-ecosystem stopword extras (sorted, lowercase; e.g. Rust:
	/// `crate`/`crates`/`rust`; npm: `js`/`node`; Go: `go`/`golang`).
	pub stopwords: &'static [&'static str],

	/// Separators whose presence marks a query as "specific" (R2). Default
	/// `['-', '_', '/', '.', '@', ':']` for all current impls; a trait item so
	/// future ecosystems can differ and the value is documented per impl.
	pub specificity_separators: &'static [char],

	/// Strip ecosystem naming conventions off a package name before the
	/// contains-bonus check (R1). Rust: `cargo`/`rust` prefix + `rs` suffix;
	/// npm: `@scope/`; Python: `py`/`python` affixes; Go: authority; others:
	/// identity.
	pub strip_conventions: fn(&str) -> &str,

	/// Rewrite a known-ecosystem query before clause building (§8.4).
	/// Identity for most impls.
	pub normalize_query: fn(&str) -> NormalizedQuery,

	/// Translate a native taxonomy entry (Cargo category, PyPI trove
	/// classifier) into the shared internal taxonomy; `None` = drop.
	pub map_category: fn(&str) -> Option<&'static str>,

	/// Multiplier normalizing this ecosystem's download volumes onto the
	/// crates.io-calibrated ranking thresholds (S4). `None` = no download
	/// source; downloads-driven ranking stages never fire.
	pub downloads_scale: Option<f32>,
}

impl SearchNorms {
	/// Whether `word` (lowercase) is a stopword for this ecosystem — shared
	/// English list plus the per-ecosystem extras.
	pub fn is_stopword(&self, word: &str) -> bool {
		ENGLISH_STOPWORDS.binary_search(&word).is_ok()
			|| self.stopwords.binary_search(&word).is_ok()
	}

	/// Whether a query string is "specific" (R2): contains any of this
	/// ecosystem's separators or is long.
	pub fn query_is_specific(&self, query: &str) -> bool {
		query.contains(self.specificity_separators) || query.len() > 15
	}
}

/// The default separator set shared by every current impl.
pub const DEFAULT_SPECIFICITY_SEPARATORS: &[char] = &['-', '_', '/', '.', '@', ':'];

/// Identity `strip_conventions`.
pub fn strip_nothing(name: &str) -> &str { name }

/// Identity `normalize_query`.
pub fn normalize_identity(terms: &str) -> NormalizedQuery {
	NormalizedQuery { terms: terms.to_owned(), namespace: None }
}

/// The shared internal category taxonomy (the lib.rs-derived slugs the
/// ranking/diversity layers already know). `map_category` targets values from
/// this list.
pub const INTERNAL_CATEGORIES: &[&str] = &[
	"algorithms",
	"async-io",
	"command-line-utilities",
	"compression",
	"cryptography",
	"data-structures",
	"database",
	"embedded",
	"encoding",
	"graphics",
	"network-programming",
	"parser",
	"science",
	"wasm",
	"web-programming",
];

/// `map_category` for ecosystems whose native taxonomy IS the internal one
/// (Rust) — pass through known slugs, drop the rest.
pub fn map_internal_category(raw: &str) -> Option<&'static str> {
	INTERNAL_CATEGORIES
		.binary_search(&raw)
		.ok()
		.map(|index| INTERNAL_CATEGORIES[index])
}

/// `map_category` for ecosystems with no native taxonomy (their keywords/tags
/// already flow as keywords).
pub fn map_no_category(_raw: &str) -> Option<&'static str> { None }
