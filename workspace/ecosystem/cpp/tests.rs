//! Spec-level tests for the `cpp` `EcosystemSpec` impl.

use super::*;
use crate::{
	LanguageExt,
	upstream::ListingStatus,
	version::{AnyVersion, VersionGrammar},
};

// ── endpoints / policy ───────────────────────────────────────────────────────

#[test]
fn listing_endpoint_is_ls_remote_marker() {
	let endpoints = Cpp::endpoints();
	assert_eq!(endpoints.listing, "git+ls-remote://{name}");
	assert_eq!(endpoints.archive, "");
	assert!(endpoints.listing_status.is_none());
}

#[test]
fn policy_is_one_rps() {
	assert_eq!(Cpp::POLICY.max_requests_per_second, 1.0);
	assert_eq!(Cpp::POLICY.retry_budget, 3);
	assert!(Cpp::POLICY.respect_retry_after);
}

#[test]
fn no_download_source() {
	assert!(Cpp::download_source().is_none());
}

// ── version listing via the spec ─────────────────────────────────────────────

#[test]
fn parse_version_listing_emits_tag_versions() {
	let body = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/tags/v1.2.3\n";
	let versions = Cpp::parse_version_listing(body);
	assert_eq!(versions.len(), 1);
	assert_eq!(versions[0].status, ListingStatus::Listed);
	assert_eq!(versions[0].raw, "v1.2.3@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
}

// ── search norms ─────────────────────────────────────────────────────────────

#[test]
fn cpp_stopwords() {
	let norms = Cpp::search_norms();
	assert!(norms.is_stopword("lib"));
	assert!(norms.is_stopword("library"));
	assert!(norms.is_stopword("cpp"));
	assert!(norms.is_stopword("cxx"));
	assert!(norms.is_stopword("c"));
	assert!(!norms.is_stopword("zlib"));
}

#[test]
fn strip_leading_lib() {
	assert_eq!(strip_cpp_conventions("libpng"), "png");
	assert_eq!(strip_cpp_conventions("libcurl"), "curl");
	// A bare "lib" is not stripped to empty.
	assert_eq!(strip_cpp_conventions("lib"), "lib");
	assert_eq!(strip_cpp_conventions("zlib"), "zlib"); // not a prefix match
}

#[test]
fn slug_query_folds_host() {
	let normalized = normalize_cpp_query("github.com/madler/zlib");
	assert_eq!(normalized.terms, "zlib");
	assert_eq!(normalized.namespace.as_deref(), Some("madler"));
	// A bare token is untouched.
	let bare = normalize_cpp_query("zlib");
	assert_eq!(bare.terms, "zlib");
	assert!(bare.namespace.is_none());
}

// ── dispatch table wiring ────────────────────────────────────────────────────

#[test]
fn dispatches_through_dynspec() {
	let spec = Language::Cpp.spec();
	assert_eq!(spec.language(), Language::Cpp);
	// Round-trip a name through the erased path.
	let structured = spec.parse_name("https://github.com/curl/curl").unwrap();
	assert_eq!(spec.render_canonical(&structured), "github.com/curl/curl");
	// Version comparison through the erased path.
	let ord = spec.compare_versions("1.0.0", "1.0.1");
	assert_eq!(ord, Some(core::cmp::Ordering::Less));
}

#[test]
fn erased_version_is_cpp_variant() {
	let erased = CppVersion::parse("1.2.3").unwrap().erase();
	assert!(matches!(erased, AnyVersion::Cpp(_)));
}
