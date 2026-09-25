//! Maven Central.

use smol_str::SmolStr;

use quick_xml::{Reader, events::Event};

use crate::ecosystem::{
    EcosystemSpec, Language,
    archive::ArchiveKind,
    manifest::{self, ExtractedFacts, ManifestCandidate},
    name,
    policy::UpstreamPolicy,
    search::{
        DEFAULT_SPECIFICITY_SEPARATORS, NormalizedQuery, SearchNorms, map_no_category,
        strip_nothing,
    },
    upstream::{self, ListedVersion, ListingStatus},
    version,
};

pub struct Java;

impl crate::ecosystem::sealed::Sealed for Java {}

impl EcosystemSpec for Java {
    /// Jars are zips; sources jar preferred, binary jar fallback (P3).
    const ARCHIVE: ArchiveKind = ArchiveKind::Zip;
    const LANGUAGE: Language = Language::Java;
    const POLICY: UpstreamPolicy = UpstreamPolicy {
        max_requests_per_second: 10.0,
        retry_budget: 3,
        respect_retry_after: true,
    };

    type Manifest = manifest::ExtractedFacts;
    type Version = version::MavenVersion;

    fn parse_name(raw: &str) -> Option<name::StructuredName> {
        // Two forms accepted:
        //   1. bare `artifactId`  — legacy form (what `canonicalize_maven_artifact`
        //      accepted)
        //   2. `groupId:artifactId` — new capability; `:` is the NEW separator
        //
        // Both sides: ASCII alphanumeric + `-`/`_`/`.`, starts and ends alphanumeric.
        // Canonical: lowercase. For colon form, canonical = `group:artifact`.
        let component_ok = |s: &str| {
            !s.is_empty()
                && s.starts_with(|c: char| c.is_ascii_alphanumeric())
                && s.ends_with(|c: char| c.is_ascii_alphanumeric())
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };

        match raw.split_once(':') {
            Some((group, artifact)) if component_ok(group) && component_ok(artifact) => {
                // `groupId:artifactId` form
                let group_lower = group.to_ascii_lowercase();
                let artifact_lower = artifact.to_ascii_lowercase();
                // Group dots become namespace segments.
                let namespace: Vec<SmolStr> = group_lower.split('.').map(SmolStr::from).collect();
                Some(name::StructuredName {
                    ecosystem: Language::Java,
                    authority: None,
                    namespace,
                    name: artifact_lower.into(),
                    major: None,
                    original: raw.into(),
                })
            }
            None if component_ok(raw) => {
                // Bare artifactId — legacy form; no namespace.
                Some(name::StructuredName {
                    ecosystem: Language::Java,
                    authority: None,
                    namespace: vec![],
                    name: raw.to_ascii_lowercase().into(),
                    major: None,
                    original: raw.into(),
                })
            }
            // `:` present but malformed — fall through to bare check, which will
            // also fail because `:` is not in the allowed set.
            Some(_) | None => None,
        }
    }

    /// Canonical for bare form: lowercased artifactId.
    /// Canonical for colon form: `lowercased-group:lowercased-artifact`.
    fn render_canonical(n: &name::StructuredName) -> String {
        if n.namespace.is_empty() {
            n.name.to_string()
        } else {
            format!("{}:{}", n.namespace.join("."), n.name)
        }
    }

    fn endpoints() -> upstream::UpstreamEndpoints {
        upstream::UpstreamEndpoints {
            // `{group_path}` = groupId with dots as slashes. Maven metadata is
            // XML — parsed by the Maven impl (P3), not serde_json.
            listing: "/{group_path}/{artifact}/maven-metadata.xml",
            // Sources jar preferred (carries Javadoc + human-readable source);
            // binary jar fallback handled in the IO layer.
            archive: "/{group_path}/{artifact}/{version}/{artifact}-{version}-sources.jar",
            listing_status: None,
        }
    }

    /// Parse Maven Central `maven-metadata.xml` via quick-xml event reader.
    /// Returns one Listed entry per `<version>` element; Maven has no
    /// unlist / yank mechanism at this endpoint.
    fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
        let mut reader = quick_xml::Reader::from_reader(body);
        reader.config_mut().trim_text(true);
        let mut versions = vec![];
        let mut in_version_tag = false;
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    in_version_tag = e.local_name().as_ref() == b"version";
                }
                Ok(quick_xml::events::Event::Text(ref e)) if in_version_tag => {
                    if let Ok(raw_str) = e.unescape() {
                        let raw = raw_str.trim();
                        if let Some(ver) = version::MavenVersion::parse(raw) {
                            versions.push(ListedVersion {
                                version: ver,
                                status: ListingStatus::Listed,
                                raw: raw.into(),
                            });
                        }
                    }
                    in_version_tag = false;
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    in_version_tag = false;
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        versions
    }

    fn manifest_candidates() -> &'static [ManifestCandidate] {
        static CANDIDATES: &[ManifestCandidate] = &[ManifestCandidate::new("pom.xml")];
        CANDIDATES
    }

    fn parse_manifest(_candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
        parse_pom_xml(bytes)
    }

    fn search_norms() -> &'static SearchNorms {
        &NORMS
    }

    /// Maven Central has **no public free per-artifact download-count API**.
    /// (Sonatype OSS Index / commercial stats require auth; search.maven.org
    /// does not expose download numbers.) Returns `None`; ranking uses
    /// dependents / fairness floor.
    ///
    /// A pure [`parse_download_count`] accepts an offline fixture shape
    /// `{"downloads": N}` (or `{"downloadCount": N}`) so corpus jobs can inject
    /// counts later without changing the trait surface.
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        None
    }

    /// Parse a documented offline JSON shape for Maven download counts.
    /// Malformed / missing → `None` (never panics). Not used at ingest while
    /// [`download_source`] is `None`.
    fn parse_download_count(body: &[u8]) -> Option<u64> {
        parse_maven_download_count(body)
    }
}

/// Pure parser for a best-effort Maven download-count JSON fixture.
///
/// Documented shape (no live free upstream today):
/// ```json
/// { "downloads": 12345 }
/// ```
/// Also accepts camelCase `downloadCount` (common in Sonatype-style payloads).
/// Explicit `0` is `Some(0)`.
pub fn parse_maven_download_count(body: &[u8]) -> Option<u64> {
    let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    v["downloads"]
        .as_u64()
        .or_else(|| v["downloadCount"].as_u64())
        .or_else(|| v["download_count"].as_u64())
}

static NORMS: SearchNorms = SearchNorms {
    stopwords: &["java", "jvm", "maven"],
    specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
    strip_conventions: strip_nothing,
    normalize_query: normalize_maven_query,
    map_category: map_no_category,
    downloads_scale: None,
};

/// Maven: `group:artifact` → namespace=Some(group), terms=artifact.
fn normalize_maven_query(terms: &str) -> NormalizedQuery {
    if let Some((group, artifact)) = terms.split_once(':') {
        NormalizedQuery {
            terms: artifact.to_owned(),
            namespace: Some(group.to_owned()),
        }
    } else {
        NormalizedQuery {
            terms: terms.to_owned(),
            namespace: None,
        }
    }
}

/// Parse a `pom.xml` file into [`ExtractedFacts`].
pub fn parse_pom_xml(bytes: &[u8]) -> Option<ExtractedFacts> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut description: Option<String> = None;
    let mut documentation = false;
    let mut repository: Option<String> = None;
    let mut license: Option<String> = None;
    let mut dependencies = crate::record::RuntimeEdgeFold::keep_first();
    let mut dep_version: Option<String> = None;
    let mut dep_optional = false;

    // Track current path stack to avoid <parent> false positives.
    let mut path: Vec<String> = vec![];
    // Pending group/artifact within <dependency>.
    let mut dep_group: Option<String> = None;
    let mut dep_artifact: Option<String> = None;
    let mut current_tag = String::new();

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                path.push(local.clone());
                current_tag = local;
                // Reset dep fields when entering a new <dependency>.
                if current_tag == "dependency" {
                    dep_group = None;
                    dep_artifact = None;
                    dep_version = None;
                    dep_optional = false;
                }
            }
            Ok(Event::End(ref e)) => {
                let local = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if local == "dependency"
                    && let (Some(g), Some(a)) = (dep_group.take(), dep_artifact.take())
                {
                    let version = dep_version.take();
                    dependencies.observe(format!("{g}:{a}"), version.as_deref(), dep_optional);
                    dep_optional = false;
                }
                path.pop();
                current_tag = path.last().cloned().unwrap_or_default();
            }
            Ok(Event::Text(ref e)) => {
                if let Ok(text) = e.unescape() {
                    let text = text.trim().to_owned();
                    if text.is_empty() {
                        buf.clear();
                        continue;
                    }
                    // Only read <description> at project level (not inside parent).
                    let depth = path.len();
                    match current_tag.as_str() {
                        "description" if depth == 2 && description.is_none() => {
                            let s = text.chars().filter(|c| !c.is_control()).collect::<String>();
                            if !s.is_empty() {
                                description = Some(s);
                            }
                        }
                        "url" if depth == 2 => {
                            if !text.is_empty() {
                                documentation = true;
                            }
                        }
                        // <scm><url> is the canonical repository URL; prefer it over
                        // <connection>/<developerConnection> (which carry protocol prefixes).
                        "url" if path.iter().any(|p| p == "scm") && repository.is_none() => {
                            if !text.is_empty() {
                                repository = Some(text.clone());
                            }
                        }
                        "connection" | "developerconnection"
                            if path.iter().any(|p| p == "scm") && repository.is_none() =>
                        {
                            if !text.is_empty() {
                                repository = Some(text.clone());
                            }
                        }
                        // First <licenses><license><name> is the license expression.
                        "name"
                            if license.is_none()
                                && path.iter().any(|p| p == "license" || p == "licenses") =>
                        {
                            if !text.is_empty() {
                                license = Some(text.clone());
                            }
                        }
                        "groupid" if path.iter().any(|p| p == "dependency") => {
                            dep_group = Some(text);
                        }
                        "artifactid" if path.iter().any(|p| p == "dependency") => {
                            dep_artifact = Some(text);
                        }
                        "version" if path.iter().any(|p| p == "dependency") => {
                            dep_version = Some(text);
                        }
                        "optional" if path.iter().any(|p| p == "dependency") => {
                            dep_optional = text.eq_ignore_ascii_case("true");
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

    Some(ExtractedFacts {
        description,
        keywords: vec![],
        categories: vec![],
        readme_hint: None,
        repository,
        documentation,
        license,
        has_license_file: false,
        dependencies: dependencies.finish(),
    })
}

/// Direct `group:artifact` names from a POM, sorted and de-duplicated.
///
/// A dependency counts when its parent element is `dependencies` and no
/// ancestor is `dependencyManagement`, `plugin`, or `pluginManagement`.
/// `test`, `provided`, and `system` scopes are omitted. A missing group or
/// artifact, or a document that is not XML, yields an empty list.
#[must_use]
pub fn pom_dependency_names(bytes: &[u8]) -> Vec<String> {
    pom_dependency_edges(bytes)
        .into_iter()
        .map(|edge| edge.name.to_string())
        .collect()
}

/// Direct POM dependencies, with `<version>` as the requirement.
///
/// The same inclusion rules as [`pom_dependency_names`]. A property
/// placeholder is kept as written. `<optional>true</optional>` sets the
/// optional bit. A repeated coordinate keeps the first row.
#[must_use]
pub fn pom_dependency_edges(bytes: &[u8]) -> Vec<crate::record::DepEdge> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut path: Vec<String> = Vec::new();
    let mut tag = String::new();
    let mut group: Option<String> = None;
    let mut artifact: Option<String> = None;
    let mut scope: Option<String> = None;
    let mut version: Option<String> = None;
    let mut optional = false;
    let mut fold = crate::record::RuntimeEdgeFold::keep_first();
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref element)) => {
                let local = local_xml_name(element.local_name().as_ref());
                path.push(local.clone());
                tag = local;
                if tag == "dependency" {
                    group = None;
                    artifact = None;
                    scope = None;
                    version = None;
                    optional = false;
                }
            }
            Ok(Event::End(ref element)) => {
                let local = local_xml_name(element.local_name().as_ref());
                if local == "dependency"
                    && let Some(name) =
                        direct_pom_dep(&path, group.take(), artifact.take(), scope.take())
                {
                    let requirement = version.take().filter(|text| !text.is_empty());
                    fold.observe(name, requirement.as_deref(), optional);
                    optional = false;
                }
                path.pop();
                tag = path.last().cloned().unwrap_or_default();
            }
            Ok(Event::Text(ref element)) => {
                if let Ok(text) = element.unescape() {
                    let text = text.trim();
                    if text.is_empty() {
                        buf.clear();
                        continue;
                    }
                    if path.iter().any(|part| part == "dependency") {
                        match tag.as_str() {
                            "groupid" => group = Some(text.to_owned()),
                            "artifactid" => artifact = Some(text.to_owned()),
                            "scope" => scope = Some(text.to_ascii_lowercase()),
                            "version" => version = Some(text.to_owned()),
                            "optional" => optional = text.eq_ignore_ascii_case("true"),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Vec::new(),
            _ => {}
        }
        buf.clear();
    }
    fold.finish()
}

fn local_xml_name(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn direct_pom_dep(
    path: &[String],
    group: Option<String>,
    artifact: Option<String>,
    scope: Option<String>,
) -> Option<String> {
    let parent = path.get(path.len().saturating_sub(2))?;
    if parent != "dependencies" {
        return None;
    }
    if path.iter().any(|part| {
        matches!(
            part.as_str(),
            "dependencymanagement" | "plugin" | "pluginmanagement"
        )
    }) {
        return None;
    }
    if matches!(scope.as_deref(), Some("test" | "provided" | "system")) {
        return None;
    }
    let group = group.filter(|name| !name.is_empty())?;
    let artifact = artifact.filter(|name| !name.is_empty())?;
    Some(format!("{group}:{artifact}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pom_xml_basic() {
        let xml = br#"<?xml version="1.0"?>
<project>
  <groupId>org.springframework</groupId>
  <artifactId>spring-core</artifactId>
  <version>5.3.0</version>
  <description>Spring Framework Core</description>
  <url>https://spring.io/projects/spring-framework</url>
  <licenses><license><name>Apache 2.0</name></license></licenses>
  <scm><connection>scm:git:https://github.com/spring-projects/spring-framework</connection></scm>
  <dependencies>
    <dependency>
      <groupId>io.micrometer</groupId>
      <artifactId>micrometer-observation</artifactId>
      <version>1.12.0</version>
      <optional>true</optional>
    </dependency>
    <dependency>
      <groupId>com.google.code.findbugs</groupId>
      <artifactId>jsr305</artifactId>
    </dependency>
  </dependencies>
</project>"#;
        let facts = parse_pom_xml(xml).expect("valid pom.xml");
        assert_eq!(facts.description.as_deref(), Some("Spring Framework Core"));
        assert!(facts.documentation);
        assert_eq!(
            facts.license.as_deref(),
            Some("Apache 2.0"),
            "license name from <licenses><license><name>"
        );
        assert!(!facts.has_license_file);
        assert_eq!(
            facts.repository.as_deref(),
            Some("scm:git:https://github.com/spring-projects/spring-framework"),
            "repository from <scm><connection>"
        );
        let micrometer = facts
            .dependencies
            .iter()
            .find(|edge| edge.name == "io.micrometer:micrometer-observation")
            .expect("micrometer");
        assert_eq!(micrometer.requirement.as_deref(), Some("1.12.0"));
        assert!(micrometer.optional);
        assert!(
            facts
                .dependency_names()
                .contains(&"com.google.code.findbugs:jsr305".to_owned())
        );
    }

    #[test]
    fn parse_pom_xml_scm_url_preferred() {
        let xml = br#"<?xml version="1.0"?>
<project>
  <scm>
    <url>https://github.com/example/lib</url>
    <connection>scm:git:git://github.com/example/lib.git</connection>
  </scm>
  <licenses><license><name>MIT License</name></license></licenses>
</project>"#;
        let facts = parse_pom_xml(xml).expect("valid pom.xml");
        // <url> within <scm> must win over <connection>.
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/example/lib")
        );
        assert_eq!(facts.license.as_deref(), Some("MIT License"));
    }

    #[test]
    fn parse_pom_xml_malformed_does_not_panic() {
        let xml = b"<broken xml <<< ";
        let _ = parse_pom_xml(xml);
    }

    #[test]
    fn pom_dependency_names_keep_direct_compile_edges_only() {
        let xml = br#"<?xml version="1.0"?>
<project>
  <dependencies>
    <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId><version>2.0.9</version><optional>true</optional></dependency>
    <dependency><groupId>junit</groupId><artifactId>junit</artifactId><scope>test</scope></dependency>
    <dependency><groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>
    <dependency><groupId></groupId><artifactId>blank</artifactId></dependency>
  </dependencies>
  <dependencyManagement>
    <dependencies>
      <dependency><groupId>com.managed</groupId><artifactId>bom</artifactId></dependency>
    </dependencies>
  </dependencyManagement>
  <build><plugins><plugin>
    <dependencies>
      <dependency><groupId>com.plugin</groupId><artifactId>tool</artifactId></dependency>
    </dependencies>
  </plugin></plugins></build>
</project>"#;
        assert_eq!(pom_dependency_names(xml), vec![
            "org.slf4j:slf4j-api".to_owned()
        ]);
        let edges = pom_dependency_edges(xml);
        assert_eq!(edges[0].requirement.as_deref(), Some("2.0.9"));
        assert!(edges[0].optional);
        assert!(pom_dependency_names(b"<broken").is_empty());
    }

    #[test]
    fn normalize_maven_query_with_colon() {
        let q = normalize_maven_query("org.springframework:spring-core");
        assert_eq!(q.namespace.as_deref(), Some("org.springframework"));
        assert_eq!(q.terms, "spring-core");
    }

    #[test]
    fn normalize_maven_query_bare() {
        let q = normalize_maven_query("spring-core");
        assert_eq!(q.namespace, None);
        assert_eq!(q.terms, "spring-core");
    }

    #[test]
    fn stopwords_java_specific() {
        assert!(NORMS.is_stopword("java"));
        assert!(NORMS.is_stopword("jvm"));
        assert!(NORMS.is_stopword("maven"));
    }

    #[test]
    fn stopwords_not_npm_words() {
        assert!(!NORMS.is_stopword("node"));
        assert!(!NORMS.is_stopword("npm"));
    }

    #[test]
    fn parse_maven_metadata_xml() {
        let xml = br#"<?xml version="1.0"?>
<metadata>
  <groupId>org.springframework</groupId>
  <artifactId>spring-core</artifactId>
  <versioning>
    <latest>5.3.27</latest>
    <release>5.3.27</release>
    <versions>
      <version>5.3.0</version>
      <version>5.3.26</version>
      <version>5.3.27</version>
    </versions>
  </versioning>
</metadata>"#;
        let versions = Java::parse_version_listing(xml);
        assert_eq!(versions.len(), 3);
        assert!(versions.iter().all(|v| v.status == ListingStatus::Listed));
        assert_eq!(versions[0].raw, "5.3.0");
        assert_eq!(versions[2].raw, "5.3.27");
    }

    #[test]
    fn parse_maven_metadata_empty_or_malformed() {
        assert!(Java::parse_version_listing(b"<metadata/>").is_empty());
        assert!(Java::parse_version_listing(b"not xml").is_empty());
    }

    // ── parse_download_count (no live free source; pure fixture parse) ────────

    #[test]
    fn download_source_is_none() {
        assert!(Java::download_source().is_none());
    }

    #[test]
    fn parse_download_count_happy_path() {
        assert_eq!(
            Java::parse_download_count(br#"{"downloads":999}"#),
            Some(999)
        );
        assert_eq!(
            Java::parse_download_count(br#"{"downloadCount":42}"#),
            Some(42)
        );
        assert_eq!(
            parse_maven_download_count(br#"{"download_count":7}"#),
            Some(7)
        );
    }

    #[test]
    fn parse_download_count_zero_is_some() {
        assert_eq!(Java::parse_download_count(br#"{"downloads":0}"#), Some(0));
    }

    #[test]
    fn parse_download_count_malformed_or_missing() {
        assert_eq!(Java::parse_download_count(b"not json"), None);
        assert_eq!(Java::parse_download_count(b"{}"), None);
        assert_eq!(Java::parse_download_count(br#"{"downloads":null}"#), None);
        assert_eq!(Java::parse_download_count(br#"{"downloads":"nope"}"#), None);
        assert_eq!(Java::parse_download_count(b""), None);
    }
}
