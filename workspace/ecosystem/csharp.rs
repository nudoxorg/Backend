//! NuGet (nuget.org).

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::{
    EcosystemSpec, Language,
    archive::ArchiveKind,
    manifest::{self, ExtractedFacts, ManifestCandidate},
    name,
    policy::UpstreamPolicy,
    search::{self, DEFAULT_SPECIFICITY_SEPARATORS, SearchNorms, map_no_category, strip_nothing},
    upstream::{self, ListedVersion, ListingStatus},
    version,
};

pub struct CSharp;

impl crate::sealed::Sealed for CSharp {}

impl EcosystemSpec for CSharp {
    /// `.nupkg` is a zip — the M1 fix; ingest must NOT treat it as tar.gz.
    const ARCHIVE: ArchiveKind = ArchiveKind::Zip;
    const LANGUAGE: Language = Language::CSharp;
    const POLICY: UpstreamPolicy = UpstreamPolicy {
        max_requests_per_second: 20.0,
        retry_budget: 3,
        respect_retry_after: true,
    };

    type Manifest = manifest::ExtractedFacts;
    type Version = version::NuGetVersion;

    fn parse_name(raw: &str) -> Option<name::StructuredName> {
        // NuGet IDs: ASCII alphanumeric + `.`/`-`/`_`, starts and ends alphanumeric.
        // Canonical: lowercase (nuget.org is case-insensitive by flat-container convention).
        // Structure: dot-separated; leading segments → namespace, last → name.
        if !raw.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !raw.ends_with(|c: char| c.is_ascii_alphanumeric())
            || !raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return None;
        }
        let lower = raw.to_ascii_lowercase();
        // Split on `.` to build namespace / name.
        let parts: Vec<smol_str::SmolStr> = lower.split('.').map(smol_str::SmolStr::from).collect();
        let (namespace, name) = if parts.len() > 1 {
            let name = parts[parts.len() - 1].clone();
            let ns = parts[..parts.len() - 1].to_vec();
            (ns, name)
        } else {
            (vec![], parts[0].clone())
        };
        Some(name::StructuredName {
            ecosystem: Language::CSharp,
            authority: None,
            namespace,
            name,
            major: None,
            original: raw.into(),
        })
    }

    /// Canonical = fully-lowercased original (matches `canonicalize_csharp`).
    fn render_canonical(n: &name::StructuredName) -> String {
        // Re-join namespace + name with `.` if there is a namespace.
        if n.namespace.is_empty() {
            n.name.to_string()
        } else {
            let mut s = n.namespace.join(".");
            s.push('.');
            s.push_str(&n.name);
            s
        }
    }

    fn endpoints() -> upstream::UpstreamEndpoints {
        upstream::UpstreamEndpoints {
            // Flat-container versions; the registration page supplies `listed`
            // via the two-request join in the IO layer (M3, P3).
            listing: "/v3-flatcontainer/{name_lower}/index.json",
            archive: "/v3-flatcontainer/{name_lower}/{version}/{name_lower}.{version}.nupkg",
            // NuGet registration page carries `listed` flag (M3).
            // Template: `{name_lower}` substituted by IO layer.
            listing_status: Some("/v3/registration5/{name_lower}/index.json"),
        }
    }

    /// Parse the NuGet flat-container index.json: `{"versions":[...]}`.
    /// All versions start as Listed; `merge_listing_status` flips unlisted ones
    /// to Withdrawn when the registration page is fetched (M3).
    fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return vec![];
        };
        v["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let raw = entry.as_str()?;
                let version = version::NuGetVersion::parse(raw)?;
                Some(ListedVersion {
                    version,
                    status: ListingStatus::Listed,
                    raw: raw.into(),
                })
            })
            .collect()
    }

    /// Merge NuGet registration-index listing status into the primary version
    /// list (M3). The registration index is an array of catalog pages; each page
    /// has an `items` array of version entries with a `catalogEntry.listed`
    /// boolean. Versions with `listed == false` are flipped to Withdrawn.
    fn merge_listing_status(
        mut versions: Vec<ListedVersion<Self::Version>>,
        status_body: &[u8],
    ) -> Vec<ListedVersion<Self::Version>> {
        let Ok(root) = serde_json::from_slice::<serde_json::Value>(status_body) else {
            return versions;
        };
        // Collect all unlisted version strings from the registration index.
        let mut unlisted: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(pages) = root["items"].as_array() {
            for page in pages {
                if let Some(entries) = page["items"].as_array() {
                    for entry in entries {
                        let cat = &entry["catalogEntry"];
                        let ver = cat["version"].as_str().unwrap_or("");
                        let listed = cat["listed"].as_bool().unwrap_or(true);
                        if !listed && !ver.is_empty() {
                            unlisted.insert(ver.to_ascii_lowercase());
                        }
                    }
                }
            }
        }
        for lv in &mut versions {
            if unlisted.contains(lv.raw.to_ascii_lowercase().as_str()) {
                lv.status = ListingStatus::Withdrawn { reason: None };
            }
        }
        versions
    }

    fn manifest_candidates() -> &'static [ManifestCandidate] {
        // `.nuspec` matched as suffix — the file is named `<id>.nuspec` inside the package.
        static CANDIDATES: &[ManifestCandidate] = &[ManifestCandidate::new(".nuspec")];
        CANDIDATES
    }

    fn parse_manifest(_candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
        parse_nuspec(bytes)
    }

    fn search_norms() -> &'static SearchNorms {
        &NORMS
    }

    /// NuGet download counts via the NuGet Search Service (US North-Central).
    /// Template: `https://azuresearch-usnc.nuget.org/query?q=packageid:{name}`
    /// Response JSON: `{"data": [{"totalDownloads": N, ...}], ...}`.
    /// Note: the `q=packageid:` query returns an exact-match hit for the package
    /// so `data[0].totalDownloads` is the all-time count. For ranking we use
    /// total-downloads as a proxy for monthly (no monthly endpoint available).
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        Some(upstream::DownloadEndpoint {
            url: "https://azuresearch-usnc.nuget.org/query?q=packageid:{name}",
        })
    }

    /// Parse the NuGet Search Service response: `data[0].totalDownloads`.
    fn parse_download_count(body: &[u8]) -> Option<u64> {
        let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
        v["data"].as_array()?.first()?["totalDownloads"].as_u64()
    }
}

static NORMS: SearchNorms = SearchNorms {
    stopwords: &["csharp", "dotnet", "net", "nuget"],
    specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
    strip_conventions: strip_nothing,
    normalize_query: search::normalize_identity,
    map_category: map_no_category,
    downloads_scale: Some(0.2),
};

/// Parse a `.nuspec` XML file into [`ExtractedFacts`].
pub fn parse_nuspec(bytes: &[u8]) -> Option<ExtractedFacts> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut description: Option<String> = None;
    let mut keywords: Vec<String> = vec![];
    let mut documentation = false;
    let mut license: Option<String> = None;
    let mut has_license_file = false;
    let mut repository: Option<String> = None;
    let mut project_url: Option<String> = None;
    let mut dependencies: Vec<String> = vec![];
    let mut current_tag = String::new();
    // Whether the current <license> element has type="expression".
    let mut license_is_expression = false;
    let mut in_metadata = false;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match local.as_str() {
                    "metadata" => in_metadata = true,
                    "dependency" if in_metadata => {
                        for attr in e.attributes().flatten() {
                            if std::str::from_utf8(attr.key.local_name().as_ref()).unwrap_or("")
                                == "id"
                                && let Ok(val) = attr.decode_and_unescape_value(reader.decoder())
                            {
                                let s = val.trim().to_owned();
                                if !s.is_empty() {
                                    dependencies.push(s);
                                }
                            }
                        }
                    }
                    "repository" if in_metadata => {
                        // Capture `url` attribute (the primary source); ignore other attributes.
                        for attr in e.attributes().flatten() {
                            let key = std::str::from_utf8(attr.key.local_name().as_ref())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            if key == "url"
                                && let Ok(val) = attr.decode_and_unescape_value(reader.decoder())
                            {
                                let s = val.trim().to_owned();
                                if !s.is_empty() && repository.is_none() {
                                    repository = Some(s);
                                }
                            }
                        }
                    }
                    "license" if in_metadata => {
                        // Detect type="expression" vs type="file".
                        let mut ty = String::new();
                        for attr in e.attributes().flatten() {
                            let key = std::str::from_utf8(attr.key.local_name().as_ref())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            if key == "type"
                                && let Ok(val) = attr.decode_and_unescape_value(reader.decoder())
                            {
                                ty = val.trim().to_ascii_lowercase();
                            }
                        }
                        match ty.as_str() {
                            "file" => {
                                has_license_file = true;
                                license_is_expression = false;
                            }
                            "expression" => {
                                license_is_expression = true;
                            }
                            _ => {
                                license_is_expression = false;
                            }
                        }
                    }
                    _ => {}
                }
                current_tag = local;
            }
            Ok(Event::End(ref e)) => {
                let local = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if local == "metadata" {
                    in_metadata = false;
                }
                current_tag.clear();
            }
            Ok(Event::Text(ref e)) if in_metadata => {
                if let Ok(text) = e.unescape() {
                    let text = text.trim().to_owned();
                    if text.is_empty() {
                        buf.clear();
                        continue;
                    }
                    match current_tag.as_str() {
                        "description" if description.is_none() => {
                            let s = text.chars().filter(|c| !c.is_control()).collect::<String>();
                            if !s.is_empty() {
                                description = Some(s);
                            }
                        }
                        "tags" if keywords.is_empty() => {
                            keywords = text
                                .split_ascii_whitespace()
                                .map(str::to_owned)
                                .take(50)
                                .collect();
                        }
                        "projecturl" => {
                            if !text.is_empty() {
                                documentation = true;
                                // Use <projectUrl> as repository fallback if no <repository url="..."> seen.
                                if project_url.is_none() {
                                    project_url = Some(text.clone());
                                }
                            }
                        }
                        // <licenseUrl> is deprecated — treat as has_license_file presence signal only.
                        "licenseurl" => {
                            if !text.is_empty() {
                                has_license_file = true;
                            }
                        }
                        // <license> text: capture as expression when type="expression".
                        "license"
                            if license_is_expression && license.is_none() && !text.is_empty() =>
                        {
                            license = Some(text.clone());
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }

    // Fall back to <projectUrl> for repository when no <repository url="..."> found.
    let final_repository = repository.or(project_url);

    Some(ExtractedFacts {
        description,
        keywords,
        categories: vec![],
        readme_hint: None,
        repository: final_repository,
        documentation,
        license,
        has_license_file,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nuspec_basic() {
        let xml = br#"<?xml version="1.0"?>
<package>
  <metadata>
    <id>Newtonsoft.Json</id>
    <version>13.0.3</version>
    <description>Json.NET is a popular JSON framework for .NET</description>
    <tags>json serialization deserialize</tags>
    <projectUrl>https://www.newtonsoft.com/json</projectUrl>
    <licenseUrl>https://licenses.nuget.org/MIT</licenseUrl>
    <repository type="git" url="https://github.com/JamesNK/Newtonsoft.Json" />
    <dependencies>
      <group targetFramework="net6.0">
        <dependency id="Microsoft.CSharp" version="4.7.0" />
      </group>
      <group targetFramework="net20">
        <dependency id="System.Text.Json" version="4.7.0" />
      </group>
    </dependencies>
  </metadata>
</package>"#;
        let facts = parse_nuspec(xml).expect("valid nuspec");
        assert!(
            facts
                .description
                .as_deref()
                .unwrap_or("")
                .contains("Json.NET")
        );
        assert!(facts.keywords.contains(&"json".to_owned()));
        assert!(facts.documentation);
        // <licenseUrl> → has_license_file only (deprecated).
        assert!(facts.has_license_file, "licenseUrl sets has_license_file");
        assert!(
            facts.license.is_none(),
            "licenseUrl does not populate license expression"
        );
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/JamesNK/Newtonsoft.Json"),
            "repository url attribute from <repository>"
        );
        assert!(facts.dependencies.contains(&"Microsoft.CSharp".to_owned()));
        assert!(facts.dependencies.contains(&"System.Text.Json".to_owned()));
    }

    #[test]
    fn parse_nuspec_license_expression() {
        let xml = br#"<?xml version="1.0"?>
<package><metadata>
  <id>MyPkg</id>
  <license type="expression">MIT OR Apache-2.0</license>
  <repository type="git" url="https://github.com/acme/mypkg" />
</metadata></package>"#;
        let facts = parse_nuspec(xml).expect("valid nuspec");
        assert_eq!(facts.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert!(!facts.has_license_file);
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/acme/mypkg")
        );
    }

    #[test]
    fn parse_nuspec_license_file_type() {
        let xml = br#"<?xml version="1.0"?>
<package><metadata>
  <license type="file">LICENSE.txt</license>
</metadata></package>"#;
        let facts = parse_nuspec(xml).expect("valid nuspec");
        assert!(facts.license.is_none(), "file-type license: no expression");
        assert!(facts.has_license_file);
    }

    #[test]
    fn parse_nuspec_project_url_fallback_repository() {
        // No <repository> element → <projectUrl> is the repository fallback.
        let xml = br#"<?xml version="1.0"?>
<package><metadata>
  <projectUrl>https://github.com/acme/norepoelement</projectUrl>
</metadata></package>"#;
        let facts = parse_nuspec(xml).expect("valid nuspec");
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/acme/norepoelement")
        );
    }

    #[test]
    fn parse_nuspec_xml_entities() {
        let xml = br#"<?xml version="1.0"?>
<package><metadata>
  <description>A &amp; B &lt;test&gt;</description>
  <tags>foo bar</tags>
</metadata></package>"#;
        let facts = parse_nuspec(xml).expect("parses");
        assert!(facts.description.as_deref().unwrap_or("").contains("A & B"));
    }

    #[test]
    fn parse_nuspec_malformed_does_not_panic() {
        let xml = b"not xml at all <<<<";
        let _ = parse_nuspec(xml);
    }

    #[test]
    fn parse_nuspec_truncated_does_not_panic() {
        let xml = b"<?xml version=\"1.0\"?><package><metadata><description>hello";
        let _ = parse_nuspec(xml);
    }

    #[test]
    fn stopwords_nuget_specific() {
        assert!(NORMS.is_stopword("dotnet"));
        assert!(NORMS.is_stopword("nuget"));
        assert!(NORMS.is_stopword("net"));
        assert!(NORMS.is_stopword("csharp"));
    }

    #[test]
    fn stopwords_not_rust_words() {
        assert!(!NORMS.is_stopword("rust"));
        assert!(!NORMS.is_stopword("crate"));
    }

    #[test]
    fn manifest_candidates_nuspec_suffix() {
        let candidates = CSharp::manifest_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path_suffix, ".nuspec");
    }

    #[test]
    fn parse_nuget_flat_container() {
        let body = br#"{"versions": ["1.0.0", "1.1.0", "2.0.0-beta"]}"#;
        let versions = CSharp::parse_version_listing(body);
        assert_eq!(versions.len(), 3);
        assert!(versions.iter().all(|v| v.status.is_listed()));
        assert_eq!(versions[0].raw, "1.0.0");
    }

    #[test]
    fn merge_listing_status_flips_unlisted() {
        let index_body = br#"{
			"items": [{
				"items": [
					{"catalogEntry": {"version": "1.0.0", "listed": true}},
					{"catalogEntry": {"version": "1.1.0", "listed": false}},
					{"catalogEntry": {"version": "2.0.0-beta", "listed": true}}
				]
			}]
		}"#;
        let primary =
            CSharp::parse_version_listing(br#"{"versions":["1.0.0","1.1.0","2.0.0-beta"]}"#);
        let merged = CSharp::merge_listing_status(primary, index_body);
        let v10 = merged.iter().find(|v| v.raw == "1.0.0").unwrap();
        assert!(v10.status.is_listed());
        let v11 = merged.iter().find(|v| v.raw == "1.1.0").unwrap();
        assert!(!v11.status.is_listed());
        let v20 = merged.iter().find(|v| v.raw == "2.0.0-beta").unwrap();
        assert!(v20.status.is_listed());
    }

    #[test]
    fn merge_listing_status_invalid_body_is_identity() {
        let primary = CSharp::parse_version_listing(br#"{"versions":["1.0.0"]}"#);
        let merged = CSharp::merge_listing_status(primary.clone(), b"not json");
        assert_eq!(merged.len(), primary.len());
        assert!(merged[0].status.is_listed());
    }

    // ── parse_download_count (NuGet Search Service) ───────────────────────────

    #[test]
    fn parse_download_count_happy_path() {
        let body = br#"{"data":[{"id":"Newtonsoft.Json","totalDownloads":1234567890}]}"#;
        assert_eq!(CSharp::parse_download_count(body), Some(1_234_567_890));
    }

    #[test]
    fn parse_download_count_zero_is_some() {
        let body = br#"{"data":[{"totalDownloads":0}]}"#;
        assert_eq!(CSharp::parse_download_count(body), Some(0));
    }

    #[test]
    fn parse_download_count_malformed_or_missing() {
        assert_eq!(CSharp::parse_download_count(b"not json"), None);
        assert_eq!(CSharp::parse_download_count(b"{}"), None);
        assert_eq!(CSharp::parse_download_count(br#"{"data":[]}"#), None);
        assert_eq!(CSharp::parse_download_count(br#"{"data":[{}]}"#), None);
        assert_eq!(
            CSharp::parse_download_count(br#"{"data":[{"totalDownloads":null}]}"#),
            None
        );
        assert_eq!(
            CSharp::parse_download_count(br#"{"data":[{"totalDownloads":"nope"}]}"#),
            None
        );
        assert_eq!(CSharp::parse_download_count(b""), None);
    }

    #[test]
    fn download_source_is_nuget_search() {
        let ep = CSharp::download_source().expect("CSharp has a download source");
        assert!(ep.url.contains("nuget.org") || ep.url.contains("azuresearch"));
        assert!(ep.url.contains("{name}"));
    }
}
