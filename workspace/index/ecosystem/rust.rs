//! crates.io.

use crate::ecosystem::{
    EcosystemSpec, Language,
    archive::ArchiveKind,
    manifest::{self, ExtractedFacts, ManifestCandidate},
    name,
    policy::UpstreamPolicy,
    search::{self, DEFAULT_SPECIFICITY_SEPARATORS, SearchNorms, map_internal_category},
    upstream::{self, ListedVersion, ListingStatus},
    version,
};

pub struct Rust;

impl crate::ecosystem::sealed::Sealed for Rust {}

impl EcosystemSpec for Rust {
    const ARCHIVE: ArchiveKind = ArchiveKind::TarGz;
    const LANGUAGE: Language = Language::Rust;
    const POLICY: UpstreamPolicy = UpstreamPolicy {
        max_requests_per_second: 1.0,
        retry_budget: 3,
        respect_retry_after: true,
    };

    type Manifest = manifest::ExtractedFacts;
    type Version = version::SemverVersion;

    fn parse_name(raw: &str) -> Option<name::StructuredName> {
        // crates.io: ASCII alphanumerics + `-`/`_`, starting alphanumeric.
        // Canonical: lowercase, `_` → `-` (crates.io treats them as identical).
        if raw.is_empty()
            || !raw.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return None;
        }
        let canonical: smol_str::SmolStr = raw.to_ascii_lowercase().replace('_', "-").into();
        Some(name::StructuredName {
            ecosystem: Language::Rust,
            authority: None,
            namespace: vec![],
            name: canonical,
            major: None,
            original: raw.into(),
        })
    }

    /// Canonical = the `name` field (already normalized in `parse_name`).
    fn render_canonical(n: &name::StructuredName) -> String {
        n.name.to_string()
    }

    fn endpoints() -> upstream::UpstreamEndpoints {
        upstream::UpstreamEndpoints {
            listing: "/api/v1/crates/{name}",
            archive: "https://static.crates.io/crates/{name}/{name}-{version}.crate",
            listing_status: None,
        }
    }

    /// Parse the crates.io `/api/v1/crates/{name}` JSON response.
    /// versions[].num → version string; versions[].yanked → Withdrawn.
    /// All versions are included (yanked ones carry Withdrawn status rather
    /// than being filtered, so exact-pin resolution can still find them).
    fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
        use version::VersionGrammar;
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return vec![];
        };
        v["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let raw = entry["num"].as_str()?;
                let version = version::SemverVersion::parse(raw)?;
                let yanked = entry["yanked"].as_bool().unwrap_or(false);
                let status = if yanked {
                    ListingStatus::Withdrawn { reason: None }
                } else {
                    ListingStatus::Listed
                };
                Some(ListedVersion {
                    version,
                    status,
                    raw: raw.into(),
                })
            })
            .collect()
    }

    fn manifest_candidates() -> &'static [ManifestCandidate] {
        static CANDIDATES: &[ManifestCandidate] = &[ManifestCandidate::new("Cargo.toml")];
        CANDIDATES
    }

    fn parse_manifest(_candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
        let text = std::str::from_utf8(bytes).ok()?;
        Some(parse_cargo_toml(text))
    }

    fn search_norms() -> &'static search::SearchNorms {
        &NORMS
    }

    /// crates.io embeds download counts in the same `/api/v1/crates/{name}`
    /// listing JSON used for version resolution. Ingest does not currently
    /// thread that body into facet extraction (resolve/listing is a separate
    /// path from archive emit), so S4 re-fetches the listing URL via the shared
    /// download-count loop — same trait shape as npm/NuGet. Prefer
    /// `crate.recent_downloads` (≈90d) over all-time `crate.downloads`.
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        Some(upstream::DownloadEndpoint {
            // Absolute URL: UpstreamClient GETs the template as-is (no registry base).
            url: "https://crates.io/api/v1/crates/{name}",
        })
    }

    /// Parse crates.io crate metadata: prefer `crate.recent_downloads`, fall
    /// back to all-time `crate.downloads`. Malformed / missing → `None` (no
    /// panic). Explicit `0` is valid (`Some(0)`).
    fn parse_download_count(body: &[u8]) -> Option<u64> {
        let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
        let krate = &v["crate"];
        krate["recent_downloads"]
            .as_u64()
            .or_else(|| krate["downloads"].as_u64())
    }
}

static NORMS: SearchNorms = SearchNorms {
    stopwords: &["crate", "crates", "rust"],
    specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
    strip_conventions: strip_rust_conventions,
    normalize_query: search::normalize_identity,
    map_category: map_internal_category,
    downloads_scale: Some(1.0),
};

/// Strip `cargo`/`rust` prefix and `rs` suffix, then trim `-`/`_`.
pub fn strip_rust_conventions(name: &str) -> &str {
    let mut s = name;
    for prefix in &["cargo-", "cargo_", "rust-", "rust_"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    for suffix in &["-rs", "_rs"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest;
            break;
        }
    }
    s.trim_matches(|c| c == '-' || c == '_')
}

/// Parse a `Cargo.toml` into [`ExtractedFacts`]. Non-fatal: malformed input
/// returns empty facts rather than `None` (so name-only facets are always
/// produced, per the design requirement).
pub fn parse_cargo_toml(text: &str) -> ExtractedFacts {
    let Ok(value) = text.parse::<toml::Value>() else {
        return ExtractedFacts::default();
    };
    let Some(package) = value.get("package").and_then(toml::Value::as_table) else {
        return ExtractedFacts::default();
    };

    let string_field = |key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
    };
    let string_array = |key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let dependencies = cargo_dependency_edges(&value);

    let repository = string_field("repository").filter(|s| !s.is_empty());
    let license_expr = string_field("license").filter(|s| !s.is_empty());
    let has_license_file = package.contains_key("license-file");

    ExtractedFacts {
        description: string_field("description"),
        keywords: string_array("keywords"),
        categories: string_array("categories"),
        readme_hint: string_field("readme"),
        repository,
        documentation: package.contains_key("documentation"),
        license: license_expr,
        has_license_file,
        dependencies,
    }
}

fn cargo_dependency_edges(value: &toml::Value) -> Vec<crate::record::DepEdge> {
    use crate::{enums::EdgeKind, record::DepClass};

    let mut edges = Vec::new();
    push_cargo_table(
        &mut edges,
        value.get("dev-dependencies"),
        DepClass::Dev,
        EdgeKind::Build,
    );
    push_cargo_table(
        &mut edges,
        value.get("build-dependencies"),
        DepClass::Build,
        EdgeKind::Build,
    );
    push_cargo_table(
        &mut edges,
        value.get("dependencies"),
        DepClass::Runtime,
        EdgeKind::Runtime,
    );
    edges.sort_by(|left, right| left.name.cmp(&right.name));
    edges
}

fn push_cargo_table(
    edges: &mut Vec<crate::record::DepEdge>,
    table: Option<&toml::Value>,
    class: crate::record::DepClass,
    kind: crate::enums::EdgeKind,
) {
    let Some(table) = table.and_then(toml::Value::as_table) else {
        return;
    };
    for (name, spec) in table {
        let (requirement, optional) = match spec {
            toml::Value::String(requirement) => (Some(requirement.as_str()), false),
            toml::Value::Table(fields) => (
                fields.get("version").and_then(toml::Value::as_str),
                fields
                    .get("optional")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false),
            ),
            _ => (None, false),
        };
        let mut edge = crate::record::DepEdge::runtime(name);
        if name.is_empty() {
            continue;
        }
        edge.class = class;
        edge.kind = kind;
        edge.optional = optional && class == crate::record::DepClass::Runtime;
        if let Some(requirement) = requirement.map(str::trim).filter(|text| !text.is_empty()) {
            edge.requirement = Some(requirement.into());
        }
        if let Some(existing) = edges.iter_mut().find(|stored| stored.name == edge.name) {
            if cargo_rank(edge.class) > cargo_rank(existing.class) {
                *existing = edge;
            }
            continue;
        }
        edges.push(edge);
    }
}

fn cargo_rank(class: crate::record::DepClass) -> u8 {
    match class {
        crate::record::DepClass::Runtime => 2,
        crate::record::DepClass::Build => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_crates_io_version_listing() {
        let body = br#"{
			"versions": [
				{"num": "1.0.0", "yanked": false},
				{"num": "0.9.0", "yanked": true},
				{"num": "0.8.0", "yanked": false}
			]
		}"#;
        let versions = Rust::parse_version_listing(body);
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].raw, "1.0.0");
        assert!(versions[0].status.is_listed());
        assert_eq!(versions[1].raw, "0.9.0");
        assert!(!versions[1].status.is_listed());
        assert!(matches!(versions[1].status, ListingStatus::Withdrawn {
            reason: None
        }));
        assert!(versions[2].status.is_listed());
    }

    #[test]
    fn parse_version_listing_empty_body() {
        assert!(Rust::parse_version_listing(b"{}").is_empty());
        assert!(Rust::parse_version_listing(b"not json").is_empty());
    }

    #[test]
    fn parse_full_cargo_toml() {
        let toml = r#"
[package]
name = "my-crate"
version = "0.1.0"
description = "A useful async runtime helper"
keywords = ["async", "runtime", "tokio"]
categories = ["async-io", "network-programming"]
repository = "https://github.com/example/my-crate"
license = "MIT"
readme = "README.md"

[dependencies]
tokio = "1.0"
serde = { version = "1.0", features = ["derive"] }
"#;
        let facts = parse_cargo_toml(toml);
        assert_eq!(
            facts.description.as_deref(),
            Some("A useful async runtime helper")
        );
        assert_eq!(facts.keywords, vec!["async", "runtime", "tokio"]);
        assert_eq!(facts.categories, vec!["async-io", "network-programming"]);
        assert_eq!(facts.readme_hint.as_deref(), Some("README.md"));
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/example/my-crate")
        );
        assert_eq!(facts.license.as_deref(), Some("MIT"));
        assert!(!facts.has_license_file);
        assert!(!facts.documentation);
        assert!(facts.dependency_names().contains(&"tokio".to_owned()));
        assert!(facts.dependency_names().contains(&"serde".to_owned()));
        let tokio = facts
            .dependencies
            .iter()
            .find(|edge| edge.name == "tokio")
            .expect("tokio");
        assert_eq!(tokio.requirement.as_deref(), Some("1.0"));
        let serde = facts
            .dependencies
            .iter()
            .find(|edge| edge.name == "serde")
            .expect("serde");
        assert_eq!(serde.requirement.as_deref(), Some("1.0"));
        assert!(!serde.optional);
    }

    #[test]
    fn dev_and_build_dependencies_keep_their_class() {
        let toml = r#"
[package]
name = "my-crate"
version = "0.1.0"

[dependencies]
tokio = "1"

[dev-dependencies]
tokio = "1"
criterion = "0.5"

[build-dependencies]
cc = "1"
"#;
        let facts = parse_cargo_toml(toml);
        let tokio = facts
            .dependencies
            .iter()
            .find(|edge| edge.name == "tokio")
            .expect("tokio");
        assert_eq!(tokio.class, crate::record::DepClass::Runtime);
        assert_eq!(tokio.kind, crate::enums::EdgeKind::Runtime);
        let criterion = facts
            .dependencies
            .iter()
            .find(|edge| edge.name == "criterion")
            .expect("criterion");
        assert_eq!(criterion.class, crate::record::DepClass::Dev);
        assert_eq!(criterion.kind, crate::enums::EdgeKind::Build);
        assert_eq!(criterion.requirement.as_deref(), Some("0.5"));
        let cc = facts
            .dependencies
            .iter()
            .find(|edge| edge.name == "cc")
            .expect("cc");
        assert_eq!(cc.class, crate::record::DepClass::Build);
        assert_eq!(cc.kind, crate::enums::EdgeKind::Build);
    }

    #[test]
    fn parse_cargo_toml_license_file_only() {
        let toml = r#"
[package]
name = "my-crate"
version = "0.1.0"
license-file = "LICENSE"
"#;
        let facts = parse_cargo_toml(toml);
        assert!(facts.license.is_none(), "license-file only → no expression");
        assert!(facts.has_license_file, "has_license_file must be true");
    }

    #[test]
    fn parse_cargo_toml_no_repository_or_license() {
        let toml = "[package]\nname = \"bare\"\nversion = \"0.1.0\"\n";
        let facts = parse_cargo_toml(toml);
        assert!(facts.repository.is_none());
        assert!(facts.license.is_none());
        assert!(!facts.has_license_file);
    }

    #[test]
    fn parse_malformed_cargo_toml_returns_default() {
        let facts = parse_cargo_toml("not valid toml {{{{");
        assert_eq!(facts, ExtractedFacts::default());
    }

    #[test]
    fn parse_missing_package_section() {
        let facts = parse_cargo_toml("[workspace]\nmembers = [\"crates/foo\"]");
        assert_eq!(facts, ExtractedFacts::default());
    }

    #[test]
    fn parse_truncated_input() {
        let facts = parse_cargo_toml("[package\n");
        assert_eq!(facts, ExtractedFacts::default());
    }

    #[test]
    fn strip_rust_conventions_prefixes_and_suffixes() {
        assert_eq!(strip_rust_conventions("cargo-fmt"), "fmt");
        assert_eq!(strip_rust_conventions("rust-analyzer"), "analyzer");
        assert_eq!(strip_rust_conventions("ripgrep-rs"), "ripgrep");
        assert_eq!(strip_rust_conventions("serde"), "serde");
        assert_eq!(strip_rust_conventions("cargo_clippy"), "clippy");
    }

    #[test]
    fn stopwords_rust_specific() {
        assert!(
            NORMS.is_stopword("rust"),
            "'rust' is stopped for Rust ecosystem"
        );
        assert!(
            NORMS.is_stopword("crate"),
            "'crate' is stopped for Rust ecosystem"
        );
        assert!(
            NORMS.is_stopword("crates"),
            "'crates' is stopped for Rust ecosystem"
        );
        // English stopwords still work.
        assert!(NORMS.is_stopword("the"));
    }

    #[test]
    fn stopwords_not_npm_words() {
        assert!(!NORMS.is_stopword("node"), "'node' is NOT stopped for Rust");
        assert!(!NORMS.is_stopword("npm"), "'npm' is NOT stopped for Rust");
    }

    #[test]
    fn manifest_candidates_cargo_toml() {
        let candidates = Rust::manifest_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path_suffix, "Cargo.toml");
    }

    // ── parse_download_count (S4) ────────────────────────────────────────────

    #[test]
    fn parse_download_count_recent_downloads() {
        let body = br#"{
			"crate": {
				"id": "serde",
				"name": "serde",
				"downloads": 999999999,
				"recent_downloads": 1234567
			},
			"versions": []
		}"#;
        assert_eq!(Rust::parse_download_count(body), Some(1_234_567));
    }

    #[test]
    fn parse_download_count_falls_back_to_all_time() {
        // recent_downloads null/missing → use all-time downloads.
        let body = br#"{"crate":{"name":"foo","downloads":42}}"#;
        assert_eq!(Rust::parse_download_count(body), Some(42));

        let body_null = br#"{"crate":{"name":"foo","downloads":42,"recent_downloads":null}}"#;
        assert_eq!(Rust::parse_download_count(body_null), Some(42));
    }

    #[test]
    fn parse_download_count_zero_is_some() {
        let body = br#"{"crate":{"name":"brand-new","downloads":0,"recent_downloads":0}}"#;
        assert_eq!(Rust::parse_download_count(body), Some(0));
    }

    #[test]
    fn parse_download_count_malformed_or_missing() {
        assert_eq!(Rust::parse_download_count(b"not json"), None);
        assert_eq!(Rust::parse_download_count(b"{}"), None);
        assert_eq!(Rust::parse_download_count(br#"{"crate":{}}"#), None);
        assert_eq!(
            Rust::parse_download_count(br#"{"crate":{"recent_downloads":"nope"}}"#),
            None
        );
        assert_eq!(Rust::parse_download_count(b""), None);
    }

    #[test]
    fn download_source_is_crates_io_listing() {
        let ep = Rust::download_source().expect("Rust has a download source");
        assert!(ep.url.contains("crates.io"));
        assert!(ep.url.contains("{name}"));
    }
}
