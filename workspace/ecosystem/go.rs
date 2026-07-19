//! The Go module proxy (proxy.golang.org).

use smol_str::SmolStr;

use crate::{
	EcosystemSpec, Language, archive::ArchiveKind, manifest::{self, ExtractedFacts, ManifestCandidate},
	name, policy::UpstreamPolicy, search::{DEFAULT_SPECIFICITY_SEPARATORS, NormalizedQuery, SearchNorms, map_no_category},
	upstream::{self, ListedVersion, ListingStatus}, version,
};

pub struct Go;

impl crate::sealed::Sealed for Go {}

impl EcosystemSpec for Go {
	const ARCHIVE: ArchiveKind = ArchiveKind::Zip;
	const LANGUAGE: Language = Language::Go;
	const POLICY: UpstreamPolicy = UpstreamPolicy {
		max_requests_per_second: 20.0,
		retry_budget: 3,
		respect_retry_after: true,
	};

	type Manifest = manifest::ExtractedFacts;
	type Version = version::GoVersion;

	fn parse_name(raw: &str) -> Option<name::StructuredName> {
		// Go module paths: no leading `/`, no `..`, valid chars only.
		// Case is preserved (Go module paths are case-sensitive).
		// Trailing `/vN` (N ≥ 2) is stripped into `major`.
		// Authority = first segment containing `.` (e.g. `github.com`).
		// Namespace = middle path segments; name = final path segment.
		if raw.is_empty()
			|| raw.starts_with('/')
			|| raw.contains("..")
			|| !raw.chars().all(|c| {
				c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~' | '+' | ':')
			})
		{
			return None;
		}

		// Strip trailing major version suffix `/vN` where N ≥ 2.
		let (path_without_major, major) = strip_major_suffix(raw);
		let segments: Vec<&str> = path_without_major.split('/').collect();

		let (authority, namespace, name) = decompose_go_segments(&segments);

		Some(name::StructuredName {
			ecosystem: Language::Go,
			authority: authority.map(SmolStr::from),
			namespace: namespace.iter().map(|s| SmolStr::from(*s)).collect(),
			name: name.into(),
			major,
			original: raw.into(),
		})
	}

	/// Canonical = the original raw string (Go module paths are identity-preserving;
	/// `canonicalize_go_module` returned the raw string verbatim, `/vN` included).
	///
	/// INVARIANT: `n.original` must be populated (every parse_name-produced
	/// value is). A hand-assembled Go `StructuredName` with an empty `original`
	/// would render an empty canonical — never construct one by hand.
	fn render_canonical(n: &name::StructuredName) -> String {
		debug_assert!(!n.original.is_empty(), "Go canonical form derives from `original`");
		n.original.to_string()
	}

	fn endpoints() -> upstream::UpstreamEndpoints {
		upstream::UpstreamEndpoints {
			// `{name}` is the goproxy-escaped module path (`!` capital
			// escaping — `escape_module_path`, P3).
			listing: "/{name}/@v/list",
			archive: "/{name}/@v/{version}.zip",
			listing_status: None,
		}
	}

	/// Parse the Go module proxy `/@v/list` endpoint.
	/// Returns plain text: a newline-separated list of version strings.
	/// All versions are Listed (the Go proxy has no unlist / retract signal at
	/// this endpoint; retractions are declared inside go.mod).
	fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
		let Ok(text) = std::str::from_utf8(body) else {
			return vec![];
		};
		text.split('\n')
			.filter(|line| !line.trim().is_empty())
			.filter_map(|raw| {
				let raw = raw.trim();
				let version = Self::Version::parse(raw)?;
				Some(ListedVersion {
					version,
					status: ListingStatus::Listed,
					raw: raw.into(),
				})
			})
			.collect()
	}

	fn manifest_candidates() -> &'static [ManifestCandidate] {
		static CANDIDATES: &[ManifestCandidate] = &[ManifestCandidate::new("go.mod")];
		CANDIDATES
	}

	fn parse_manifest(
		_candidate: &ManifestCandidate,
		bytes: &[u8],
	) -> Option<Self::Manifest> {
		let text = std::str::from_utf8(bytes).ok()?;
		Some(parse_go_mod(text))
	}

	fn search_norms() -> &'static SearchNorms { &NORMS }

	/// Go modules have **no public download-count API** (proxy.golang.org
	/// exposes no statistics endpoint; pkg.go.dev has no free per-module JSON
	/// download feed). Returns `None`; ranking uses dependents / fairness floor.
	///
	/// A pure [`parse_download_count`] is still provided for offline/fixture
	/// JSON shaped as `{"downloads": N}` so corpus jobs can inject counts later
	/// without touching the trait surface.
	fn download_source() -> Option<upstream::DownloadEndpoint> { None }

	/// Parse a documented offline JSON shape: `{"downloads": N}` (or
	/// `{"download_count": N}`). Malformed / missing → `None` (never panics).
	/// Not used at ingest while [`download_source`] is `None`.
	fn parse_download_count(body: &[u8]) -> Option<u64> {
		parse_go_download_count(body)
	}
}

/// Pure parser for a best-effort Go download-count JSON fixture.
///
/// Documented shape (no live upstream today):
/// ```json
/// { "downloads": 12345 }
/// ```
/// Also accepts `download_count` as an alternate key. Explicit `0` is `Some(0)`.
pub fn parse_go_download_count(body: &[u8]) -> Option<u64> {
	let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
	v["downloads"]
		.as_u64()
		.or_else(|| v["download_count"].as_u64())
}

static NORMS: SearchNorms = SearchNorms {
	stopwords: &["go", "golang"],
	specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
	strip_conventions: strip_go_authority,
	normalize_query: normalize_go_query,
	map_category: map_no_category,
	downloads_scale: None,
};

// ── Goproxy capital-escaping protocol ───────────────────────────────────────

/// Escape a Go module path using the goproxy capital-escaping protocol:
/// each uppercase ASCII letter `A`..=`Z` is replaced by `!` followed by its
/// lowercase equivalent. E.g. `"github.com/Azure/azure-sdk"` →
/// `"github.com/!azure/azure-sdk"`.
pub fn escape_module_path(path: &str) -> String {
	let mut out = String::with_capacity(path.len() + 4);
	for ch in path.chars() {
		if ch.is_ascii_uppercase() {
			out.push('!');
			out.push(ch.to_ascii_lowercase());
		} else {
			out.push(ch);
		}
	}
	out
}

/// Inverse of [`escape_module_path`]: converts `!x` back to `X`.
/// Silently passes through invalid `!` sequences (trailing `!`, `!` followed
/// by a non-ASCII-lowercase character) unchanged.
pub fn unescape_module_path(escaped: &str) -> String {
	let mut out = String::with_capacity(escaped.len());
	let mut chars = escaped.chars().peekable();
	while let Some(ch) = chars.next() {
		if ch == '!' {
			if let Some(&next) = chars.peek()
				&& next.is_ascii_lowercase() {
					chars.next();
					out.push(next.to_ascii_uppercase());
					continue;
				}
			// Unrecognised escape: pass through unchanged.
			out.push('!');
		} else {
			out.push(ch);
		}
	}
	out
}

/// Strip authority (through first `/` when first segment contains `.`).
pub fn strip_go_authority(name: &str) -> &str {
	let first_slash = name.find('/');
	if let Some(pos) = first_slash {
		let first_seg = &name[..pos];
		if first_seg.contains('.') {
			return &name[pos + 1..];
		}
	}
	name
}

/// Go: `host.tld/org/module` → namespace=org, terms=module.
fn normalize_go_query(terms: &str) -> NormalizedQuery {
	// If the first segment contains a dot, strip it as the authority.
	let stripped = strip_go_authority(terms);
	if stripped == terms {
		// No authority stripped — return as-is.
		return NormalizedQuery { terms: terms.to_owned(), namespace: None };
	}
	// Split remaining path into dirs + last segment.
	let parts: Vec<&str> = stripped.split('/').collect();
	match parts.as_slice() {
		[] => NormalizedQuery { terms: terms.to_owned(), namespace: None },
		[name] => NormalizedQuery { terms: (*name).to_owned(), namespace: None },
		[dirs @ .., name] => NormalizedQuery {
			terms: (*name).to_owned(),
			namespace: Some(dirs.join(" ")),
		},
	}
}

/// Derive a repository URL from a Go module path when the hosting forge is
/// identifiable. Recognises the four common forge authorities; takes the first
/// three path segments (`host/owner/repo`).
///
/// Returns `None` for standard library, private, or unrecognised paths.
fn repo_from_module_path(module_path: &str) -> Option<String> {
	const KNOWN_FORGES: &[&str] = &[
		"github.com/",
		"gitlab.com/",
		"bitbucket.org/",
		"codeberg.org/",
	];
	for forge in KNOWN_FORGES {
		if let Some(rest) = module_path.strip_prefix(forge) {
			// Extract owner + repo (first two segments of `rest`).
			let segs: Vec<&str> = rest.splitn(3, '/').collect();
			return match segs.as_slice() {
				[owner, repo, ..] if !owner.is_empty() && !repo.is_empty() => Some(format!(
					"https://{}/{}/{}",
					forge.trim_end_matches('/'),
					owner,
					repo
				)),
				// Only one segment (owner-only) or empty — not a resolvable repo path.
				_ => None,
			};
		}
	}
	None
}

/// Parse a `go.mod` file: extract module path + require dependencies.
pub fn parse_go_mod(text: &str) -> ExtractedFacts {
	let mut dependencies: Vec<String> = vec![];
	let mut module_path: Option<&str> = None;
	let mut in_require_block = false;

	for line in text.lines() {
		let trimmed = line.trim();

		// `module` directive — first occurrence wins.
		if module_path.is_none()
			&& let Some(rest) = trimmed.strip_prefix("module ")
		{
			let path = rest.split_whitespace().next().unwrap_or("").trim();
			if !path.is_empty() { module_path = Some(path); }
		}

		// Block `require (...)`.
		if trimmed == "require (" {
			in_require_block = true;
			continue;
		}
		if trimmed == ")" && in_require_block {
			in_require_block = false;
			continue;
		}

		if in_require_block {
			// Each line: `module/path version [// indirect]`
			if let Some(path) = extract_require_path(trimmed) {
				dependencies.push(path);
			}
			continue;
		}

		// Single-line `require module/path version`.
		if let Some(rest) = trimmed.strip_prefix("require ")
			&& let Some(path) = extract_require_path(rest.trim()) {
				dependencies.push(path);
			}
	}

	let repository = module_path.and_then(repo_from_module_path);

	// Go modules have no description field — README carries the weight.
	ExtractedFacts {
		description: None,
		keywords: vec![],
		categories: vec![],
		readme_hint: None,
		repository,
		documentation: false,
		license: None,
		has_license_file: false,
		dependencies,
	}
}

/// Extract the module path from a `require` line entry.
fn extract_require_path(line: &str) -> Option<String> {
	// Strip `// indirect` comment.
	let line = line.split("//").next().unwrap_or(line).trim();
	// `module/path version` — take the first whitespace-delimited token.
	let path = line.split_ascii_whitespace().next()?;
	if path.is_empty() || path.starts_with('(') || path.starts_with(')') {
		return None;
	}
	Some(path.to_owned())
}

/// Strip a trailing `/vN` major-version suffix where N ≥ 2.
/// Returns (path_without_suffix, Some(N)) or (path, None).
fn strip_major_suffix(raw: &str) -> (&str, Option<u32>) {
	if let Some(slash_pos) = raw.rfind('/') {
		let suffix = &raw[slash_pos + 1..];
		if let Some(n_str) = suffix.strip_prefix('v')
			&& let Ok(n) = n_str.parse::<u32>()
				&& n >= 2 {
					return (&raw[..slash_pos], Some(n));
				}
	}
	(raw, None)
}

/// Split a list of path segments into (authority, namespace, name).
/// Authority = first segment with a `.`; name = last; middle = namespace.
fn decompose_go_segments<'a>(segments: &'a [&'a str]) -> (Option<&'a str>, &'a [&'a str], &'a str) {
	if segments.is_empty() {
		return (None, &[], "");
	}
	let name = segments[segments.len() - 1];
	if segments.len() == 1 {
		return (None, &[], name);
	}
	// Authority = first segment if it contains a dot (domain name).
	if segments[0].contains('.') {
		let rest = &segments[1..];
		if rest.is_empty() {
			(Some(segments[0]), &[], name)
		} else {
			let namespace = &rest[..rest.len() - 1];
			(Some(segments[0]), namespace, name)
		}
	} else {
		let namespace = &segments[..segments.len() - 1];
		(None, namespace, name)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_go_mod_block_require() {
		let text = r#"module github.com/gorilla/mux

go 1.19

require (
	github.com/gorilla/context v1.1.1
	golang.org/x/net v0.0.0-20230101 // indirect
)
"#;
		let facts = parse_go_mod(text);
		assert!(facts.dependencies.contains(&"github.com/gorilla/context".to_owned()));
		assert!(facts.dependencies.contains(&"golang.org/x/net".to_owned()));
		assert!(facts.description.is_none(), "go.mod has no description");
		assert_eq!(
			facts.repository.as_deref(),
			Some("https://github.com/gorilla/mux"),
			"repository derived from github.com module path"
		);
	}

	#[test]
	fn parse_go_mod_single_line_require() {
		let text = "module example.com/foo\n\nrequire golang.org/x/sync v0.1.0\n";
		let facts = parse_go_mod(text);
		assert!(facts.dependencies.contains(&"golang.org/x/sync".to_owned()));
	}

	#[test]
	fn parse_go_mod_gitlab_module_path() {
		let text = "module gitlab.com/acme/tools\n\ngo 1.21\n";
		let facts = parse_go_mod(text);
		assert_eq!(facts.repository.as_deref(), Some("https://gitlab.com/acme/tools"));
	}

	#[test]
	fn parse_go_mod_unknown_host_no_repository() {
		let text = "module example.internal/foo\n\ngo 1.21\n";
		let facts = parse_go_mod(text);
		assert!(facts.repository.is_none(), "unknown host → no repository");
	}

	#[test]
	fn parse_go_mod_empty_returns_default() {
		let facts = parse_go_mod("");
		assert!(facts.dependencies.is_empty());
		assert!(facts.repository.is_none());
	}

	#[test]
	fn strip_go_authority_strips() {
		assert_eq!(strip_go_authority("github.com/gorilla/mux"), "gorilla/mux");
		assert_eq!(strip_go_authority("gorilla/mux"), "gorilla/mux");
		assert_eq!(strip_go_authority("mux"), "mux");
	}

	#[test]
	fn normalize_go_query_with_authority() {
		let q = normalize_go_query("github.com/gorilla/mux");
		assert_eq!(q.namespace.as_deref(), Some("gorilla"));
		assert_eq!(q.terms, "mux");
	}

	#[test]
	fn normalize_go_query_no_authority() {
		let q = normalize_go_query("mux");
		assert_eq!(q.namespace, None);
		assert_eq!(q.terms, "mux");
	}

	#[test]
	fn stopwords_go_specific() {
		assert!(NORMS.is_stopword("go"));
		assert!(NORMS.is_stopword("golang"));
	}

	#[test]
	fn stopwords_not_rust_words() {
		assert!(!NORMS.is_stopword("rust"));
	}

	#[test]
	fn escape_simple_path() {
		assert_eq!(escape_module_path("github.com/user/repo"), "github.com/user/repo");
	}

	#[test]
	fn escape_uppercase_segments() {
		assert_eq!(
			escape_module_path("github.com/Azure/azure-sdk"),
			"github.com/!azure/azure-sdk"
		);
	}

	#[test]
	fn escape_all_caps() {
		assert_eq!(escape_module_path("github.com/FOO/BAR"), "github.com/!f!o!o/!b!a!r");
	}

	#[test]
	fn round_trip_escape_unescape() {
		let paths = [
			"github.com/Azure/azure-sdk",
			"github.com/FOO/BAR",
			"github.com/user/repo",
			"golang.org/x/net",
		];
		for path in &paths {
			let escaped = escape_module_path(path);
			let unescaped = unescape_module_path(&escaped);
			assert_eq!(&unescaped, path, "round-trip failed for {path}");
		}
	}

	#[test]
	fn parse_version_listing_newline_separated() {
		let body = b"v1.0.0\nv1.1.0\nv2.0.0\n";
		let versions = Go::parse_version_listing(body);
		assert_eq!(versions.len(), 3);
		assert!(versions.iter().all(|v| v.status == ListingStatus::Listed));
		assert_eq!(versions[0].raw, "v1.0.0");
	}

	#[test]
	fn parse_version_listing_empty_lines_skipped() {
		let body = b"v1.0.0\n\nv1.1.0\n";
		let versions = Go::parse_version_listing(body);
		assert_eq!(versions.len(), 2);
	}

	// ── parse_download_count (no live source; pure fixture parse) ─────────────

	#[test]
	fn download_source_is_none() {
		assert!(Go::download_source().is_none());
	}

	#[test]
	fn parse_download_count_happy_path() {
		assert_eq!(Go::parse_download_count(br#"{"downloads":12345}"#), Some(12_345));
		assert_eq!(
			Go::parse_download_count(br#"{"download_count":99}"#),
			Some(99)
		);
		assert_eq!(parse_go_download_count(br#"{"downloads":1}"#), Some(1));
	}

	#[test]
	fn parse_download_count_zero_is_some() {
		assert_eq!(Go::parse_download_count(br#"{"downloads":0}"#), Some(0));
	}

	#[test]
	fn parse_download_count_malformed_or_missing() {
		assert_eq!(Go::parse_download_count(b"not json"), None);
		assert_eq!(Go::parse_download_count(b"{}"), None);
		assert_eq!(Go::parse_download_count(br#"{"downloads":null}"#), None);
		assert_eq!(Go::parse_download_count(br#"{"downloads":"nope"}"#), None);
		assert_eq!(Go::parse_download_count(b""), None);
	}
}
