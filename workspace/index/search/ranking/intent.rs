//! Query-intent classification for package search ranking.
//!
//! Callers that want intent-aware ranking must go through
//! [`super::policy::RankingPolicy`] (or [`super::cascade::rank_with_intent`]) so
//! the Navigate / Explore distinction cannot be skipped accidentally.

use crate::ecosystem::search::{DEFAULT_SPECIFICITY_SEPARATORS, SearchNorms};
use heart::ecosystem::Language;

/// How the user is searching: looking up a known package vs browsing by topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryIntent {
	/// Exact-ish lookup: single token, package-id shape, or specificity separators.
	/// Ranking prefers exact / contains name matches over popularity.
	Navigate,
	/// Multi-word prose or bland-only queries. Ranking leans on quality and
	/// popularity so household-name packages beat keyword-spam names.
	Explore,
}

/// Classify free-text (post-structured-parse terms) into [`QueryIntent`].
///
/// Rules (first match wins):
/// 1. Empty → [`QueryIntent::Explore`].
/// 2. Looks like a package id (`@scope/name`, `group:artifact`, path with `/`)
///    → [`QueryIntent::Navigate`].
/// 3. Multi-word (whitespace) prose → [`QueryIntent::Explore`].
/// 4. Contains ecosystem specificity separators (from `norms` when known,
///    else [`DEFAULT_SPECIFICITY_SEPARATORS`]) → [`QueryIntent::Navigate`].
/// 5. Only bland / stopword tokens → [`QueryIntent::Explore`].
/// 6. Otherwise single non-bland token (exact-ish) → [`QueryIntent::Navigate`].
///
/// `ecosystem_scope` is used to resolve norms when `norms` is `None`. When the
/// ecosystem is known, `SearchNorms` separator / stopword tables are mandatory
/// inputs to the decision — do not hard-code ecosystem conventions here.
pub fn classify_intent(
	query: &str,
	ecosystem_scope: Option<Language>,
	norms: Option<&SearchNorms>,
) -> QueryIntent {
	let q = query.trim();
	if q.is_empty() {
		return QueryIntent::Explore;
	}

	if looks_like_package_id(q) {
		return QueryIntent::Navigate;
	}

	// Multi-word prose is explore even if one token is long or separator-like.
	if q.split_whitespace().count() > 1 {
		return QueryIntent::Explore;
	}

	let resolved = resolve_norms(ecosystem_scope, norms);
	let is_specific = match resolved {
		Some(n) => n.query_is_specific(q),
		None => q.contains(DEFAULT_SPECIFICITY_SEPARATORS) || q.len() > 15,
	};
	if is_specific {
		return QueryIntent::Navigate;
	}

	if is_only_bland(q, resolved) {
		return QueryIntent::Explore;
	}

	// Short / single-token exact-ish name lookup.
	QueryIntent::Navigate
}

/// Package-id shapes that almost always mean Navigate, independent of ecosystem.
///
/// - `@scope/name` (npm scoped)
/// - `group:artifact` (Maven coordinates)
/// - any path-like segment with `/` (Go modules, scoped paths)
fn looks_like_package_id(query: &str) -> bool {
	let q = query.trim();
	if q.is_empty() {
		return false;
	}
	// npm scoped package
	if q.starts_with('@') && q.contains('/') {
		return true;
	}
	// Maven-style group:artifact (reject bare URLs / times by requiring both sides)
	if let Some((left, right)) = q.split_once(':')
		&& !left.is_empty()
		&& !right.is_empty()
		&& !q.contains(char::is_whitespace)
	{
		return true;
	}
	// Path-like module / package path
	if q.contains('/') && !q.contains(char::is_whitespace) {
		return true;
	}
	false
}

fn resolve_norms(
	ecosystem_scope: Option<Language>,
	norms: Option<&SearchNorms>,
) -> Option<&SearchNorms> {
	if norms.is_some() {
		return norms;
	}
	ecosystem_scope.map(|lang| {
		use crate::ecosystem::LanguageExt;
		lang.spec().search_norms()
	})
}

/// True when every whitespace-separated token is a stopword / bland filler.
fn is_only_bland(query: &str, norms: Option<&SearchNorms>) -> bool {
	let tokens: Vec<&str> = query
		.split(|c: char| c.is_whitespace() || c == '-' || c == '_')
		.filter(|t| !t.is_empty())
		.collect();
	if tokens.is_empty() {
		return true;
	}
	tokens.iter().all(|t| {
		let lower = t.to_ascii_lowercase();
		match norms {
			Some(n) => n.is_stopword(&lower),
			None => crate::ecosystem::search::ENGLISH_STOPWORDS
				.binary_search(&lower.as_str())
				.is_ok(),
		}
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ecosystem::LanguageExt;

	#[test]
	fn types_node_is_navigate() {
		assert_eq!(
			classify_intent("@types/node", Some(Language::Typescript), None),
			QueryIntent::Navigate
		);
	}

	#[test]
	fn http_client_is_explore() {
		assert_eq!(
			classify_intent("http client", None, None),
			QueryIntent::Explore
		);
	}

	#[test]
	fn lang_separators_mark_navigate() {
		// Java separators (`.` / `:`) via SearchNorms.
		let java = Language::Java.spec().search_norms();
		assert_eq!(
			classify_intent("org.springframework", Some(Language::Java), Some(java)),
			QueryIntent::Navigate
		);
		assert_eq!(
			classify_intent("org:springframework", Some(Language::Java), Some(java)),
			QueryIntent::Navigate
		);
		// Rust hyphenated package id shape via separators.
		let rust = Language::Rust.spec().search_norms();
		assert_eq!(
			classify_intent("serde-json", Some(Language::Rust), Some(rust)),
			QueryIntent::Navigate
		);
	}

	#[test]
	fn short_single_token_is_navigate() {
		assert_eq!(
			classify_intent("serde", Some(Language::Rust), None),
			QueryIntent::Navigate
		);
	}

	#[test]
	fn bland_only_is_explore() {
		// English stopword alone → explore.
		assert_eq!(
			classify_intent("library", None, None),
			QueryIntent::Explore
		);
	}

	#[test]
	fn maven_and_path_ids_are_navigate() {
		assert_eq!(
			classify_intent("com.google.guava:guava", Some(Language::Java), None),
			QueryIntent::Navigate
		);
		assert_eq!(
			classify_intent("github.com/gorilla/mux", Some(Language::Go), None),
			QueryIntent::Navigate
		);
	}

	#[test]
	fn norms_from_ecosystem_scope_when_none_passed() {
		// Scope alone must still pick up Java separators.
		assert_eq!(
			classify_intent("org.springframework", Some(Language::Java), None),
			QueryIntent::Navigate
		);
	}
}
