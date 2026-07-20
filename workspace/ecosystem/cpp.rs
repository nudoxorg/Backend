//! The C/C++ registry-less ecosystem (`"cpp"`), git-native (REGISTRYLESS-PLAN).
//!
//! Identity is a normalized repository slug ([`repo::normalize_repo_url`]);
//! versions come from tags with a Go-style pseudo-version fallback
//! ([`version::CppVersion`]); the "listing" is `git ls-remote` output the IO
//! layer intercepts (RL-14); manifests are the seven build-descriptor families
//! scanned as pure text ([`manifest::CppManifest`]). There is no download-count
//! signal (RL-12), so `download_source` stays `None`.

pub mod alias;
pub mod interop;
pub mod listing;
pub mod manifest;
pub mod name;
pub mod version;

use crate::{
	EcosystemSpec, Language,
	archive::ArchiveKind,
	manifest::{ManifestCandidate, ManifestFacts},
	name::StructuredName,
	policy::UpstreamPolicy,
	search::{DEFAULT_SPECIFICITY_SEPARATORS, NormalizedQuery, SearchNorms, map_no_category},
	upstream::{self, ListedVersion, UpstreamEndpoints},
};

pub use manifest::{CppManifest, DependencyMechanism, DependencyRecord};
pub use version::{CppVersion, TagVersion, synthesize_pseudo_version};

/// The `cpp` ecosystem marker ZST.
pub struct Cpp;

impl crate::sealed::Sealed for Cpp {}

impl EcosystemSpec for Cpp {
	const ARCHIVE: ArchiveKind = ArchiveKind::TarGz;
	const LANGUAGE: Language = Language::Cpp;
	/// Git hosts are shared infrastructure — 1 rps, retry 3, honor Retry-After
	/// (REGISTRYLESS §6.3).
	const POLICY: UpstreamPolicy = UpstreamPolicy {
		max_requests_per_second: 1.0,
		retry_budget: 3,
		respect_retry_after: true,
	};

	type Manifest = CppManifest;
	type Version = CppVersion;

	fn parse_name(raw: &str) -> Option<StructuredName> { name::parse_name(raw) }

	fn render_canonical(structured_name: &StructuredName) -> String {
		name::render_canonical(structured_name)
	}

	/// The listing template is a marker the IO layer intercepts (RL-14): rather
	/// than an HTTP endpoint, `git+ls-remote://{name}` tells the IO seam to run
	/// `git ls-remote --tags --heads <url>` and feed the bytes to
	/// [`Self::parse_version_listing`]. The archive template is empty: `cpp`
	/// acquires source by checking out a rev, not by downloading a tarball.
	fn endpoints() -> UpstreamEndpoints {
		UpstreamEndpoints {
			listing: "git+ls-remote://{name}",
			archive: "",
			listing_status: None,
		}
	}

	/// Parse raw `git ls-remote --tags --heads` bytes (RL-14). Prefers the
	/// peeled (`^{}`) commit oid for annotated tags and records `"<tag>@<oid>"`
	/// in `raw` so the ingestor can pin `source_rev` without a second call.
	fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
		listing::parse_ls_remote(body)
	}

	fn manifest_candidates() -> &'static [ManifestCandidate] { manifest::manifest_candidates() }

	fn parse_manifest(candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
		manifest::parse_manifest(candidate, bytes)
	}

	fn search_norms() -> &'static SearchNorms { &NORMS }

	/// No download-count signal exists for `cpp` (RL-12): the `presence` facet
	/// (count of distinct feeds aliasing a stem) substitutes downstream.
	fn download_source() -> Option<upstream::DownloadEndpoint> { None }
}

/// Assert the folded facts round-trip through the manifest trait (compile-time
/// use of [`ManifestFacts`] so the import is load-bearing).
const _: fn(CppManifest) -> crate::manifest::ExtractedFacts = ManifestFacts::into_facts;

static NORMS: SearchNorms = SearchNorms {
	// Ubiquitous C/C++ naming words carry no search signal (REGISTRYLESS §6.3).
	stopwords: &["c", "cpp", "cxx", "lib", "library"],
	specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
	strip_conventions: strip_cpp_conventions,
	normalize_query: normalize_cpp_query,
	map_category: map_no_category,
	// No download source (RL-12) → downloads-driven ranking stages never fire.
	downloads_scale: None,
};

/// Strip the leading `lib` naming convention before the contains-bonus check
/// (REGISTRYLESS §6.3): `libpng` and `png` should share a search surface. The
/// prefix is only stripped when something non-empty remains.
fn strip_cpp_conventions(name: &str) -> &str {
	name.strip_prefix("lib").filter(|rest| !rest.is_empty()).unwrap_or(name)
}

/// A slug-shaped `cpp` query (`github.com/madler/zlib`) folds its host away and
/// treats the leading path segments as a namespace, mirroring Go's handling.
fn normalize_cpp_query(terms: &str) -> NormalizedQuery {
	let trimmed = terms.trim();
	// Only treat it as a slug when the first segment looks like a host.
	let looks_like_slug = trimmed
		.split_once('/')
		.is_some_and(|(host, _)| host.contains('.'));
	if !looks_like_slug {
		return NormalizedQuery { terms: trimmed.to_owned(), namespace: None };
	}
	let mut segments = trimmed.split('/');
	let _host = segments.next();
	let rest: Vec<&str> = segments.filter(|s| !s.is_empty()).collect();
	match rest.as_slice() {
		[] => NormalizedQuery { terms: trimmed.to_owned(), namespace: None },
		[name] => NormalizedQuery { terms: (*name).to_owned(), namespace: None },
		[dirs @ .., name] => NormalizedQuery {
			terms: (*name).to_owned(),
			namespace: Some(dirs.join(" ")),
		},
	}
}

#[cfg(test)]
mod tests;
