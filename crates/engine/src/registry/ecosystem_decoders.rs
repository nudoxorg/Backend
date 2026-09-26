//! Parsers for the native package-registry protocols.
//!
//! The wire shapes here are the public upstream protocols themselves. The
//! transport owns follow-up requests for protocols whose listing omits an
//! authenticated archive digest (Maven and Go); this module stays pure and
//! never turns a missing claim into a fabricated checksum.

use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductText,
    RegistryNativeObservation, RegistryNativeVulnerability, admit_dependency_rows,
    collapse_dependency_rows, dependency_optional,
};
use quick_xml::{events::Event, reader::Reader};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    sync::Arc,
};

use super::super::identity::coordinate_from_registry_parts;
use super::super::transport::ArchiveIntegrity;
use super::{
    EcosystemAdapter, NativeArtifact, NativeArtifactKind, NativeDistTag, NativeFeature,
    PackageName, RegistryChecksum, TransportFailure, component, resolve_archive_url,
};
use crate::registry::{
    DownloadCount, DownloadCountGap, ReleaseFacts, ReleaseStanding, SecurityStanding,
};

#[path = "ecosystem_decoders/decode.rs"]
mod decode;

/// Registry metadata is deliberately bounded before it is parsed. The HTTP
/// transport applies the configured feed limit as well, but keeping the same
/// guard at this adapter boundary makes direct fixture/replay calls safe.
const MAX_NATIVE_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_XML_DEPTH: usize = 64;
const MAX_XML_NODES: usize = 100_000;
const MAX_XML_TEXT_BYTES: usize = 1 * 1024 * 1024;
const MAX_MAVEN_VERSIONS: usize = 100_000;
const MAX_MAVEN_SNAPSHOT_ROWS: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MavenVersionKind {
    Release,
    Snapshot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MavenVersionRecord {
    pub(crate) version: String,
    pub(crate) kind: MavenVersionKind,
    pub(crate) timestamped_jar: Option<String>,
    pub(crate) timestamped_sources: Option<String>,
    pub(crate) timestamped_pom: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MavenMetadata {
    pub(crate) group: String,
    pub(crate) artifact: String,
    pub(crate) latest: Option<String>,
    pub(crate) release: Option<String>,
    pub(crate) versions: Vec<MavenVersionRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct XmlNode {
    name: String,
    text: String,
    children: Vec<XmlNode>,
}

impl XmlNode {
    fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|child| child.name == name)
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |child| child.name == name)
    }

    fn text_value(&self) -> &str {
        self.text.trim()
    }
}

fn bounded_xml(bytes: &[u8]) -> Result<XmlNode, TransportFailure> {
    if bytes.len() > MAX_NATIVE_XML_BYTES {
        return Err(TransportFailure::Overrun {
            measured: u64::try_from(bytes.len()).map_err(|_| TransportFailure::Bounds)?,
            limit: u64::try_from(MAX_NATIVE_XML_BYTES).map_err(|_| TransportFailure::Bounds)?,
        });
    }
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::with_capacity(4096);
    let mut stack = Vec::<XmlNode>::new();
    let mut root = None;
    let mut nodes = 0usize;
    let mut text_bytes = 0usize;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|_| TransportFailure::Protocol)?;
        match event {
            Event::Start(start) => {
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(TransportFailure::Bounds);
                }
                let name = std::str::from_utf8(start.name().as_ref())
                    .map_err(|_| TransportFailure::Protocol)?
                    .to_owned();
                // Attribute values are intentionally ignored. Parsing them
                // still validates quotes/escapes without retaining untrusted
                // extension metadata.
                for attribute in start.attributes().with_checks(true) {
                    attribute.map_err(|_| TransportFailure::Protocol)?;
                }
                nodes = nodes.checked_add(1).ok_or(TransportFailure::Bounds)?;
                if nodes > MAX_XML_NODES {
                    return Err(TransportFailure::Bounds);
                }
                stack.push(XmlNode {
                    name,
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            Event::Empty(empty) => {
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(TransportFailure::Bounds);
                }
                let name = std::str::from_utf8(empty.name().as_ref())
                    .map_err(|_| TransportFailure::Protocol)?
                    .to_owned();
                for attribute in empty.attributes().with_checks(true) {
                    attribute.map_err(|_| TransportFailure::Protocol)?;
                }
                nodes = nodes.checked_add(1).ok_or(TransportFailure::Bounds)?;
                if nodes > MAX_XML_NODES {
                    return Err(TransportFailure::Bounds);
                }
                attach_xml_node(
                    XmlNode {
                        name,
                        text: String::new(),
                        children: Vec::new(),
                    },
                    &mut stack,
                    &mut root,
                )?;
            }
            Event::Text(text) => {
                let value = text.decode().map_err(|_| TransportFailure::Protocol)?;
                text_bytes = text_bytes
                    .checked_add(value.len())
                    .ok_or(TransportFailure::Bounds)?;
                if text_bytes > MAX_XML_TEXT_BYTES {
                    return Err(TransportFailure::Bounds);
                }
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&value);
                } else if !value.trim().is_empty() {
                    return Err(TransportFailure::Protocol);
                }
            }
            Event::CData(text) => {
                let value =
                    std::str::from_utf8(text.as_ref()).map_err(|_| TransportFailure::Protocol)?;
                text_bytes = text_bytes
                    .checked_add(value.len())
                    .ok_or(TransportFailure::Bounds)?;
                if text_bytes > MAX_XML_TEXT_BYTES {
                    return Err(TransportFailure::Bounds);
                }
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(value);
                } else if !value.trim().is_empty() {
                    return Err(TransportFailure::Protocol);
                }
            }
            Event::End(end) => {
                let node = stack.pop().ok_or(TransportFailure::Protocol)?;
                if node.name.as_bytes() != end.name().as_ref() {
                    return Err(TransportFailure::Protocol);
                }
                attach_xml_node(node, &mut stack, &mut root)?;
            }
            Event::Decl(_) | Event::Comment(_) => {}
            // DTDs, processing instructions, and general entity references
            // are rejected. This keeps the parser XML 1.0-only and makes
            // external entity expansion impossible by construction.
            Event::DocType(_) | Event::PI(_) | Event::GeneralRef(_) => {
                return Err(TransportFailure::Protocol);
            }
            Event::Eof => {
                if !stack.is_empty() || root.is_none() {
                    return Err(TransportFailure::Protocol);
                }
                return root.ok_or(TransportFailure::Protocol);
            }
        }
        buffer.clear();
    }
}

fn attach_xml_node(
    node: XmlNode,
    stack: &mut Vec<XmlNode>,
    root: &mut Option<XmlNode>,
) -> Result<(), TransportFailure> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else if root.replace(node).is_some() {
        return Err(TransportFailure::Protocol);
    }
    Ok(())
}

/// Unauthenticated timestamp metadata from a Go proxy `.info` response.
/// The raw response remains part of release provenance, but its timestamp is
/// intentionally not used as an integrity claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoInfo {
    pub(crate) version: String,
    pub(crate) provenance: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoMod {
    pub(crate) module: String,
    pub(crate) requires: Vec<GoRequire>,
    pub(crate) retracts: Vec<GoRetract>,
    pub(crate) provenance: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoRequire {
    pub(crate) module: String,
    pub(crate) version: String,
    pub(crate) indirect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoRetract {
    pub(crate) lower: String,
    pub(crate) upper: String,
}

impl GoRetract {
    pub(crate) fn contains(&self, version: &str) -> bool {
        go_version_cmp(&self.lower, version).is_le() && go_version_cmp(version, &self.upper).is_le()
    }
}

fn valid_go_version(version: &str) -> bool {
    let Some(rest) = version.strip_prefix('v') else {
        return false;
    };
    if rest.is_empty() || rest.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return false;
    }
    let core = rest.split_once('+').map_or(rest, |(core, _)| core);
    let core = core.split_once('-').map_or(core, |(core, _)| core);
    let mut numbers = core.split('.');
    let Some(major) = numbers.next() else {
        return false;
    };
    let Some(minor) = numbers.next() else {
        return false;
    };
    let Some(patch) = numbers.next() else {
        return false;
    };
    numbers.next().is_none()
        && !major.is_empty()
        && !minor.is_empty()
        && !patch.is_empty()
        && major.parse::<u64>().is_ok()
        && minor.parse::<u64>().is_ok()
        && patch.parse::<u64>().is_ok()
}

fn go_version_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left_key = go_version_key(left);
    let right_key = go_version_key(right);
    left_key.cmp(&right_key).then_with(|| left.cmp(right))
}

fn go_version_key(value: &str) -> (u64, u64, u64, bool, String, String) {
    let rest = value.strip_prefix('v').unwrap_or(value);
    let (without_build, build) = rest.split_once('+').map_or((rest, ""), |parts| parts);
    let (core, pre) = without_build
        .split_once('-')
        .map_or((without_build, ""), |parts| parts);
    let mut numbers = core.split('.');
    let major = numbers
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let minor = numbers
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let patch = numbers
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (
        major,
        minor,
        patch,
        pre.is_empty(),
        pre.to_owned(),
        build.to_owned(),
    )
}

fn valid_rfc3339(value: &str) -> bool {
    // Keep this parser allocation-free and intentionally conservative. The Go
    // protocol requires an RFC 3339 timestamp when Time is present; accepting
    // only the canonical UTC form avoids locale/time-zone normalization in the
    // content identity while still admitting real proxy responses.
    value.len() >= 20
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value.as_bytes().get(10) == Some(&b'T')
        && value.as_bytes().get(13) == Some(&b':')
        && value.as_bytes().get(16) == Some(&b':')
        && value.ends_with('Z')
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16)
                || (index < value.len() - 1 && byte.is_ascii_digit())
                || (index >= 19 && (byte.is_ascii_digit() || byte == b'.'))
                || index == value.len() - 1
        })
}

fn parse_go_mod(
    text: &str,
    expected_module: &str,
    requested: &str,
) -> Result<GoMod, TransportFailure> {
    let mut module = None;
    let mut requires = Vec::new();
    let mut retracts = Vec::new();
    let mut block = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let (line, comment) = line.split_once("//").map_or((line, ""), |parts| parts);
        let line = line.trim();
        if line == ")" {
            if block.take().is_none() {
                return Err(TransportFailure::Protocol);
            }
            continue;
        }
        if let Some(kind) = block {
            match kind {
                GoModBlock::Require => parse_go_require_line(line, comment, &mut requires)?,
                GoModBlock::Retract => parse_go_retract_line(line, &mut retracts)?,
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("module ") {
            if module.replace(path.trim().to_owned()).is_some() || path.trim() != expected_module {
                return Err(TransportFailure::Protocol);
            }
        } else if line == "require (" {
            block = Some(GoModBlock::Require);
        } else if line == "retract (" {
            block = Some(GoModBlock::Retract);
        } else if let Some(rest) = line.strip_prefix("require ") {
            parse_go_require_line(rest, comment, &mut requires)?;
        } else if let Some(rest) = line.strip_prefix("retract ") {
            parse_go_retract_line(rest, &mut retracts)?;
        }
    }
    if block.is_some() || module.as_deref() != Some(expected_module) {
        return Err(TransportFailure::Protocol);
    }
    if retracts.iter().any(|range| range.contains(requested)) {
        // The caller projects this into a mutable standing; retaining all
        // ranges in the typed fact keeps later version-delta refreshes cheap.
    }
    Ok(GoMod {
        module: expected_module.to_owned(),
        requires,
        retracts,
        provenance: text.as_bytes().to_owned(),
    })
}

#[derive(Clone, Copy)]
enum GoModBlock {
    Require,
    Retract,
}

fn parse_go_require_line(
    line: &str,
    comment: &str,
    requires: &mut Vec<GoRequire>,
) -> Result<(), TransportFailure> {
    let mut fields = line.split_ascii_whitespace();
    let module = fields.next().ok_or(TransportFailure::Protocol)?;
    let version = fields.next().ok_or(TransportFailure::Protocol)?;
    if fields.next().is_some() || module.contains(char::is_whitespace) || !valid_go_version(version)
    {
        return Err(TransportFailure::Protocol);
    }
    requires.push(GoRequire {
        module: module.to_owned(),
        version: version.to_owned(),
        indirect: comment.trim() == "indirect",
    });
    Ok(())
}

fn parse_go_retract_line(
    line: &str,
    retracts: &mut Vec<GoRetract>,
) -> Result<(), TransportFailure> {
    let mut fields = line.split_ascii_whitespace();
    let first = fields.next().ok_or(TransportFailure::Protocol)?;
    let second = fields.next();
    if fields.next().is_some() {
        return Err(TransportFailure::Protocol);
    }
    let (lower, upper) = if let Some(second) = second {
        let lower = first
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(','))
            .ok_or(TransportFailure::Protocol)?;
        let upper = second.strip_suffix(']').ok_or(TransportFailure::Protocol)?;
        (lower, upper)
    } else {
        (first, first)
    };
    if !valid_go_version(lower) || !valid_go_version(upper) || go_version_cmp(lower, upper).is_gt()
    {
        return Err(TransportFailure::Protocol);
    }
    retracts.push(GoRetract {
        lower: lower.to_owned(),
        upper: upper.to_owned(),
    });
    Ok(())
}

fn cargo_dependencies(
    source: &super::PackageCoordinate,
    row: &Value,
    provenance: &[u8],
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
    let Some(raw_values) = row.get("deps") else {
        return Ok(DependencyFacts::Unknown(
            ProductText::new("Cargo index row omits dependency metadata")
                .map_err(|_| TransportFailure::Protocol)?,
        ));
    };
    let values = raw_values.as_array().ok_or(TransportFailure::Protocol)?;
    if values.len() > super::MAX_NATIVE_RELEASES {
        return Err(TransportFailure::Overrun {
            measured: u64::try_from(values.len()).map_err(|_| TransportFailure::Bounds)?,
            limit: u64::try_from(super::MAX_NATIVE_RELEASES)
                .map_err(|_| TransportFailure::Bounds)?,
        });
    }
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        let name = value
            .get("package")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .ok_or(TransportFailure::Protocol)?;
        let requirement = value
            .get("req")
            .and_then(Value::as_str)
            .ok_or(TransportFailure::Protocol)?;
        let scope = match value.get("kind").and_then(Value::as_str) {
            Some("dev") => DependencyScope::Development,
            Some("build") => DependencyScope::Build,
            Some("normal") | None => DependencyScope::Runtime,
            Some(_) => return Err(TransportFailure::Protocol),
        };
        let optional = dependency_optional(scope, strict_bool(value, "optional")?.unwrap_or(false));
        rows.push(dependency_record(
            source,
            backend_semantic::vocabulary::RegistryEcosystem::Cargo,
            name,
            requirement,
            scope,
            optional,
            provenance,
        )?);
    }
    Ok(DependencyFacts::Known(
        admit_dependency_rows(collapse_dependency_rows(rows))
            .map_err(|_| TransportFailure::Protocol)?,
    ))
}

fn npm_dependencies(
    source: &super::PackageCoordinate,
    row: &Value,
    provenance: &[u8],
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
    let mut rows = Vec::new();
    for (field_name, scope) in [
        ("dependencies", DependencyScope::Runtime),
        ("optionalDependencies", DependencyScope::Optional),
        ("peerDependencies", DependencyScope::Peer),
        ("devDependencies", DependencyScope::Development),
    ] {
        let Some(raw_values) = row.get(field_name) else {
            continue;
        };
        let values = raw_values.as_object().ok_or(TransportFailure::Protocol)?;
        if values.len() > super::MAX_NATIVE_RELEASES {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(values.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(super::MAX_NATIVE_RELEASES)
                    .map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let declared_optional = matches!(scope, DependencyScope::Optional);
        for (name, requirement) in values {
            let requirement = requirement.as_str().ok_or(TransportFailure::Protocol)?;
            rows.push(dependency_record(
                source,
                backend_semantic::vocabulary::RegistryEcosystem::Npm,
                name,
                requirement,
                scope,
                dependency_optional(scope, declared_optional),
                provenance,
            )?);
        }
    }
    if row.get("dependencies").is_none()
        && row.get("optionalDependencies").is_none()
        && row.get("peerDependencies").is_none()
        && row.get("devDependencies").is_none()
    {
        return Ok(DependencyFacts::Unknown(
            ProductText::new("npm packument version omits dependency metadata")
                .map_err(|_| TransportFailure::Protocol)?,
        ));
    }
    Ok(DependencyFacts::Known(
        admit_dependency_rows(collapse_dependency_rows(rows))
            .map_err(|_| TransportFailure::Protocol)?,
    ))
}

fn nuget_dependencies(
    source: &super::PackageCoordinate,
    groups: Option<&Value>,
    provenance: &[u8],
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
    let Some(groups) = groups else {
        return Ok(DependencyFacts::Unknown(
            ProductText::new("NuGet registration metadata omits dependency groups")
                .map_err(|_| TransportFailure::Protocol)?,
        ));
    };
    let groups = groups.as_array().ok_or(TransportFailure::Protocol)?;
    let mut rows = Vec::new();
    for group in groups {
        let Some(dependencies) = group.get("dependencies") else {
            // NuGet permits a target-framework group with no dependencies.
            continue;
        };
        let Some(dependencies) = dependencies.as_array() else {
            if dependencies.is_null() {
                continue;
            }
            return Err(TransportFailure::Protocol);
        };
        for dependency in dependencies {
            let name = field(dependency, "id")?;
            let requirement = dependency
                .get("range")
                .and_then(Value::as_str)
                .ok_or(TransportFailure::Protocol)?;
            rows.push(dependency_record(
                source,
                backend_semantic::vocabulary::RegistryEcosystem::Nuget,
                name,
                requirement,
                DependencyScope::Runtime,
                false,
                provenance,
            )?);
        }
    }
    Ok(DependencyFacts::Known(
        admit_dependency_rows(rows).map_err(|_| TransportFailure::Protocol)?,
    ))
}

fn pypi_requirement_name(requirement: &str) -> Result<&str, TransportFailure> {
    let end = requirement
        .find(|character: char| {
            character.is_whitespace()
                || matches!(character, '[' | ';' | '<' | '>' | '=' | '!' | '~' | '@')
        })
        .unwrap_or(requirement.len());
    let name = &requirement[..end];
    if name.is_empty()
        || !name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(TransportFailure::Protocol);
    }
    Ok(name)
}

fn pypi_requirement_is_extra(requirement: &str) -> bool {
    let Some((_, marker)) = requirement.split_once(';') else {
        return false;
    };
    let bytes = marker.as_bytes();
    let mut index = 0;
    let mut quoted = false;
    let mut quote = b'"';
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if byte == quote {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            quoted = true;
            quote = byte;
            index += 1;
            continue;
        }
        if marker[index..].starts_with("extra") {
            let before = index == 0 || !is_marker_identifier_byte(bytes[index - 1]);
            let after = index + 5;
            let after_boundary = after >= bytes.len() || !is_marker_identifier_byte(bytes[after]);
            if before && after_boundary {
                let rest = marker[after..].trim_start();
                if rest.starts_with("===")
                    || rest.starts_with("==")
                    || rest.starts_with("!=")
                    || rest.starts_with("~=")
                    || rest.starts_with("<=")
                    || rest.starts_with(">=")
                    || rest.starts_with('<')
                    || rest.starts_with('>')
                    || rest.starts_with("in")
                    || rest.starts_with("not")
                {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn is_marker_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn dependency_record(
    source: &super::PackageCoordinate,
    ecosystem: backend_semantic::vocabulary::RegistryEcosystem,
    name: &str,
    requirement: &str,
    scope: DependencyScope,
    optional: bool,
    provenance: &[u8],
) -> Result<PackageDependencyRecord, TransportFailure> {
    let source = PackageReference::parse(source.as_str().to_owned())
        .map_err(|_| TransportFailure::Protocol)?;
    let target = PackageDependencyTarget::new(ecosystem, name, requirement, None)
        .map_err(|_| TransportFailure::Protocol)?;
    let digest = *blake3::hash(provenance).as_bytes();
    Ok(PackageDependencyRecord::new(
        source,
        target,
        scope,
        optional,
        DependencyEvidence {
            authority: DependencyAuthority::RegistryMetadata,
            frontier: digest,
            provenance: digest,
        },
    ))
}

pub(crate) struct NugetReleaseMetadata {
    pub(crate) version: String,
    pub(crate) archive_url: String,
    pub(crate) checksum: Option<RegistryChecksum>,
    pub(crate) provenance: Vec<u8>,
    pub(crate) dependency_groups: Option<Value>,
    pub(crate) vulnerabilities: RegistryNativeObservation<Box<[RegistryNativeVulnerability]>>,
    pub(crate) deprecation: Option<String>,
    pub(crate) facts: ReleaseFacts,
}

/// One authenticated Conan v2 recipe/package file row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConanFileEntry {
    pub(crate) name: String,
    pub(crate) sha256: Option<RegistryChecksum>,
    pub(crate) size: Option<u64>,
}

/// Bounded file manifest returned by a Conan v2 revision endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConanFileManifest {
    pub(crate) entries: Vec<ConanFileEntry>,
}

impl ConanFileManifest {
    pub(crate) fn preferred_archive_name(&self) -> Result<String, TransportFailure> {
        ["conan_sources.tgz", "conan_export.tgz", "conan_package.tgz"]
            .iter()
            .find(|name| self.entries.iter().any(|entry| entry.name == **name))
            .map(|name| (*name).to_owned())
            .ok_or(TransportFailure::DownloadUnavailable)
    }

    pub(crate) fn entry(&self, name: &str) -> Option<&ConanFileEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    pub(crate) fn source_availability(&self) -> ConanSourceAvailability {
        if self.entry("conan_sources.tgz").is_some() {
            ConanSourceAvailability::Archive
        } else if self.entry("conan_export.tgz").is_some() {
            ConanSourceAvailability::RecipeOnly
        } else {
            ConanSourceAvailability::Unavailable
        }
    }
}

/// Explicit source availability for one Conan recipe revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConanSourceAvailability {
    /// The registry has a source archive that can be ingested directly.
    Archive,
    /// Only the recipe export is present; source may still be fetched from a
    /// verified `conandata.yml` mirror.
    RecipeOnly,
    /// No source or recipe archive is present at this revision.
    Unavailable,
}

fn safe_conan_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 512
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn valid_conan_timestamp(value: &str) -> bool {
    // Conan's documented API uses `2024-12-17T09:16:40.334+0000`. Keep the
    // accepted grammar bounded and timezone-aware without normalizing it.
    value.len() >= 24
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value.as_bytes().get(10) == Some(&b'T')
        && value.as_bytes().get(13) == Some(&b':')
        && value.as_bytes().get(16) == Some(&b':')
        && value.as_bytes().get(value.len().saturating_sub(5)) == Some(&b'+')
        && value[value.len().saturating_sub(4)..]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
}

fn facts(standing: ReleaseStanding, downloads: DownloadCount) -> ReleaseFacts {
    ReleaseFacts::new(standing, downloads, SecurityStanding::Unassessed)
}

fn standing(yanked: bool) -> ReleaseStanding {
    if yanked {
        ReleaseStanding::Yanked
    } else {
        ReleaseStanding::Available
    }
}

fn strict_bool(value: &Value, name: &str) -> Result<Option<bool>, TransportFailure> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(TransportFailure::Protocol),
    }
}

fn cargo_features(row: &Value) -> Result<Vec<NativeFeature>, TransportFailure> {
    let Some(raw_features) = row.get("features") else {
        return Ok(Vec::new());
    };
    let features = raw_features.as_object().ok_or(TransportFailure::Protocol)?;
    if features.len() > super::MAX_NATIVE_RELEASES {
        return Err(TransportFailure::Overrun {
            measured: u64::try_from(features.len()).map_err(|_| TransportFailure::Bounds)?,
            limit: u64::try_from(super::MAX_NATIVE_RELEASES)
                .map_err(|_| TransportFailure::Bounds)?,
        });
    }
    let mut output = Vec::with_capacity(features.len());
    for (name, members) in features {
        if name.is_empty() || name.len() > 1024 {
            return Err(TransportFailure::Protocol);
        }
        let members = members.as_array().ok_or(TransportFailure::Protocol)?;
        let mut members = members
            .iter()
            .map(|member| {
                let member = member.as_str().ok_or(TransportFailure::Protocol)?;
                if member.is_empty() || member.len() > 1024 {
                    return Err(TransportFailure::Protocol);
                }
                Ok(Arc::<str>::from(member))
            })
            .collect::<Result<Vec<_>, TransportFailure>>()?;
        members.sort();
        members.dedup();
        output.push(NativeFeature {
            name: Arc::from(name.as_str()),
            members: members.into_boxed_slice(),
        });
    }
    output.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(output)
}

fn npm_dist_tags(root: &Value) -> Result<Vec<NativeDistTag>, TransportFailure> {
    let Some(raw_tags) = root.get("dist-tags") else {
        return Ok(Vec::new());
    };
    let tags = raw_tags.as_object().ok_or(TransportFailure::Protocol)?;
    let mut output = Vec::with_capacity(tags.len());
    for (name, version) in tags {
        let version = version.as_str().ok_or(TransportFailure::Protocol)?;
        if name.is_empty() || version.is_empty() || name.len() > 128 || version.len() > 128 {
            return Err(TransportFailure::Protocol);
        }
        output.push(NativeDistTag {
            name: Arc::from(name.as_str()),
            version: Arc::from(version),
        });
    }
    output.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(output)
}

fn npm_checksum(
    dist: &serde_json::Map<String, Value>,
) -> Result<RegistryChecksum, TransportFailure> {
    if let Some(integrity) = dist.get("integrity") {
        let integrity = integrity.as_str().ok_or(TransportFailure::Protocol)?;
        let mut candidates = Vec::new();
        for token in integrity.split_ascii_whitespace() {
            let Some((algorithm, value)) = token.split_once('-') else {
                return Err(TransportFailure::Protocol);
            };
            candidates.push((algorithm, value));
        }
        for algorithm in ["sha512", "sha256", "sha1"] {
            if let Some((_, value)) = candidates.iter().find(|(name, _)| *name == algorithm) {
                return match algorithm {
                    "sha512" => RegistryChecksum::sha512_base64(value),
                    "sha256" => RegistryChecksum::sha256_base64(value),
                    "sha1" => RegistryChecksum::sha1_base64(value),
                    _ => unreachable!(),
                };
            }
        }
    }
    dist.get("shasum")
        .and_then(Value::as_str)
        .ok_or(TransportFailure::Protocol)
        .and_then(RegistryChecksum::sha1_hex)
}

fn field_value<'a>(
    value: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a str, TransportFailure> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(TransportFailure::Protocol)
}

fn npm_filename(url: &str, package: &str, version: &str) -> Arc<str> {
    url.rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .map_or_else(
            || Arc::from(format!("{}-{}.tgz", package.replace('/', "-"), version)),
            Arc::from,
        )
}

struct PythonFile {
    filename: Arc<str>,
    url: String,
    checksum: RegistryChecksum,
    kind: NativeArtifactKind,
    requires_python: Option<Arc<str>>,
    size: Option<u64>,
    yanked: bool,
    yanked_reason: Option<Arc<str>>,
    provenance: Vec<u8>,
}

fn python_checksum(
    file: &serde_json::Map<String, Value>,
) -> Result<RegistryChecksum, TransportFailure> {
    let hashes = file
        .get("hashes")
        .and_then(Value::as_object)
        .ok_or(TransportFailure::Protocol)?;
    for algorithm in ["sha512", "sha256", "sha1"] {
        if let Some(value) = hashes.get(algorithm) {
            let value = value.as_str().ok_or(TransportFailure::Protocol)?;
            return match algorithm {
                "sha512" => RegistryChecksum::sha512_hex(value),
                "sha256" => RegistryChecksum::sha256_hex(value),
                "sha1" => RegistryChecksum::sha1_hex(value),
                _ => unreachable!(),
            };
        }
    }
    Err(TransportFailure::Protocol)
}

fn python_yanked(
    file: &serde_json::Map<String, Value>,
) -> Result<(bool, Option<Arc<str>>), TransportFailure> {
    match file.get("yanked") {
        None | Some(Value::Bool(false)) => Ok((false, None)),
        Some(Value::Bool(true)) => Ok((true, None)),
        Some(Value::String(reason)) => Ok((
            true,
            (!reason.is_empty()).then(|| Arc::from(reason.as_str())),
        )),
        Some(_) => Err(TransportFailure::Protocol),
    }
}

fn python_kind_rank(kind: NativeArtifactKind) -> u8 {
    match kind {
        NativeArtifactKind::PythonSdist => 0,
        NativeArtifactKind::PythonWheel => 1,
        NativeArtifactKind::PythonSignature => 2,
        NativeArtifactKind::MavenJar
        | NativeArtifactKind::MavenSources
        | NativeArtifactKind::MavenPom
        | NativeArtifactKind::MavenSignature
        | NativeArtifactKind::MavenChecksum
        | NativeArtifactKind::NugetPackage
        | NativeArtifactKind::GoSource
        | NativeArtifactKind::ConanSource
        | NativeArtifactKind::ConanRecipe
        | NativeArtifactKind::Other => 3,
        NativeArtifactKind::CargoCrate | NativeArtifactKind::NpmTarball => 4,
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, TransportFailure> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or(TransportFailure::Protocol)
}

fn is_sdist(filename: &str) -> bool {
    [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".zip"]
        .iter()
        .any(|suffix| filename.ends_with(suffix))
}

fn is_wheel(filename: &str) -> bool {
    filename.ends_with(".whl")
}

fn python_file_identity(
    filename: &str,
    package: &str,
) -> Result<(String, NativeArtifactKind), TransportFailure> {
    let (candidate, kind) = if filename.ends_with(".asc") {
        (
            filename
                .strip_suffix(".asc")
                .ok_or(TransportFailure::Protocol)?,
            NativeArtifactKind::PythonSignature,
        )
    } else if filename.ends_with(".sig") {
        (
            filename
                .strip_suffix(".sig")
                .ok_or(TransportFailure::Protocol)?,
            NativeArtifactKind::PythonSignature,
        )
    } else if filename.ends_with(".metadata") {
        (
            filename
                .strip_suffix(".metadata")
                .ok_or(TransportFailure::Protocol)?,
            NativeArtifactKind::Other,
        )
    } else {
        (filename, NativeArtifactKind::Other)
    };
    let (stem, kind) = if let Some(stem) = candidate.strip_suffix(".whl") {
        (
            stem,
            if matches!(kind, NativeArtifactKind::PythonSignature) {
                kind
            } else {
                NativeArtifactKind::PythonWheel
            },
        )
    } else if let Some(stem) = candidate.strip_suffix(".tar.gz") {
        (
            stem,
            if matches!(kind, NativeArtifactKind::PythonSignature) {
                kind
            } else {
                NativeArtifactKind::PythonSdist
            },
        )
    } else if let Some(stem) = candidate.strip_suffix(".tar.bz2") {
        (
            stem,
            if matches!(kind, NativeArtifactKind::PythonSignature) {
                kind
            } else {
                NativeArtifactKind::PythonSdist
            },
        )
    } else if let Some(stem) = candidate.strip_suffix(".tar.xz") {
        (
            stem,
            if matches!(kind, NativeArtifactKind::PythonSignature) {
                kind
            } else {
                NativeArtifactKind::PythonSdist
            },
        )
    } else if let Some(stem) = candidate.strip_suffix(".tar.zst") {
        (
            stem,
            if matches!(kind, NativeArtifactKind::PythonSignature) {
                kind
            } else {
                NativeArtifactKind::PythonSdist
            },
        )
    } else if let Some(stem) = candidate.strip_suffix(".zip") {
        (
            stem,
            if matches!(kind, NativeArtifactKind::PythonSignature) {
                kind
            } else {
                NativeArtifactKind::PythonSdist
            },
        )
    } else {
        (candidate, kind)
    };
    let normalized_package = super::normalized_pypi_name(package);
    // PEP 503 normalizes the distribution component, while the filename
    // keeps its original separators. Find the boundary in the raw stem so
    // names such as ``zope.interface`` and versions with local segments are
    // not collapsed into an ambiguous token stream.
    let boundary = stem
        .char_indices()
        .filter(|(_, character)| *character == '-')
        .find_map(|(index, _)| {
            (super::normalized_pypi_name(&stem[..index]) == normalized_package).then_some(index)
        })
        .ok_or(TransportFailure::Protocol)?;
    let tail = &stem[boundary + 1..];
    let version = if matches!(kind, NativeArtifactKind::PythonWheel) {
        // Wheel names are distribution-version-build-python-abi-platform.
        let mut parts = tail.split('-');
        let version = parts.next().unwrap_or_default();
        let tag_count = parts.count();
        if tag_count < 3 {
            return Err(TransportFailure::Protocol);
        }
        version
    } else {
        tail.split('-').next().unwrap_or_default()
    };
    if version.is_empty() {
        return Err(TransportFailure::Protocol);
    }
    Ok((version.to_owned(), kind))
}

fn collect_registration_leaves<'a>(
    value: &'a Value,
    leaves: &mut Vec<&'a Value>,
) -> Result<(), TransportFailure> {
    let object = value.as_object().ok_or(TransportFailure::Protocol)?;
    if object.contains_key("catalogEntry") {
        leaves.push(value);
        return Ok(());
    }
    let items = object
        .get("items")
        .and_then(Value::as_array)
        .ok_or(TransportFailure::Protocol)?;
    for item in items {
        collect_registration_leaves(item, leaves)?;
    }
    Ok(())
}

fn nuget_security(entry: &Value) -> Result<SecurityStanding, TransportFailure> {
    let Some(vulnerabilities) = entry.get("vulnerabilities") else {
        return Ok(SecurityStanding::Unassessed);
    };
    let Some(vulnerabilities) = vulnerabilities.as_array() else {
        if vulnerabilities.is_null() {
            return Ok(SecurityStanding::Unassessed);
        }
        return Err(TransportFailure::Protocol);
    };
    if vulnerabilities.is_empty() {
        return Ok(SecurityStanding::NoKnownAdvisory);
    }
    let mut maximum = 0u8;
    for vulnerability in vulnerabilities {
        let severity = vulnerability
            .get("severity")
            .and_then(|value| value.as_str().or_else(|| value.as_u64().map(|_| "0")))
            .unwrap_or("0")
            .parse::<u8>()
            .map_err(|_| TransportFailure::Protocol)?;
        maximum = maximum.max(severity.min(4));
    }
    Ok(SecurityStanding::Affected {
        advisories: u32::try_from(vulnerabilities.len()).map_err(|_| TransportFailure::Bounds)?,
        maximum_severity: maximum,
    })
}

fn nuget_vulnerabilities(
    entry: &Value,
) -> Result<RegistryNativeObservation<Box<[RegistryNativeVulnerability]>>, TransportFailure> {
    let Some(value) = entry.get("vulnerabilities") else {
        return Ok(RegistryNativeObservation::NotRecorded(
            "NuGet registration entry omits vulnerability metadata".to_owned(),
        ));
    };
    if value.is_null() {
        return Ok(RegistryNativeObservation::Unavailable(
            "NuGet registration returned no vulnerability observation".to_owned(),
        ));
    }
    let rows = value.as_array().ok_or(TransportFailure::Protocol)?;
    let mut vulnerabilities = Vec::with_capacity(rows.len());
    for row in rows {
        let object = row.as_object().ok_or(TransportFailure::Protocol)?;
        let severity = match object.get("severity") {
            Some(Value::String(value)) => value
                .parse::<u8>()
                .map_err(|_| TransportFailure::Protocol)?,
            Some(Value::Number(value)) => value
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(TransportFailure::Protocol)?,
            _ => return Err(TransportFailure::Protocol),
        };
        if severity > 4 {
            return Err(TransportFailure::Protocol);
        }
        let advisory_url = match object.get("advisoryUrl") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            Some(Value::String(_)) => return Err(TransportFailure::Protocol),
            Some(_) => return Err(TransportFailure::Protocol),
        };
        vulnerabilities.push(RegistryNativeVulnerability {
            advisory_url,
            severity,
        });
    }
    vulnerabilities.sort_by(|left, right| {
        left.advisory_url
            .cmp(&right.advisory_url)
            .then_with(|| left.severity.cmp(&right.severity))
    });
    Ok(RegistryNativeObservation::Recorded(
        vulnerabilities.into_boxed_slice(),
    ))
}

fn nuget_deprecation(entry: &Value) -> Result<Option<String>, TransportFailure> {
    let Some(value) = entry.get("deprecation") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let object = value.as_object().ok_or(TransportFailure::Protocol)?;
    match object.get("message") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(TransportFailure::Protocol),
        Some(_) => Err(TransportFailure::Protocol),
    }
}

fn optional_text(node: Option<&XmlNode>) -> Option<String> {
    node.map(XmlNode::text_value)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::super::PackageCoordinate;
    use super::{TransportFailure, cargo_dependencies, npm_dependencies};
    use backend_library::{DependencyFacts, DependencyScope};

    fn cargo_coordinate() -> PackageCoordinate {
        PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate")
    }

    fn npm_coordinate() -> PackageCoordinate {
        PackageCoordinate::parse("pkg:npm/demo@1.0.0").expect("coordinate")
    }

    #[test]
    fn cargo_dev_and_normal_same_name_stays_runtime() {
        let row = serde_json::json!({
            "deps": [
                {"name": "serde", "req": "^1", "kind": "dev"},
                {"name": "serde", "req": "^1", "kind": "normal"},
            ]
        });
        let facts = cargo_dependencies(&cargo_coordinate(), &row, b"provenance").expect("decode");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "serde");
        assert_eq!(rows[0].scope, DependencyScope::Runtime);
        assert!(!rows[0].optional);
    }

    #[test]
    fn cargo_dev_only_stays_development_and_optional_false() {
        let row = serde_json::json!({
            "deps": [
                {"name": "serde", "req": "^1", "kind": "dev"},
            ]
        });
        let facts = cargo_dependencies(&cargo_coordinate(), &row, b"provenance").expect("decode");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scope, DependencyScope::Development);
        assert!(!rows[0].optional);
    }

    #[test]
    fn cargo_unknown_kind_returns_protocol_error() {
        let row = serde_json::json!({
            "deps": [{"name": "serde", "req": "^1", "kind": "mystery"}]
        });
        assert!(matches!(
            cargo_dependencies(&cargo_coordinate(), &row, b"provenance"),
            Err(TransportFailure::Protocol)
        ));
    }

    #[test]
    fn npm_dev_dependencies_vitest_is_development_optional_false() {
        let row = serde_json::json!({
            "devDependencies": {"vitest": "^1.0.0"}
        });
        let facts = npm_dependencies(&npm_coordinate(), &row, b"provenance").expect("decode");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "vitest");
        assert_eq!(rows[0].scope, DependencyScope::Development);
        assert!(!rows[0].optional);
    }

    #[test]
    fn npm_name_in_dependencies_and_dev_dependencies_stays_runtime() {
        let row = serde_json::json!({
            "dependencies": {"lodash": "^4.0.0"},
            "devDependencies": {"lodash": "^4.0.0"},
        });
        let facts = npm_dependencies(&npm_coordinate(), &row, b"provenance").expect("decode");
        let DependencyFacts::Known(rows) = facts else {
            panic!("expected known dependency facts");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.name.as_str(), "lodash");
        assert_eq!(rows[0].scope, DependencyScope::Runtime);
        assert!(!rows[0].optional);
    }
}
