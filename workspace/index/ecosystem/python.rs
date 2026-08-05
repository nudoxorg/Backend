//! PyPI.

use crate::ecosystem::{
    EcosystemSpec, Language,
    archive::ArchiveKind,
    manifest::{self, ExtractedFacts, ManifestCandidate},
    name,
    policy::UpstreamPolicy,
    search::{self, DEFAULT_SPECIFICITY_SEPARATORS, SearchNorms},
    upstream::{self, ListedVersion, ListingStatus},
    version,
};

pub struct Python;

impl crate::ecosystem::sealed::Sealed for Python {}

impl EcosystemSpec for Python {
    const ARCHIVE: ArchiveKind = ArchiveKind::TarGz;
    const LANGUAGE: Language = Language::Python;
    const POLICY: UpstreamPolicy = UpstreamPolicy {
        max_requests_per_second: 10.0,
        retry_budget: 3,
        respect_retry_after: true,
    };

    type Manifest = manifest::ExtractedFacts;
    type Version = version::Pep440Version;

    fn parse_name(raw: &str) -> Option<name::StructuredName> {
        // PEP 508: starts and ends alphanumeric; interior may add `-`/`_`/`.`.
        // PEP 503 canonical: lowercase, runs of separators collapsed to `-`.
        if !raw.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !raw.ends_with(|c: char| c.is_ascii_alphanumeric())
            || !raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return None;
        }
        let canonical: String = raw
            .to_ascii_lowercase()
            .split(['-', '_', '.'])
            .filter(|run| !run.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        Some(name::StructuredName {
            ecosystem: Language::Python,
            authority: None,
            namespace: vec![],
            name: canonical.into(),
            major: None,
            original: raw.into(),
        })
    }

    fn render_canonical(n: &name::StructuredName) -> String {
        n.name.to_string()
    }

    fn endpoints() -> upstream::UpstreamEndpoints {
        upstream::UpstreamEndpoints {
            // The sdist URL comes from the same JSON (two-step acquisition kept
            // in the IO layer); the archive template is the metadata endpoint.
            listing: "/pypi/{name}/json",
            archive: "/pypi/{name}/{version}/json",
            listing_status: None,
        }
    }

    /// Parse the PyPI `/pypi/{name}/json` response.
    /// `releases` object: keys are version strings, values are arrays of file
    /// objects. A release is Withdrawn iff ALL its file objects have
    /// `yanked: true` (empty file list = Listed). M7 fix.
    /// `yanked_reason` from the first yanked file is used as the reason.
    fn parse_version_listing(body: &[u8]) -> Vec<ListedVersion<Self::Version>> {
        use version::VersionGrammar;
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return vec![];
        };
        let Some(releases) = v["releases"].as_object() else {
            return vec![];
        };
        releases
            .iter()
            .filter_map(|(raw, files)| {
                let version = version::Pep440Version::parse(raw)?;
                let files_arr = files.as_array();
                let status = match files_arr {
                    Some(arr) if !arr.is_empty() => {
                        // Withdrawn iff every file is yanked
                        let all_yanked = arr.iter().all(|f| f["yanked"].as_bool().unwrap_or(false));
                        if all_yanked {
                            let reason = arr
                                .iter()
                                .find_map(|f| f["yanked_reason"].as_str())
                                .map(smol_str::SmolStr::from);
                            ListingStatus::Withdrawn { reason }
                        } else {
                            ListingStatus::Listed
                        }
                    }
                    // Empty file list or null: treat as Listed (data absent, not yanked)
                    _ => ListingStatus::Listed,
                };
                Some(ListedVersion {
                    version,
                    status,
                    raw: raw.as_str().into(),
                })
            })
            .collect()
    }

    fn manifest_candidates() -> &'static [ManifestCandidate] {
        static CANDIDATES: &[ManifestCandidate] = &[
            ManifestCandidate::new("pyproject.toml"),
            ManifestCandidate::new("PKG-INFO"),
            ManifestCandidate::new("setup.cfg"),
        ];
        CANDIDATES
    }

    fn parse_manifest(candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
        let text = std::str::from_utf8(bytes).ok()?;
        match candidate.path_suffix {
            "pyproject.toml" => Some(parse_pyproject_toml(text)),
            "PKG-INFO" => Some(parse_pkg_info(text)),
            "setup.cfg" => Some(parse_setup_cfg(text)),
            _ => None,
        }
    }

    fn search_norms() -> &'static SearchNorms {
        &NORMS
    }

    /// Monthly downloads via the free, no-auth [pypistats](https://pypistats.org)
    /// recent-downloads API.
    ///
    /// Template: `https://pypistats.org/api/packages/{name}/recent`
    /// Documented JSON shape (see [`parse_pypistats_recent`]):
    /// ```json
    /// { "data": { "last_day": N, "last_week": N, "last_month": N }, "package": "..." }
    /// ```
    /// Prefer `data.last_month` as the monthly count. PyPI's own JSON API no
    /// longer exposes download stats; BigQuery remains an offline alternative.
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        Some(upstream::DownloadEndpoint {
            url: "https://pypistats.org/api/packages/{name}/recent",
        })
    }

    /// Parse a pypistats-shaped recent-downloads body into a monthly count.
    /// Malformed / missing → `None` (never panics). Explicit `0` is `Some(0)`.
    fn parse_download_count(body: &[u8]) -> Option<u64> {
        parse_pypistats_recent(body)
    }
}

/// Parse pypistats.org `/api/packages/{name}/recent` JSON.
///
/// Documented shape:
/// ```json
/// {
///   "data": { "last_day": 1, "last_week": 10, "last_month": 100 },
///   "package": "requests",
///   "type": "recent_downloads"
/// }
/// ```
/// Returns `data.last_month` when present and numeric. Falls back to
/// `last_week` then `last_day` if month is absent (best-effort).
pub fn parse_pypistats_recent(body: &[u8]) -> Option<u64> {
    let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let data = &v["data"];
    data["last_month"]
        .as_u64()
        .or_else(|| data["last_week"].as_u64())
        .or_else(|| data["last_day"].as_u64())
}

static NORMS: SearchNorms = SearchNorms {
    stopwords: &["py", "pypi", "python"],
    specificity_separators: DEFAULT_SPECIFICITY_SEPARATORS,
    strip_conventions: strip_python_conventions,
    normalize_query: search::normalize_identity,
    map_category: map_pypi_category,
    // pypistats last_month is true monthly; scale ≈ crates.io recent (≈90d)
    // reference. Corpus-percentile calibration will supersede this.
    downloads_scale: Some(0.1),
};

/// Strip `py`/`python` prefix and `-python`/`-py`/`py` suffix, then trim separators.
pub fn strip_python_conventions(name: &str) -> &str {
    let mut s = name;
    // Prefix strip.
    for prefix in &["python-", "python_", "py-", "py_", "python", "py"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    // Suffix strip.
    for suffix in &["-python", "_python", "-py", "_py", "python", "py"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest;
            break;
        }
    }
    s.trim_matches(|c| c == '-' || c == '_')
}

/// Parse a `pyproject.toml` (PEP 517/518 `[project]` table).
pub fn parse_pyproject_toml(text: &str) -> ExtractedFacts {
    let Ok(value) = text.parse::<toml::Value>() else {
        return ExtractedFacts::default();
    };
    let Some(project) = value.get("project").and_then(toml::Value::as_table) else {
        return ExtractedFacts::default();
    };

    let description = project
        .get("description")
        .and_then(toml::Value::as_str)
        .map(|s| s.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|s| !s.is_empty());

    let keywords: Vec<String> = project
        .get("keywords")
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .take(50)
                .collect()
        })
        .unwrap_or_default();

    let classifiers: Vec<String> = project
        .get("classifiers")
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let categories: Vec<String> = classifiers
        .iter()
        .filter_map(|c| map_pypi_category(c))
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // [project.urls]: detect Homepage/Documentation/Repository.
    let mut repository: Option<String> = None;
    let mut documentation = false;
    if let Some(urls) = project.get("urls").and_then(toml::Value::as_table) {
        for (key, val) in urls {
            let key_lower = key.to_ascii_lowercase();
            if let Some(url) = val.as_str()
                && !url.is_empty()
            {
                if repository.is_none()
                    && (key_lower.contains("repo")
                        || key_lower.contains("source")
                        || key_lower.contains("code")
                        || key_lower.contains("github")
                        || key_lower.contains("gitlab"))
                {
                    repository = Some(url.to_owned());
                }
                if key_lower.contains("doc")
                    || key_lower.contains("homepage")
                    || key_lower.contains("home")
                {
                    documentation = true;
                }
            }
        }
    }

    // `[project] license` table (PEP 621): `{text = "MIT"}` or `{file = "LICENSE"}`.
    // `license-expression` (PEP 639) is a plain string.
    let license_expr = project.get("license").and_then(|v| {
        // PEP 639: string form is the expression directly.
        if let Some(s) = v.as_str() {
            return if s.is_empty() {
                None
            } else {
                Some(s.to_owned())
            };
        }
        // PEP 621 table: `{text = "MIT"}`.
        v.as_table()?
            .get("text")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    });
    // PEP 639 `license-expression` key (takes precedence when present).
    let license_expr = project
        .get("license-expression")
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or(license_expr);
    let has_license_file = project.contains_key("license-files")
        || project
            .get("license")
            .and_then(toml::Value::as_table)
            .is_some_and(|t| t.contains_key("file"));
    // Classifier-only license detection (no expression present).
    let license_from_classifier =
        license_expr.is_none() && classifiers.iter().any(|c| c.starts_with("License ::"));
    let license = if license_from_classifier {
        // Presence signal only — no parseable expression.
        None
    } else {
        license_expr
    };

    // PEP 508 dependency names: take the leading name token (stop at `[`, `>`, `<`, `=`, `!`, `;`, ` `).
    let dependencies: Vec<String> = project
        .get("dependencies")
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(pep508_name)
                .collect()
        })
        .unwrap_or_default();

    ExtractedFacts {
        description,
        keywords,
        categories,
        readme_hint: project
            .get("readme")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        repository,
        documentation,
        license,
        has_license_file: has_license_file || license_from_classifier,
        dependencies,
    }
}

/// Extract the package name from a PEP 508 requirement string.
fn pep508_name(dep: &str) -> Option<String> {
    let name: &str = dep
        .split(['[', '>', '<', '=', '!', ';', ' ', '\t'])
        .next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Parse a `PKG-INFO` (email-header style) file.
pub fn parse_pkg_info(text: &str) -> ExtractedFacts {
    let mut description: Option<String> = None;
    let mut keywords: Vec<String> = vec![];
    let mut categories: Vec<String> = vec![];
    let mut repository: Option<String> = None;
    let mut documentation = false;
    // `License:` header value (≤ 64 bytes, no newline — skip whole-file blobs).
    let mut license: Option<String> = None;
    // `License-Expression:` (PEP 639) takes precedence when present.
    let mut license_expression: Option<String> = None;
    let mut license_from_classifier = false;
    let mut dependencies: Vec<String> = vec![];

    for line in text.lines() {
        if let Some((key, val)) = line.split_once(':') {
            let key = key.trim();
            let val = val.trim();
            match key {
                "Summary" if description.is_none() => {
                    let s = val.chars().filter(|c| !c.is_control()).collect::<String>();
                    if !s.is_empty() {
                        description = Some(s);
                    }
                }
                "Keywords" if keywords.is_empty() => {
                    keywords = val
                        .split([',', ' '])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .take(50)
                        .collect();
                }
                "Classifier" => {
                    if let Some(cat) = map_pypi_category(val) {
                        let slug = cat.to_owned();
                        if !categories.contains(&slug) {
                            categories.push(slug);
                        }
                    }
                    if val.starts_with("License ::") {
                        license_from_classifier = true;
                    }
                }
                "Home-page" | "Project-URL" => {
                    let (label, url_val) = if key == "Project-URL" {
                        // `Project-URL: Label, URL`
                        match val.split_once(',') {
                            Some((label, url)) => (label.trim(), url.trim()),
                            None => ("", val),
                        }
                    } else {
                        ("Home-page", val)
                    };
                    let label_lower = label.to_ascii_lowercase();
                    if !url_val.is_empty() {
                        if repository.is_none()
                            && (label_lower.contains("source")
                                || label_lower.contains("repo")
                                || label_lower.contains("github")
                                || label_lower.contains("gitlab"))
                        {
                            repository = Some(url_val.to_owned());
                        } else if repository.is_none() && key == "Home-page" && !documentation {
                            // Home-page as repository fallback only if nothing better found yet.
                        }
                        if label_lower.contains("doc")
                            || label_lower.contains("home")
                            || key == "Home-page"
                        {
                            documentation = true;
                        }
                    }
                }
                "License-Expression" if !val.is_empty() && val.len() <= 64 => {
                    license_expression = Some(val.to_owned());
                }
                "License" if !val.is_empty() && val.len() <= 64 && !val.contains('\n') => {
                    if license.is_none() {
                        license = Some(val.to_owned());
                    }
                }
                "Requires-Dist" => {
                    if let Some(name) = pep508_name(val) {
                        dependencies.push(name);
                    }
                }
                _ => {}
            }
        }
    }

    // PEP 639 expression wins; fall back to `License:` header value.
    let resolved_license = license_expression.or(license);
    // Classifier-only → has_license_file signal, no expression.
    let (final_license, has_license_file) = match resolved_license {
        Some(expr) => (Some(expr), false),
        None => (None, license_from_classifier),
    };

    ExtractedFacts {
        description,
        keywords,
        categories,
        readme_hint: None,
        repository,
        documentation,
        license: final_license,
        has_license_file,
        dependencies,
    }
}

/// Parse a `setup.cfg` (ini-style) file.
pub fn parse_setup_cfg(text: &str) -> ExtractedFacts {
    let mut description: Option<String> = None;
    let mut keywords: Vec<String> = vec![];
    let mut categories: Vec<String> = vec![];
    let mut repository: Option<String> = None;
    let mut documentation = false;
    let mut license: Option<String> = None;
    let mut license_from_classifier = false;
    let mut dependencies: Vec<String> = vec![];
    let mut in_metadata = false;
    let mut in_options = false;
    // Which multi-line key indented continuation lines belong to (ini values
    // may continue on following indented lines: `install_requires =\n    six`).
    enum Continuation {
        Dependencies,
        Classifiers,
    }
    let mut continuation: Option<Continuation> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') {
            in_metadata = trimmed == "[metadata]";
            in_options = trimmed == "[options]";
            continuation = None;
            continue;
        }
        if !in_metadata && !in_options {
            continue;
        }
        // An indented line under a multi-line key is a value item, not a `k = v`.
        if line.starts_with([' ', '\t']) {
            match continuation {
                Some(Continuation::Dependencies) => {
                    if let Some(name) = pep508_name(trimmed) {
                        dependencies.push(name);
                    }
                }
                Some(Continuation::Classifiers) => {
                    if let Some(cat) = map_pypi_category(trimmed) {
                        let slug = cat.to_owned();
                        if !categories.contains(&slug) {
                            categories.push(slug);
                        }
                    }
                    if trimmed.starts_with("License ::") {
                        license_from_classifier = true;
                    }
                }
                None => {}
            }
            continue;
        }
        continuation = None;
        if let Some((key, val)) = trimmed.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            if in_metadata {
                match key {
                    "description" | "summary" if description.is_none() => {
                        let s = val.chars().filter(|c| !c.is_control()).collect::<String>();
                        if !s.is_empty() {
                            description = Some(s);
                        }
                    }
                    "keywords" if keywords.is_empty() => {
                        keywords = val
                            .split([',', ' '])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .take(50)
                            .collect();
                    }
                    "url" | "home_page" => {
                        if !val.is_empty() {
                            documentation = true;
                        }
                    }
                    "project_urls" | "project-urls" => {
                        // Single-line value: `Repository = https://...`
                        let v_lower = val.to_ascii_lowercase();
                        if repository.is_none()
                            && (v_lower.contains("repo")
                                || v_lower.contains("source")
                                || v_lower.contains("code"))
                        {
                            // Take the URL after the `=` (the value side of the INI pair
                            // we've already split on): val is whatever follows the `=`.
                            if !val.is_empty() {
                                repository = Some(val.to_owned());
                            }
                        }
                        if v_lower.contains("doc") {
                            documentation = true;
                        }
                    }
                    "license" if !val.is_empty() && val.len() <= 64 => {
                        if license.is_none() {
                            license = Some(val.to_owned());
                        }
                    }
                    "classifiers" | "classifier" => {
                        continuation = Some(Continuation::Classifiers);
                        for cls in val.split('\n').map(str::trim).filter(|s| !s.is_empty()) {
                            if let Some(cat) = map_pypi_category(cls) {
                                let slug = cat.to_owned();
                                if !categories.contains(&slug) {
                                    categories.push(slug);
                                }
                            }
                            if cls.starts_with("License ::") {
                                license_from_classifier = true;
                            }
                        }
                    }
                    _ => {}
                }
            } else if in_options && (key == "install_requires" || key == "install-requires") {
                continuation = Some(Continuation::Dependencies);
                for dep_line in val.split('\n').map(str::trim).filter(|s| !s.is_empty()) {
                    if let Some(name) = pep508_name(dep_line) {
                        dependencies.push(name);
                    }
                }
            }
        }
    }

    let (final_license, has_license_file) = match license {
        Some(expr) => (Some(expr), false),
        None => (None, license_from_classifier),
    };

    ExtractedFacts {
        description,
        keywords,
        categories,
        readme_hint: None,
        repository,
        documentation,
        license: final_license,
        has_license_file,
        dependencies,
    }
}

/// Map a PyPI trove classifier to an internal category slug.
/// Returns `None` for classifiers without a meaningful mapping.
pub fn map_pypi_category(classifier: &str) -> Option<&'static str> {
    let c = classifier.trim();
    // Match on prefix — ordered most-specific first.
    if c.starts_with("Topic :: Internet :: WWW/HTTP") {
        return Some("web-programming");
    }
    if c.starts_with("Topic :: Internet :: WWW") {
        return Some("web-programming");
    }
    if c.starts_with("Topic :: Internet") {
        return Some("network-programming");
    }
    if c.starts_with("Topic :: Security :: Cryptography") {
        return Some("cryptography");
    }
    if c.starts_with("Topic :: Security") {
        return Some("cryptography");
    }
    if c.starts_with("Topic :: Database :: Front-Ends") {
        return Some("database");
    }
    if c.starts_with("Topic :: Database") {
        return Some("database");
    }
    if c.starts_with("Topic :: Scientific/Engineering :: Artificial Intelligence") {
        return Some("science");
    }
    if c.starts_with("Topic :: Scientific/Engineering :: Mathematics") {
        return Some("science");
    }
    if c.starts_with("Topic :: Scientific/Engineering :: Physics") {
        return Some("science");
    }
    if c.starts_with("Topic :: Scientific/Engineering :: Bio-Informatics") {
        return Some("science");
    }
    if c.starts_with("Topic :: Scientific/Engineering :: Visualization") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: Scientific/Engineering :: Image Processing") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: Scientific/Engineering") {
        return Some("science");
    }
    if c.starts_with("Topic :: Multimedia :: Graphics") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: Multimedia :: Sound") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: Multimedia :: Video") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: Multimedia") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: System :: Distributed Computing") {
        return Some("async-io");
    }
    if c.starts_with("Topic :: System :: Networking") {
        return Some("network-programming");
    }
    if c.starts_with("Topic :: System :: Archiving :: Compression") {
        return Some("compression");
    }
    if c.starts_with("Topic :: System :: Archiving") {
        return Some("compression");
    }
    if c.starts_with("Topic :: System :: Shells") {
        return Some("command-line-utilities");
    }
    if c.starts_with("Topic :: Terminals") {
        return Some("command-line-utilities");
    }
    if c.starts_with("Topic :: Utilities") {
        return Some("command-line-utilities");
    }
    if c.starts_with("Topic :: Text Processing :: Markup :: HTML") {
        return Some("web-programming");
    }
    if c.starts_with("Topic :: Text Processing :: Markup :: XML") {
        return Some("encoding");
    }
    if c.starts_with("Topic :: Text Processing :: Markup :: JSON") {
        return Some("encoding");
    }
    if c.starts_with("Topic :: Text Processing :: Markup") {
        return Some("encoding");
    }
    if c.starts_with("Topic :: Text Processing :: Linguistic") {
        return Some("parser");
    }
    if c.starts_with("Topic :: Text Processing :: Parsing") {
        return Some("parser");
    }
    if c.starts_with("Topic :: Text Processing :: Filters") {
        return Some("parser");
    }
    if c.starts_with("Topic :: Text Processing :: Indexing") {
        return Some("data-structures");
    }
    if c.starts_with("Topic :: Text Processing") {
        return Some("encoding");
    }
    if c.starts_with("Topic :: Software Development :: Embedded Systems") {
        return Some("embedded");
    }
    if c.starts_with("Topic :: Software Development :: Code Generators") {
        return Some("encoding");
    }
    if c.starts_with("Topic :: Software Development :: Compilers") {
        return Some("parser");
    }
    if c.starts_with("Topic :: Software Development :: Interpreters") {
        return Some("parser");
    }
    if c.starts_with("Topic :: Software Development :: Libraries") {
        return None;
    }
    if c.starts_with("Topic :: Software Development") {
        return None;
    }
    if c.starts_with("Topic :: Games") {
        return Some("graphics");
    }
    if c.starts_with("Topic :: Education") {
        return None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pyproject_toml_full() {
        let text = r#"
[project]
name = "requests"
description = "Python HTTP for Humans"
keywords = ["http", "rest", "client"]
classifiers = [
    "Topic :: Internet :: WWW/HTTP",
    "License :: OSI Approved :: MIT License",
]
dependencies = ["urllib3>=1.21.1", "certifi>=2017.4.17", "charset-normalizer>=2,<4"]

[project.urls]
Repository = "https://github.com/psf/requests"
Documentation = "https://requests.readthedocs.io"
"#;
        let facts = parse_pyproject_toml(text);
        assert_eq!(facts.description.as_deref(), Some("Python HTTP for Humans"));
        assert_eq!(facts.keywords, vec!["http", "rest", "client"]);
        assert!(facts.categories.contains(&"web-programming".to_owned()));
        // License :: OSI Approved :: MIT License → classifier-only, no expression.
        assert!(
            facts.license.is_none(),
            "classifier-only license: no expression"
        );
        assert!(facts.has_license_file, "classifier sets has_license_file");
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/psf/requests"),
            "Repository URL from [project.urls]"
        );
        assert!(facts.documentation);
        assert!(facts.dependencies.contains(&"urllib3".to_owned()));
        assert!(facts.dependencies.contains(&"certifi".to_owned()));
    }

    #[test]
    fn parse_pkg_info_basic() {
        let text = "Metadata-Version: 2.1\nName: requests\nSummary: HTTP for Humans\nKeywords: http, rest\nLicense: MIT\nProject-URL: Repository, https://github.com/psf/requests\nRequires-Dist: certifi>=2017\n";
        let facts = parse_pkg_info(text);
        assert_eq!(facts.description.as_deref(), Some("HTTP for Humans"));
        assert_eq!(
            facts.license.as_deref(),
            Some("MIT"),
            "License: header value captured"
        );
        assert!(!facts.has_license_file);
        assert!(
            facts.repository.as_deref() == Some("https://github.com/psf/requests"),
            "Repository from Project-URL"
        );
        assert!(facts.dependencies.contains(&"certifi".to_owned()));
    }

    #[test]
    fn parse_setup_cfg_basic() {
        let text = "[metadata]\ndescription = A test package\nkeywords = test, demo\nlicense = MIT\n[options]\ninstall_requires =\n    requests>=2.0\n    six\n";
        let facts = parse_setup_cfg(text);
        assert_eq!(facts.description.as_deref(), Some("A test package"));
        assert_eq!(
            facts.license.as_deref(),
            Some("MIT"),
            "setup.cfg license value"
        );
        assert!(!facts.has_license_file);
        assert!(facts.dependencies.contains(&"requests".to_owned()));
    }

    #[test]
    fn parse_pyproject_toml_license_expression() {
        let text = r#"
[project]
name = "mylib"
license = "MIT OR Apache-2.0"
"#;
        let facts = parse_pyproject_toml(text);
        assert_eq!(facts.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert!(!facts.has_license_file);
    }

    #[test]
    fn parse_pyproject_toml_license_file_table() {
        let text = r#"
[project]
name = "mylib"

[project.license]
file = "LICENSE.txt"
"#;
        // The TOML key `[project.license]` is a table `license = {file = "..."}`.
        let facts = parse_pyproject_toml(text);
        assert!(facts.license.is_none(), "file-only license: no expression");
        assert!(facts.has_license_file);
    }

    #[test]
    fn parse_malformed_pyproject_returns_default() {
        let facts = parse_pyproject_toml("{{not valid toml");
        assert_eq!(facts, ExtractedFacts::default());
    }

    #[test]
    fn pep508_name_basic() {
        assert_eq!(pep508_name("requests>=2.0"), Some("requests".to_owned()));
        assert_eq!(pep508_name("certifi[security]"), Some("certifi".to_owned()));
        assert_eq!(pep508_name("six"), Some("six".to_owned()));
        assert_eq!(pep508_name(""), None);
    }

    #[test]
    fn map_pypi_classifier_web() {
        assert_eq!(
            map_pypi_category("Topic :: Internet :: WWW/HTTP"),
            Some("web-programming")
        );
        assert_eq!(
            map_pypi_category("Topic :: Security :: Cryptography"),
            Some("cryptography")
        );
        assert_eq!(map_pypi_category("Topic :: Database"), Some("database"));
        assert_eq!(
            map_pypi_category("Topic :: Software Development :: Libraries"),
            None
        );
    }

    #[test]
    fn stopwords_python_specific() {
        assert!(NORMS.is_stopword("python"), "'python' stopped for PyPI");
        assert!(NORMS.is_stopword("py"), "'py' stopped for PyPI");
        assert!(NORMS.is_stopword("pypi"), "'pypi' stopped for PyPI");
    }

    #[test]
    fn stopwords_not_rust_words() {
        assert!(!NORMS.is_stopword("rust"));
        assert!(!NORMS.is_stopword("crate"));
    }

    #[test]
    fn strip_python_conventions_prefixes() {
        assert_eq!(strip_python_conventions("python-requests"), "requests");
        assert_eq!(strip_python_conventions("py-utils"), "utils");
        assert_eq!(strip_python_conventions("requests-python"), "requests");
        assert_eq!(strip_python_conventions("requests"), "requests");
    }

    #[test]
    fn parse_pypi_versions_yanked_all_files() {
        let body = br#"{
			"releases": {
				"1.0.0": [{"yanked": false, "filename": "pkg-1.0.0.tar.gz"}],
				"0.9.0": [
					{"yanked": true, "yanked_reason": "Security issue", "filename": "pkg-0.9.0.tar.gz"},
					{"yanked": true, "filename": "pkg-0.9.0-wheel.whl"}
				],
				"0.8.0": [
					{"yanked": true, "filename": "pkg-0.8.0.tar.gz"},
					{"yanked": false, "filename": "pkg-0.8.0-wheel.whl"}
				],
				"0.7.0": []
			}
		}"#;
        let versions = Python::parse_version_listing(body);
        assert_eq!(versions.len(), 4);
        let v1 = versions.iter().find(|v| v.raw == "1.0.0").unwrap();
        assert!(v1.status.is_listed());
        let v09 = versions.iter().find(|v| v.raw == "0.9.0").unwrap();
        assert!(!v09.status.is_listed());
        assert!(
            matches!(&v09.status, ListingStatus::Withdrawn { reason: Some(r) } if r == "Security issue")
        );
        // Not all yanked → listed
        let v08 = versions.iter().find(|v| v.raw == "0.8.0").unwrap();
        assert!(v08.status.is_listed());
        // Empty file list → listed
        let v07 = versions.iter().find(|v| v.raw == "0.7.0").unwrap();
        assert!(v07.status.is_listed());
    }

    // ── parse_download_count / pypistats ──────────────────────────────────────

    #[test]
    fn parse_download_count_pypistats_happy_path() {
        let body = br#"{"data":{"last_day":100,"last_week":1000,"last_month":50000},"package":"requests","type":"recent_downloads"}"#;
        assert_eq!(Python::parse_download_count(body), Some(50_000));
        assert_eq!(parse_pypistats_recent(body), Some(50_000));
    }

    #[test]
    fn parse_download_count_falls_back_to_week() {
        let body = br#"{"data":{"last_day":10,"last_week":999}}"#;
        assert_eq!(Python::parse_download_count(body), Some(999));
    }

    #[test]
    fn parse_download_count_zero_is_some() {
        let body = br#"{"data":{"last_month":0}}"#;
        assert_eq!(Python::parse_download_count(body), Some(0));
    }

    #[test]
    fn parse_download_count_malformed_or_missing() {
        assert_eq!(Python::parse_download_count(b"not json"), None);
        assert_eq!(Python::parse_download_count(b"{}"), None);
        assert_eq!(Python::parse_download_count(br#"{"data":{}}"#), None);
        assert_eq!(
            Python::parse_download_count(br#"{"data":{"last_month":null}}"#),
            None
        );
        assert_eq!(
            Python::parse_download_count(br#"{"data":{"last_month":"nope"}}"#),
            None
        );
        assert_eq!(Python::parse_download_count(b""), None);
    }

    #[test]
    fn download_source_is_pypistats() {
        let ep = Python::download_source().expect("Python has a download source");
        assert!(ep.url.contains("pypistats.org"));
        assert!(ep.url.contains("{name}"));
    }
}
