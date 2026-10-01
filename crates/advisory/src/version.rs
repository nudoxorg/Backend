use std::cmp::Ordering;

use super::model::{
    AffectedRange, PackageIdentity, VersionEventKind, VersionMatcher, VersionSyntax,
};

/// Why a package or version could not be proven equivalent.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PackageNormalizationError {
    /// Ecosystem token was not one of the supported registry families.
    UnsupportedEcosystem(
        /// Original ecosystem token.
        String,
    ),
    /// Name was empty or exceeded the bounded identity size.
    InvalidName,
    /// A range or version uses semantics this build cannot prove.
    UnsupportedVersion {
        /// Grammar which could not be proven.
        syntax: VersionSyntax,
        /// Original range/version text.
        value: String,
    },
}

/// A comparison failed because the source versions are not in one grammar.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VersionCompareError {
    /// The syntax was explicitly unsupported.
    Unsupported(
        /// Grammar which cannot be compared by this build.
        VersionSyntax,
    ),
    /// The text is not valid in its syntax.
    Invalid {
        /// Grammar that rejected the value.
        syntax: VersionSyntax,
        /// Original version text.
        value: String,
    },
}

/// A normalized identity with its original ecosystem spelling removed.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NormalizedVersion {
    /// Proven grammar.
    pub syntax: VersionSyntax,
    /// Canonical version spelling used in journal keys.
    pub canonical: String,
    key: VersionKey,
}

impl Ord for NormalizedVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .cmp(&other.key)
            .then_with(|| self.syntax.cmp(&other.syntax))
    }
}

impl PartialOrd for NormalizedVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum VersionKey {
    Semver {
        release: Box<[u64]>,
        pre: Box<[String]>,
    },
    Pep440 {
        epoch: u64,
        release: Box<[u64]>,
        phase: PepPhase,
        serial: u64,
    },
    Maven(Box<[MavenPart]>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
enum PepPhase {
    Dev = 0,
    Alpha = 1,
    Beta = 2,
    Rc = 3,
    Final = 4,
    Post = 5,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum MavenPart {
    Number(u64),
    Text(String),
}

/// Canonicalizes a package name under one of the supported registry grammars.
///
/// # Errors
///
/// Returns an ecosystem-specific error when the package name is malformed or the ecosystem is
/// outside the proven set.
pub fn normalize_package(
    ecosystem: &str,
    name: &str,
) -> Result<PackageIdentity, PackageNormalizationError> {
    let ecosystem = match ecosystem.trim().to_ascii_lowercase().as_str() {
        "python" => "pypi".to_owned(),
        "golang" | "go" => "go".to_owned(),
        // OSV names Cargo's registry by its host.
        "crates.io" => "cargo".to_owned(),
        "generic" => "conan".to_owned(),
        value => value.to_owned(),
    };
    if name.is_empty() || name.len() > 512 || name.bytes().any(|b| b.is_ascii_control()) {
        return Err(PackageNormalizationError::InvalidName);
    }
    let name = match ecosystem.as_str() {
        "cargo" => name.trim().replace('_', "-").to_ascii_lowercase(),
        "npm" | "nuget" => name.trim().to_ascii_lowercase(),
        "pypi" | "python" => name
            .trim()
            .chars()
            .map(|c| {
                if matches!(c, '-' | '_' | '.') {
                    '-'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect(),
        "maven" | "golang" | "go" | "cpp" | "conan" => name.trim().to_owned(),
        _ => return Err(PackageNormalizationError::UnsupportedEcosystem(ecosystem)),
    };
    if name.is_empty() {
        return Err(PackageNormalizationError::InvalidName);
    }
    Ok(PackageIdentity {
        ecosystem,
        name,
        canonical_purl: None,
    })
}

/// Parses and canonicalizes one version with a proven grammar.
///
/// # Errors
///
/// Returns a typed error when the grammar is unsupported or the version cannot be parsed.
pub fn normalize_version(
    syntax: VersionSyntax,
    value: &str,
) -> Result<NormalizedVersion, VersionCompareError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(VersionCompareError::Invalid {
            syntax,
            value: value.to_owned(),
        });
    }
    let (canonical, key) = match syntax {
        VersionSyntax::Semver | VersionSyntax::Go | VersionSyntax::Conan => {
            // Build metadata does not participate in SemVer precedence.  Dropping it before
            // parsing also admits the versions emitted by npm/Cargo registries verbatim instead
            // of treating a harmless `+platform` suffix as an invalid release.
            let value = value.split_once('+').map_or(value, |(value, _)| value);
            let canonical = value.strip_prefix('v').unwrap_or(value).to_owned();
            let key = parse_semver(&canonical)?;
            (canonical, key)
        }
        VersionSyntax::Pep440 => {
            let (canonical, key) = parse_pep440(value)?;
            (canonical, key)
        }
        VersionSyntax::Maven => {
            let key = parse_maven(value)?;
            let canonical = canonical_maven(&key);
            (canonical, key)
        }
        VersionSyntax::Unsupported => {
            return Err(VersionCompareError::Unsupported(VersionSyntax::Unsupported));
        }
    };
    Ok(NormalizedVersion {
        syntax,
        canonical,
        key,
    })
}

/// Returns whether a release is covered by one source range.
///
/// # Errors
///
/// Returns a typed comparison error when the source range or candidate is outside the proven
/// grammar.
pub fn matches(matcher: &VersionMatcher, version: &str) -> Result<bool, VersionCompareError> {
    match matcher {
        VersionMatcher::Events { syntax, events } => {
            let candidate = normalize_version(*syntax, version)?;
            let mut affected = false;
            for event in events {
                if event.version.is_empty() {
                    if event.kind == VersionEventKind::Introduced {
                        affected = true;
                    }
                    continue;
                }
                let boundary = normalize_version(*syntax, &event.version)?;
                match event.kind {
                    VersionEventKind::Introduced => {
                        if candidate.key >= boundary.key {
                            affected = true;
                        }
                    }
                    VersionEventKind::Fixed | VersionEventKind::Limit => {
                        if candidate.key >= boundary.key {
                            affected = false;
                        }
                    }
                    VersionEventKind::LastAffected => {
                        if candidate.key > boundary.key {
                            affected = false;
                        } else if candidate.key >= boundary.key {
                            affected = true;
                        }
                    }
                }
            }
            Ok(affected)
        }
        VersionMatcher::RustSec {
            patched,
            unaffected,
        } => {
            let candidate = normalize_version(VersionSyntax::Semver, version)?;
            if unaffected.iter().any(|value| {
                rustsec_requirement_matches(value, &candidate, false).is_ok_and(|matched| matched)
            }) {
                return Ok(false);
            }
            for fixed in patched {
                if rustsec_requirement_matches(fixed, &candidate, true)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        VersionMatcher::Unsupported { .. } => {
            Err(VersionCompareError::Unsupported(VersionSyntax::Unsupported))
        }
    }
}

/// Validates a `RustSec` semver requirement while preserving the original source spelling in the
/// durable matcher.  The parser calls this before admission so unsupported requirements cannot
/// silently turn into an empty range.
pub(crate) fn validate_rustsec_requirement(value: &str) -> Result<(), String> {
    let candidate = normalize_version(VersionSyntax::Semver, "0.0.0")
        .map_err(|_| "invalid semver sentinel".to_owned())?;
    rustsec_requirement_matches(value, &candidate, false)
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

fn rustsec_requirement_matches(
    requirement: &str,
    candidate: &NormalizedVersion,
    patched: bool,
) -> Result<bool, VersionCompareError> {
    let tokens = requirement_tokens(requirement);
    if tokens.is_empty() {
        return Err(VersionCompareError::Invalid {
            syntax: VersionSyntax::Semver,
            value: requirement.to_owned(),
        });
    }
    let parsed_tokens = tokens
        .iter()
        .map(|token| {
            let (operator, value) = split_requirement_token(token);
            if !matches!(operator, "" | ">=" | ">" | "<=" | "<" | "=" | "^" | "~") {
                return Err(VersionCompareError::Invalid {
                    syntax: VersionSyntax::Semver,
                    value: requirement.to_owned(),
                });
            }
            normalize_version(VersionSyntax::Semver, value).map(|parsed| (operator, parsed))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (operator, parsed) in parsed_tokens {
        let matched = match operator {
            "" => {
                if patched {
                    candidate.key >= parsed.key
                } else {
                    candidate.key == parsed.key
                }
            }
            ">=" => candidate.key >= parsed.key,
            ">" => candidate.key > parsed.key,
            "<=" => candidate.key <= parsed.key,
            "<" => candidate.key < parsed.key,
            "=" => candidate.key == parsed.key,
            "^" => candidate.key >= parsed.key && candidate.key < caret_upper(&parsed.key),
            "~" => candidate.key >= parsed.key && candidate.key < tilde_upper(&parsed.key),
            _ => {
                return Err(VersionCompareError::Invalid {
                    syntax: VersionSyntax::Semver,
                    value: requirement.to_owned(),
                });
            }
        };
        if !matched {
            return Ok(false);
        }
    }
    Ok(true)
}

fn requirement_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut pending_operator = None;
    for raw in value.split(|character: char| character.is_whitespace() || character == ',') {
        if raw.is_empty() {
            continue;
        }
        if let Some(operator) = pending_operator.take() {
            tokens.push(format!("{operator}{raw}"));
        } else if matches!(raw, ">" | ">=" | "<" | "<=" | "=" | "^" | "~") {
            pending_operator = Some(raw.to_owned());
        } else {
            tokens.push(raw.to_owned());
        }
    }
    if let Some(operator) = pending_operator {
        // Leave a malformed trailing operator visible to the validator.
        tokens.push(operator);
    }
    tokens
}

fn split_requirement_token(token: &str) -> (&str, &str) {
    for operator in [">=", "<=", ">", "<", "=", "^", "~"] {
        if let Some(value) = token.strip_prefix(operator) {
            return (operator, value);
        }
    }
    ("", token)
}

fn caret_upper(version: &VersionKey) -> VersionKey {
    let VersionKey::Semver { release, .. } = version else {
        return version.clone();
    };
    let mut upper = release.to_vec();
    while upper.len() < 3 {
        upper.push(0);
    }
    if upper[0] > 0 {
        upper[0] = upper[0].saturating_add(1);
        upper[1] = 0;
        upper[2] = 0;
    } else if upper[1] > 0 {
        upper[1] = upper[1].saturating_add(1);
        upper[2] = 0;
    } else {
        upper[2] = upper[2].saturating_add(1);
    }
    VersionKey::Semver {
        release: upper.into_boxed_slice(),
        pre: Box::new([]),
    }
}

fn tilde_upper(version: &VersionKey) -> VersionKey {
    let VersionKey::Semver { release, .. } = version else {
        return version.clone();
    };
    let mut upper = release.to_vec();
    while upper.len() < 2 {
        upper.push(0);
    }
    upper[1] = upper[1].saturating_add(1);
    upper.truncate(2);
    VersionKey::Semver {
        release: upper.into_boxed_slice(),
        pre: Box::new([]),
    }
}

/// Returns whether a release is covered by an affected claim, including exact versions.
///
/// # Errors
///
/// Returns a typed comparison error when the affected claim is unsupported or malformed.
pub fn range_matches(range: &AffectedRange, version: &str) -> Result<bool, VersionCompareError> {
    if range.exact_versions.iter().any(|exact| exact == version) {
        return Ok(true);
    }
    let syntax = match &range.matcher {
        VersionMatcher::Events { syntax, .. } => *syntax,
        VersionMatcher::RustSec { .. } => VersionSyntax::Semver,
        VersionMatcher::Unsupported { .. } => VersionSyntax::Unsupported,
    };
    if range.exact_versions.iter().any(|exact| {
        normalize_version(syntax, exact)
            .and_then(|left| normalize_version(syntax, version).map(|right| left.key == right.key))
            .unwrap_or(false)
    }) {
        return Ok(true);
    }
    matches(&range.matcher, version)
}

fn parse_semver(value: &str) -> Result<VersionKey, VersionCompareError> {
    let (core, pre) = value.split_once('-').map_or((value, ""), |(a, b)| (a, b));
    let release: Vec<u64> = core
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| VersionCompareError::Invalid {
                    syntax: VersionSyntax::Semver,
                    value: value.to_owned(),
                })
        })
        .collect::<Result<_, _>>()?;
    if release.is_empty() || release.len() > 4 {
        return Err(VersionCompareError::Invalid {
            syntax: VersionSyntax::Semver,
            value: value.to_owned(),
        });
    }
    let pre: Box<[String]> = pre
        .split('.')
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok(VersionKey::Semver {
        release: release.into_boxed_slice(),
        pre,
    })
}

fn parse_pep440(value: &str) -> Result<(String, VersionKey), VersionCompareError> {
    let (epoch, remainder) = if let Some((epoch, remainder)) = value.split_once('!') {
        (
            epoch
                .parse::<u64>()
                .map_err(|_| VersionCompareError::Invalid {
                    syntax: VersionSyntax::Pep440,
                    value: value.to_owned(),
                })?,
            remainder,
        )
    } else {
        (0, value)
    };
    let remainder = remainder.split('+').next().unwrap_or(remainder);
    let marker = remainder
        .find(|character: char| character.is_ascii_alphabetic())
        .unwrap_or(remainder.len());
    let release = remainder[..marker].trim_end_matches(['.', '-']);
    let suffix = remainder[marker..].trim_start_matches(['.', '-']);
    let release: Vec<u64> = release
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| VersionCompareError::Invalid {
                    syntax: VersionSyntax::Pep440,
                    value: value.to_owned(),
                })
        })
        .collect::<Result<_, _>>()?;
    if release.is_empty() {
        return Err(VersionCompareError::Invalid {
            syntax: VersionSyntax::Pep440,
            value: value.to_owned(),
        });
    }
    let suffix = suffix.to_ascii_lowercase();
    let (phase, serial) = if suffix.is_empty() {
        (PepPhase::Final, 0)
    } else if let Some(serial) = suffix
        .strip_prefix("alpha")
        .or_else(|| suffix.strip_prefix('a'))
    {
        (PepPhase::Alpha, parse_pep_serial(serial, value)?)
    } else if let Some(serial) = suffix
        .strip_prefix("beta")
        .or_else(|| suffix.strip_prefix('b'))
    {
        (PepPhase::Beta, parse_pep_serial(serial, value)?)
    } else if let Some(serial) = suffix.strip_prefix("rc") {
        (PepPhase::Rc, parse_pep_serial(serial, value)?)
    } else if let Some(serial) = suffix
        .strip_prefix("post")
        .or_else(|| suffix.strip_prefix("rev"))
        .or_else(|| suffix.strip_prefix('r'))
    {
        (PepPhase::Post, parse_pep_serial(serial, value)?)
    } else if let Some(serial) = suffix.strip_prefix("dev") {
        (PepPhase::Dev, parse_pep_serial(serial, value)?)
    } else {
        return Err(VersionCompareError::Invalid {
            syntax: VersionSyntax::Pep440,
            value: value.to_owned(),
        });
    };
    let canonical = format!(
        "{epoch}!{}{}",
        release
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
        pep_suffix(phase, serial)
    );
    Ok((
        canonical,
        VersionKey::Pep440 {
            epoch,
            release: release.into_boxed_slice(),
            phase,
            serial,
        },
    ))
}

fn parse_pep_serial(serial: &str, value: &str) -> Result<u64, VersionCompareError> {
    if serial.is_empty() {
        return Ok(0);
    }
    serial
        .parse::<u64>()
        .map_err(|_| VersionCompareError::Invalid {
            syntax: VersionSyntax::Pep440,
            value: value.to_owned(),
        })
}

fn pep_suffix(phase: PepPhase, serial: u64) -> String {
    match phase {
        PepPhase::Dev => format!(".dev{serial}"),
        PepPhase::Alpha => format!("a{serial}"),
        PepPhase::Beta => format!("b{serial}"),
        PepPhase::Rc => format!("rc{serial}"),
        PepPhase::Final => String::new(),
        PepPhase::Post => format!(".post{serial}"),
    }
}

impl Ord for VersionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let ordering = version_kind(self).cmp(&version_kind(other));
        if ordering != Ordering::Equal {
            return ordering;
        }
        match (self, other) {
            (
                Self::Semver {
                    release: left,
                    pre: left_pre,
                },
                Self::Semver {
                    release: right,
                    pre: right_pre,
                },
            ) => compare_release(left, right).then_with(|| compare_pre(left_pre, right_pre)),
            (
                Self::Pep440 {
                    epoch: left_epoch,
                    release: left,
                    phase: left_phase,
                    serial: left_serial,
                },
                Self::Pep440 {
                    epoch: right_epoch,
                    release: right,
                    phase: right_phase,
                    serial: right_serial,
                },
            ) => left_epoch
                .cmp(right_epoch)
                .then_with(|| compare_release(left, right))
                .then_with(|| left_phase.cmp(right_phase))
                .then_with(|| left_serial.cmp(right_serial)),
            (Self::Maven(left), Self::Maven(right)) => compare_maven(left, right),
            _ => Ordering::Equal,
        }
    }
}

fn version_kind(value: &VersionKey) -> u8 {
    match value {
        VersionKey::Maven(_) => 0,
        VersionKey::Semver { .. } => 1,
        VersionKey::Pep440 { .. } => 2,
    }
}

impl PartialOrd for VersionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_release(left: &[u64], right: &[u64]) -> Ordering {
    let width = left.len().max(right.len());
    (0..width)
        .map(|index| {
            left.get(index)
                .copied()
                .unwrap_or(0)
                .cmp(&right.get(index).copied().unwrap_or(0))
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

fn compare_pre(left: &[String], right: &[String]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .iter()
            .zip(right)
            .map(|(left, right)| compare_pre_token(left, right))
            .find(|ordering| *ordering != Ordering::Equal)
            .unwrap_or_else(|| left.len().cmp(&right.len())),
    }
}

fn compare_pre_token(left: &str, right: &str) -> Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => left.cmp(right),
    }
}

fn compare_maven(left: &[MavenPart], right: &[MavenPart]) -> Ordering {
    let width = left.len().max(right.len());
    for index in 0..width {
        let Some(left) = left.get(index) else {
            return Ordering::Less;
        };
        let Some(right) = right.get(index) else {
            return Ordering::Greater;
        };
        let ordering = match (left, right) {
            (MavenPart::Number(left), MavenPart::Number(right)) => left.cmp(right),
            (MavenPart::Text(left), MavenPart::Text(right)) => qualifier_rank(left)
                .cmp(&qualifier_rank(right))
                .then_with(|| left.cmp(right)),
            (MavenPart::Number(_), MavenPart::Text(_)) => Ordering::Greater,
            (MavenPart::Text(_), MavenPart::Number(_)) => Ordering::Less,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn qualifier_rank(value: &str) -> u8 {
    match value {
        "alpha" | "a" => 0,
        "beta" | "b" => 1,
        "rc" => 2,
        "snapshot" => 3,
        "final" | "ga" | "release" => 5,
        _ => 4,
    }
}

fn parse_maven(value: &str) -> Result<VersionKey, VersionCompareError> {
    let mut parts = Vec::new();
    for token in value.split(['.', '-', '_', '+']) {
        if token.is_empty() {
            continue;
        }
        if let Ok(number) = token.parse::<u64>() {
            parts.push(MavenPart::Number(number));
        } else if token.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            parts.push(MavenPart::Text(token.to_ascii_lowercase()));
        } else {
            return Err(VersionCompareError::Invalid {
                syntax: VersionSyntax::Maven,
                value: value.to_owned(),
            });
        }
    }
    if parts.is_empty() {
        return Err(VersionCompareError::Invalid {
            syntax: VersionSyntax::Maven,
            value: value.to_owned(),
        });
    }
    // Maven treats trailing numeric zero segments as equivalent (`1.0`, `1.0.0`, and `1` are
    // the same release).  Canonicalizing them once keeps range matching and cache keys stable.
    while parts.len() > 1 && matches!(parts.last(), Some(MavenPart::Number(0))) {
        parts.pop();
    }
    Ok(VersionKey::Maven(parts.into_boxed_slice()))
}

fn canonical_maven(key: &VersionKey) -> String {
    let VersionKey::Maven(parts) = key else {
        return String::new();
    };
    parts
        .iter()
        .map(|part| match part {
            MavenPart::Number(value) => value.to_string(),
            MavenPart::Text(value) => value.clone(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

// ---------------------------------------------------------------------------
// Cargo requirements: the one semver authority for dependency questions
// ---------------------------------------------------------------------------

/// One Cargo release: `major.minor.patch-pre`, build metadata dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoVersion {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Box<[String]>,
}

impl CargoVersion {
    fn parse(value: &str) -> Result<Self, VersionCompareError> {
        let invalid = || VersionCompareError::Invalid {
            syntax: VersionSyntax::Semver,
            value: value.to_owned(),
        };
        let core = value
            .trim()
            .split_once('+')
            .map_or(value.trim(), |(core, _)| core);
        let (release, pre) = core
            .split_once('-')
            .map_or((core, ""), |(release, pre)| (release, pre));
        let mut parts = release.split('.');
        let mut number = || -> Result<u64, VersionCompareError> {
            parts
                .next()
                .ok_or_else(invalid)?
                .parse::<u64>()
                .map_err(|_| invalid())
        };
        let (major, minor, patch) = (number()?, number()?, number()?);
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre: pre
                .split('.')
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        })
    }

    fn cmp_pre(&self, pre: &[String]) -> Ordering {
        compare_pre(&self.pre, pre)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CargoOp {
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
    Tilde,
    Caret,
    Wildcard,
}

/// One comparator of a Cargo requirement; partial versions keep `None`.
#[derive(Clone, Debug)]
struct CargoComparator {
    op: CargoOp,
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Box<[String]>,
}

impl CargoComparator {
    fn parse(text: &str, requirement: &str) -> Result<Self, VersionCompareError> {
        let invalid = || VersionCompareError::Invalid {
            syntax: VersionSyntax::Semver,
            value: requirement.to_owned(),
        };
        let text = text.trim();
        let (mut op, rest) = [
            (">=", CargoOp::GreaterEq),
            ("<=", CargoOp::LessEq),
            (">", CargoOp::Greater),
            ("<", CargoOp::Less),
            ("=", CargoOp::Exact),
            ("~", CargoOp::Tilde),
            ("^", CargoOp::Caret),
        ]
        .into_iter()
        .find_map(|(token, op)| text.strip_prefix(token).map(|rest| (op, rest.trim_start())))
        .unwrap_or((CargoOp::Caret, text));
        let rest = rest.split_once('+').map_or(rest, |(core, _)| core);
        let (release, pre) = rest
            .split_once('-')
            .map_or((rest, ""), |(release, pre)| (release, pre));
        let wild = |part: &str| matches!(part, "*" | "x" | "X");
        let mut parts = release.split('.');
        let major_text = parts.next().ok_or_else(invalid)?;
        if wild(major_text) {
            return Err(invalid());
        }
        let major = major_text.parse::<u64>().map_err(|_| invalid())?;
        let mut component =
            |parts: &mut std::str::Split<'_, char>| -> Result<Option<u64>, VersionCompareError> {
                match parts.next() {
                    None => Ok(None),
                    Some(part) if wild(part) => {
                        op = CargoOp::Wildcard;
                        Ok(None)
                    }
                    Some(part) => part.parse::<u64>().map(Some).map_err(|_| invalid()),
                }
            };
        let minor = component(&mut parts)?;
        let patch = if minor.is_some() {
            component(&mut parts)?
        } else {
            None
        };
        if parts.next().is_some() || (!pre.is_empty() && patch.is_none()) {
            return Err(invalid());
        }
        Ok(Self {
            op,
            major,
            minor,
            patch,
            pre: pre
                .split('.')
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        })
    }

    fn exact(&self, version: &CargoVersion) -> bool {
        version.major == self.major
            && self.minor.is_none_or(|minor| version.minor == minor)
            && self.patch.is_none_or(|patch| version.patch == patch)
            && (self.patch.is_none() || version.cmp_pre(&self.pre) == Ordering::Equal)
    }

    fn greater(&self, version: &CargoVersion) -> bool {
        if version.major != self.major {
            return version.major > self.major;
        }
        let Some(minor) = self.minor else {
            return false;
        };
        if version.minor != minor {
            return version.minor > minor;
        }
        let Some(patch) = self.patch else {
            return false;
        };
        if version.patch != patch {
            return version.patch > patch;
        }
        version.cmp_pre(&self.pre) == Ordering::Greater
    }

    fn less(&self, version: &CargoVersion) -> bool {
        if version.major != self.major {
            return version.major < self.major;
        }
        let Some(minor) = self.minor else {
            return false;
        };
        if version.minor != minor {
            return version.minor < minor;
        }
        let Some(patch) = self.patch else {
            return false;
        };
        if version.patch != patch {
            return version.patch < patch;
        }
        version.cmp_pre(&self.pre) == Ordering::Less
    }

    fn matches(&self, version: &CargoVersion) -> bool {
        match self.op {
            CargoOp::Exact | CargoOp::Wildcard => {
                version.major == self.major
                    && self.minor.is_none_or(|minor| version.minor == minor)
                    && self.patch.is_none_or(|patch| version.patch == patch)
                    && (self.op == CargoOp::Wildcard
                        || self.patch.is_none()
                        || version.cmp_pre(&self.pre) == Ordering::Equal)
            }
            CargoOp::Greater => self.greater(version),
            CargoOp::GreaterEq => self.exact(version) || self.greater(version),
            CargoOp::Less => self.less(version),
            CargoOp::LessEq => self.exact(version) || self.less(version),
            CargoOp::Tilde => {
                if version.major != self.major {
                    return false;
                }
                if let Some(minor) = self.minor
                    && version.minor != minor
                {
                    return false;
                }
                if let Some(patch) = self.patch
                    && version.patch != patch
                {
                    return version.patch > patch;
                }
                version.cmp_pre(&self.pre) != Ordering::Less
            }
            CargoOp::Caret => {
                if version.major != self.major {
                    return false;
                }
                let Some(minor) = self.minor else { return true };
                let Some(patch) = self.patch else {
                    return if self.major > 0 {
                        version.minor >= minor
                    } else {
                        version.minor == minor
                    };
                };
                if self.major > 0 {
                    if version.minor != minor {
                        return version.minor > minor;
                    }
                    if version.patch != patch {
                        return version.patch > patch;
                    }
                } else if minor > 0 {
                    if version.minor != minor {
                        return false;
                    }
                    if version.patch != patch {
                        return version.patch > patch;
                    }
                } else if version.minor != minor || version.patch != patch {
                    return false;
                }
                version.cmp_pre(&self.pre) != Ordering::Less
            }
        }
    }
}

/// Whether `version` satisfies the Cargo requirement `requirement`
/// (`"0.8.23"`, `"^1.0"`, `"~0.4"`, `"=1.2.3"`, `">=1, <3"`, `"1.*"`, `"*"`).
///
/// Cargo's rules: a bare version is a caret requirement, comparators joined
/// by commas must all hold, partial versions match their whole range, build
/// metadata is ignored, and a pre-release matches only a comparator that
/// names the same `major.minor.patch` with a pre-release of its own.
///
/// # Errors
///
/// Returns [`VersionCompareError::Invalid`] when either side is not Cargo
/// semver syntax.
pub fn cargo_requirement_matches(
    requirement: &str,
    version: &str,
) -> Result<bool, VersionCompareError> {
    let candidate = CargoVersion::parse(version)?;
    let requirement = requirement.trim();
    if requirement.is_empty() || requirement == "*" {
        return Ok(candidate.pre.is_empty());
    }
    let comparators = requirement
        .split(',')
        .map(|part| CargoComparator::parse(part, requirement))
        .collect::<Result<Vec<_>, _>>()?;
    if !comparators
        .iter()
        .all(|comparator| comparator.matches(&candidate))
    {
        return Ok(false);
    }
    Ok(candidate.pre.is_empty()
        || comparators.iter().any(|comparator| {
            comparator.major == candidate.major
                && comparator.minor == Some(candidate.minor)
                && comparator.patch == Some(candidate.patch)
                && !comparator.pre.is_empty()
        }))
}

/// Cargo's compatibility class of a version: two versions in one class can
/// be unified into one copy (`1.4.2` → `1`, `0.8.23` → `0.8`, `0.0.3` → `0.0.3`).
///
/// # Errors
///
/// Returns [`VersionCompareError::Invalid`] when `version` is not Cargo semver.
pub fn cargo_compatibility_class(version: &str) -> Result<String, VersionCompareError> {
    let version = CargoVersion::parse(version)?;
    Ok(if version.major > 0 {
        version.major.to_string()
    } else if version.minor > 0 {
        format!("0.{}", version.minor)
    } else {
        format!("0.0.{}", version.patch)
    })
}

/// Orders two Cargo versions by semver precedence; build metadata is ignored.
///
/// # Errors
///
/// Returns [`VersionCompareError::Invalid`] when either side is not Cargo semver.
pub fn cargo_version_cmp(left: &str, right: &str) -> Result<Ordering, VersionCompareError> {
    let (left, right) = (CargoVersion::parse(left)?, CargoVersion::parse(right)?);
    Ok((left.major, left.minor, left.patch)
        .cmp(&(right.major, right.minor, right.patch))
        .then_with(|| compare_pre(&left.pre, &right.pre)))
}

#[cfg(test)]
mod cargo_tests {
    use super::{cargo_compatibility_class, cargo_requirement_matches, cargo_version_cmp};
    use std::cmp::Ordering;

    fn holds(requirement: &str, version: &str) -> bool {
        cargo_requirement_matches(requirement, version).expect("cargo syntax")
    }

    // The table is the Cargo book's "Specifying dependencies" examples.
    #[test]
    fn caret_tilde_wildcard_and_comparison_follow_the_cargo_book() {
        for (requirement, inside, outside) in [
            ("1.2.3", "1.9.0", "2.0.0"),
            ("1.2", "1.2.0", "2.0.0"),
            ("1", "1.99.1", "2.0.0"),
            ("0.2.3", "0.2.9", "0.3.0"),
            ("0.2", "0.2.0", "0.3.0"),
            ("0.0.3", "0.0.3", "0.0.4"),
            ("0.0", "0.0.9", "0.1.0"),
            ("0", "0.99.0", "1.0.0"),
            ("~1.2.3", "1.2.9", "1.3.0"),
            ("~1.2", "1.2.0", "1.3.0"),
            ("~1", "1.9.0", "2.0.0"),
            ("1.*", "1.4.0", "2.0.0"),
            ("1.2.*", "1.2.7", "1.3.0"),
            ("=1.2.3", "1.2.3", "1.2.4"),
            (">= 1.2, < 1.5", "1.4.9", "1.5.0"),
        ] {
            assert!(
                holds(requirement, inside),
                "{requirement} must accept {inside}"
            );
            assert!(
                !holds(requirement, outside),
                "{requirement} must refuse {outside}"
            );
        }
        assert!(!holds("1.2.3", "1.2.2"), "a caret floor holds");
        assert!(holds("*", "3.1.4"));
    }

    #[test]
    fn prereleases_need_a_prerelease_comparator_and_build_metadata_is_ignored() {
        assert!(
            !holds("1.0", "1.1.0-alpha"),
            "a stable requirement never picks a pre-release"
        );
        assert!(holds("1.1.0-alpha", "1.1.0-beta"));
        assert!(
            holds("1.0", "1.1.5+spec-1.1.0"),
            "build metadata plays no part"
        );
        assert!(holds("0.8.23", "0.8.23"));
        assert!(
            !holds("0.8.23", "1.1.5+spec-1.1.0"),
            "toml 0.8 and 1.x are two copies"
        );
    }

    #[test]
    fn classes_and_order() {
        assert_eq!(cargo_compatibility_class("0.8.23").as_deref(), Ok("0.8"));
        assert_eq!(
            cargo_compatibility_class("1.1.5+spec-1.1.0").as_deref(),
            Ok("1")
        );
        assert_eq!(
            cargo_compatibility_class("0.0.341").as_deref(),
            Ok("0.0.341")
        );
        assert_eq!(
            cargo_version_cmp("0.8.23", "1.1.5+spec-1.1.0"),
            Ok(Ordering::Less)
        );
        assert_eq!(cargo_version_cmp("2.0.0-rc.1", "2.0.0"), Ok(Ordering::Less));
        assert!(cargo_requirement_matches("banana", "1.0.0").is_err());
    }
}
