//! GitHub global advisory (GHSA) object admission.

use super::{
    Advisory, AdvisoryCategory, AdvisoryKey, AdvisorySchema, AdvisorySource, AffectedRange, Alias,
    CanonicalAdvisoryId, Evidence, EvidenceKind, GhsaParseError, MAX_ADVISORY_DOCUMENT_BYTES,
    MalwareCoverage, NativeAdvisoryId, PackageNormalizationError, Reference, Severity,
    SeverityLevel, VersionEvent, VersionEventKind, VersionMatcher, normalize_package,
    optional_text, parse_github_range, severity_from_text, syntax_for,
};
use serde_json::{Map, Value};

/// Parses the GitHub global advisory JSON shape.
///
/// GHSA is admitted only when the producer includes an explicit `malware_coverage` boolean (or
/// the same field under `database_specific`).  This prevents an omitted field from becoming a
/// false claim that the feed covers malicious packages.
///
/// # Errors
///
/// Returns a typed parse error when the malware declaration, advisory identity, package, or
/// range is absent or unsupported.
pub fn parse_ghsa_global(bytes: &[u8], observed_at: u64) -> Result<Advisory, GhsaParseError> {
    if bytes.len() > MAX_ADVISORY_DOCUMENT_BYTES {
        return Err(GhsaParseError::BoundExceeded("document-bytes"));
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| GhsaParseError::InvalidJson)?;
    parse_ghsa_value(&root, observed_at)
}

/// Parses one already-decoded GHSA object without serializing it again.
pub(crate) fn parse_ghsa_value(root: &Value, observed_at: u64) -> Result<Advisory, GhsaParseError> {
    let object = root.as_object().ok_or(GhsaParseError::InvalidJson)?;
    let malware = object
        .get("malware_coverage")
        .and_then(Value::as_bool)
        .or_else(|| {
            object
                .get("database_specific")
                .and_then(Value::as_object)
                .and_then(|value| value.get("malware_coverage"))
                .and_then(Value::as_bool)
        })
        .ok_or(GhsaParseError::MissingMalwareCoverage)?;
    let id = NativeAdvisoryId::new(
        AdvisorySource::Ghsa,
        object
            .get("ghsa_id")
            .or_else(|| object.get("id"))
            .and_then(Value::as_str)
            .ok_or(GhsaParseError::MissingField("ghsa_id"))?,
    )
    .map_err(|_| GhsaParseError::InvalidIdentifier)?;
    let mut aliases = object
        .get("identifiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|item| item.get("value").and_then(Value::as_str))
        .chain(object.get("cve_id").and_then(Value::as_str))
        .map(|value| Alias {
            value: value.to_owned(),
            source: AdvisorySource::Ghsa,
        })
        .collect::<Vec<_>>();
    aliases.sort();
    aliases.dedup();
    let mut affected = ghsa_affected(object)?;
    affected.sort();
    affected.dedup();
    let categories = ghsa_categories(object);
    let severity_text = object
        .get("severity")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Ok(Advisory {
        schema: AdvisorySchema::V1,
        key: AdvisoryKey {
            canonical: CanonicalAdvisoryId(id.id.clone()),
            native: id,
        },
        aliases: aliases.into_boxed_slice(),
        affected: affected.into_boxed_slice(),
        severity: severity_text.as_deref().map_or_else(
            || Severity {
                level: SeverityLevel::Unknown,
                source: None,
                score_hundredths: None,
            },
            severity_from_text,
        ),
        categories: categories.into_boxed_slice(),
        published: optional_text(object, "published_at"),
        modified: optional_text(object, "updated_at"),
        withdrawn: optional_text(object, "withdrawn_at"),
        references: ghsa_references(object),
        malware: if malware {
            MalwareCoverage::Covered
        } else {
            MalwareCoverage::NotCovered
        },
        evidence: Evidence {
            snapshot: None,
            observed_at,
            verification: EvidenceKind::ParsedOnly,
        },
    })
}

fn ghsa_affected(object: &Map<String, Value>) -> Result<Vec<AffectedRange>, GhsaParseError> {
    let vulnerabilities = object
        .get("vulnerabilities")
        .and_then(Value::as_array)
        .ok_or(GhsaParseError::MissingField("vulnerabilities"))?;
    vulnerabilities
        .iter()
        .map(|vulnerability| {
            let item = vulnerability
                .as_object()
                .ok_or(GhsaParseError::InvalidJson)?;
            let package = item
                .get("package")
                .and_then(Value::as_object)
                .ok_or(GhsaParseError::MissingField("vulnerabilities.package"))?;
            let ecosystem = package.get("ecosystem").and_then(Value::as_str).ok_or(
                GhsaParseError::MissingField("vulnerabilities.package.ecosystem"),
            )?;
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .ok_or(GhsaParseError::MissingField("vulnerabilities.package.name"))?;
            let package =
                normalize_package(ecosystem, name).map_err(GhsaParseError::Unsupported)?;
            let raw = item
                .get("vulnerable_version_range")
                .and_then(Value::as_str)
                .ok_or(GhsaParseError::MissingField("vulnerable_version_range"))?;
            let syntax = syntax_for(ecosystem);
            let mut events = parse_github_range(raw, syntax).map_err(|()| {
                GhsaParseError::Unsupported(PackageNormalizationError::UnsupportedVersion {
                    syntax,
                    value: raw.to_owned(),
                })
            })?;
            if let Some(patched) = item
                .get("first_patched_version")
                .and_then(Value::as_object)
                .and_then(|value| value.get("identifier"))
                .and_then(Value::as_str)
            {
                events.push(VersionEvent {
                    kind: VersionEventKind::Fixed,
                    version: patched.to_owned(),
                });
            }
            Ok(AffectedRange {
                package,
                matcher: VersionMatcher::Events {
                    syntax,
                    events: events.into_boxed_slice(),
                },
                exact_versions: Box::new([]),
            })
        })
        .collect()
}

fn ghsa_categories(object: &Map<String, Value>) -> Vec<AdvisoryCategory> {
    let mut categories = vec![AdvisoryCategory::Vulnerability];
    if object
        .get("database_specific")
        .and_then(Value::as_object)
        .and_then(|value| value.get("categories"))
        .and_then(Value::as_array)
        .is_some_and(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .any(|value| value.eq_ignore_ascii_case("malware"))
        })
    {
        categories.push(AdvisoryCategory::Malicious);
    }
    categories.sort();
    categories.dedup();
    categories
}

fn ghsa_references(object: &Map<String, Value>) -> Box<[Reference]> {
    let mut references = object
        .get("references")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_object())
                .filter_map(|item| item.get("url").and_then(Value::as_str))
                .map(|url| Reference {
                    url: url.to_owned(),
                    kind: None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    references.sort();
    references.dedup();
    references.into_boxed_slice()
}
