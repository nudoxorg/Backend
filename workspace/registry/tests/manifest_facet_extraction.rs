//! Acceptance tests for manifest parsing + rich metadata extraction across all
//! seven ecosystems (Phase 4: manifest facts + facet parity, S1/S3/R3).
//!
//! `extract_facets` in `server/coordination/indexing.rs` is private to the
//! server binary, so we test the two-step composition it performs:
//!   1. `ecosystem::spec(lang).extract_facts(candidate, bytes)` — manifest parse
//!   2. `registry::metadata::rich::extract(&input, norms, …)` — rich facets
//!
//! This covers the full pipeline for every ecosystem, using real-shaped fixture
//! manifests under `tests/fixtures/manifests/`.

use ecosystem::{Language, LanguageExt, manifest::ManifestCandidate};
use registry::metadata::rich::{ExtractionInput, extract};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load a fixture file relative to the tests directory.
fn fixture(name: &str) -> Vec<u8> {
	let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures/manifests")
		.join(name);
	std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name} not readable: {e}"))
}

/// Run the full extraction for one ecosystem:
/// parse the manifest bytes, then call `rich::extract`, and return the result.
fn extract_for(
	lang: Language,
	pkg_name: &str,
	manifest_file: &str,
	fixture_name: &str,
) -> (registry::metadata::rich::RichMetadata, ecosystem::manifest::ExtractedFacts) {
	let spec = lang.spec();
	let norms = spec.search_norms();
	let bytes = fixture(fixture_name);

	// Use the spec's own candidate (exercises the real suffix table) rather
	// than fabricating one — `ManifestCandidate` holds a `&'static str`.
	let candidate = spec
		.manifest_candidates()
		.iter()
		.find(|c| {
			manifest_file
				.to_ascii_lowercase()
				.ends_with(&c.path_suffix.to_ascii_lowercase())
		})
		.copied()
		.unwrap_or_else(|| panic!("{lang:?} spec declares no candidate matching {manifest_file}"));
	let facts = spec
		.extract_facts(&candidate, &bytes)
		.unwrap_or_default();

	let input = ExtractionInput {
		name: pkg_name,
		description: facts.description.as_deref(),
		manifest_keywords: &facts.keywords,
		manifest_categories: &facts.categories,
		readme: None,
		identifiers: &[],
		dependencies: &facts.dependencies,
		has_repository: facts.repository.is_some(),
		has_documentation: facts.documentation,
		has_license: facts.license.is_some() || facts.has_license_file,
		loc: 500,
		release_count: None,
		withdrawn_count: None,
	};

	let rich = extract(&input, norms, None, None);
	(rich, facts)
}

// ---------------------------------------------------------------------------
// Per-ecosystem acceptance tests
// ---------------------------------------------------------------------------

/// Rust / Cargo.toml — description + keywords present, quality ≥ 0.4.
#[test]
fn rust_cargo_toml_yields_facets() {
	let (rich, facts) = extract_for(
		Language::Rust,
		"tokio-async-runtime",
		"Cargo.toml",
		"Cargo.toml",
	);
	assert!(
		facts.description.is_some(),
		"Cargo.toml must yield a description"
	);
	assert!(!facts.keywords.is_empty(), "Cargo.toml must yield keywords");
	assert!(!facts.categories.is_empty(), "Cargo.toml must yield categories");
	assert!(facts.repository.is_some(), "Cargo.toml has repository");
	assert!(facts.license.is_some() || facts.has_license_file, "Cargo.toml has license");
	assert!(!rich.keywords.is_empty(), "rich extraction must yield keywords");
	assert!(
		rich.quality >= 0.4,
		"well-formed Cargo.toml must score ≥ 0.4, got {}",
		rich.quality
	);
}

/// TypeScript / package.json — description + keywords, repository-as-object, quality ≥ 0.4.
#[test]
fn ts_package_json_yields_facets() {
	let (rich, facts) = extract_for(
		Language::Typescript,
		"@types/node",
		"package.json",
		"package.json",
	);
	assert!(
		facts.description.is_some(),
		"package.json must yield a description"
	);
	assert!(!facts.keywords.is_empty(), "package.json must yield keywords");
	assert!(facts.repository.is_some(), "repository object must be detected");
	assert!(facts.documentation, "homepage → documentation");
	assert!(facts.license.is_some() || facts.has_license_file, "package.json has license");
	assert!(!rich.keywords.is_empty(), "rich extraction must yield keywords");
	assert!(
		rich.quality >= 0.4,
		"well-formed package.json must score ≥ 0.4, got {}",
		rich.quality
	);
}

/// npm stopword: "typescript"/"node" must not appear in the rich keywords for npm.
#[test]
fn ts_npm_stopwords_filtered() {
	let (rich, _) = extract_for(
		Language::Typescript,
		"@types/node",
		"package.json",
		"package.json",
	);
	let kw_slugs: Vec<&str> = rich.keywords.iter().map(|(_, k)| k.as_str()).collect();
	assert!(
		!kw_slugs.contains(&"typescript"),
		"'typescript' is an npm stopword and must be filtered"
	);
	assert!(
		!kw_slugs.contains(&"node"),
		"'node' is an npm stopword and must be filtered"
	);
}

/// Python / pyproject.toml — description + classifiers + quality ≥ 0.4.
#[test]
fn python_pyproject_toml_yields_facets() {
	let (rich, facts) = extract_for(
		Language::Python,
		"requests",
		"pyproject.toml",
		"pyproject.toml",
	);
	assert!(
		facts.description.is_some(),
		"pyproject.toml must yield a description"
	);
	assert!(!facts.keywords.is_empty(), "pyproject.toml must yield keywords");
	// PyPI classifiers are already mapped → categories.
	assert!(
		!facts.categories.is_empty(),
		"pyproject.toml must map classifiers to categories"
	);
	assert!(facts.repository.is_some(), "pyproject.toml has repository URL");
	assert!(facts.documentation, "pyproject.toml has documentation URL");
	assert!(facts.license.is_some() || facts.has_license_file, "pyproject.toml has license classifier");
	assert!(!rich.keywords.is_empty(), "rich extraction must yield keywords");
	assert!(
		rich.quality >= 0.4,
		"well-formed pyproject.toml must score ≥ 0.4, got {}",
		rich.quality
	);
}

/// C# / .nuspec — description + tags + dependency groups + quality ≥ 0.4.
#[test]
fn csharp_nuspec_yields_facets() {
	let (rich, facts) = extract_for(
		Language::CSharp,
		"newtonsoft.json",
		".nuspec",
		"Newtonsoft.Json.nuspec",
	);
	assert!(
		facts.description.is_some(),
		".nuspec must yield a description"
	);
	assert!(!facts.keywords.is_empty(), ".nuspec must yield keywords (from tags)");
	assert!(facts.repository.is_some(), ".nuspec repository element detected");
	assert!(facts.documentation, ".nuspec projectUrl → documentation");
	assert!(facts.license.is_some() || facts.has_license_file, ".nuspec licenseUrl → license");
	assert!(
		facts.dependencies.contains(&"Microsoft.CSharp".to_owned())
			|| facts.dependencies.contains(&"System.Text.Json".to_owned()),
		"dependency groups must be flattened: {:?}",
		facts.dependencies
	);
	assert!(!rich.keywords.is_empty(), "rich extraction must yield keywords");
	assert!(
		rich.quality >= 0.4,
		"well-formed .nuspec must score ≥ 0.4, got {}",
		rich.quality
	);
}

/// Go / go.mod — no description field, but block `require` deps extracted.
#[test]
fn go_mod_yields_dependencies() {
	let (rich, facts) = extract_for(
		Language::Go,
		"github.com/gorilla/mux",
		"go.mod",
		"go.mod",
	);
	assert!(facts.description.is_none(), "go.mod has no description field");
	assert!(
		facts.dependencies.contains(&"github.com/gorilla/context".to_owned()),
		"block require must be parsed: {:?}",
		facts.dependencies
	);
	assert!(
		facts.dependencies.contains(&"golang.org/x/net".to_owned()),
		"indirect deps must also be parsed"
	);
	// Quality may be low (no description/keywords) but extraction must not panic.
	let _ = rich.quality;
}

/// Java / pom.xml — description + SCM + licenses + dependency group:artifact.
#[test]
fn java_pom_xml_yields_facets() {
	let (rich, facts) = extract_for(
		Language::Java,
		"org.springframework:spring-core",
		"pom.xml",
		"pom.xml",
	);
	assert!(
		facts.description.is_some(),
		"pom.xml must yield a description"
	);
	assert!(facts.repository.is_some(), "pom.xml SCM → repository");
	assert!(facts.documentation, "pom.xml <url> → documentation");
	assert!(facts.license.is_some() || facts.has_license_file, "pom.xml <licenses> → license");
	assert!(
		facts.dependencies.contains(&"io.micrometer:micrometer-observation".to_owned()),
		"group:artifact deps must be extracted: {:?}",
		facts.dependencies
	);
	assert!(!rich.keywords.is_empty(), "rich extraction must yield keywords");
	assert!(
		rich.quality >= 0.4,
		"well-formed pom.xml must score ≥ 0.4, got {}",
		rich.quality
	);
}

/// Nix / flake.nix — description extracted via text scan.
#[test]
fn nix_flake_nix_yields_description() {
	let (rich, facts) = extract_for(
		Language::Nix,
		"nixos/nixpkgs",
		"flake.nix",
		"flake.nix",
	);
	assert!(
		facts.description.is_some(),
		"flake.nix must yield a description via text scan"
	);
	assert!(
		facts.description.as_deref().unwrap_or("").len() > 5,
		"description must be non-trivial"
	);
	assert!(!rich.keywords.is_empty(), "rich extraction must yield keywords from description");
}

// ---------------------------------------------------------------------------
// BM25 / quality parity smoke test
// ---------------------------------------------------------------------------

/// npm-vs-Rust fused-score parity: for packages with equivalent metadata
/// richness, `rich::extract` quality scores should be within 2× of each other.
#[test]
fn npm_vs_rust_quality_parity_smoke() {
	let (rust_rich, _) = extract_for(
		Language::Rust,
		"tokio-async-runtime",
		"Cargo.toml",
		"Cargo.toml",
	);
	let (npm_rich, _) = extract_for(
		Language::Typescript,
		"@types/node",
		"package.json",
		"package.json",
	);
	// Both fixtures are well-formed; quality scores should be within 2× of each other.
	let ratio = rust_rich.quality.max(npm_rich.quality)
		/ rust_rich.quality.min(npm_rich.quality).max(f32::EPSILON);
	assert!(
		ratio <= 2.0,
		"npm ({}) vs Rust ({}) quality ratio {ratio:.2} exceeds 2×",
		npm_rich.quality,
		rust_rich.quality
	);
}

// ---------------------------------------------------------------------------
// Adversarial / edge-case tests
// ---------------------------------------------------------------------------

/// Truncated bytes must never panic — all parsers must return None or default.
#[test]
fn truncated_manifests_do_not_panic() {
	let langs_and_candidates: &[(Language, &str, &[u8])] = &[
		(Language::Rust, "Cargo.toml", b"[package\n"),
		(Language::Typescript, "package.json", b"{\"description\":"),
		(Language::Python, "pyproject.toml", b"[project\n"),
		(Language::CSharp, ".nuspec", b"<?xml version=\"1.0\"?><package><metadata>"),
		(Language::Go, "go.mod", b"module foo\n\nrequire ("),
		(Language::Java, "pom.xml", b"<project><description>hello"),
		(Language::Nix, "flake.nix", b"{ description = \""),
	];
	for (lang, suffix, bytes) in langs_and_candidates {
		let spec = lang.spec();
		let candidate = ManifestCandidate::new(suffix);
		// Must not panic; result may be None or default facts.
		let _ = spec.extract_facts(&candidate, bytes);
	}
}

/// Completely empty bytes must not panic for any ecosystem.
#[test]
fn empty_bytes_do_not_panic() {
	for lang in [
		Language::Rust,
		Language::Typescript,
		Language::Python,
		Language::CSharp,
		Language::Go,
		Language::Java,
		Language::Nix,
	] {
		let spec = lang.spec();
		for candidate in spec.manifest_candidates() {
			let _ = spec.extract_facts(candidate, b"");
		}
	}
}

/// BOM-prefixed JSON is parsed by the TypeScript impl (covered in ts.rs unit
/// tests; ensure it survives the DynSpec path too).
#[test]
fn bom_prefixed_json_parsed_via_dynspec() {
	let mut bytes = b"\xef\xbb\xbf".to_vec();
	bytes.extend_from_slice(br#"{"description":"hello BOM","keywords":["bom"]}"#);
	let spec = Language::Typescript.spec();
	let candidate = ManifestCandidate::new("package.json");
	let facts = spec.extract_facts(&candidate, &bytes).expect("BOM-prefixed JSON must parse");
	assert_eq!(facts.description.as_deref(), Some("hello BOM"));
}

/// XML entities in nuspec descriptions must be decoded (not left as `&amp;`).
#[test]
fn nuspec_xml_entities_decoded() {
	let xml = br#"<?xml version="1.0"?>
<package><metadata>
  <description>A &amp; B &lt;test&gt;</description>
  <tags>foo bar</tags>
</metadata></package>"#;
	let spec = Language::CSharp.spec();
	let candidate = ManifestCandidate::new(".nuspec");
	let facts = spec.extract_facts(&candidate, xml).expect("entity XML parses");
	let desc = facts.description.as_deref().unwrap_or("");
	assert!(desc.contains("A & B"), "& entity must be decoded, got: {desc:?}");
}

/// Huge keyword lists must be capped and not cause allocation issues.
#[test]
fn huge_keyword_list_capped() {
	let kws: Vec<String> = (0..200).map(|i| format!("keyword{i}")).collect();
	let json = serde_json::json!({
		"description": "a package",
		"keywords": kws,
	});
	let bytes = serde_json::to_vec(&json).unwrap();
	let spec = Language::Typescript.spec();
	let candidate = ManifestCandidate::new("package.json");
	let facts = spec.extract_facts(&candidate, &bytes).expect("parses");
	// The npm impl caps at 50.
	assert!(facts.keywords.len() <= 50, "keywords must be capped, got {}", facts.keywords.len());
}

/// Control characters in descriptions must be stripped.
#[test]
fn control_chars_stripped_from_description() {
	// JSON with control char \x01 in description.
	let json = b"{\"description\":\"hello\\u0001world\",\"keywords\":[\"test\"]}";
	let spec = Language::Typescript.spec();
	let candidate = ManifestCandidate::new("package.json");
	let facts = spec.extract_facts(&candidate, json).expect("parses");
	let desc = facts.description.as_deref().unwrap_or("");
	assert!(!desc.contains('\x01'), "control chars must be stripped: {desc:?}");
	assert_eq!(desc, "helloworld");
}
