//! Pure upstream facts: endpoint templates, listing status, download sources.
//!
//! The IO half (shared reqwest stack, rate limiting, catalog followers) lives
//! in `registry/upstream/` and consumes these types.

use smol_str::SmolStr;

/// Static endpoint templates for one ecosystem's registry. `{name}`,
/// `{name_lower}`, `{version}` placeholders; the base URL comes from server
/// config / `RegistryOrigin` (existing pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamEndpoints {
    /// Template for the version-list/metadata request.
    pub listing: &'static str,
    /// Template for the archive download.
    pub archive: &'static str,
    /// Optional secondary endpoint template for listing-status data (e.g. NuGet
    /// registration pages that carry `catalogEntry.listed`). When `Some`, the IO
    /// layer fetches this URL and passes the body to
    /// [`super::EcosystemSpec::merge_listing_status`] so per-ecosystem unlisted /
    /// withdrawn logic can be applied in one place without touching the primary
    /// listing parse.
    pub listing_status: Option<&'static str>,
}

/// One published version together with its listing status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedVersion<V> {
    pub version: V,
    pub status: ListingStatus,
    /// The original, unparsed version string as returned by the registry.
    /// Preserved so the resolve path can pass the raw string to the type grammar
    /// without requiring `V: Display`.
    pub raw: SmolStr,
}

/// Whether a published version is currently visible on its registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ListingStatus {
    Listed,
    /// Yanked (crates.io/PyPI), unlisted (NuGet), deprecated (npm).
    /// Withdrawn versions resolve only under exact pins and never enter
    /// search.
    Withdrawn {
        reason: Option<SmolStr>,
    },
}

impl ListingStatus {
    pub fn is_listed(&self) -> bool {
        matches!(self, ListingStatus::Listed)
    }
}

/// Where (if anywhere) an ecosystem's download counts come from (S4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadEndpoint {
    /// Template for the download-count request. `{name}` placeholder.
    pub url: &'static str,
}
