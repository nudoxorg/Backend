use serde_json::{Map, Value};

use super::model::VersionSyntax;
use super::model::{
    Advisory, AdvisoryCategory, AdvisoryKey, AdvisorySchema, AdvisorySource, AffectedRange, Alias,
    CanonicalAdvisoryId, Evidence, EvidenceKind, MalwareCoverage, NativeAdvisoryId,
    PackageIdentity, Reference, Severity, SeverityLevel, VersionEvent, VersionEventKind,
    VersionMatcher,
};
use super::version::{PackageNormalizationError, normalize_package};

mod ghsa;

pub use ghsa::parse_ghsa_global;
pub(crate) use ghsa::parse_ghsa_value;

/// Maximum size accepted by the standalone parsers.
///
/// The local service applies a tighter source-specific limit before calling us, but keeping a
/// hard ceiling here prevents a caller which uses the portable crate directly from handing the
/// JSON/TOML decoders an unbounded allocation.  Feed entries are capped separately by the
/// authority parser.
pub const MAX_ADVISORY_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of advisory objects admitted from one JSON batch.
pub const MAX_ADVISORY_BATCH_OBJECTS: usize = 100_000;

/// Errors common to OSV and GHSA object admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// Source bytes are not valid JSON or have the wrong shape.
    InvalidJson,
    /// A required field is absent or has the wrong type.
    MissingField(&'static str),
    /// A source package or range cannot be proven.
    Unsupported(PackageNormalizationError),
    /// A source version event is malformed.
    InvalidRange(String),
    /// The object identifier is not admissible.
    InvalidIdentifier,
    /// The source document exceeded the parser's hard bound.
    BoundExceeded(&'static str),
}

/// RustSec-specific parse failures retain the TOML context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RustSecParseError {
    /// TOML syntax error.
    InvalidToml,
    /// Required advisory field is absent.
    MissingField(&'static str),
    /// Package/range semantics cannot be proven.
    Unsupported(PackageNormalizationError),
    /// A patched or unaffected requirement is not valid semver syntax.
    InvalidRange(String),
    /// Identifier is malformed.
    InvalidIdentifier,
    /// The source document exceeded the parser's hard bound.
    BoundExceeded(&'static str),
}

/// GHSA global advisory parse failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GhsaParseError {
    /// JSON shape or syntax is invalid.
    InvalidJson,
    /// Required field is absent or malformed.
    MissingField(&'static str),
    /// A malware coverage declaration is mandatory for this feed.
    MissingMalwareCoverage,
    /// Package/range semantics cannot be proven.
    Unsupported(PackageNormalizationError),
    /// Identifier is malformed.
    InvalidIdentifier,
    /// The source document exceeded the parser's hard bound.
    BoundExceeded(&'static str),
}

/// Parses one OSV JSON advisory object.
///
/// # Errors
///
/// Returns a typed parse error when required fields, package identity, or range semantics are
/// invalid.
pub fn parse_osv(bytes: &[u8], observed_at: u64) -> Result<Advisory, ParseError> {
    if bytes.len() > MAX_ADVISORY_DOCUMENT_BYTES {
        return Err(ParseError::BoundExceeded("document-bytes"));
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| ParseError::InvalidJson)?;
    parse_osv_value(&root, observed_at)
}

/// Parses one already-decoded OSV object without serializing it again.
///
/// This is `pub(crate)` so batch ingestion can retain the single decoder allocation and avoid
/// an otherwise surprisingly expensive `Value -> Vec<u8> -> Value` round trip for every object.
pub(crate) fn parse_osv_value(root: &Value, observed_at: u64) -> Result<Advisory, ParseError> {
    let object = root.as_object().ok_or(ParseError::InvalidJson)?;
    let id = NativeAdvisoryId::new(AdvisorySource::Osv, text(object, "id")?)
        .map_err(|_| ParseError::InvalidIdentifier)?;
    let aliases = aliases(object, AdvisorySource::Osv);
    let mut affected = osv_affected(object)?;
    // OSV permits equivalent package rows in any order.  Stable ordering makes the durable
    // object digest independent of transport/source-file ordering and lets downstream indexes
    // compare one compact prefix before touching the ranges.
    affected.sort();
    affected.dedup();
    Ok(Advisory {
        schema: AdvisorySchema::V1,
        key: AdvisoryKey {
            canonical: CanonicalAdvisoryId(id.id.clone()),
            native: id,
        },
        aliases: aliases.into_boxed_slice(),
        affected: affected.into_boxed_slice(),
        severity: osv_severity(object),
        categories: categories(object),
        published: optional_text(object, "published"),
        modified: optional_text(object, "modified"),
        withdrawn: optional_withdrawn(object),
        references: references(object),
        malware: MalwareCoverage::NotCovered,
        evidence: Evidence {
            snapshot: None,
            observed_at,
            verification: EvidenceKind::ParsedOnly,
        },
    })
}

/// Parses a `RustSec` advisory TOML document.
///
/// # Errors
///
/// Returns a typed parse error when the document is malformed or its semver requirements cannot
/// be proven.
pub fn parse_rustsec(bytes: &[u8], observed_at: u64) -> Result<Advisory, RustSecParseError> {
    if bytes.len() > MAX_ADVISORY_DOCUMENT_BYTES {
        return Err(RustSecParseError::BoundExceeded("document-bytes"));
    }
    let root: toml::Value = std::str::from_utf8(bytes)
        .map_err(|_| RustSecParseError::InvalidToml)?
        .parse()
        .map_err(|_| RustSecParseError::InvalidToml)?;
    let advisory = root
        .get("advisory")
        .and_then(toml::Value::as_table)
        .ok_or(RustSecParseError::MissingField("advisory"))?;
    let id = NativeAdvisoryId::new(
        AdvisorySource::RustSec,
        advisory
            .get("id")
            .and_then(toml::Value::as_str)
            .ok_or(RustSecParseError::MissingField("advisory.id"))?,
    )
    .map_err(|_| RustSecParseError::InvalidIdentifier)?;
    let package_name = advisory
        .get("package")
        .and_then(toml::Value::as_str)
        .ok_or(RustSecParseError::MissingField("advisory.package"))?;
    let package =
        normalize_package("cargo", package_name).map_err(RustSecParseError::Unsupported)?;
    let versions = root
        .get("versions")
        .and_then(toml::Value::as_table)
        .ok_or(RustSecParseError::MissingField("versions"))?;
    let patched = string_array(versions.get("patched"));
    let unaffected = string_array(versions.get("unaffected"));
    for requirement in patched.iter().chain(&unaffected) {
        super::version::validate_rustsec_requirement(requirement)
            .map_err(RustSecParseError::InvalidRange)?;
    }
    let range = AffectedRange {
        package,
        matcher: VersionMatcher::RustSec {
            patched: patched.into_boxed_slice(),
            unaffected: unaffected.into_boxed_slice(),
        },
        exact_versions: Box::new([]),
    };
    let aliases = string_array(advisory.get("aliases"))
        .into_iter()
        .map(|value| Alias {
            value,
            source: AdvisorySource::RustSec,
        })
        .collect::<Vec<_>>();
    let mut categories = string_array(advisory.get("categories"))
        .into_iter()
        .map(|value| category(&value))
        .collect::<Vec<_>>();
    categories.sort();
    categories.dedup();
    let published = advisory
        .get("date")
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned);
    let withdrawn = advisory
        .get("withdrawn")
        .and_then(toml::Value::as_str)
        .map(ToOwned::to_owned);
    Ok(Advisory {
        schema: AdvisorySchema::V1,
        key: AdvisoryKey {
            canonical: CanonicalAdvisoryId(id.id.clone()),
            native: id,
        },
        aliases: aliases.into_boxed_slice(),
        affected: Box::new([range]),
        severity: Severity {
            level: SeverityLevel::Unknown,
            source: None,
            score_hundredths: None,
        },
        categories: if categories.is_empty() {
            Box::new([AdvisoryCategory::Vulnerability])
        } else {
            categories.into_boxed_slice()
        },
        published,
        modified: None,
        withdrawn,
        references: advisory
            .get("url")
            .and_then(toml::Value::as_str)
            .map(|url| Reference {
                url: url.to_owned(),
                kind: Some("advisory".to_owned()),
            })
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        malware: MalwareCoverage::NotCovered,
        evidence: Evidence {
            snapshot: None,
            observed_at,
            verification: EvidenceKind::ParsedOnly,
        },
    })
}

fn osv_affected(object: &Map<String, Value>) -> Result<Vec<AffectedRange>, ParseError> {
    let rows = object
        .get("affected")
        .and_then(Value::as_array)
        .ok_or(ParseError::MissingField("affected"))?;
    let mut output = Vec::with_capacity(rows.len());
    for row in rows {
        let row = row.as_object().ok_or(ParseError::InvalidJson)?;
        let package = row
            .get("package")
            .and_then(Value::as_object)
            .ok_or(ParseError::MissingField("affected.package"))?;
        let ecosystem = package
            .get("ecosystem")
            .and_then(Value::as_str)
            .ok_or(ParseError::MissingField("affected.package.ecosystem"))?;
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ParseError::MissingField("affected.package.name"))?;
        let mut package = normalize_package(ecosystem, name).map_err(ParseError::Unsupported)?;
        package.canonical_purl = package_object_purl(&package, ecosystem, row);
        let syntax = syntax_for(ecosystem);
        let ranges = row.get("ranges").and_then(Value::as_array);
        let mut events = Vec::new();
        let mut unsupported = None;
        for range in ranges.into_iter().flatten() {
            let range = range.as_object().ok_or(ParseError::InvalidJson)?;
            let kind = range
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(kind, "SEMVER" | "ECOSYSTEM")
                || (kind == "SEMVER"
                    && !matches!(syntax, VersionSyntax::Semver | VersionSyntax::Go))
            {
                unsupported = Some(kind.to_owned());
                continue;
            }
            if let Some(row_events) = range.get("events").and_then(Value::as_array) {
                for event in row_events {
                    let event = event.as_object().ok_or(ParseError::InvalidJson)?;
                    for (key, value) in event {
                        let kind = match key.as_str() {
                            "introduced" => VersionEventKind::Introduced,
                            "fixed" => VersionEventKind::Fixed,
                            "last_affected" => VersionEventKind::LastAffected,
                            "limit" => VersionEventKind::Limit,
                            _ => return Err(ParseError::InvalidRange(key.clone())),
                        };
                        let version = value
                            .as_str()
                            .ok_or(ParseError::InvalidRange(key.clone()))?;
                        events.push(VersionEvent {
                            kind,
                            version: version.to_owned(),
                        });
                    }
                }
            }
        }
        let mut exact_versions = row
            .get("versions")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        exact_versions.sort();
        exact_versions.dedup();
        let matcher = unsupported.map_or_else(
            || VersionMatcher::Events {
                syntax,
                events: events.into_boxed_slice(),
            },
            |raw| VersionMatcher::Unsupported {
                raw,
                reason: "OSV range type is not proven".to_owned(),
            },
        );
        output.push(AffectedRange {
            package,
            matcher,
            exact_versions: exact_versions.into_boxed_slice(),
        });
    }
    Ok(output)
}

fn parse_github_range(raw: &str, syntax: VersionSyntax) -> Result<Vec<VersionEvent>, ()> {
    if raw.trim() == "*" || raw.trim().is_empty() {
        return Ok(vec![VersionEvent {
            kind: VersionEventKind::Introduced,
            version: String::new(),
        }]);
    }
    let mut output = Vec::new();
    let mut pending_operator = None;
    for token in raw.split(|character: char| character.is_whitespace() || character == ',') {
        if token.is_empty() {
            continue;
        }
        if let Some(operator) = pending_operator.take() {
            let token = format!("{operator}{token}");
            parse_github_token(&token, syntax, &mut output)?;
            continue;
        }
        if matches!(token, ">" | ">=" | "<" | "<=" | "=" | "^" | "~") {
            pending_operator = Some(token);
            continue;
        }
        parse_github_token(token, syntax, &mut output)?;
    }
    if pending_operator.is_some() {
        return Err(());
    }
    Ok(output)
}

fn parse_github_token(
    token: &str,
    syntax: VersionSyntax,
    output: &mut Vec<VersionEvent>,
) -> Result<(), ()> {
    let (kind, version) = if let Some(value) = token.strip_prefix(">=") {
        (VersionEventKind::Introduced, value)
    } else if let Some(value) = token.strip_prefix("<") {
        if let Some(value) = value.strip_prefix('=') {
            (VersionEventKind::LastAffected, value)
        } else {
            (VersionEventKind::Limit, value)
        }
    } else {
        return Err(());
    };
    if version.is_empty() || matches!(syntax, VersionSyntax::Unsupported) {
        return Err(());
    }
    output.push(VersionEvent {
        kind,
        version: version.to_owned(),
    });
    Ok(())
}

fn syntax_for(ecosystem: &str) -> VersionSyntax {
    match ecosystem.to_ascii_lowercase().as_str() {
        "cargo" | "npm" | "nuget" => VersionSyntax::Semver,
        "pypi" | "python" => VersionSyntax::Pep440,
        "maven" => VersionSyntax::Maven,
        "golang" | "go" => VersionSyntax::Go,
        "cpp" | "conan" => VersionSyntax::Conan,
        _ => VersionSyntax::Unsupported,
    }
}

fn aliases(object: &Map<String, Value>, source: AdvisorySource) -> Vec<Alias> {
    let mut aliases: Vec<Alias> = object
        .get("aliases")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|value| Alias {
                    value: value.to_owned(),
                    source,
                })
                .collect()
        })
        .unwrap_or_default();
    aliases.sort();
    aliases.dedup();
    aliases
}

fn references(object: &Map<String, Value>) -> Box<[Reference]> {
    let mut references: Vec<Reference> = object
        .get("references")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|item| {
                    item.get("url")
                        .and_then(Value::as_str)
                        .map(|url| Reference {
                            url: url.to_owned(),
                            kind: item
                                .get("type")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                        })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    references.sort();
    references.dedup();
    references.into_boxed_slice()
}

fn package_object_purl(
    package: &PackageIdentity,
    ecosystem: &str,
    row: &Map<String, Value>,
) -> Option<String> {
    row.get("package")
        .and_then(Value::as_object)
        .and_then(|package| package.get("purl").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .or_else(|| {
            Some(format!(
                "pkg:{}/{}",
                ecosystem.to_ascii_lowercase(),
                package.name
            ))
        })
}

fn categories(object: &Map<String, Value>) -> Box<[AdvisoryCategory]> {
    let mut categories = object
        .get("database_specific")
        .and_then(Value::as_object)
        .and_then(|v| v.get("categories"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(category)
                .collect::<Vec<_>>()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| vec![AdvisoryCategory::Vulnerability]);
    categories.sort();
    categories.dedup();
    categories.into_boxed_slice()
}

fn category(value: &str) -> AdvisoryCategory {
    if value.eq_ignore_ascii_case("malware") || value.eq_ignore_ascii_case("malicious") {
        AdvisoryCategory::Malicious
    } else if value.eq_ignore_ascii_case("unmaintained") {
        AdvisoryCategory::Unmaintained
    } else if value.eq_ignore_ascii_case("unsound") {
        AdvisoryCategory::Unsound
    } else {
        AdvisoryCategory::Vulnerability
    }
}

fn osv_severity(object: &Map<String, Value>) -> Severity {
    object
        .get("severity")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_object)
        .and_then(|item| item.get("score").and_then(Value::as_str))
        .map_or_else(
            || Severity {
                level: SeverityLevel::Unknown,
                source: None,
                score_hundredths: None,
            },
            severity_from_text,
        )
}

fn severity_from_text(value: &str) -> Severity {
    let score_hundredths = decimal_score_hundredths(value);
    let level = score_hundredths.map_or_else(
        || match value.to_ascii_lowercase().as_str() {
            "low" => SeverityLevel::Low,
            "moderate" | "medium" => SeverityLevel::Moderate,
            "high" => SeverityLevel::High,
            "critical" => SeverityLevel::Critical,
            _ => SeverityLevel::Unknown,
        },
        |score| match score {
            0..=399 => SeverityLevel::Low,
            400..=699 => SeverityLevel::Moderate,
            700..=899 => SeverityLevel::High,
            _ => SeverityLevel::Critical,
        },
    );
    Severity {
        level,
        source: Some(value.to_owned()),
        score_hundredths,
    }
}

fn decimal_score_hundredths(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<u16>().ok()?;
    if whole > 10 || fraction.bytes().any(|byte| !byte.is_ascii_digit()) {
        return None;
    }
    let mut digits = fraction.bytes().take(3);
    let first = digits.next().map_or(0, |byte| u16::from(byte - b'0'));
    let second = digits.next().map_or(0, |byte| u16::from(byte - b'0'));
    let round = digits.next().is_some_and(|byte| byte >= b'5');
    let fractional = first * 10 + second + u16::from(round);
    let score = whole
        .checked_mul(100)
        .and_then(|score| score.checked_add(fractional))?;
    (score <= 1000).then_some(score)
}

fn string_array(value: Option<&toml::Value>) -> Vec<String> {
    let mut strings: Vec<String> = value
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    strings.sort();
    strings.dedup();
    strings
}

fn text<'a>(object: &'a Map<String, Value>, key: &'static str) -> Result<&'a str, ParseError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or(ParseError::MissingField(key))
}
fn optional_text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
fn optional_withdrawn(object: &Map<String, Value>) -> Option<String> {
    object.get("withdrawn").and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str().map(ToOwned::to_owned)
        }
    })
}
