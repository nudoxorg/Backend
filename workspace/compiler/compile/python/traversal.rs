//! Version resolution over PyPI / git for external Python dependencies.
//!
//! The flow for an external dependency `name` constrained by a PEP 440
//! requirement (e.g. `>=1.2,<2`) is:
//!   1. Query the PyPI JSON API (`https://pypi.org/pypi/<name>/json`).
//!   2. Parse the `releases` map into [`Release`]s (one per uploaded file).
//!   3. Pick the newest *satisfying* version with [`select_best_release`],
//!      preferring its source distribution (`sdist`, a `.tar.gz`).
//!   4. Download + extract that sdist into a workspace directory so pyrefly can
//!      check it.
//!
//! A git-tag alternative ([`resolve_version_from_tags`]) matches a PEP 440
//! requirement against a repo's tags (e.g. `v1.2.0`), for deps sourced from a
//! VCS rather than PyPI.
//!
//! ## Offline testing
//! The version-*selection* and metadata-*parsing* logic is pure and is unit
//! tested with in-memory fixtures. The functions that actually touch the
//! network ([`fetch_pypi_metadata`], [`download_and_extract_sdist`],
//! [`resolve_and_fetch_sdist`]) are integration-only; any test exercising them
//! must be marked `#[ignore]`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use version::{Constraint, TagContext, VersionGrammar, VersionRequest, resolve_from_tags};
use uv_pep440::{Version, VersionSpecifiers};

/// PEP 440 specifiers (`>=1.2,<2`) as the shared [`Constraint`] predicate:
/// a version matches when the specifier set `contains` it. This is what lets
/// Python carry its constraint as `VersionRequest::Constraint(..)` on the ONE
/// shared vocabulary instead of a side-channel field plus an always-`Latest`
/// request.
///
/// Both `Constraint` (from the `version` crate) and `VersionSpecifiers` (from
/// `uv_pep440`) are foreign to this crate, so the impl must go through a local
/// newtype to satisfy the orphan rules — mirroring Go's `GoPrefix` and Java's
/// `PrefixConstraint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pep440Constraint(pub VersionSpecifiers);

impl Constraint<Version> for Pep440Constraint {
    fn matches(&self, v: &Version) -> bool {
        self.0.contains(v)
    }
}

/// The shared [`version::VersionRequest`] specialised to a PEP 440 [`Version`]
/// carrying [`VersionSpecifiers`] in its constraint case. Python only ever
/// resolves with a specifier set (never a bare `Exact` pin here), so its use
/// is `Constraint(specifiers)`.
pub type PythonVersionRequest = VersionRequest<Version, Pep440Constraint>;

#[derive(Debug, thiserror::Error)]
pub enum PythonTraversalError {
    #[error("requesting PyPI metadata for `{name}`")]
    MetadataRequest { name: String, #[source] source: reqwest::Error },

    #[error("PyPI returned an error for `{name}`")]
    MetadataHttp { name: String, #[source] source: reqwest::Error },

    #[error("reading PyPI metadata body")]
    MetadataBody(#[source] reqwest::Error),

    #[error("parsing PyPI metadata JSON")]
    MetadataParse(#[source] serde_json::Error),

    #[error("creating extraction dir {dir}")]
    CreateDir { dir: PathBuf, #[source] source: std::io::Error },

    #[error("downloading sdist {url}")]
    Download { url: String, #[source] source: reqwest::Error },

    #[error("sdist download returned an error status")]
    DownloadHttp(#[source] reqwest::Error),

    #[error("reading sdist bytes")]
    DownloadBody(#[source] reqwest::Error),

    #[error("gunzipping sdist")]
    Gunzip(#[source] std::io::Error),

    #[error("untarring sdist into {dir}")]
    Untar { dir: PathBuf, #[source] source: std::io::Error },

    #[error("selected release for package has no download URL")]
    NoDownloadUrl,

    #[error("no sdist satisfying the request found on PyPI")]
    NoMatchingSdist,
}

pub type Result<T> = std::result::Result<T, PythonTraversalError>;

/// One uploaded artifact for a release, as listed under PyPI's `releases` map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The parsed PEP 440 version this artifact belongs to.
    pub version:      Version,
    /// The artifact filename (e.g. `foo-1.2.0.tar.gz`).
    pub filename:     String,
    /// The download URL.
    pub url:          String,
    /// PyPI `packagetype` (`sdist`, `bdist_wheel`, ...).
    pub package_type: String,
    /// Whether this artifact has been yanked (PEP 592).
    pub yanked:       bool,
}

impl Release {
    /// `true` when this artifact is a source distribution we can extract.
    pub fn is_sdist(&self) -> bool {
        self.package_type == "sdist"
            || self.filename.ends_with(".tar.gz")
            || self.filename.ends_with(".tgz")
            || self.filename.ends_with(".zip")
    }
}

/// Parse PyPI's JSON `releases` object into a flat list of [`Release`]s.
///
/// The PyPI JSON shape is:
/// ```json
/// { "releases": { "1.2.0": [ { "filename": "...", "url": "...",
///   "packagetype": "sdist", "yanked": false }, ... ], ... } }
/// ```
/// Versions that don't parse as PEP 440 are skipped (PyPI occasionally hosts
/// legacy/non-conforming versions).
pub fn parse_releases(metadata: &serde_json::Value) -> Vec<Release> {
    let mut out = Vec::new();
    let Some(releases) = metadata.get("releases").and_then(|r| r.as_object()) else {
        return out;
    };

    for (version_str, files) in releases {
        let Ok(version) = Version::from_str(version_str) else {
            continue;
        };
        let Some(files) = files.as_array() else {
            continue;
        };
        for file in files {
            let filename = file
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let url = file
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let package_type = file
                .get("packagetype")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let yanked = file.get("yanked").and_then(|v| v.as_bool()).unwrap_or(false);
            out.push(Release {
                version: version.clone(),
                filename,
                url,
                package_type,
                yanked,
            });
        }
    }
    out
}

/// Pick the newest version that satisfies `req` from a list of candidate
/// versions, following PEP 440 prerelease rules: a prerelease is only chosen
/// when no non-prerelease version satisfies the requirement.
///
/// Yanked-ness is not considered here (it is per-artifact, not per-version);
/// use [`select_best_release`] for artifact-level selection.
pub fn select_best_version(versions: &[Version], req: &VersionSpecifiers) -> Option<Version> {
    let mut best_stable: Option<&Version> = None;
    let mut best_any: Option<&Version> = None;
    for ver in versions {
        if !req.contains(ver) {
            continue;
        }
        if best_any.map_or(true, |b| ver > b) {
            best_any = Some(ver);
        }
        if !ver.any_prerelease() && best_stable.map_or(true, |b| ver > b) {
            best_stable = Some(ver);
        }
    }
    // Prefer the newest stable release; fall back to the newest prerelease.
    best_stable.or(best_any).cloned()
}

/// Choose the best *artifact* to download for `req`: the newest satisfying,
/// non-yanked version, preferring its `sdist`. Returns the matching [`Release`].
///
/// Versions whose only artifacts are yanked are skipped. If the chosen version
/// has no sdist (wheels only), `None` is returned — sdist-only extraction is
/// what pyrefly needs and the higher layer can decide how to proceed.
pub fn select_best_release<'a>(
    releases: &'a [Release],
    req: &VersionSpecifiers,
) -> Option<&'a Release> {
    // Candidate versions = those with at least one non-yanked artifact.
    let candidate_versions: Vec<Version> = {
        let mut v: Vec<Version> = releases
            .iter()
            .filter(|r| !r.yanked)
            .map(|r| r.version.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    };

    let best = select_best_version(&candidate_versions, req)?;

    // Prefer the sdist artifact of the chosen version; else any non-yanked one.
    releases
        .iter()
        .filter(|r| r.version == best && !r.yanked)
        .find(|r| r.is_sdist())
}

// ---------------------------------------------------------------------------
// VersionGrammar implementation — wires uv_pep440::Version into the shared loop
// ---------------------------------------------------------------------------

/// Grammar adapter that makes [`resolve_from_tags`] work for Python/PEP 440.
///
/// `parse_tag` strips a single leading `v`/`V` and parses the remainder as a
/// PEP 440 version. The `Constraint(VersionSpecifiers)` case of the shared
/// request supplies the real filtering (via [`VersionSpecifiers::contains`]),
/// so the default `matches_request` — which defers to the constraint's own
/// [`Constraint::matches`] — is exactly right and needs no override.
struct PythonTagGrammar;

impl VersionGrammar for PythonTagGrammar {
    type V = Version;
    type C = Pep440Constraint;

    fn parse_tag<'t>(&self, raw_tag: &'t str, _ctx: &TagContext<'_>) -> Option<(Version, &'t str)> {
        let trimmed = raw_tag.strip_prefix(['v', 'V']).unwrap_or(raw_tag);
        let version = Version::from_str(trimmed).ok()?;
        Some((version, raw_tag))
    }

    fn is_prerelease(&self, v: &Version) -> bool {
        v.any_prerelease()
    }
}

/// Resolve a PEP 440 requirement against a set of git tags (e.g. `v1.2.0`,
/// `1.3.0rc1`). Returns the *original tag string* of the newest satisfying
/// version, so the caller can check that ref out.
///
/// A single optional leading `v`/`V` is stripped before parsing; tags that
/// don't parse as PEP 440 versions are ignored. The stable-before-prerelease
/// selection loop is provided by [`version::resolve_from_tags`].
pub fn resolve_version_from_tags<S: AsRef<str>>(
    tags: &[S],
    req: &VersionSpecifiers,
) -> Option<String> {
    let ctx = TagContext::default();
    // Carry the PEP 440 specifiers as the shared `Constraint` case — no more
    // side-channel field plus always-`Latest` bypass.
    let request: PythonVersionRequest = VersionRequest::Constraint(Pep440Constraint(req.clone()));
    resolve_from_tags(tags, &request, &ctx, &PythonTagGrammar)
}

// ---------------------------------------------------------------------------
// Network-gated paths (integration only — any test must be `#[ignore]`).
// ---------------------------------------------------------------------------

/// The PyPI JSON metadata endpoint for a package.
pub fn pypi_metadata_url(name: &str) -> String {
    format!("https://pypi.org/pypi/{name}/json")
}

/// Fetch and parse a package's PyPI JSON metadata. **Network.**
pub fn fetch_pypi_metadata(name: &str) -> Result<serde_json::Value> {
    let url = pypi_metadata_url(name);
    let resp = reqwest::blocking::get(&url)
        .map_err(|source| PythonTraversalError::MetadataRequest { name: name.to_owned(), source })?;
    let resp = resp
        .error_for_status()
        .map_err(|source| PythonTraversalError::MetadataHttp { name: name.to_owned(), source })?;
    let body = resp.text()
        .map_err(PythonTraversalError::MetadataBody)?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(PythonTraversalError::MetadataParse)?;
    Ok(value)
}

/// Download a `.tar.gz` sdist from `url` and extract it under `dest_dir`.
/// Returns the directory the archive was extracted into. **Network + IO.**
pub fn download_and_extract_sdist(url: &str, dest_dir: &Path) -> std::result::Result<PathBuf, PythonTraversalError> {
    std::fs::create_dir_all(dest_dir)
        .map_err(|source| PythonTraversalError::CreateDir { dir: dest_dir.to_path_buf(), source })?;

    let resp = reqwest::blocking::get(url)
        .map_err(|source| PythonTraversalError::Download { url: url.to_owned(), source })?;
    let resp = resp
        .error_for_status()
        .map_err(PythonTraversalError::DownloadHttp)?;
    let bytes = resp.bytes()
        .map_err(PythonTraversalError::DownloadBody)?;

    extract_tar_gz(&bytes, dest_dir)?;
    Ok(dest_dir.to_path_buf())
}

/// Extract an in-memory `.tar.gz` archive into `dest_dir`. Pure IO (no
/// network) — usable in tests with a fixture archive if desired.
pub fn extract_tar_gz(bytes: &[u8], dest_dir: &Path) -> Result<()> {
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    // Read fully decompressed bytes, then hand to the tar reader.
    let mut tar_bytes = Vec::new();
    decoder
        .read_to_end(&mut tar_bytes)
        .map_err(PythonTraversalError::Gunzip)?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    archive
        .unpack(dest_dir)
        .map_err(|source| PythonTraversalError::Untar { dir: dest_dir.to_path_buf(), source })?;
    Ok(())
}

/// End-to-end: resolve `name @ req` on PyPI and materialise its sdist under
/// `workspace`. Returns the extraction directory. **Network + IO.**
pub fn resolve_and_fetch_sdist(
    name: &str,
    req: &VersionSpecifiers,
    workspace: &Path,
) -> Result<PathBuf> {
    let metadata = fetch_pypi_metadata(name)?;
    let releases = parse_releases(&metadata);
    let chosen = select_best_release(&releases, req)
        .ok_or(PythonTraversalError::NoMatchingSdist)?;
    if chosen.url.is_empty() {
        return Err(PythonTraversalError::NoDownloadUrl);
    }
    let dest = workspace.join(format!("{name}-{}", chosen.version));
    download_and_extract_sdist(&chosen.url, &dest)
}
