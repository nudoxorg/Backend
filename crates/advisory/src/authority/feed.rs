//! Authority feed admission for OSV, GHSA, and RustSec bodies.

use super::super::parse::{parse_ghsa_value, parse_osv_value};
use super::super::{
    Advisory, AdvisorySource, FeedFreshness, GhsaParseError, MAX_ADVISORY_BATCH_OBJECTS,
    MAX_ADVISORY_DOCUMENT_BYTES, ParseError, RustSecParseError, SyncMode, parse_rustsec,
};

/// A configured authority feed. The bytes are supplied by the composition
/// root so network policy remains outside the portable advisory crate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityFeed {
    /// Authority which signed/owns these objects.
    pub source: AdvisorySource,
    /// Snapshot or delta semantics of the supplied body.
    pub mode: SyncMode,
    /// Whether a snapshot is complete and may retire absent objects.
    pub complete: bool,
    /// Conditional response and observation evidence.
    pub freshness: FeedFreshness,
    /// Fully parsed source objects.
    pub entries: Vec<Advisory>,
}

impl AuthorityFeed {
    /// Parses a bounded JSON/TOML authority body into one deterministic feed.
    ///
    /// OSV and GHSA accept either one object or a conventional `vulns` /
    /// `advisories` array. RustSec accepts one advisory TOML document; callers
    /// with a directory can use [`Self::from_entries`].
    pub fn parse(
        source: AdvisorySource,
        bytes: &[u8],
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Result<Self, AuthorityParseError> {
        if bytes.len() > MAX_ADVISORY_DOCUMENT_BYTES {
            return Err(AuthorityParseError::BoundExceeded("document-bytes"));
        }
        let entries = match source {
            AdvisorySource::Osv => parse_osv_feed(bytes, observed_at)?,
            AdvisorySource::RustSec => vec![parse_rustsec(bytes, observed_at)?],
            AdvisorySource::Ghsa => parse_ghsa_feed(bytes, observed_at)?,
        };
        let snapshot = etag
            .clone()
            .or_else(|| last_modified.clone())
            .or_else(|| Some(blake3::hash(bytes).to_hex().to_string()));
        let mut entries = entries;
        for advisory in &mut entries {
            advisory.evidence.snapshot = snapshot.clone();
        }
        entries.sort_by(|left, right| left.key.native.cmp(&right.key.native));
        Ok(Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                not_modified: false,
            },
            entries,
        })
    }

    /// Builds a feed from already parsed entries, useful for a RustSec tree
    /// and deterministic fixture harnesses.
    #[must_use]
    pub fn from_entries(
        source: AdvisorySource,
        mut entries: Vec<Advisory>,
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        let snapshot = etag.clone().or_else(|| last_modified.clone());
        for advisory in &mut entries {
            if advisory.evidence.snapshot.is_none() {
                advisory.evidence.snapshot = snapshot.clone();
            }
        }
        entries.sort_by(|left, right| left.key.native.cmp(&right.key.native));
        Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                not_modified: false,
            },
            entries,
        }
    }

    /// Creates a validated conditional `304 Not Modified` response.
    #[must_use]
    pub fn not_modified(
        source: AdvisorySource,
        observed_at: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        Self {
            source,
            mode: SyncMode::Snapshot,
            complete: true,
            freshness: FeedFreshness {
                etag,
                last_modified,
                observed_at,
                not_modified: true,
            },
            entries: Vec::new(),
        }
    }
}

fn parse_osv_feed(bytes: &[u8], observed_at: u64) -> Result<Vec<Advisory>, AuthorityParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AuthorityParseError::Osv(ParseError::InvalidJson))?;
    if let Some(values) = value.as_array() {
        if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
            return Err(AuthorityParseError::BoundExceeded("batch-objects"));
        }
        return values
            .iter()
            .map(|value| parse_osv_value(value, observed_at).map_err(AuthorityParseError::Osv))
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Err(AuthorityParseError::Osv(ParseError::InvalidJson));
    };
    let values = object
        .get("vulns")
        .or_else(|| object.get("advisories"))
        .and_then(serde_json::Value::as_array);
    match values {
        Some(values) => {
            if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
                return Err(AuthorityParseError::BoundExceeded("batch-objects"));
            }
            values
                .iter()
                .map(|value| parse_osv_value(value, observed_at).map_err(AuthorityParseError::Osv))
                .collect()
        }
        None => Ok(vec![
            parse_osv_value(&value, observed_at).map_err(AuthorityParseError::Osv)?,
        ]),
    }
}

fn parse_ghsa_feed(bytes: &[u8], observed_at: u64) -> Result<Vec<Advisory>, AuthorityParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| AuthorityParseError::Ghsa(GhsaParseError::InvalidJson))?;
    if let Some(values) = value.as_array() {
        if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
            return Err(AuthorityParseError::BoundExceeded("batch-objects"));
        }
        return values
            .iter()
            .map(|value| parse_ghsa_value(value, observed_at).map_err(AuthorityParseError::Ghsa))
            .collect();
    }
    let Some(object) = value.as_object() else {
        return Err(AuthorityParseError::Ghsa(GhsaParseError::InvalidJson));
    };
    if let Some(values) = object
        .get("advisories")
        .and_then(serde_json::Value::as_array)
    {
        if values.len() > MAX_ADVISORY_BATCH_OBJECTS {
            return Err(AuthorityParseError::BoundExceeded("batch-objects"));
        }
        return values
            .iter()
            .map(|value| parse_ghsa_value(value, observed_at).map_err(AuthorityParseError::Ghsa))
            .collect();
    }
    Ok(vec![
        parse_ghsa_value(&value, observed_at).map_err(AuthorityParseError::Ghsa)?,
    ])
}

/// Parse failures are source-qualified so one malformed authority cannot be
/// mistaken for a transport failure from another.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorityParseError {
    /// OSV object or batch failed admission.
    Osv(ParseError),
    /// RustSec TOML failed admission.
    RustSec(RustSecParseError),
    /// GHSA object or batch failed admission.
    Ghsa(GhsaParseError),
    /// A feed exceeded a parser bound before allocation/admission.
    BoundExceeded(&'static str),
}

impl std::fmt::Display for AuthorityParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "advisory authority parse failed: {self:?}")
    }
}
impl std::error::Error for AuthorityParseError {}

impl From<ParseError> for AuthorityParseError {
    fn from(value: ParseError) -> Self {
        Self::Osv(value)
    }
}
impl From<RustSecParseError> for AuthorityParseError {
    fn from(value: RustSecParseError) -> Self {
        Self::RustSec(value)
    }
}
impl From<GhsaParseError> for AuthorityParseError {
    fn from(value: GhsaParseError) -> Self {
        Self::Ghsa(value)
    }
}
