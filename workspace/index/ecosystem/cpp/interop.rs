//! Derived interop renderings for `cpp` stems (REGISTRYLESS §3.4, RL-8).
//!
//! Pure functions producing purl and SWHID strings from `(stem, version, rev)`.
//! These are export/query-time projections only — **never persisted as
//! identity** (the SWHID-is-T0-dedup-only law, and purl names what a registry
//! knows, not what we do).

use crate::ecosystem::name::StructuredName;

/// The github.com host, for which purl has a dedicated `pkg:github/...` type.
const GITHUB_HOST: &str = "github.com";

/// Render a Package URL (purl) for a `cpp` stem (REGISTRYLESS §3.4).
///
/// - github.com host → `pkg:github/<org>/<repo>@<version>`.
/// - any other host → `pkg:generic/<name>@<version>?vcs_url=https://<slug>.git[@<rev>]`.
///
/// `rev` (a git oid) is appended to the `vcs_url` qualifier for the generic form
/// when present; the github form carries the version only (the rev lives in the
/// SWHID, not the purl, to match common tooling).
pub fn purl(stem: &StructuredName, version: &str, rev: Option<&str>) -> String {
    let host = stem.authority.as_deref().unwrap_or_default();
    if host == GITHUB_HOST {
        // namespace is `[org]`, name is `repo` for a github slug.
        let org = stem
            .namespace
            .first()
            .map(|s| s.as_str())
            .unwrap_or_default();
        return format!("pkg:github/{org}/{}@{version}", stem.name);
    }
    // Generic: carry the full vcs_url so the identity is recoverable.
    let slug = slug_of(stem);
    let mut vcs_url = format!("https://{slug}.git");
    if let Some(rev) = rev {
        vcs_url.push('@');
        vcs_url.push_str(rev);
    }
    format!("pkg:generic/{}@{version}?vcs_url={vcs_url}", stem.name)
}

/// Render the SWHID revision identifier for a git commit oid (REGISTRYLESS
/// §3.4): `swh:1:rev:<hex>`.
pub fn swhid_rev(rev_sha1: &str) -> String {
    format!("swh:1:rev:{rev_sha1}")
}

/// Reconstruct the lowercase `host/path` slug from a structured name.
fn slug_of(stem: &StructuredName) -> String {
    let mut slug = String::new();
    if let Some(host) = &stem.authority {
        slug.push_str(host);
        slug.push('/');
    }
    for segment in &stem.namespace {
        slug.push_str(segment);
        slug.push('/');
    }
    slug.push_str(&stem.name);
    slug
}

#[cfg(test)]
mod tests;
