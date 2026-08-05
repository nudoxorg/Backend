//! FlakeHub.

use smol_str::SmolStr;

use crate::ecosystem::{
    EcosystemSpec, Language,
    archive::ArchiveKind,
    manifest::{self, ExtractedFacts, ManifestCandidate},
    name,
    policy::UpstreamPolicy,
    search::{self, DEFAULT_SPECIFICITY_SEPARATORS, SearchNorms, map_no_category, strip_nothing},
    upstream::{self, ListedVersion, ListingStatus},
    version,
};

pub struct Nix;

impl crate::ecosystem::sealed::Sealed for Nix {}

impl EcosystemSpec for Nix {
    const ARCHIVE: ArchiveKind = ArchiveKind::TarGz;
    const LANGUAGE: Language = Language::Nix;
    const POLICY: UpstreamPolicy = UpstreamPolicy {
        max_requests_per_second: 10.0,
        retry_budget: 3,
        respect_retry_after: true,
    };

    type Manifest = manifest::ExtractedFacts;
    type Version = version::SemverVersion;

    fn parse_name(raw: &str) -> Option<name::StructuredName> {
        // FlakeHub: `org/project` or bare `project`; GitHub-slug chars on each side.
        // Case-folded (FlakeHub is case-insensitive). At most one `/`.
        if raw.is_empty()
            || raw.starts_with('/')
            || raw.ends_with('/')
            || raw.contains("..")
            || raw.matches('/').count() > 1
            || !raw.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        {
            return None;
        }
        let lower = raw.to_ascii_lowercase();
        let (namespace, name) = match lower.split_once('/') {
            Some((org, proj)) => (vec![SmolStr::from(org)], SmolStr::from(proj)),
            None => (vec![], SmolStr::from(lower.as_str())),
        };
        Some(name::StructuredName {
            ecosystem: Language::Nix,
            authority: None,
            namespace,
            name,
            major: None,
            original: raw.into(),
        })
    }

    fn render_canonical(n: &name::StructuredName) -> String {
        match n.namespace.first() {
            Some(org) => format!("{org}/{}", n.name),
            None => n.name.to_string(),
        }
    }

    fn endpoints() -> upstream::UpstreamEndpoints {
        upstream::UpstreamEndpoints {
            listing: "/f/{name}/releases",
            archive: "/f/{name}/{version}.tar.gz",
            listing_status: None,
        }
    }

    /// Parse the FlakeHub `/f/{name}/releases` JSON response.
    /// FlakeHub returns a JSON array of release objects, each with a `version`
    /// field. All versions are Listed (FlakeHub has no yank mechanism).
    fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
        use version::VersionGrammar;
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return vec![];
        };
        let Some(releases) = v.as_array() else {
            return vec![];
        };
        releases
            .iter()
            .filter_map(|entry| {
                let raw = entry["version"].as_str()?;
                let version = version::SemverVersion::parse(raw)?;
                Some(ListedVersion {
                    version,
                    status: ListingStatus::Listed,
                    raw: raw.into(),
                })
            })
            .collect()
    }

    fn manifest_candidates() -> &'static [ManifestCandidate] {
        static CANDIDATES: &[ManifestCandidate] = &[ManifestCandidate::new("flake.nix")];
        CANDIDATES
    }

    fn parse_manifest(_candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
        let text = std::str::from_utf8(bytes).ok()?;
        Some(parse_flake_nix(text))
    }

    fn search_norms() -> &'static SearchNorms {
        &NORMS
    }

    /// FlakeHub has no public download-count API. Returns `None`; Nix packages
    /// participate in downloads-driven ranking stages at the fairness floor.
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        None
    }
}

static NORMS: SearchNorms = SearchNorms {
    stopwords: &["flake", "flakes", "nix", "nixos"],
    specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
    strip_conventions: strip_nothing,
    normalize_query: search::normalize_identity,
    map_category: map_no_category,
    downloads_scale: None,
};

/// Parse a `flake.nix` by scanning for `description = "..."` — no Nix evaluation.
pub fn parse_flake_nix(text: &str) -> ExtractedFacts {
    let description = scan_nix_description(text);
    ExtractedFacts {
        description,
        keywords: vec![],
        categories: vec![],
        readme_hint: None,
        repository: None,
        documentation: false,
        license: None,
        has_license_file: false,
        dependencies: vec![],
    }
}

/// Hand-scan for `description = "..."` in Nix source. Handles basic escaped quotes.
/// Returns `None` if not found or empty.
fn scan_nix_description(text: &str) -> Option<String> {
    // Find `description` = `"` ... `"` — one pass.
    let needle = "description";
    let mut pos = 0;
    while let Some(offset) = text[pos..].find(needle) {
        let start = pos + offset + needle.len();
        // Skip whitespace then `=`.
        let rest = text[start..].trim_start();
        let rest = rest.strip_prefix('=')?;
        let rest = rest.trim_start();
        // Must start with `"`.
        let rest = rest.strip_prefix('"')?;
        // Collect until closing `"`, handling `\"` escapes.
        let mut s = String::new();
        let mut chars = rest.chars().peekable();
        let mut found = false;
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        match next {
                            '"' => s.push('"'),
                            '\\' => s.push('\\'),
                            'n' => s.push('\n'),
                            other => {
                                s.push('\\');
                                s.push(other);
                            }
                        }
                    }
                }
                '"' => {
                    found = true;
                    break;
                }
                c if c.is_control() => { /* strip control chars */ }
                c => s.push(c),
            }
        }
        if found && !s.is_empty() {
            return Some(s);
        }
        pos = start + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flake_nix_basic() {
        let text = r#"
{
  description = "A pure functional package manager";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs";
  outputs = { ... }: {};
}
"#;
        let facts = parse_flake_nix(text);
        assert_eq!(
            facts.description.as_deref(),
            Some("A pure functional package manager")
        );
    }

    #[test]
    fn parse_flake_nix_escaped_quotes() {
        let text = r#"{ description = "A tool for \"quoted\" strings"; }"#;
        let facts = parse_flake_nix(text);
        assert_eq!(
            facts.description.as_deref(),
            Some("A tool for \"quoted\" strings")
        );
    }

    #[test]
    fn parse_flake_nix_no_description() {
        let text = "{ outputs = { ... }: {}; }";
        let facts = parse_flake_nix(text);
        assert!(facts.description.is_none());
    }

    #[test]
    fn parse_flake_nix_empty_does_not_panic() {
        let facts = parse_flake_nix("");
        assert_eq!(facts, ExtractedFacts::default());
    }

    #[test]
    fn stopwords_nix_specific() {
        assert!(NORMS.is_stopword("nix"));
        assert!(NORMS.is_stopword("nixos"));
        assert!(NORMS.is_stopword("flake"));
        assert!(NORMS.is_stopword("flakes"));
    }

    #[test]
    fn stopwords_not_rust_words() {
        assert!(!NORMS.is_stopword("rust"));
        assert!(!NORMS.is_stopword("crate"));
    }

    #[test]
    fn parse_flakehub_releases() {
        let body = br#"[
			{"version": "1.0.0", "created_at": "2024-01-01"},
			{"version": "1.1.0", "created_at": "2024-02-01"},
			{"version": "2.0.0", "created_at": "2024-03-01"}
		]"#;
        let versions = Nix::parse_version_listing(body);
        assert_eq!(versions.len(), 3);
        assert!(versions.iter().all(|v| v.status == ListingStatus::Listed));
        assert_eq!(versions[0].raw, "1.0.0");
    }

    #[test]
    fn parse_flakehub_releases_empty() {
        assert!(Nix::parse_version_listing(b"[]").is_empty());
        assert!(Nix::parse_version_listing(b"not json").is_empty());
    }
}
