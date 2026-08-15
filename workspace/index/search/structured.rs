//! Structured query — parsed form of a registry search request.
//!
//! `StructuredQuery::parse` splits a raw input string into an optional
//! ecosystem scope, an optional namespace constraint, free terms, dep/license
//! filters, quoted phrase spans, and synonym-expanded terms.
//!
//! # Q1 note
//! Raw terms are passed through verbatim (post length/control-char validation
//! in `LiteralQuery`); NO tantivy grammar escaping is performed here. The
//! search path builds a hand-written `BooleanQuery` tree that never feeds text
//! to tantivy's `QueryParser`, so grammar escaping has no consumer and would
//! only corrupt qualified/hyphenated queries (Q1 defect).

use crate::ecosystem::{Language, LanguageExt};

/// The parsed, structured form of a registry search request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredQuery {
	/// Restrict to one ecosystem, or search across all when `None`.
	/// Set by an API-level scope parameter or by `lang:`/`ecosystem:` tokens.
	/// API-level scope WINS over inline tokens.
	pub ecosystem: Option<Language>,

	/// Free terms, verbatim (whitespace-joined remaining tokens).
	/// May be empty when the query is namespace-only.
	pub terms: String,

	/// Namespace constraint extracted from `scope:`/`group:`/`ns:` tokens,
	/// or derived by `SearchNorms::normalize_query` when the ecosystem is known.
	/// An explicit token WINS over a derived namespace.
	/// Leading `@` is stripped from scope tokens (npm `@types` → `types`).
	pub namespace: Option<String>,

	/// `dep:NAME` filter tokens (lowercased dependency names) — "packages that
	/// depend on NAME".
	pub deps: Vec<String>,

	/// `license:VALUE` filter (lowercased), e.g. `license:mit`.
	pub license: Option<String>,

	/// Quoted phrase spans (`"http client"`), quotes stripped, order preserved.
	/// Phrase text also joins `terms` (so the ranking name-bonus still sees the
	/// words) — but the phrase itself additionally rides in `phrases`.
	pub phrases: Vec<String>,

	/// Query-time synonym expansions of the free terms (populated by
	/// [`StructuredQuery::expand_synonyms`]; empty until then).
	pub expanded_terms: Vec<String>,
	// `kinds` and `package` are symbols-path only — not carried here (§8.1).
}

impl StructuredQuery {
	/// Parse a raw query string into a structured form.
	///
	/// - `api_scope`: ecosystem provided at the API level (URL path param or
	///   request field). Wins over any inline `lang:`/`ecosystem:` token.
	/// - Tokens matching `^(lang|ecosystem):\S+$` (case-insensitive) are parsed
	///   via [`Language::from_token`]. Unknown values are re-appended as free text.
	/// - `scope:X`/`group:X`/`ns:X` set the namespace (strip leading `@`).
	/// - `dep:X` (case-insensitive key, value lowercased, non-empty) appends to `deps`.
	/// - `license:X` (value lowercased, non-empty) sets `license` (last wins).
	/// - Double-quoted spans are extracted as phrases; their content also joins `terms`.
	///   A lone unmatched opening quote is treated as a literal character.
	/// - Everything else joins into `terms` (single-space separated).
	/// - When ecosystem is known, `spec.search_norms().normalize_query(&terms)`
	///   is applied; the result may merge a derived namespace. An explicit
	///   `scope:`/`group:`/`ns:` token WINS over the derived namespace.
	pub fn parse(raw: &str, api_scope: Option<Language>) -> Self {
		let mut inline_ecosystem: Option<Language> = None;
		let mut explicit_namespace: Option<String> = None;
		let mut deps: Vec<String> = Vec::new();
		let mut license: Option<String> = None;
		let mut phrases: Vec<String> = Vec::new();
		let mut free: Vec<String> = Vec::new();

		// A `key:` prefix is matched case-insensitively; the VALUE keeps its
		// original bytes (namespaces are case-significant downstream; language
		// tokens are folded by `Language::from_token` itself).
		fn key_value<'t>(token: &'t str, keys: &[&str]) -> Option<&'t str> {
			let lower = token.to_ascii_lowercase();
			keys.iter()
				.find_map(|key| lower.strip_prefix(key).map(|rest| &token[token.len() - rest.len()..]))
		}

		// ---------------------------------------------------------------------------
		// Phase 1: extract double-quoted phrases from the raw string BEFORE splitting
		// on whitespace. A matching close-quote ends the phrase. An unmatched opening
		// quote is treated as a literal character (the remaining text becomes free).
		// ---------------------------------------------------------------------------
		let mut remaining = raw;
		let mut raw_parts: Vec<&str> = Vec::new(); // unquoted fragments (split later)

		while let Some(open) = remaining.find('"') {
			// Everything before the quote is unquoted text.
			let before = &remaining[..open];
			if !before.is_empty() {
				raw_parts.push(before);
			}
			let after_open = &remaining[open + 1..];
			if let Some(close) = after_open.find('"') {
				// Well-formed quoted span.
				let phrase_text = &after_open[..close];
				if !phrase_text.trim().is_empty() {
					phrases.push(phrase_text.trim().to_owned());
					// Phrase words also participate in free text so the name-bonus
					// tiers see them.
					raw_parts.push(phrase_text);
				}
				remaining = &after_open[close + 1..];
			} else {
				// Unmatched opening quote — treat the quote and everything after as
				// literal free text.
				raw_parts.push(after_open);
				remaining = "";
				break;
			}
		}
		// Append any tail after the last matched quote.
		if !remaining.is_empty() {
			raw_parts.push(remaining);
		}

		// ---------------------------------------------------------------------------
		// Phase 2: whitespace-tokenize all unquoted fragments and classify tokens.
		// ---------------------------------------------------------------------------
		for part in &raw_parts {
			for token in part.split_whitespace() {
				// lang:/ecosystem:
				if let Some(value) = key_value(token, &["lang:", "ecosystem:"]) {
					match Language::from_token(value) {
						Some(lang) => inline_ecosystem = Some(lang),
						None => free.push(token.to_owned()),
					}
					continue;
				}

				// scope:/group:/ns:
				if let Some(value) = key_value(token, &["scope:", "group:", "ns:"]) {
					let stripped = value.strip_prefix('@').unwrap_or(value);
					if !stripped.is_empty() {
						explicit_namespace = Some(stripped.to_owned());
					}
					continue;
				}

				// dep:NAME
				if let Some(value) = key_value(token, &["dep:"]) {
					if !value.is_empty() {
						deps.push(value.to_ascii_lowercase());
					}
					continue;
				}

				// license:VALUE
				if let Some(value) = key_value(token, &["license:"]) {
					if !value.is_empty() {
						license = Some(value.to_ascii_lowercase());
					}
					continue;
				}

				free.push(token.to_owned());
			}
		}

		// API scope wins over inline token.
		let ecosystem = api_scope.or(inline_ecosystem);

		let mut terms = free.join(" ");

		// When the ecosystem is known, apply per-ecosystem query normalization.
		// An explicit `scope:`/`group:`/`ns:` token wins over any derived namespace.
		let derived_namespace: Option<String> = ecosystem.and_then(|lang| {
			let norms = lang.spec().search_norms();
			let normalized = (norms.normalize_query)(&terms);
			terms = normalized.terms;
			normalized.namespace
		});

		// Explicit ns token wins.
		let namespace = explicit_namespace.or(derived_namespace);

		Self { ecosystem, terms, namespace, deps, license, phrases, expanded_terms: Vec::new() }
	}

	/// Expand each free-term token through the runtime-loaded synonyms table
	/// (min_votes 3), collecting canonical forms that differ from the input.
	/// Deduplicated; never contains a token already present in `terms`.
	pub fn expand_synonyms(&mut self, synonyms: &crate::metadata::Synonyms) {
		let existing_terms: std::collections::HashSet<&str> =
			self.terms.split_whitespace().collect();

		let mut expanded: Vec<String> = Vec::new();
		let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

		for token in self.terms.split_whitespace() {
			let (canonical, _weight) = synonyms.normalize(token, 3);
			if canonical != token && !existing_terms.contains(canonical) {
				let owned = canonical.to_owned();
				if seen.insert(owned.clone()) {
					expanded.push(owned);
				}
			}
		}

		self.expanded_terms = expanded;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parse(raw: &str) -> StructuredQuery {
		StructuredQuery::parse(raw, None)
	}

	fn parse_scoped(raw: &str, scope: Language) -> StructuredQuery {
		StructuredQuery::parse(raw, Some(scope))
	}

	// =========================================================================
	// Existing tests (preserved verbatim)
	// =========================================================================

	#[test]
	fn plain_terms_pass_through() {
		let q = parse("serde tokio");
		assert_eq!(q.terms, "serde tokio");
		assert_eq!(q.ecosystem, None);
		assert_eq!(q.namespace, None);
	}

	#[test]
	fn lang_token_parsed_case_insensitive() {
		assert_eq!(parse("lang:go mux").ecosystem, Some(Language::Go));
		assert_eq!(parse("LANG:TS react").ecosystem, Some(Language::Typescript));
		assert_eq!(parse("Lang:Rust serde").ecosystem, Some(Language::Rust));
	}

	#[test]
	fn ecosystem_token_accepted() {
		let q = parse("ecosystem:python requests");
		assert_eq!(q.ecosystem, Some(Language::Python));
		assert_eq!(q.terms, "requests");
	}

	#[test]
	fn lang_aliases_accepted() {
		// ts → Typescript
		assert_eq!(parse("lang:ts react").ecosystem, Some(Language::Typescript));
		// js → Typescript
		assert_eq!(parse("lang:js react").ecosystem, Some(Language::Typescript));
		// c# → CSharp
		assert_eq!(parse("lang:c# IEnumerable").ecosystem, Some(Language::CSharp));
		// golang → Go
		assert_eq!(parse("lang:golang mux").ecosystem, Some(Language::Go));
	}

	#[test]
	fn unknown_lang_value_stays_as_free_text() {
		let q = parse("lang:brainfuck hello");
		assert_eq!(q.ecosystem, None);
		// The whole token is kept as free text.
		assert!(q.terms.contains("lang:brainfuck"), "unknown lang token must stay: {}", q.terms);
	}

	#[test]
	fn scope_token_sets_namespace() {
		let q = parse("scope:types node");
		assert_eq!(q.namespace, Some("types".to_owned()));
		assert_eq!(q.terms, "node");
	}

	#[test]
	fn scope_token_strips_leading_at() {
		// npm `@types` prefix — user might write `scope:@types`
		let q = parse("scope:@types node");
		assert_eq!(q.namespace, Some("types".to_owned()));
	}

	#[test]
	fn group_token_sets_namespace() {
		let q = parse("group:org.springframework spring-core");
		assert_eq!(q.namespace, Some("org.springframework".to_owned()));
		assert_eq!(q.terms, "spring-core");
	}

	#[test]
	fn ns_token_sets_namespace() {
		let q = parse("ns:myorg mylib");
		assert_eq!(q.namespace, Some("myorg".to_owned()));
	}

	#[test]
	fn api_scope_wins_over_inline_lang_token() {
		// User writes `lang:python` but route is Go-scoped.
		let q = parse_scoped("lang:python requests", Language::Go);
		assert_eq!(q.ecosystem, Some(Language::Go));
		// The lang:python token is consumed (no stray token in terms), but
		// api_scope wins so ecosystem = Go.
		// terms should be just "requests" (lang token consumed).
		assert_eq!(q.terms, "requests");
	}

	#[test]
	fn explicit_ns_token_wins_over_derived_namespace() {
		// npm's normalize_query derives namespace "types" from `@types/node`;
		// the explicit `scope:` token must win over the derived one.
		let q = StructuredQuery::parse("scope:override @types/node", Some(Language::Typescript));
		assert_eq!(q.namespace, Some("override".to_owned()));
	}

	#[test]
	fn derived_namespace_from_npm_scope() {
		// With no explicit token, npm's normalize_query folds `@types/node`
		// into namespace=types, terms=node.
		let q = StructuredQuery::parse("@types/node", Some(Language::Typescript));
		assert_eq!(q.namespace, Some("types".to_owned()));
		assert_eq!(q.terms, "node");
	}

	#[test]
	fn last_explicit_ns_token_wins() {
		let q = StructuredQuery::parse("scope:myorg ns:override mylib", None);
		assert_eq!(q.namespace, Some("override".to_owned()));
		assert_eq!(q.terms, "mylib");
	}

	#[test]
	fn empty_terms_after_extraction_is_allowed() {
		// A namespace-only query is valid (§8.1).
		let q = parse("scope:types");
		assert_eq!(q.terms, "");
		assert_eq!(q.namespace, Some("types".to_owned()));
	}

	#[test]
	fn types_node_via_explicit_scope_token() {
		let q = parse("scope:types node");
		assert_eq!(q.namespace, Some("types".to_owned()));
		assert_eq!(q.terms, "node");
	}

	#[test]
	fn tantivy_grammar_chars_pass_through_as_free_text() {
		// Q1: these must NOT be escaped — the query builder handles them directly.
		let q = parse("axum::Router");
		assert_eq!(q.terms, "axum::Router");

		let q = parse("Option<T>");
		assert_eq!(q.terms, "Option<T>");

		let q = parse("react-query");
		assert_eq!(q.terms, "react-query");
	}

	#[test]
	fn multiple_tokens_join_correctly() {
		let q = parse("lang:go gorilla mux");
		assert_eq!(q.ecosystem, Some(Language::Go));
		assert_eq!(q.terms, "gorilla mux");
	}

	#[test]
	fn unicode_terms_pass_through() {
		let q = parse("résumé parser");
		assert_eq!(q.terms, "résumé parser");
	}

	// =========================================================================
	// New tests — phrase extraction
	// =========================================================================

	#[test]
	fn single_phrase_extracted() {
		let q = parse(r#""http client""#);
		assert_eq!(q.phrases, vec!["http client".to_owned()]);
		// Phrase words also appear in terms so ranking tiers see them.
		assert!(q.terms.contains("http"), "phrase words must join terms");
		assert!(q.terms.contains("client"), "phrase words must join terms");
	}

	#[test]
	fn multiple_phrases_extracted() {
		let q = parse(r#""http client" rust "async runtime""#);
		assert_eq!(q.phrases.len(), 2);
		assert!(q.phrases.contains(&"http client".to_owned()));
		assert!(q.phrases.contains(&"async runtime".to_owned()));
		// Free word also in terms.
		assert!(q.terms.contains("rust"));
	}

	#[test]
	fn unmatched_quote_treated_as_literal_text() {
		// A lone opening quote: everything after is free text, no phrase produced.
		let q = parse(r#"http "client"#);
		assert!(q.phrases.is_empty(), "unmatched quote must not produce a phrase");
		// Both words should be in terms.
		assert!(q.terms.contains("http"));
		assert!(q.terms.contains("client"));
	}

	#[test]
	fn empty_quoted_span_produces_no_phrase() {
		let q = parse(r#""""#);
		assert!(q.phrases.is_empty());
	}

	#[test]
	fn phrase_with_free_terms_and_lang_token() {
		let q = parse(r#"lang:rust "async io" runtime"#);
		assert_eq!(q.ecosystem, Some(Language::Rust));
		assert_eq!(q.phrases, vec!["async io".to_owned()]);
		assert!(q.terms.contains("runtime"));
		assert!(q.terms.contains("async"));
		assert!(q.terms.contains("io"));
	}

	// =========================================================================
	// New tests — dep: parsing
	// =========================================================================

	#[test]
	fn dep_token_parsed() {
		let q = parse("dep:serde");
		assert_eq!(q.deps, vec!["serde".to_owned()]);
		// dep tokens must NOT appear in free terms.
		assert!(!q.terms.contains("dep:serde"), "dep: token must not leak into terms");
		assert!(!q.terms.contains("serde"), "dep value must not appear in terms");
	}

	#[test]
	fn dep_token_value_lowercased() {
		let q = parse("dep:SERDE");
		assert_eq!(q.deps, vec!["serde".to_owned()]);
	}

	#[test]
	fn dep_token_case_insensitive_key() {
		let q = parse("DEP:tokio");
		assert_eq!(q.deps, vec!["tokio".to_owned()]);
	}

	#[test]
	fn multiple_dep_tokens_collected() {
		let q = parse("dep:serde dep:tokio http");
		assert!(q.deps.contains(&"serde".to_owned()));
		assert!(q.deps.contains(&"tokio".to_owned()));
		assert_eq!(q.terms, "http");
	}

	#[test]
	fn empty_dep_value_ignored() {
		let q = parse("dep: serde");
		assert!(q.deps.is_empty(), "empty dep: value must be ignored");
		assert!(q.terms.contains("serde"));
	}

	#[test]
	fn dep_token_removed_from_terms() {
		let q = parse("dep:serde tokio");
		assert_eq!(q.terms, "tokio");
		assert_eq!(q.deps, vec!["serde".to_owned()]);
	}

	// =========================================================================
	// New tests — license: parsing
	// =========================================================================

	#[test]
	fn license_token_parsed() {
		let q = parse("license:mit");
		assert_eq!(q.license, Some("mit".to_owned()));
		assert!(!q.terms.contains("license:mit"), "license: token must not appear in terms");
	}

	#[test]
	fn license_value_lowercased() {
		let q = parse("license:MIT");
		assert_eq!(q.license, Some("mit".to_owned()));
	}

	#[test]
	fn license_key_case_insensitive() {
		let q = parse("LICENSE:apache-2.0 http");
		assert_eq!(q.license, Some("apache-2.0".to_owned()));
		assert_eq!(q.terms, "http");
	}

	#[test]
	fn last_license_token_wins() {
		let q = parse("license:mit license:apache-2.0");
		assert_eq!(q.license, Some("apache-2.0".to_owned()));
	}

	#[test]
	fn empty_license_value_ignored() {
		let q = parse("license: mit");
		assert!(q.license.is_none(), "empty license: value must be ignored");
		assert!(q.terms.contains("mit"));
	}

	// =========================================================================
	// New tests — dep:/license: interactions with other tokens
	// =========================================================================

	#[test]
	fn dep_and_scope_combined() {
		let q = parse("dep:tokio scope:myorg mylib");
		assert_eq!(q.deps, vec!["tokio".to_owned()]);
		assert_eq!(q.namespace, Some("myorg".to_owned()));
		assert_eq!(q.terms, "mylib");
	}

	#[test]
	fn license_and_lang_combined() {
		let q = parse("lang:rust license:mit serde");
		assert_eq!(q.ecosystem, Some(Language::Rust));
		assert_eq!(q.license, Some("mit".to_owned()));
		assert_eq!(q.terms, "serde");
	}

	#[test]
	fn dep_only_query_leaves_terms_empty() {
		let q = parse("dep:tokio");
		assert_eq!(q.deps, vec!["tokio".to_owned()]);
		assert_eq!(q.terms, "");
	}

	#[test]
	fn license_only_query_leaves_terms_empty() {
		let q = parse("license:mit");
		assert_eq!(q.license, Some("mit".to_owned()));
		assert_eq!(q.terms, "");
	}

	// =========================================================================
	// New tests — expand_synonyms
	// =========================================================================

	/// Build a `Synonyms` from an in-memory CSV string for testing.
	/// The CSV has no headers: find,replace,score
	fn make_synonyms(csv: &str) -> crate::metadata::Synonyms {
		use std::io::Write as _;
		// Write to a temp dir so `Synonyms::new` can read the file eagerly.
		// `Synonyms` owns its data in a HashMap so the dir can drop immediately.
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("tag-synonyms.csv");
		{
			let mut f = std::fs::File::create(&path).unwrap();
			f.write_all(csv.as_bytes()).unwrap();
		}
		crate::metadata::Synonyms::new(dir.path()).expect("Synonyms::new")
	}

	#[test]
	fn expand_synonyms_maps_known_terms() {
		// "async-io" → "tokio" with score 4 (≥ 3 min_votes).
		let synonyms = make_synonyms("async-io,tokio,4\n");
		let mut q = parse("async-io");
		q.expand_synonyms(&synonyms);
		assert!(
			q.expanded_terms.contains(&"tokio".to_owned()),
			"expected tokio in expanded_terms, got {:?}",
			q.expanded_terms,
		);
	}

	#[test]
	fn expand_synonyms_skips_low_vote_entries() {
		// score 2 < min_votes 3 → should NOT expand.
		let synonyms = make_synonyms("async-io,tokio,2\n");
		let mut q = parse("async-io");
		q.expand_synonyms(&synonyms);
		assert!(
			q.expanded_terms.is_empty(),
			"low-vote synonym must not be expanded, got {:?}",
			q.expanded_terms,
		);
	}

	#[test]
	fn expand_synonyms_deduplicates() {
		// Two tokens that both expand to the same canonical.
		let synonyms = make_synonyms("async-io,tokio,4\nasync,tokio,4\n");
		let mut q = parse("async-io async");
		q.expand_synonyms(&synonyms);
		let count = q.expanded_terms.iter().filter(|t| t.as_str() == "tokio").count();
		assert_eq!(count, 1, "canonical form must appear at most once in expanded_terms");
	}

	#[test]
	fn expand_synonyms_excludes_existing_terms() {
		// If the canonical is already in terms, it must not appear in expanded_terms.
		let synonyms = make_synonyms("http-client,http,4\n");
		let mut q = parse("http-client http");
		q.expand_synonyms(&synonyms);
		assert!(
			!q.expanded_terms.contains(&"http".to_owned()),
			"canonical already in terms must not appear in expanded_terms",
		);
	}

	#[test]
	fn expand_synonyms_unknown_term_produces_no_expansion() {
		let synonyms = make_synonyms("other,thing,4\n");
		let mut q = parse("serde");
		q.expand_synonyms(&synonyms);
		assert!(q.expanded_terms.is_empty());
	}

	#[test]
	fn expand_synonyms_interaction_with_lang_and_scope() {
		let synonyms = make_synonyms("http-client,reqwest,4\n");
		let mut q = parse("lang:rust scope:myorg http-client");
		q.expand_synonyms(&synonyms);
		assert_eq!(q.ecosystem, Some(Language::Rust));
		assert_eq!(q.namespace, Some("myorg".to_owned()));
		assert!(q.expanded_terms.contains(&"reqwest".to_owned()));
	}
}
