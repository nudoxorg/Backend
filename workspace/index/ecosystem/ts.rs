//! npm (the Typescript ecosystem's registry).

use crate::ecosystem::{
	EcosystemSpec, Language, archive::ArchiveKind, manifest::{self, ExtractedFacts, ManifestCandidate},
	name, policy::UpstreamPolicy, search::{DEFAULT_SPECIFICITY_SEPARATORS, NormalizedQuery, SearchNorms, map_no_category},
	upstream::{self, ListedVersion, ListingStatus}, version,
};

pub struct TypeScript;

impl crate::ecosystem::sealed::Sealed for TypeScript {}

impl EcosystemSpec for TypeScript {
	const ARCHIVE: ArchiveKind = ArchiveKind::TarGz;
	const LANGUAGE: Language = Language::Typescript;
	const POLICY: UpstreamPolicy = UpstreamPolicy {
		max_requests_per_second: 10.0,
		retry_budget: 3,
		respect_retry_after: true,
	};

	type Manifest = manifest::ExtractedFacts;
	type Version = version::SemverVersion;

	fn parse_name(raw: &str) -> Option<name::StructuredName> {
		// npm: optional `@scope/name`; each segment is ASCII alphanumeric + `-`/`_`/`.`,
		// not starting with `.`. Canonical: lowercase.
		let segment_ok = |s: &str| {
			!s.is_empty()
				&& !s.starts_with('.')
				&& s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
		};
		let (namespace, bare_name) = match raw.strip_prefix('@') {
			Some(scoped) => match scoped.split_once('/') {
				Some((scope, pkg)) if segment_ok(scope) && segment_ok(pkg) && !pkg.contains('/') => {
					(vec![smol_str::SmolStr::from(scope.to_ascii_lowercase())],
					 pkg.to_ascii_lowercase())
				}
				_ => return None,
			},
			None if segment_ok(raw) && !raw.contains('/') => (vec![], raw.to_ascii_lowercase()),
			_ => return None,
		};
		Some(name::StructuredName {
			ecosystem: Language::Typescript,
			authority: None,
			namespace,
			name: bare_name.into(),
			major: None,
			original: raw.into(),
		})
	}

	/// Canonical: `@scope/name` for scoped packages, else just `name`.
	fn render_canonical(n: &name::StructuredName) -> String {
		match n.namespace.first() {
			Some(scope) => format!("@{scope}/{}", n.name),
			None => n.name.to_string(),
		}
	}

	fn endpoints() -> upstream::UpstreamEndpoints {
		upstream::UpstreamEndpoints {
			listing: "/{name}",
			archive: "/{name}/-/{leaf}-{version}.tgz",
			listing_status: None,
		}
	}

	/// Parse the npm registry `/{name}` response.
	/// `versions` object keys are version strings; a `deprecated` field present
	/// on the version entry (any non-null value) → Withdrawn with that message.
	/// M7 fix.
	fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
		use version::VersionGrammar;
		let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
			return vec![];
		};
		let Some(versions) = v["versions"].as_object() else {
			return vec![];
		};
		versions
			.iter()
			.filter_map(|(raw, entry)| {
				let version = version::SemverVersion::parse(raw)?;
				let status = match entry.get("deprecated") {
					Some(msg) if !msg.is_null() => {
						let reason = msg.as_str().map(smol_str::SmolStr::from);
						ListingStatus::Withdrawn { reason }
					}
					_ => ListingStatus::Listed,
				};
				Some(ListedVersion { version, status, raw: raw.as_str().into() })
			})
			.collect()
	}

	fn manifest_candidates() -> &'static [ManifestCandidate] {
		static CANDIDATES: &[ManifestCandidate] = &[ManifestCandidate::new("package.json")];
		CANDIDATES
	}

	fn parse_manifest(
		_candidate: &ManifestCandidate,
		bytes: &[u8],
	) -> Option<Self::Manifest> {
		// Strip BOM if present.
		let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
		parse_package_json(bytes)
	}

	fn search_norms() -> &'static SearchNorms { &NORMS }

	/// npm download counts via the npm Downloads API (last 30 days).
	/// Template: `https://api.npmjs.org/downloads/point/last-month/{name}`
	/// Response JSON: `{"downloads": 12345, ...}`.
	fn download_source() -> Option<upstream::DownloadEndpoint> {
		Some(upstream::DownloadEndpoint {
			url: "https://api.npmjs.org/downloads/point/last-month/{name}",
		})
	}

	/// Parse the npm Downloads API response: `{"downloads": N, ...}`.
	/// Explicit `0` is valid (`Some(0)`). Malformed / missing field → `None`.
	fn parse_download_count(body: &[u8]) -> Option<u64> {
		let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
		v["downloads"].as_u64()
	}
}

static NORMS: SearchNorms = SearchNorms {
	stopwords: &["deno", "js", "node", "npm", "typescript"],
	specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
	strip_conventions: strip_npm_scope,
	normalize_query: normalize_npm_query,
	map_category: map_no_category,
	downloads_scale: Some(0.05),
};

/// Strip a leading `@scope/` from a package name.
pub fn strip_npm_scope(name: &str) -> &str {
	if name.starts_with('@')
		&& let Some(slash) = name.find('/') {
			return &name[slash + 1..];
		}
	name
}

/// `@scope/name` → namespace=Some(scope), terms=name; else identity.
fn normalize_npm_query(terms: &str) -> NormalizedQuery {
	if let Some(rest) = terms.strip_prefix('@')
		&& let Some((scope, pkg)) = rest.split_once('/') {
			return NormalizedQuery {
				terms: pkg.to_owned(),
				namespace: Some(scope.to_owned()),
			};
		}
	NormalizedQuery { terms: terms.to_owned(), namespace: None }
}

#[derive(serde::Deserialize)]
struct PackageJson {
	description: Option<String>,
	keywords: Option<serde_json::Value>,
	repository: Option<serde_json::Value>,
	homepage: Option<String>,
	license: Option<serde_json::Value>,
	dependencies: Option<serde_json::Value>,
}

pub fn parse_package_json(bytes: &[u8]) -> Option<ExtractedFacts> {
	let pkg: PackageJson = serde_json::from_slice(bytes).ok()?;

	let keywords: Vec<String> = match pkg.keywords {
		Some(serde_json::Value::Array(arr)) => arr
			.into_iter()
			.filter_map(|v| v.as_str().map(str::to_owned))
			.take(50)
			.collect(),
		_ => vec![],
	};

	let repository: Option<String> = match &pkg.repository {
		Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
		Some(serde_json::Value::Object(obj)) => obj
			.get("url")
			.and_then(|v| v.as_str())
			.filter(|s| !s.is_empty())
			.map(str::to_owned),
		_ => None,
	};

	let documentation = pkg.homepage.as_deref().is_some_and(|s| !s.is_empty());

	// Object-form license (`{"type":"MIT","url":"..."}`) existed in old npm packages —
	// extract `type` when present; fall back to the whole object being a presence signal.
	let license: Option<String> = match &pkg.license {
		Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
		Some(serde_json::Value::Object(obj)) => obj
			.get("type")
			.and_then(|v| v.as_str())
			.filter(|s| !s.is_empty())
			.map(str::to_owned),
		_ => None,
	};

	let dependencies: Vec<String> = match pkg.dependencies {
		Some(serde_json::Value::Object(obj)) => obj.keys().cloned().collect(),
		_ => vec![],
	};

	// Sanitize description: strip control chars.
	let description = pkg.description.map(|d| {
		d.chars().filter(|c| !c.is_control()).collect::<String>()
	}).filter(|d| !d.is_empty());

	Some(ExtractedFacts {
		description,
		keywords,
		categories: vec![],
		readme_hint: None,
		repository,
		documentation,
		license,
		has_license_file: false,
		dependencies,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_basic_package_json() {
		let json = br#"{
			"name": "axios",
			"description": "Promise based HTTP client",
			"keywords": ["http", "xhr", "promise"],
			"license": "MIT",
			"repository": "https://github.com/axios/axios",
			"homepage": "https://axios-http.com",
			"dependencies": {"follow-redirects": "^1.15.0"}
		}"#;
		let facts = parse_package_json(json).expect("valid JSON");
		assert_eq!(facts.description.as_deref(), Some("Promise based HTTP client"));
		assert_eq!(facts.keywords, vec!["http", "xhr", "promise"]);
		assert_eq!(facts.repository.as_deref(), Some("https://github.com/axios/axios"));
		assert!(facts.documentation);
		assert_eq!(facts.license.as_deref(), Some("MIT"));
		assert!(!facts.has_license_file);
		assert!(facts.dependencies.contains(&"follow-redirects".to_owned()));
	}

	#[test]
	fn parse_scoped_package_with_repository_object() {
		let json = br#"{
			"name": "@types/node",
			"description": "TypeScript definitions for node.js",
			"keywords": ["node", "typescript"],
			"repository": {"type": "git", "url": "https://github.com/DefinitelyTyped/DefinitelyTyped"},
			"dependencies": {"@types/globals": "*"}
		}"#;
		let facts = parse_package_json(json).expect("valid JSON");
		assert_eq!(
			facts.repository.as_deref(),
			Some("https://github.com/DefinitelyTyped/DefinitelyTyped"),
			"object-form repository URL must be extracted"
		);
		assert!(facts.dependencies.contains(&"@types/globals".to_owned()));
	}

	#[test]
	fn parse_object_form_license_extracts_type() {
		let json = br#"{
			"name": "old-pkg",
			"license": {"type": "ISC", "url": "https://opensource.org/licenses/ISC"}
		}"#;
		let facts = parse_package_json(json).expect("valid JSON");
		assert_eq!(facts.license.as_deref(), Some("ISC"), "object-form license type must be extracted");
	}

	#[test]
	fn parse_npm_shorthand_repository_string() {
		// npm shorthand strings (e.g. "github:user/repo") are preserved verbatim;
		// normalization happens in repo::normalize_repo_url.
		let json = br#"{"name":"foo","repository":"github:user/my-pkg"}"#;
		let facts = parse_package_json(json).expect("valid JSON");
		assert_eq!(facts.repository.as_deref(), Some("github:user/my-pkg"));
	}

	#[test]
	fn parse_bom_prefixed_json() {
		let mut bytes = b"\xef\xbb\xbf".to_vec();
		bytes.extend_from_slice(br#"{"description":"hello","keywords":["foo"]}"#);
		let facts = TypeScript::parse_manifest(&ManifestCandidate::new("package.json"), &bytes)
			.expect("BOM-prefixed JSON parses");
		assert_eq!(facts.description.as_deref(), Some("hello"));
	}

	#[test]
	fn parse_malformed_json_returns_none() {
		assert!(parse_package_json(b"not json {{").is_none());
	}

	#[test]
	fn parse_truncated_json_returns_none() {
		assert!(parse_package_json(b"{\"description\":").is_none());
	}

	#[test]
	fn parse_huge_keyword_list_caps_at_50() {
		let kws: Vec<String> = (0..100).map(|i| format!("kw{i}")).collect();
		let json = serde_json::json!({
			"description": "test",
			"keywords": kws,
		});
		let bytes = serde_json::to_vec(&json).unwrap();
		let facts = parse_package_json(&bytes).expect("valid JSON");
		assert_eq!(facts.keywords.len(), 50);
	}

	#[test]
	fn parse_description_strips_control_chars() {
		let json = b"{\"description\":\"hello\\u0001world\"}";
		let facts = parse_package_json(json).expect("valid JSON");
		assert_eq!(facts.description.as_deref(), Some("helloworld"));
	}

	#[test]
	fn stopwords_npm_specific() {
		assert!(NORMS.is_stopword("node"), "'node' stopped for npm");
		assert!(NORMS.is_stopword("npm"), "'npm' stopped for npm");
		assert!(NORMS.is_stopword("typescript"), "'typescript' stopped for npm");
	}

	#[test]
	fn stopwords_not_rust_words() {
		assert!(!NORMS.is_stopword("rust"), "'rust' NOT stopped for npm");
		assert!(!NORMS.is_stopword("crate"), "'crate' NOT stopped for npm");
	}

	#[test]
	fn normalize_npm_query_scoped() {
		let q = normalize_npm_query("@types/node");
		assert_eq!(q.namespace.as_deref(), Some("types"));
		assert_eq!(q.terms, "node");
	}

	#[test]
	fn normalize_npm_query_bare() {
		let q = normalize_npm_query("axios");
		assert_eq!(q.namespace, None);
		assert_eq!(q.terms, "axios");
	}

	#[test]
	fn strip_npm_scope_removes_scope() {
		assert_eq!(strip_npm_scope("@types/node"), "node");
		assert_eq!(strip_npm_scope("axios"), "axios");
	}

	#[test]
	fn parse_npm_versions_with_deprecated() {
		let body = br#"{
			"versions": {
				"1.0.0": {},
				"0.9.0": {"deprecated": "Use 1.0.0 instead"},
				"0.8.0": {"deprecated": null}
			}
		}"#;
		let versions = TypeScript::parse_version_listing(body);
		assert_eq!(versions.len(), 3);
		let v1 = versions.iter().find(|v| v.raw == "1.0.0").unwrap();
		assert!(v1.status.is_listed());
		let v09 = versions.iter().find(|v| v.raw == "0.9.0").unwrap();
		assert!(!v09.status.is_listed());
		assert!(matches!(&v09.status, ListingStatus::Withdrawn { reason: Some(r) } if r == "Use 1.0.0 instead"));
		// null deprecated = listed
		let v08 = versions.iter().find(|v| v.raw == "0.8.0").unwrap();
		assert!(v08.status.is_listed());
	}

	// ── parse_download_count (S4) ────────────────────────────────────────────

	#[test]
	fn parse_download_count_happy_path() {
		assert_eq!(
			TypeScript::parse_download_count(br#"{"downloads":123,"start":"2024-01-01","end":"2024-01-31","package":"lodash"}"#),
			Some(123)
		);
	}

	#[test]
	fn parse_download_count_zero_is_some() {
		assert_eq!(TypeScript::parse_download_count(br#"{"downloads":0}"#), Some(0));
	}

	#[test]
	fn parse_download_count_malformed_or_missing() {
		assert_eq!(TypeScript::parse_download_count(b"not json"), None);
		assert_eq!(TypeScript::parse_download_count(b"{}"), None);
		assert_eq!(TypeScript::parse_download_count(br#"{"downloads":null}"#), None);
		assert_eq!(TypeScript::parse_download_count(br#"{"downloads":"nope"}"#), None);
		assert_eq!(TypeScript::parse_download_count(b""), None);
	}
}
