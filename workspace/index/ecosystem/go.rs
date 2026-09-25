//! The Go module proxy (proxy.golang.org).
//!
//! ## `description`: still no source available
//!
//! `go.mod` carries no description field, and Go has no second manifest that
//! would (no GitHub/GitLab API fetch is in scope for a pure offline parser).
//! README text carries that weight instead (see `facets::extract_facets`'s
//! README-discovery step, ecosystem-generic).
//!
//! ## `license` / `has_license_file`: now sourced from a standalone LICENSE file
//!
//! `go.mod` itself carries neither field, but
//! `server::coordination::indexing::facets::extract_facets` now merges facts
//! across *every* matching manifest candidate in priority
//! order (`ExtractedFacts::merge`) instead of stopping at the first one that
//! parses, so a `LICENSE`/`LICENCE`/`COPYING` candidate appended after
//! `go.mod` (see [`Go::manifest_candidates`]) is finally reachable: `go.mod`
//! still wins (and is tried first) for `dependencies`/`repository`, and the
//! license candidate only fills in the fields `go.mod` never had. Detection
//! is conservative — [`crate::ecosystem::license::detect_spdx`] recognizes a
//! short list of unambiguous signatures and otherwise leaves `license: None`
//! with `has_license_file: true` rather than guess from the filename alone.

use smol_str::SmolStr;

use crate::ecosystem::{
    EcosystemSpec, Language,
    archive::ArchiveKind,
    license,
    manifest::{self, ExtractedFacts, ManifestCandidate},
    name,
    policy::UpstreamPolicy,
    search::{DEFAULT_SPECIFICITY_SEPARATORS, NormalizedQuery, SearchNorms, map_no_category},
    upstream::{self, ListedVersion, ListingStatus},
    version,
};

pub struct Go;

impl crate::ecosystem::sealed::Sealed for Go {}

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

    /// Canonical = the original raw string (Go module paths are
    /// identity-preserving; `canonicalize_go_module` returned the raw
    /// string verbatim, `/vN` included).
    ///
    /// INVARIANT: `n.original` must be populated (every parse_name-produced
    /// value is). A hand-assembled Go `StructuredName` with an empty `original`
    /// would render an empty canonical — never construct one by hand.
    fn render_canonical(n: &name::StructuredName) -> String {
        debug_assert!(
            !n.original.is_empty(),
            "Go canonical form derives from `original`"
        );
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
    /// this endpoint; retractions are declared inside go.mod — see
    /// [`retract_specs`] and [`version_retracted`]).
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

    /// `go.mod` first (authoritative for module path / dependencies), then
    /// the standalone license filenames — last, since a license file is a
    /// heuristic content sniff with nothing to ever outrank (`go.mod` sets
    /// neither `license` nor `has_license_file`). Kept in sync with
    /// [`license::LICENSE_FILENAMES`]; see the
    /// `license_candidates_match_shared_list` test.
    fn manifest_candidates() -> &'static [ManifestCandidate] {
        static CANDIDATES: &[ManifestCandidate] = &[
            ManifestCandidate::new("go.mod"),
            ManifestCandidate::new("LICENSE"),
            ManifestCandidate::new("LICENSE.txt"),
            ManifestCandidate::new("LICENSE.md"),
            ManifestCandidate::new("LICENCE"),
            ManifestCandidate::new("LICENCE.txt"),
            ManifestCandidate::new("LICENCE.md"),
            ManifestCandidate::new("COPYING"),
            ManifestCandidate::new("COPYING.txt"),
        ];
        CANDIDATES
    }

    fn parse_manifest(candidate: &ManifestCandidate, bytes: &[u8]) -> Option<Self::Manifest> {
        let text = std::str::from_utf8(bytes).ok()?;
        // Strip a leading UTF-8 BOM — left in place it corrupts the very
        // first line, which is almost always the `module ...` directive (or,
        // for a license file, the first line of its boilerplate).
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if license::LICENSE_FILENAMES
            .iter()
            .any(|f| candidate.path_suffix.eq_ignore_ascii_case(f))
        {
            return Some(ExtractedFacts {
                has_license_file: true,
                license: license::detect_spdx(text),
                ..ExtractedFacts::default()
            });
        }
        Some(parse_go_mod(text))
    }

    fn search_norms() -> &'static SearchNorms {
        &NORMS
    }

    /// Go modules have **no public download-count API** (proxy.golang.org
    /// exposes no statistics endpoint; pkg.go.dev has no free per-module JSON
    /// download feed). Returns `None`; ranking uses dependents / fairness
    /// floor.
    ///
    /// A pure [`parse_download_count`] is still provided for offline/fixture
    /// JSON shaped as `{"downloads": N}` so corpus jobs can inject counts later
    /// without touching the trait surface.
    fn download_source() -> Option<upstream::DownloadEndpoint> {
        None
    }

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
/// Also accepts `download_count` as an alternate key. Explicit `0` is
/// `Some(0)`.
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
                && next.is_ascii_lowercase()
            {
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
        return NormalizedQuery {
            terms: terms.to_owned(),
            namespace: None,
        };
    }
    // Split remaining path into dirs + last segment.
    let parts: Vec<&str> = stripped.split('/').collect();
    match parts.as_slice() {
        [] => NormalizedQuery {
            terms: terms.to_owned(),
            namespace: None,
        },
        [name] => NormalizedQuery {
            terms: (*name).to_owned(),
            namespace: None,
        },
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
    let mut module_path: Option<&str> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        // `module` directive — first occurrence wins.
        if module_path.is_none()
            && let Some(rest) = trimmed.strip_prefix("module ")
        {
            let path = rest.split_whitespace().next().unwrap_or("").trim();
            if !path.is_empty() {
                module_path = Some(path);
            }
        }
    }
    let dependencies = scan_requires(text)
        .into_iter()
        .map(|(path, _)| path)
        .collect();

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

/// Module paths required by `go.mod`, sorted and de-duplicated.
///
/// Indirect requirements are included: they are still module edges. A comment
/// and the version token are not part of the name.
pub fn require_names(text: &str) -> Vec<String> {
    require_edges(text)
        .into_iter()
        .map(|edge| edge.name.to_string())
        .collect()
}

/// Module edges from `go.mod`, with the version token as the requirement.
///
/// Indirect requirements stay. A repeated path keeps the first version.
/// Names are sorted.
#[must_use]
pub fn require_edges(text: &str) -> Vec<crate::record::DepEdge> {
    let mut edges: Vec<crate::record::DepEdge> = Vec::new();
    for (path, version) in scan_requires(text) {
        if edges.iter().any(|edge| edge.name == path) {
            continue;
        }
        let mut edge = crate::record::DepEdge::runtime(path);
        if let Some(version) = version {
            edge.requirement = Some(version.into());
        }
        edges.push(edge);
    }
    edges.sort_by(|left, right| left.name.cmp(&right.name));
    edges
}

fn scan_requires(text: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut in_require_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "require (" {
            in_require_block = true;
            continue;
        }
        if trimmed == ")" && in_require_block {
            in_require_block = false;
            continue;
        }
        if in_require_block {
            if let Some(pair) = split_require(trimmed) {
                out.push(pair);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("require ")
            && let Some(pair) = split_require(rest.trim())
        {
            out.push(pair);
        }
    }
    out
}

fn split_require(line: &str) -> Option<(String, Option<String>)> {
    let line = line.split("//").next().unwrap_or(line).trim();
    let mut parts = line.split_ascii_whitespace();
    let path = parts.next()?;
    if path.is_empty() || path.starts_with('(') || path.starts_with(')') {
        return None;
    }
    let version = parts
        .next()
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    Some((path.to_owned(), version))
}

/// Retracted version expressions from a `go.mod` file, in source order.
///
/// Each string is one `retract` operand, kept as raw text: a single version
/// (`v1.0.0`) or an inclusive range (`[v1.3.0, v1.4.0]`). `//` line comments
/// are discarded, including comment-only lines. The module is not loaded or
/// executed.
///
/// [`version_retracted`] tests a listed version against the result. The proxy
/// `@v/list` body carries no retract data, so [`Go::parse_version_listing`]
/// still reports every version as [`ListingStatus::Listed`].
pub fn retract_specs(go_mod: &str) -> Vec<String> {
    let go_mod = go_mod.strip_prefix('\u{feff}').unwrap_or(go_mod);
    let mut specs = Vec::new();
    let mut in_block = false;

    for line in go_mod.lines() {
        let code = strip_go_line_comment(line).trim();
        if code.is_empty() {
            continue;
        }
        if in_block {
            if consume_retract_block_line(code, &mut specs) {
                in_block = false;
            }
            continue;
        }
        let Some(rest) = retract_directive_body(code) else {
            continue;
        };
        let rest = rest.trim();
        if let Some(after) = rest.strip_prefix('(') {
            in_block = true;
            let after = after.trim();
            if !after.is_empty() && consume_retract_block_line(after, &mut specs) {
                in_block = false;
            }
            continue;
        }
        if let Some(spec) = retract_spec_token(rest) {
            specs.push(spec);
        }
    }
    specs
}

/// Whether `raw_version` is covered by any spec from [`retract_specs`].
///
/// An exact spec matches when both sides parse as [`version::GoVersion`] and
/// compare equal (`+incompatible` is ignored, matching Go's version order).
/// A `[low, high]` spec is inclusive under that same order. A version that
/// does not parse matches only an identical exact-spec string.
pub fn version_retracted(raw_version: &str, specs: &[String]) -> bool {
    let raw_version = raw_version.trim();
    let parsed = version::GoVersion::parse(raw_version);
    specs
        .iter()
        .any(|spec| retract_spec_covers(spec, raw_version, parsed.as_ref()))
}

/// Mark versions named by `go.mod` `retract` directives as withdrawn.
///
/// Versions outside every spec stay [`ListingStatus::Listed`]. An empty
/// retract set leaves the slice unchanged.
pub fn mark_retracted<V>(versions: &mut [ListedVersion<V>], go_mod: &str) {
    let specs = retract_specs(go_mod);
    if specs.is_empty() {
        return;
    }
    for version in versions {
        if version_retracted(version.raw.as_str(), &specs) {
            version.status = ListingStatus::Withdrawn {
                reason: Some(SmolStr::new("retract")),
            };
        }
    }
}

/// `@latest` URL for a module whose version list is `{prefix}/@v/list`.
pub fn proxy_latest_url(list_url: &str) -> Option<String> {
    let prefix = list_url.strip_suffix("/@v/list")?;
    Some(format!("{prefix}/@latest"))
}

/// `.mod` URL for `version` on the same module as `list_url`.
pub fn proxy_mod_url(list_url: &str, version: &str) -> Option<String> {
    let prefix = list_url.strip_suffix("/@v/list")?;
    Some(format!("{prefix}/@v/{version}.mod"))
}

/// `Version` field of a proxy `@latest` document.
pub fn latest_version(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Latest {
        #[serde(rename = "Version")]
        version: String,
    }
    let parsed: Latest = serde_json::from_slice(body).ok()?;
    if parsed.version.is_empty() {
        None
    } else {
        Some(parsed.version)
    }
}

fn strip_go_line_comment(line: &str) -> &str {
    match line.split_once("//") {
        Some((code, _)) => code,
        None => line,
    }
}

/// Text after a leading `retract` keyword, when `line` is that directive.
fn retract_directive_body(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("retract")?;
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_whitespace() || c == '(' => Some(rest),
        Some(_) => None,
    }
}

/// Parse one line of a `retract (` block. Returns whether the block closed.
fn consume_retract_block_line(line: &str, specs: &mut Vec<String>) -> bool {
    let line = line.trim();
    if line == ")" {
        return true;
    }
    if let Some(spec) = retract_spec_token(line) {
        specs.push(spec);
    }
    retract_block_closed(line)
}

fn retract_block_closed(line: &str) -> bool {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return false;
        };
        return rest[end + 1..].trim() == ")";
    }
    line.split_ascii_whitespace().any(|tok| tok == ")")
}

/// One retract operand: a `v…` token, or the raw `[low, high]` text.
fn retract_spec_token(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line == "(" || line == ")" {
        return None;
    }
    if line.starts_with('[') {
        let end = line.find(']')?;
        let spec = &line[..=end];
        if spec.contains(',') {
            return Some(spec.to_owned());
        }
        return None;
    }
    let token = line.split_ascii_whitespace().next()?;
    if token == ")" {
        return None;
    }
    token.starts_with('v').then(|| token.to_owned())
}

fn retract_spec_covers(spec: &str, raw_version: &str, parsed: Option<&version::GoVersion>) -> bool {
    let spec = spec.trim();
    if let Some(inner) = spec.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let Some(version) = parsed else {
            return false;
        };
        return retract_range_covers(inner, version);
    }
    if let Some(version) = parsed
        && let Some(spec_v) = version::GoVersion::parse(spec)
    {
        return version.cmp(&spec_v).is_eq();
    }
    spec == raw_version
}

fn retract_range_covers(inner: &str, version: &version::GoVersion) -> bool {
    let Some((low_s, high_s)) = inner.split_once(',') else {
        return false;
    };
    let low_s = low_s.trim();
    let high_s = high_s.trim();
    if low_s.is_empty() || high_s.is_empty() || high_s.contains(',') {
        return false;
    }
    let Some(low) = version::GoVersion::parse(low_s) else {
        return false;
    };
    let Some(high) = version::GoVersion::parse(high_s) else {
        return false;
    };
    version.cmp(&low).is_ge() && version.cmp(&high).is_le()
}

/// Extract the module path from a `require` line entry.

/// Strip a trailing `/vN` major-version suffix where N ≥ 2.
/// Returns (path_without_suffix, Some(N)) or (path, None).
fn strip_major_suffix(raw: &str) -> (&str, Option<u32>) {
    if let Some(slash_pos) = raw.rfind('/') {
        let suffix = &raw[slash_pos + 1..];
        if let Some(n_str) = suffix.strip_prefix('v')
            && let Ok(n) = n_str.parse::<u32>()
            && n >= 2
        {
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
    fn require_names_keep_direct_and_indirect_modules() {
        let text = "\
module example.com/foo

require golang.org/x/text v0.3.0

require (
    rsc.io/quote v1.5.2
    golang.org/x/text v0.3.0 // indirect
)
";
        assert_eq!(require_names(text), vec![
            "golang.org/x/text".to_owned(),
            "rsc.io/quote".to_owned(),
        ]);
        let edges = require_edges(text);
        assert_eq!(edges[0].requirement.as_deref(), Some("v0.3.0"));
        assert_eq!(edges[1].requirement.as_deref(), Some("v1.5.2"));
        assert!(require_names("module example.com/foo\n").is_empty());
    }

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
        assert!(
            facts
                .dependencies
                .contains(&"github.com/gorilla/context".to_owned())
        );
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
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://gitlab.com/acme/tools")
        );
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

    // ── Hostile-input hardening ───────────────────────────────────────────────

    #[test]
    fn parse_go_mod_bom_stripped() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(b"module github.com/gorilla/mux\n");
        let facts = Go::parse_manifest(&ManifestCandidate::new("go.mod"), &bytes)
            .expect("valid utf8 with BOM parses");
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/gorilla/mux")
        );
    }

    #[test]
    fn parse_go_mod_crlf_line_endings() {
        let text = "module github.com/example/foo\r\n\r\nrequire golang.org/x/sync v0.1.0\r\n";
        let facts = parse_go_mod(text);
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/example/foo")
        );
        assert!(facts.dependencies.contains(&"golang.org/x/sync".to_owned()));
    }

    #[test]
    fn parse_go_mod_unterminated_require_block_no_hang() {
        let text = "module example.com/foo\n\nrequire (\n    github.com/a/b v1.0.0\n";
        let facts = parse_go_mod(text);
        assert!(facts.dependencies.contains(&"github.com/a/b".to_owned()));
    }

    #[test]
    fn parse_go_mod_unicode_and_control_bytes_no_panic() {
        let text =
            "module example.com/日本語モジュール\n\nrequire github.com/a/b v1.0.0\n\0garbage\0\n";
        let facts = parse_go_mod(text);
        assert!(facts.dependencies.contains(&"github.com/a/b".to_owned()));
    }

    #[test]
    fn parse_go_mod_enormous_require_block_no_panic() {
        use std::fmt::Write as _;
        let mut text = String::from("module example.com/foo\n\nrequire (\n");
        for i in 0..50_000 {
            let _ = writeln!(text, "    example.com/dep{i} v1.0.0");
        }
        text.push(')');
        let facts = parse_go_mod(&text);
        assert_eq!(facts.dependencies.len(), 50_000);
    }

    #[test]
    fn parse_go_mod_module_line_with_only_whitespace_after_keyword() {
        let text = "module   \n\nrequire github.com/a/b v1.0.0\n";
        let facts = parse_go_mod(text);
        assert!(facts.repository.is_none());
    }

    #[test]
    fn parse_go_mod_invalid_utf8_bytes_returns_none() {
        let bytes = [0xff, 0xfe, b'm', b'o', b'd'];
        assert!(Go::parse_manifest(&ManifestCandidate::new("go.mod"), &bytes).is_none());
    }

    #[test]
    fn parse_go_mod_real_world_shape() {
        // Shaped after a real go.mod (gorilla/mux-style, mixed single + block
        // require forms, indirect markers, replace/exclude directives that
        // must be silently ignored).
        let text = r#"module github.com/gorilla/mux

go 1.20

require github.com/stretchr/testify v1.8.4

require (
	github.com/davecgh/go-spew v1.1.1 // indirect
	github.com/pmezard/go-difflib v1.0.0 // indirect
	gopkg.in/yaml.v3 v3.0.1 // indirect
)

replace github.com/old/pkg => github.com/new/pkg v1.2.3

exclude github.com/bad/pkg v0.0.1
"#;
        let facts = parse_go_mod(text);
        assert_eq!(
            facts.repository.as_deref(),
            Some("https://github.com/gorilla/mux")
        );
        assert!(
            facts
                .dependencies
                .contains(&"github.com/stretchr/testify".to_owned())
        );
        assert!(
            facts
                .dependencies
                .contains(&"github.com/davecgh/go-spew".to_owned())
        );
        assert!(facts.dependencies.contains(&"gopkg.in/yaml.v3".to_owned()));
        assert!(facts.description.is_none());
        assert!(facts.license.is_none());
        assert!(!facts.has_license_file);
    }

    #[test]
    fn retract_specs_single_version() {
        let text = "module example.com/foo\n\nretract v1.0.0\n";
        assert_eq!(retract_specs(text), vec!["v1.0.0".to_owned()]);
    }

    #[test]
    fn retract_specs_block_versions_and_range() {
        let text = "\
module example.com/foo

retract (
    v1.0.0
    v1.2.0
    [v1.3.0, v1.4.0]
)
";
        let specs = retract_specs(text);
        assert_eq!(specs, vec![
            "v1.0.0".to_owned(),
            "v1.2.0".to_owned(),
            "[v1.3.0, v1.4.0]".to_owned(),
        ]);
        assert!(version_retracted("v1.0.0", &specs));
        assert!(version_retracted("v1.2.0", &specs));
        assert!(version_retracted("v1.3.0", &specs));
        assert!(version_retracted("v1.3.5", &specs));
        assert!(version_retracted("v1.4.0", &specs));
        assert!(!version_retracted("v1.2.1", &specs));
        assert!(!version_retracted("v1.4.1", &specs));
    }

    #[test]
    fn retract_specs_ignores_comment_only_lines() {
        let text = "\
module example.com/foo

// retract v9.9.9
retract (
    // retracted for a security issue
    v1.0.0 // accidental publish
    // comment-only line between specs
    [v1.1.0, v1.2.0]
)
";
        assert_eq!(retract_specs(text), vec![
            "v1.0.0".to_owned(),
            "[v1.1.0, v1.2.0]".to_owned()
        ]);
    }

    #[test]
    fn retract_specs_single_line_range_and_other_directives() {
        let text = "\
module example.com/foo

require (
    github.com/a/b v1.2.3
)

exclude github.com/a/b v1.2.3

retract [v1.0.0, v1.2.0]
";
        assert_eq!(retract_specs(text), vec!["[v1.0.0, v1.2.0]".to_owned()]);
    }

    #[test]
    fn version_retracted_exact_token_and_inclusive_range() {
        let specs = vec!["v1.0.0".to_owned(), "[v1.9.0, v1.10.0]".to_owned()];
        assert!(version_retracted("v1.0.0", &specs));
        assert!(!version_retracted("v1.0.1", &specs));
        // Go version order ignores +incompatible.
        assert!(version_retracted("v1.0.0+incompatible", &specs));
        assert!(version_retracted("v1.9.0", &specs));
        assert!(version_retracted("v1.10.0", &specs));
        // v1.10.0-alpha sits inside [v1.9.0, v1.10.0] under Go version order.
        assert!(version_retracted("v1.10.0-alpha", &specs));
        assert!(!version_retracted("v1.8.9", &specs));
        assert!(!version_retracted("v1.10.1", &specs));
        assert!(version_retracted("not-a-version", &[
            "not-a-version".to_owned()
        ]));
        assert!(!version_retracted("not-a-version", &specs));
    }

    #[test]
    fn mark_retracted_withdraws_only_the_named_versions() {
        let go_mod = "module example.com/foo\n\nretract v1.0.0\nretract [v1.2.0, v1.2.1]\n";
        let mut versions = vec![listed("v1.0.0"), listed("v1.2.0"), listed("v1.2.2")];
        mark_retracted(&mut versions, go_mod);
        assert!(matches!(
            versions[0].status,
            ListingStatus::Withdrawn { .. }
        ));
        assert!(matches!(
            versions[1].status,
            ListingStatus::Withdrawn { .. }
        ));
        assert_eq!(versions[2].status, ListingStatus::Listed);
        let mut untouched = vec![listed("v1.0.0")];
        mark_retracted(&mut untouched, "module example.com/foo\n");
        assert_eq!(untouched[0].status, ListingStatus::Listed);
    }

    #[test]
    fn proxy_urls_share_the_list_prefix() {
        let list = "https://proxy.golang.org/example.com/foo/@v/list";
        assert_eq!(
            proxy_latest_url(list).as_deref(),
            Some("https://proxy.golang.org/example.com/foo/@latest")
        );
        assert_eq!(
            proxy_mod_url(list, "v1.2.3").as_deref(),
            Some("https://proxy.golang.org/example.com/foo/@v/v1.2.3.mod")
        );
        assert!(proxy_latest_url("https://example.com/not-a-list").is_none());
        assert_eq!(
            latest_version(br#"{"Version":"v1.2.3","Time":"2020-01-01T00:00:00Z"}"#).as_deref(),
            Some("v1.2.3")
        );
        assert!(latest_version(b"{}").is_none());
    }

    fn listed(raw: &str) -> ListedVersion<version::GoVersion> {
        ListedVersion {
            version: version::GoVersion::parse(raw).expect("version"),
            status: ListingStatus::Listed,
            raw: SmolStr::new(raw),
        }
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
        assert_eq!(
            escape_module_path("github.com/user/repo"),
            "github.com/user/repo"
        );
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
        assert_eq!(
            escape_module_path("github.com/FOO/BAR"),
            "github.com/!f!o!o/!b!a!r"
        );
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
        assert_eq!(
            Go::parse_download_count(br#"{"downloads":12345}"#),
            Some(12_345)
        );
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

    // ── LICENSE candidates (previously unreachable — see module doc) ─────────

    #[test]
    fn manifest_candidates_go_mod_then_license_files() {
        let suffixes: Vec<&str> = Go::manifest_candidates()
            .iter()
            .map(|c| c.path_suffix)
            .collect();
        assert_eq!(suffixes[0], "go.mod", "go.mod must stay highest priority");
        assert_eq!(&suffixes[1..], license::LICENSE_FILENAMES);
    }

    #[test]
    fn parse_manifest_dispatches_go_mod_by_default() {
        let facts = Go::parse_manifest(
            &ManifestCandidate::new("go.mod"),
            b"module example.com/foo\n",
        )
        .unwrap();
        assert!(facts.license.is_none());
        assert!(!facts.has_license_file);
    }

    #[test]
    fn parse_manifest_dispatches_license_candidate() {
        let text = b"MIT License\n\nPermission is hereby granted, free of charge, to any \
person obtaining a copy of this software and associated documentation files (the \"Software\")...";
        let facts = Go::parse_manifest(&ManifestCandidate::new("LICENSE"), text).unwrap();
        assert!(facts.has_license_file);
        assert_eq!(facts.license.as_deref(), Some("MIT"));
        // A license file contributes nothing else — go.mod is still the sole
        // source of dependencies/repository.
        assert!(facts.dependencies.is_empty());
        assert!(facts.repository.is_none());
    }

    #[test]
    fn parse_manifest_license_candidate_unrecognized_content_no_fabrication() {
        let facts = Go::parse_manifest(
            &ManifestCandidate::new("LICENCE"),
            b"Proprietary. All rights reserved.",
        )
        .unwrap();
        assert!(facts.has_license_file);
        assert!(facts.license.is_none());
    }
}
