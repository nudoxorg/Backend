//! Index-time enrichment helpers for package search (pure, no I/O).
//!
//! Called from the absorb path to fold name dash/separator parts into the
//! `keywords` TEXT field so free-text and keyword tiers can hit subtokens that
//! the identifier tokenizer would already split on `name_tokens` — without a
//! schema migration for a separate `extra` field.
//!
//! # Design choice (no SCHEMA_VERSION bump)
//!
//! Research contemplated a dedicated `extra` TEXT field (boost 0.6). Adding a
//! field requires a wipe/resync. Prefer appending extras onto the existing
//! `keywords` field: the keywords tier already boosts at 1.2, and absorb still
//! keeps `name_exact` / `name_tokens` for exact and subtoken name match.

use std::collections::HashSet;

use crate::ecosystem::{Language, LanguageExt};

/// Maximum number of name-derived extra parts folded into keywords per package.
const MAX_EXTRA_PARTS: usize = 8;

/// Result of index-time name enrichment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Enrichment {
	/// Individual extra tokens (name dash parts, collapsed form, …).
	pub keyword_extras: Vec<String>,
	/// Space-joined extras, ready to append onto a keywords field (or write
	/// alone when facets are missing).
	pub extra_text: String,
}

/// Derive keyword extras from a package name for `ecosystem`.
///
/// `name` should be the index search surface (or bare package name) — for Go,
/// pass the authority-stripped surface so host tokens never re-enter. Parts are
/// split on `-`, `_`, `.`, `/`, and whitespace; length &lt; 2, ecosystem stopwords,
/// and [`name_part_skip`] entries are dropped. Tokens already present in
/// `existing_keywords` are not re-emitted. Output is capped at [`MAX_EXTRA_PARTS`].
pub fn enrich_package_text(
	name: &str,
	existing_keywords: &str,
	ecosystem: Language,
) -> Enrichment {
	let name = name.trim();
	if name.is_empty() {
		return Enrichment::default();
	}

	let norms = ecosystem.spec().search_norms();
	let skip = name_part_skip(ecosystem);

	let existing: HashSet<String> = existing_keywords
		.split_whitespace()
		.map(|s| s.to_ascii_lowercase())
		.collect();

	let lower = name.to_ascii_lowercase();
	let mut extras: Vec<String> = Vec::new();
	let mut seen: HashSet<String> = HashSet::new();

	// Split on common package-name separators (and path/space for search_surface).
	for part in lower.split(['-', '_', '.', '/', ' ']) {
		if part.len() < 2 {
			continue;
		}
		if norms.is_stopword(part) {
			continue;
		}
		if is_name_part_skip(part, skip) {
			continue;
		}
		// Defence in depth: never index Go-style authority fragments even if a
		// caller passes a full module path instead of search_surface.
		if ecosystem == Language::Go && is_authority_like(part) {
			continue;
		}
		if existing.contains(part) {
			continue;
		}
		if seen.insert(part.to_owned()) {
			extras.push(part.to_owned());
		}
		if extras.len() >= MAX_EXTRA_PARTS {
			break;
		}
	}

	// Collapsed form without separators (`aws-s3` → `awss3`) when multi-part and
	// still under the cap — helps bare-concat typos/queries.
	if extras.len() >= 2 && extras.len() < MAX_EXTRA_PARTS {
		let collapsed: String = lower
			.chars()
			.filter(|c| c.is_ascii_alphanumeric())
			.collect();
		if collapsed.len() >= 2
			&& !existing.contains(&collapsed)
			&& seen.insert(collapsed.clone())
		{
			extras.push(collapsed);
		}
	}

	// Hard cap (collapsed may push over if we didn't check mid-loop).
	extras.truncate(MAX_EXTRA_PARTS);
	let extra_text = extras.join(" ");
	Enrichment { keyword_extras: extras, extra_text }
}

/// Merge base keyword text with enrichment extras (space-joined, no trailing
/// space). Empty enrichment leaves `existing` unchanged.
#[must_use]
pub fn merge_keywords(existing: &str, enrichment: &Enrichment) -> String {
	let existing = existing.trim();
	if enrichment.extra_text.is_empty() {
		return existing.to_owned();
	}
	if existing.is_empty() {
		return enrichment.extra_text.clone();
	}
	format!("{existing} {}", enrichment.extra_text)
}

/// Per-ecosystem name-part skip list (not full stopwords — short suffix/prefix
/// fragments that pollute keyword bags, e.g. Cargo `rs` / `impl`).
fn name_part_skip(ecosystem: Language) -> &'static [&'static str] {
	match ecosystem {
		Language::Rust => &["rs", "impl", "internal", "shared"],
		Language::Typescript => &["js", "ts", "node"],
		Language::Python => &["py", "python"],
		Language::Go => &["go", "golang"],
		Language::Java => &["java", "jdk"],
		Language::CSharp => &["net", "dotnet"],
		Language::Nix => &["nix"],
		// C/C++ naming noise: the `lib` prefix and the language words themselves.
		Language::Cpp => &["lib", "cpp", "cxx", "c"],
	}
}

fn is_name_part_skip(part: &str, skip: &[&str]) -> bool {
	skip.contains(&part)
}

/// Host / TLD fragments that appear in Go module paths. `search_surface` already
/// strips authority; this blocks residual leakage if a full path is enriched.
fn is_authority_like(part: &str) -> bool {
	matches!(
		part,
		"github"
			| "gitlab"
			| "bitbucket"
			| "codeberg"
			| "gopkg"
			| "golang"
			| "com"
			| "org"
			| "net"
			| "io"
			| "dev"
			| "app"
			| "cloud"
			| "edu"
			| "co"
	)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn serde_json_produces_parts_and_keeps_semantic_tokens() {
		let e = enrich_package_text("serde-json", "", Language::Rust);
		assert!(
			e.keyword_extras.iter().any(|p| p == "serde"),
			"expected 'serde' part, got: {:?}",
			e.keyword_extras
		);
		assert!(
			e.keyword_extras.iter().any(|p| p == "json"),
			"expected 'json' part, got: {:?}",
			e.keyword_extras
		);
		// Whole name is not "dropped" — collapsed form or parts still cover it.
		assert!(
			!e.keyword_extras.is_empty(),
			"extras must not be empty for a dashed multi-part name"
		);
		assert!(e.extra_text.contains("serde"));
		assert!(e.extra_text.contains("json"));
	}

	#[test]
	fn rust_stopword_parts_filtered() {
		// "rust" is a Rust ecosystem stopword → stripped from `rust-http`.
		let e = enrich_package_text("rust-http", "", Language::Rust);
		assert!(
			!e.keyword_extras.iter().any(|p| p == "rust"),
			"'rust' must be filtered as a Rust stopword, got: {:?}",
			e.keyword_extras
		);
		assert!(
			e.keyword_extras.iter().any(|p| p == "http"),
			"expected 'http' to survive, got: {:?}",
			e.keyword_extras
		);
	}

	#[test]
	fn name_part_skip_rs_for_rust() {
		let e = enrich_package_text("http-rs", "", Language::Rust);
		assert!(
			!e.keyword_extras.iter().any(|p| p == "rs"),
			"'rs' is name_part_skip for Rust, got: {:?}",
			e.keyword_extras
		);
		assert!(e.keyword_extras.iter().any(|p| p == "http"));
	}

	#[test]
	fn empty_name_yields_empty_extras() {
		let e = enrich_package_text("", "already here", Language::Rust);
		assert!(e.keyword_extras.is_empty());
		assert!(e.extra_text.is_empty());

		let e2 = enrich_package_text("   ", "", Language::Python);
		assert!(e2.keyword_extras.is_empty());
	}

	#[test]
	fn existing_keywords_not_duplicated() {
		let e = enrich_package_text("serde-json", "serde json serialization", Language::Rust);
		assert!(
			!e.keyword_extras.iter().any(|p| p == "serde" || p == "json"),
			"parts already in keywords must not be re-emitted: {:?}",
			e.keyword_extras
		);
	}

	#[test]
	fn merge_keywords_appends_extras() {
		let e = enrich_package_text("serde-json", "serialization", Language::Rust);
		let merged = merge_keywords("serialization", &e);
		assert!(merged.starts_with("serialization"));
		assert!(merged.contains("serde"));
		assert!(merged.contains("json"));
	}

	#[test]
	fn merge_keywords_empty_enrichment_is_identity() {
		let e = Enrichment::default();
		assert_eq!(merge_keywords("a b", &e), "a b");
		assert_eq!(merge_keywords("", &e), "");
	}

	#[test]
	fn go_authority_like_tokens_skipped() {
		// Full module path should not inject host fragments.
		let e = enrich_package_text("github.com/gorilla/mux", "", Language::Go);
		assert!(
			!e.keyword_extras.iter().any(|p| matches!(p.as_str(), "github" | "com")),
			"authority-like tokens must not appear: {:?}",
			e.keyword_extras
		);
		assert!(
			e.keyword_extras.iter().any(|p| p == "gorilla"),
			"namespace segment should appear: {:?}",
			e.keyword_extras
		);
		assert!(
			e.keyword_extras.iter().any(|p| p == "mux"),
			"name segment should appear: {:?}",
			e.keyword_extras
		);
	}

	#[test]
	fn underscore_and_dot_split() {
		let e = enrich_package_text("foo_bar.baz", "", Language::Python);
		assert!(e.keyword_extras.iter().any(|p| p == "foo"));
		assert!(e.keyword_extras.iter().any(|p| p == "bar"));
		assert!(e.keyword_extras.iter().any(|p| p == "baz"));
	}

	#[test]
	fn extras_capped() {
		// Many short parts — must not exceed MAX_EXTRA_PARTS.
		let name = (0..20).map(|i| format!("p{i:02}")).collect::<Vec<_>>().join("-");
		let e = enrich_package_text(&name, "", Language::Rust);
		assert!(
			e.keyword_extras.len() <= MAX_EXTRA_PARTS,
			"cap exceeded: {} parts",
			e.keyword_extras.len()
		);
	}
}
