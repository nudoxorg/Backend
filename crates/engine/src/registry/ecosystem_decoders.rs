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

impl EcosystemAdapter {
    pub(super) fn decode_cargo(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut releases = Vec::new();
        for (line_index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > 1024 * 1024 || releases.len() >= super::MAX_NATIVE_RELEASES {
                return Err(TransportFailure::Overrun {
                    measured: u64::try_from(line.len()).unwrap_or(u64::MAX),
                    limit: 1024 * 1024,
                });
            }
            let row: Value = serde_json::from_str(line).map_err(|_| TransportFailure::Protocol)?;
            if field(&row, "name")? != self.package_name() {
                return Err(TransportFailure::Protocol);
            }
            let version = field(&row, "vers")?;
            let checksum = RegistryChecksum::sha256_hex(field(&row, "cksum")?)?;
            let yanked = strict_bool(&row, "yanked")?.unwrap_or(false);
            let download_root = if self.endpoint_url() == "https://index.crates.io" {
                "https://static.crates.io"
            } else {
                self.endpoint_url()
            };
            let url = format!(
                "{download_root}/crates/{}/{}-{}.crate",
                component(self.package_name()),
                component(self.package_name()),
                component(version)
            );
            let mut release = self.release_from_checksum(
                version,
                url,
                checksum,
                line.as_bytes(),
                facts(
                    standing(yanked),
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                ),
            )?;
            release.dependency_facts =
                cargo_dependencies(&release.coordinate, &row, line.as_bytes())?;
            release.set_features(cargo_features(&row)?);
            let archive_url = release.archive_url.clone();
            release.set_artifacts(vec![NativeArtifact {
                filename: Arc::from(format!("{}-{}.crate", self.package_name(), version)),
                url: Arc::from(archive_url.as_str()),
                checksum: release.checksum.clone(),
                kind: NativeArtifactKind::CargoCrate,
                requires_python: None,
                size: None,
                yanked,
                yanked_reason: None,
            }]);
            release.record_common_native_metadata(self.endpoint.ecosystem(), line.as_bytes())?;
            let _ = line_index;
            releases.push(release);
        }
        Ok(releases)
    }

    pub(super) fn decode_npm(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let versions = root
            .get("versions")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        if versions.len() > super::MAX_NATIVE_RELEASES {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(versions.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(super::MAX_NATIVE_RELEASES)
                    .map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let expected_name = self.namespace_name().map_or_else(
            || self.package_name().to_owned(),
            |namespace| format!("{namespace}/{}", self.package_name()),
        );
        if root.get("name").and_then(Value::as_str) != Some(expected_name.as_str()) {
            return Err(TransportFailure::Protocol);
        }
        let dist_tags = npm_dist_tags(&root)?;
        versions
            .iter()
            .map(|(version, row)| {
                let row_object = row.as_object().ok_or(TransportFailure::Protocol)?;
                if let Some(row_name) = row_object.get("name") {
                    if row_name.as_str() != Some(expected_name.as_str()) {
                        return Err(TransportFailure::Protocol);
                    }
                }
                if let Some(row_version) = row_object.get("version") {
                    if row_version.as_str() != Some(version.as_str()) {
                        return Err(TransportFailure::Protocol);
                    }
                }
                let dist = row.get("dist").ok_or(TransportFailure::Protocol)?;
                let dist = dist.as_object().ok_or(TransportFailure::Protocol)?;
                let checksum = npm_checksum(dist)?;
                let deprecated = match row.get("deprecated") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(message)) => {
                        (!message.is_empty()).then_some(message.as_str())
                    }
                    Some(_) => return Err(TransportFailure::Protocol),
                };
                let encoded = serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?;
                let mut release = self.release_from_checksum(
                    version,
                    field_value(dist, "tarball")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(
                        deprecated
                            .map_or(ReleaseStanding::Available, |_| ReleaseStanding::Deprecated),
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    ),
                )?;
                release.dependency_facts = npm_dependencies(&release.coordinate, row, &encoded)?;
                release.set_dist_tags(Arc::from(dist_tags.clone().into_boxed_slice()));
                release.set_standing_reason(deprecated.map(Arc::from));
                let archive_url = release.archive_url.clone();
                release.set_artifacts(vec![NativeArtifact {
                    filename: npm_filename(&archive_url, expected_name.as_str(), version),
                    url: Arc::from(archive_url.as_str()),
                    checksum: release.checksum.clone(),
                    kind: NativeArtifactKind::NpmTarball,
                    requires_python: None,
                    size: dist.get("size").and_then(Value::as_u64),
                    yanked: false,
                    yanked_reason: None,
                }]);
                release.record_common_native_metadata(self.endpoint.ecosystem(), &encoded)?;
                Ok(release)
            })
            .collect()
    }

    pub(super) fn decode_python(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let api_version = root
            .get("meta")
            .and_then(|meta| meta.get("api-version"))
            .and_then(Value::as_str)
            .ok_or(TransportFailure::Protocol)?;
        // The Simple API increments its minor version as fields are added
        // (PyPI currently advertises 1.4). This decoder only consumes the
        // stable file/name/hash subset, so admit every well-formed 1.x
        // document while continuing to reject a future incompatible major.
        let Some((major, minor)) = api_version.split_once('.') else {
            return Err(TransportFailure::Protocol);
        };
        if major != "1" || minor.is_empty() || minor.parse::<u16>().is_err() {
            return Err(TransportFailure::Protocol);
        }
        if let Some(name) = root.get("name").and_then(Value::as_str) {
            if super::normalized_pypi_name(name) != super::normalized_pypi_name(self.package_name())
            {
                return Err(TransportFailure::Protocol);
            }
        }
        let files = root
            .get("files")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        if files.len() > super::MAX_NATIVE_RELEASES {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(files.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(super::MAX_NATIVE_RELEASES)
                    .map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let mut grouped = BTreeMap::<String, Vec<PythonFile>>::new();
        for file in files {
            let file_object = file.as_object().ok_or(TransportFailure::Protocol)?;
            let filename = field_value(file_object, "filename")?;
            if filename.len() > 1024 {
                return Err(TransportFailure::Overrun {
                    measured: u64::try_from(filename.len())
                        .map_err(|_| TransportFailure::Bounds)?,
                    limit: 1024,
                });
            }
            let (version, kind) = python_file_identity(filename, self.package_name())?;
            let checksum = python_checksum(file_object)?;
            let url = field_value(file_object, "url")?;
            let requires_python = match file_object.get("requires-python") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) if !value.is_empty() => Some(Arc::from(value.as_str())),
                Some(Value::String(_)) => return Err(TransportFailure::Protocol),
                Some(_) => return Err(TransportFailure::Protocol),
            };
            let size = match file_object.get("size") {
                None | Some(Value::Null) => None,
                Some(Value::Number(value)) => value.as_u64(),
                Some(_) => return Err(TransportFailure::Protocol),
            };
            let (yanked, yanked_reason) = python_yanked(file_object)?;
            let encoded = serde_json::to_vec(file).map_err(|_| TransportFailure::Protocol)?;
            grouped.entry(version).or_default().push(PythonFile {
                filename: Arc::from(filename),
                url: url.to_owned(),
                checksum,
                kind,
                requires_python,
                size,
                yanked,
                yanked_reason,
                provenance: encoded,
            });
        }
        grouped
            .into_iter()
            .map(|(version, mut files)| {
                files.sort_by(|left, right| left.filename.cmp(&right.filename));
                let primary_index = files
                    .iter()
                    .enumerate()
                    .filter(|(_, file)| {
                        matches!(
                            file.kind,
                            NativeArtifactKind::PythonSdist | NativeArtifactKind::PythonWheel
                        )
                    })
                    .min_by_key(|(_, file)| {
                        (
                            file.yanked,
                            python_kind_rank(file.kind),
                            file.filename.as_ref(),
                        )
                    })
                    .map(|(index, _)| index)
                    .ok_or(TransportFailure::DownloadUnavailable)?;
                let primary = &files[primary_index];
                let archive_url = resolve_archive_url(&self.metadata_url(), &primary.url)?;
                let archive_checksum = primary.checksum.clone();
                let all_yanked = files
                    .iter()
                    .filter(|file| {
                        matches!(
                            file.kind,
                            NativeArtifactKind::PythonSdist | NativeArtifactKind::PythonWheel
                        )
                    })
                    .all(|file| file.yanked);
                let standing_reason = all_yanked.then(|| primary.yanked_reason.clone()).flatten();
                let mut provenance = Vec::new();
                for file in &files {
                    provenance.extend_from_slice(&file.provenance);
                    provenance.push(0);
                }
                let mut release = self.release_from_checksum(
                    &version,
                    archive_url,
                    archive_checksum,
                    &provenance,
                    facts(
                        standing(all_yanked),
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    ),
                )?;
                release.set_standing_reason(standing_reason);
                release.set_requires_python(primary.requires_python.clone());
                release.set_artifacts(
                    files
                        .into_iter()
                        .map(|file| {
                            let url = resolve_archive_url(&self.metadata_url(), &file.url)?;
                            Ok(NativeArtifact {
                                filename: file.filename,
                                url: Arc::from(url),
                                checksum: file.checksum,
                                kind: file.kind,
                                requires_python: file.requires_python,
                                size: file.size,
                                yanked: file.yanked,
                                yanked_reason: file.yanked_reason,
                            })
                        })
                        .collect::<Result<Vec<_>, TransportFailure>>()?,
                );
                release.record_common_native_metadata(self.endpoint.ecosystem(), &provenance)?;
                Ok(release)
            })
            .collect()
    }

    pub(crate) fn maven_metadata(&self, bytes: &[u8]) -> Result<MavenMetadata, TransportFailure> {
        let root = bounded_xml(bytes)?;
        if root.name != "metadata" {
            return Err(TransportFailure::Protocol);
        }
        let group = root
            .child("groupId")
            .map(XmlNode::text_value)
            .filter(|value| !value.is_empty())
            .ok_or(TransportFailure::Protocol)?;
        let artifact = root
            .child("artifactId")
            .map(XmlNode::text_value)
            .filter(|value| !value.is_empty())
            .ok_or(TransportFailure::Protocol)?;
        let expected_group = self.namespace_name().ok_or(TransportFailure::Protocol)?;
        if group != expected_group || artifact != self.package_name() {
            return Err(TransportFailure::Protocol);
        }
        let versioning = root.child("versioning").ok_or(TransportFailure::Protocol)?;
        let latest = optional_text(versioning.child("latest"));
        let release = optional_text(versioning.child("release"));
        let versions_node = versioning
            .child("versions")
            .ok_or(TransportFailure::Protocol)?;
        let mut versions = Vec::new();
        for version in versions_node.children_named("version") {
            let value = version.text_value();
            if value.is_empty() {
                return Err(TransportFailure::Protocol);
            }
            if versions.len() >= MAX_MAVEN_VERSIONS {
                return Err(TransportFailure::Bounds);
            }
            versions.push(MavenVersionRecord {
                version: value.to_owned(),
                kind: if value.ends_with("-SNAPSHOT") {
                    MavenVersionKind::Snapshot
                } else {
                    MavenVersionKind::Release
                },
                timestamped_jar: None,
                timestamped_sources: None,
                timestamped_pom: None,
            });
        }
        if versions.is_empty() {
            return Err(TransportFailure::Protocol);
        }
        versions.sort_by(|left, right| left.version.cmp(&right.version));
        if versions
            .windows(2)
            .any(|pair| pair[0].version == pair[1].version)
        {
            return Err(TransportFailure::Protocol);
        }
        if let Some(snapshot_versions) = versioning.child("snapshotVersions") {
            let mut snapshot_values = BTreeMap::<(String, String), String>::new();
            let mut rows = 0usize;
            for row in snapshot_versions.children_named("snapshotVersion") {
                rows = rows.checked_add(1).ok_or(TransportFailure::Bounds)?;
                if rows > MAX_MAVEN_SNAPSHOT_ROWS {
                    return Err(TransportFailure::Bounds);
                }
                let extension = row
                    .child("extension")
                    .map(XmlNode::text_value)
                    .filter(|value| !value.is_empty())
                    .ok_or(TransportFailure::Protocol)?;
                let value = row
                    .child("value")
                    .map(XmlNode::text_value)
                    .filter(|value| !value.is_empty())
                    .ok_or(TransportFailure::Protocol)?;
                let classifier = row
                    .child("classifier")
                    .map(XmlNode::text_value)
                    .unwrap_or_default();
                let key = (extension.to_owned(), classifier.to_owned());
                if snapshot_values.insert(key, value.to_owned()).is_some() {
                    return Err(TransportFailure::Protocol);
                }
            }
            for record in &mut versions {
                if record.kind != MavenVersionKind::Snapshot {
                    continue;
                }
                record.timestamped_jar = snapshot_values
                    .get(&(String::from("jar"), String::new()))
                    .cloned();
                record.timestamped_sources = snapshot_values
                    .get(&(String::from("jar"), String::from("sources")))
                    .cloned();
                record.timestamped_pom = snapshot_values
                    .get(&(String::from("pom"), String::new()))
                    .cloned();
            }
        }
        Ok(MavenMetadata {
            group: group.to_owned(),
            artifact: artifact.to_owned(),
            latest,
            release,
            versions,
        })
    }

    pub(crate) fn maven_versions(&self, bytes: &[u8]) -> Result<Vec<String>, TransportFailure> {
        Ok(self
            .maven_metadata(bytes)?
            .versions
            .into_iter()
            .map(|version| version.version)
            .collect())
    }

    pub(crate) fn maven_release(
        &self,
        version: &str,
        checksum: RegistryChecksum,
        checksum_body: &[u8],
    ) -> Result<super::NativeRelease, TransportFailure> {
        self.release_from_checksum(
            version,
            self.maven_source_archive_url(version),
            checksum,
            checksum_body,
            facts(
                ReleaseStanding::Available,
                DownloadCount::NotReported(DownloadCountGap::Unsupported),
            ),
        )
    }

    pub(crate) fn maven_release_with_dependencies(
        &self,
        version: &str,
        archive_url: String,
        checksum: RegistryChecksum,
        provenance: &[u8],
        dependency_facts: DependencyFacts<Box<[PackageDependencyRecord]>>,
    ) -> Result<super::NativeRelease, TransportFailure> {
        let mut release = self.release_from_checksum(
            version,
            archive_url,
            checksum,
            provenance,
            facts(
                if version.ends_with("-SNAPSHOT") {
                    ReleaseStanding::Available
                } else {
                    ReleaseStanding::Available
                },
                DownloadCount::NotReported(DownloadCountGap::Unsupported),
            ),
        )?;
        release.dependency_facts = dependency_facts;
        Ok(release)
    }

    pub(crate) fn maven_archive_url(&self, version: &str) -> String {
        self.maven_archive_url_with_classifier(version, "")
    }

    pub(crate) fn maven_source_archive_url(&self, version: &str) -> String {
        self.maven_archive_url_with_classifier(version, "-sources")
    }

    pub(crate) fn maven_source_archive_url_for(
        &self,
        version: &str,
        timestamped: Option<&str>,
    ) -> String {
        self.maven_archive_url_with_classifier_and_value(version, "-sources", timestamped)
    }

    pub(crate) fn maven_archive_url_with_timestamped(
        &self,
        version: &str,
        timestamped: Option<&str>,
    ) -> String {
        self.maven_archive_url_with_classifier_and_value(version, "", timestamped)
    }

    fn maven_archive_url_with_classifier(&self, version: &str, classifier: &str) -> String {
        self.maven_archive_url_with_classifier_and_value(version, classifier, None)
    }

    fn maven_archive_url_with_classifier_and_value(
        &self,
        version: &str,
        classifier: &str,
        value: Option<&str>,
    ) -> String {
        let namespace = self.maven_namespace_path();
        let filename_version = value.unwrap_or(version);
        format!(
            "{}/{}/{}/{}/{}-{}{}.jar",
            self.endpoint_url(),
            namespace,
            component(self.package_name()),
            component(version),
            component(self.package_name()),
            component(filename_version),
            classifier
        )
    }

    fn maven_namespace_path(&self) -> String {
        self.namespace_name()
            .map(|value| {
                value
                    .split('.')
                    .map(component)
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default()
    }

    pub(crate) fn maven_pom_url_for(&self, version: &str, timestamped: Option<&str>) -> String {
        format!(
            "{}/{}/{}/{}/{}-{}.pom",
            self.endpoint_url(),
            self.maven_namespace_path(),
            component(self.package_name()),
            component(version),
            component(self.package_name()),
            component(timestamped.unwrap_or(version)),
        )
    }

    pub(crate) fn coordinate_for_version(
        &self,
        version: &str,
    ) -> Result<super::PackageCoordinate, TransportFailure> {
        let name = match self.namespace.as_ref() {
            None => self.package.clone(),
            Some(namespace) => {
                PackageName::new(format!("{}:{}", namespace.as_str(), self.package.as_str()))
                    .map_err(|_| TransportFailure::Protocol)?
            }
        };
        coordinate_from_registry_parts(self.endpoint.ecosystem(), name.as_str(), version)
            .map_err(|_| TransportFailure::Protocol)
    }

    pub(crate) fn maven_dependencies(
        &self,
        bytes: &[u8],
        source: &super::PackageCoordinate,
        provenance: &[u8],
    ) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
        let root = bounded_xml(bytes)?;
        if root.name != "project" {
            return Err(TransportFailure::Protocol);
        }
        let Some(dependencies) = root.child("dependencies") else {
            return Ok(DependencyFacts::Known(Box::new([])));
        };
        let mut rows = Vec::with_capacity(dependencies.children.len());
        let mut seen = BTreeSet::new();
        for dependency in dependencies.children_named("dependency") {
            let group = dependency
                .child("groupId")
                .map(XmlNode::text_value)
                .filter(|value| !value.is_empty())
                .ok_or(TransportFailure::Protocol)?;
            let artifact = dependency
                .child("artifactId")
                .map(XmlNode::text_value)
                .filter(|value| !value.is_empty())
                .ok_or(TransportFailure::Protocol)?;
            let name = format!("{group}:{artifact}");
            let Some(requirement) = dependency
                .child("version")
                .map(XmlNode::text_value)
                .filter(|value| !value.is_empty())
            else {
                return Ok(DependencyFacts::Unavailable(
                    ProductText::new("Maven POM dependency omits its version requirement")
                        .map_err(|_| TransportFailure::Protocol)?,
                ));
            };
            let scope = match dependency
                .child("scope")
                .map(XmlNode::text_value)
                .unwrap_or("compile")
            {
                "test" => DependencyScope::Development,
                "provided" | "system" => DependencyScope::Build,
                "runtime" | "compile" | "" => DependencyScope::Runtime,
                _ => return Err(TransportFailure::Protocol),
            };
            let optional = match dependency.child("optional").map(XmlNode::text_value) {
                None | Some("false") | Some("") => false,
                Some("true") => true,
                Some(_) => return Err(TransportFailure::Protocol),
            };
            let row = dependency_record(
                source,
                backend_semantic::vocabulary::RegistryEcosystem::Maven,
                &name,
                requirement,
                if optional {
                    DependencyScope::Optional
                } else {
                    scope
                },
                optional,
                provenance,
            )?;
            if !seen.insert(row.facts_version) {
                return Err(TransportFailure::Protocol);
            }
            rows.push(row);
        }
        Ok(DependencyFacts::Known(
            admit_dependency_rows(rows).map_err(|_| TransportFailure::Protocol)?,
        ))
    }

    pub(crate) fn conan_revision(&self, bytes: &[u8]) -> Result<String, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        if let Some(reference) = root.get("reference").and_then(Value::as_str) {
            let expected_prefix =
                format!("{}/{}@", self.package_name(), self.conan_recipe_version());
            if !reference.starts_with(&expected_prefix) {
                return Err(TransportFailure::Protocol);
            }
        }
        let revisions = root
            .get("revisions")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        let mut seen = BTreeSet::new();
        let mut selected = None;
        for row in revisions {
            let revision = field(row, "revision")?;
            if revision.is_empty() || revision.len() > 128 || !seen.insert(revision.to_owned()) {
                return Err(TransportFailure::Protocol);
            }
            if let Some(time) = row.get("time").and_then(Value::as_str) {
                if !valid_conan_timestamp(time) {
                    return Err(TransportFailure::Protocol);
                }
            }
            // Conan servers return newest first. The response order is part of
            // the authenticated listing, so retaining the first row avoids
            // inventing a local wall-clock ordering across mirrors.
            selected.get_or_insert_with(|| revision.to_owned());
        }
        selected.ok_or(TransportFailure::DownloadUnavailable)
    }

    pub(crate) fn conan_files_url(&self, revision: &str) -> String {
        format!(
            "{}/v2/conans/{}/{}/_/_/revisions/{revision}/files",
            self.endpoint_url(),
            component(self.package_name()),
            component(self.conan_recipe_version())
        )
    }

    pub(crate) fn conan_archive_url(&self, revision: &str, file: &str) -> String {
        format!(
            "{}/v2/conans/{}/{}/_/_/revisions/{revision}/files/{file}",
            self.endpoint_url(),
            component(self.package_name()),
            component(self.conan_recipe_version()),
            file = component(file)
        )
    }

    pub(crate) fn conan_archive_name(&self, bytes: &[u8]) -> Result<String, TransportFailure> {
        self.conan_file_manifest(bytes)?.preferred_archive_name()
    }

    pub(crate) fn conan_file_manifest(
        &self,
        bytes: &[u8],
    ) -> Result<ConanFileManifest, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let files = root
            .get("files")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        if files.is_empty() || files.len() > 4_096 {
            return Err(if files.is_empty() {
                TransportFailure::DownloadUnavailable
            } else {
                TransportFailure::Overrun {
                    measured: u64::try_from(files.len()).map_err(|_| TransportFailure::Bounds)?,
                    limit: 4_096,
                }
            });
        }
        let mut entries = Vec::with_capacity(files.len());
        for (name, value) in files {
            if !safe_conan_file_name(name) {
                return Err(TransportFailure::Protocol);
            }
            let object = value.as_object().ok_or(TransportFailure::Protocol)?;
            let sha256 = match object.get("sha256") {
                Some(Value::String(value)) => Some(RegistryChecksum::sha256_hex(value)?),
                Some(Value::Null) | None => None,
                Some(_) => return Err(TransportFailure::Protocol),
            };
            let size = match object.get("size") {
                Some(Value::Number(value)) => value.as_u64(),
                Some(Value::Null) | None => None,
                Some(_) => return Err(TransportFailure::Protocol),
            };
            entries.push(ConanFileEntry {
                name: name.to_owned(),
                sha256,
                size,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(ConanFileManifest { entries })
    }

    pub(crate) fn nuget_metadata(
        &self,
        bytes: &[u8],
        package_base: Option<&str>,
    ) -> Result<Vec<NugetReleaseMetadata>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut leaves = Vec::new();
        collect_registration_leaves(&root, &mut leaves)?;
        self.nuget_metadata_from_leaves(leaves, package_base)
    }

    pub(crate) fn nuget_metadata_from_leaves(
        &self,
        leaves: Vec<&Value>,
        package_base: Option<&str>,
    ) -> Result<Vec<NugetReleaseMetadata>, TransportFailure> {
        let mut releases = leaves
            .into_iter()
            .map(|row| {
                let entry = row.get("catalogEntry").ok_or(TransportFailure::Protocol)?;
                if !entry.is_object() {
                    return Err(TransportFailure::Protocol);
                }
                let version = field(entry, "version")?.to_owned();
                let archive_url = row
                    .get("packageContent")
                    .and_then(Value::as_str)
                    .filter(|url| !url.is_empty())
                    .map(str::to_owned)
                    .or_else(|| {
                        package_base.map(|base| {
                            format!(
                                "{}/{}/{}.nupkg",
                                base.trim_end_matches('/'),
                                self.package_name().to_ascii_lowercase(),
                                version.to_ascii_lowercase()
                            )
                        })
                    })
                    .ok_or(TransportFailure::DownloadUnavailable)?;
                let checksum = match row.get("packageHash") {
                    Some(Value::String(value)) => Some(RegistryChecksum::sha512_base64(value)?),
                    Some(Value::Null) | None => None,
                    Some(_) => return Err(TransportFailure::Protocol),
                };
                let standing = if entry.get("listed").and_then(Value::as_bool) == Some(false) {
                    ReleaseStanding::Unlisted
                } else if entry
                    .get("deprecation")
                    .is_some_and(|value| !value.is_null())
                {
                    ReleaseStanding::Deprecated
                } else {
                    ReleaseStanding::Available
                };
                let downloads = entry.get("downloads").and_then(Value::as_u64).map_or(
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    DownloadCount::Exact,
                );
                let facts = facts(standing, downloads).with_security(nuget_security(entry)?);
                Ok(NugetReleaseMetadata {
                    version,
                    archive_url,
                    checksum,
                    provenance: serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?,
                    dependency_groups: entry.get("dependencyGroups").cloned(),
                    vulnerabilities: nuget_vulnerabilities(entry)?,
                    deprecation: nuget_deprecation(entry)?,
                    facts,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        releases.sort_by(|left, right| {
            left.version
                .to_ascii_lowercase()
                .cmp(&right.version.to_ascii_lowercase())
                .then_with(|| left.version.cmp(&right.version))
        });
        if releases
            .windows(2)
            .any(|pair| pair[0].version.eq_ignore_ascii_case(&pair[1].version))
        {
            return Err(TransportFailure::Protocol);
        }
        Ok(releases)
    }

    pub(super) fn decode_nuget(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        self.nuget_metadata(bytes, None)?
            .into_iter()
            .map(|release| {
                let dependency_groups = release.dependency_groups.clone();
                let vulnerabilities = release.vulnerabilities;
                let deprecation = release.deprecation;
                let checksum = release
                    .checksum
                    .ok_or(TransportFailure::DownloadUnavailable)?;
                let mut release = self.release_from_checksum(
                    &release.version,
                    release.archive_url,
                    checksum,
                    &release.provenance,
                    release.facts,
                )?;
                let provenance = release.provenance.as_bytes();
                release.dependency_facts = nuget_dependencies(
                    &release.coordinate,
                    dependency_groups.as_ref(),
                    &provenance,
                )?;
                let archive_url = release.archive_url.clone();
                let filename = archive_url
                    .rsplit('/')
                    .next()
                    .filter(|value| !value.is_empty())
                    .ok_or(TransportFailure::Protocol)?;
                release.set_artifacts(vec![NativeArtifact {
                    filename: Arc::from(filename),
                    url: Arc::from(archive_url.as_str()),
                    checksum: release.checksum.clone(),
                    kind: NativeArtifactKind::NugetPackage,
                    requires_python: None,
                    size: None,
                    yanked: matches!(release.facts.standing(), ReleaseStanding::Yanked),
                    yanked_reason: None,
                }]);
                release.record_nuget_native_metadata(
                    vulnerabilities,
                    RegistryNativeObservation::Recorded(release.dependency_facts.clone()),
                    deprecation,
                )?;
                Ok(release)
            })
            .collect()
    }

    pub(crate) fn go_versions(&self, bytes: &[u8]) -> Result<Vec<String>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut seen = BTreeSet::new();
        let mut versions = Vec::new();
        for line in text.lines() {
            let version = line.trim();
            if version.is_empty()
                || version != line.trim_end_matches('\r')
                || version.split_whitespace().count() != 1
                || !valid_go_version(version)
                || !seen.insert(version.to_owned())
            {
                // The proxy protocol is deliberately line-oriented. A blank
                // line is harmless (many mirrors append one), but any other
                // whitespace or duplicate is evidence that this is not the
                // version list for the requested module. Silently deduping a
                // changed listing would make the durable cursor non-replayable.
                if version.is_empty() {
                    continue;
                }
                return Err(TransportFailure::Protocol);
            }
            versions.push(version.to_owned());
        }
        versions.sort_by(|left, right| go_version_cmp(left, right));
        Ok(versions)
    }

    /// Decodes the authenticated `.info` response for one exact version.
    ///
    /// The proxy specification permits future fields, but the version field
    /// is the binding that prevents a branch/revision lookup from being
    /// accidentally admitted under a different immutable coordinate.
    pub(crate) fn go_info(
        &self,
        bytes: &[u8],
        requested: &str,
    ) -> Result<GoInfo, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let version = field(&root, "Version")?;
        if version != requested || !valid_go_version(version) {
            return Err(TransportFailure::Protocol);
        }
        if let Some(time) = root.get("Time") {
            let time = time.as_str().ok_or(TransportFailure::Protocol)?;
            if !valid_rfc3339(time) {
                return Err(TransportFailure::Protocol);
            }
        }
        Ok(GoInfo {
            version: version.to_owned(),
            provenance: bytes.to_owned(),
        })
    }

    /// Decodes a Go module file, retaining only facts that can be projected
    /// into the shared package graph. Unknown directives remain forward
    /// compatible, while malformed `require`/`retract` blocks fail closed.
    pub(crate) fn go_mod(&self, bytes: &[u8], requested: &str) -> Result<GoMod, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let module_path = self.go_module_path();
        parse_go_mod(text, &module_path, requested)
    }

    pub(crate) fn go_dependencies(
        &self,
        source: &super::PackageCoordinate,
        module: &GoMod,
        provenance: &[u8],
    ) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
        let source = PackageReference::parse(source.as_str().to_owned())
            .map_err(|_| TransportFailure::Protocol)?;
        let digest = *blake3::hash(provenance).as_bytes();
        let mut rows = Vec::with_capacity(module.requires.len());
        for requirement in &module.requires {
            let target = PackageDependencyTarget::new(
                backend_semantic::vocabulary::RegistryEcosystem::Golang,
                requirement.module.clone(),
                requirement.version.clone(),
                None,
            )
            .map_err(|_| TransportFailure::Protocol)?;
            rows.push(PackageDependencyRecord::new(
                source.clone(),
                target,
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: digest,
                    provenance: digest,
                },
            ));
        }
        Ok(DependencyFacts::Known(
            admit_dependency_rows(rows).map_err(|_| TransportFailure::Protocol)?,
        ))
    }

    pub(crate) fn go_release(
        &self,
        version: &str,
        checksum: RegistryChecksum,
        provenance: &[u8],
        facts: ReleaseFacts,
    ) -> Result<super::NativeRelease, TransportFailure> {
        self.release_from_checksum(
            version,
            format!(
                "{}/{}/@v/{}.zip",
                self.endpoint_url(),
                self.go_proxy_package(),
                component(version)
            ),
            checksum,
            provenance,
            facts,
        )
    }

    pub(crate) fn page_start(
        &self,
        source: &[u8],
        request: super::FeedRequest,
        total: usize,
    ) -> Result<(usize, [u8; 24]), TransportFailure> {
        let snapshot = self.page_identity(source);
        let mut prefix = [0_u8; 24];
        prefix.copy_from_slice(&snapshot.as_bytes()[..24]);
        let token = request.cursor.token();
        let start = if token == [0; 32] || token[..24] != prefix {
            0
        } else {
            usize::try_from(u64::from_be_bytes(
                token[24..]
                    .try_into()
                    .map_err(|_| TransportFailure::Protocol)?,
            ))
            .map_err(|_| TransportFailure::Bounds)?
        };
        if start > total {
            return Err(TransportFailure::Protocol);
        }
        Ok((start, prefix))
    }

    pub(crate) fn admit_window(
        &self,
        releases: Vec<super::NativeRelease>,
        request: super::FeedRequest,
        start: usize,
        prefix: [u8; 24],
        total: usize,
    ) -> Result<super::FeedPage, TransportFailure> {
        let end = start.saturating_add(releases.len());
        if end > total || releases.len() > request.max_items {
            return Err(TransportFailure::Bounds);
        }
        let packages = releases
            .iter()
            .map(|release| super::RemotePackage {
                coordinate: release.coordinate.clone(),
                integrity: ArchiveIntegrity::Native(release.checksum.clone()),
                provenance: release.provenance,
                facts: release.facts,
                native_metadata: release.native_metadata.clone(),
                advisory: None,
                dependency_facts: release.dependency_facts.clone(),
                archive_url: std::sync::Arc::from(release.archive_url.as_str()),
            })
            .collect();
        if releases.is_empty() && self.target_version().is_some() {
            return Ok(super::FeedPage {
                base: request.cursor,
                next_token: request.cursor.token(),
                packages,
            });
        }
        let mut next_token = [0_u8; 32];
        next_token[..24].copy_from_slice(&prefix);
        next_token[24..].copy_from_slice(
            &u64::try_from(end)
                .map_err(|_| TransportFailure::Bounds)?
                .to_be_bytes(),
        );
        Ok(super::FeedPage {
            base: request.cursor,
            next_token,
            packages,
        })
    }

    pub(crate) fn go_info_url(&self, version: &str) -> String {
        format!(
            "{}/{}/@v/{}.info",
            self.endpoint_url(),
            self.go_proxy_package(),
            component(version)
        )
    }

    pub(crate) fn go_mod_url(&self, version: &str) -> String {
        format!(
            "{}/{}/@v/{}.mod",
            self.endpoint_url(),
            self.go_proxy_package(),
            component(version)
        )
    }

    pub(crate) fn go_sum_lookup_url(&self, version: &str) -> String {
        format!(
            "https://sum.golang.org/lookup/{}@{}",
            self.go_proxy_package(),
            component(version)
        )
    }

    pub(crate) fn go_module_path(&self) -> String {
        self.namespace.as_ref().map_or_else(
            || self.package_name().to_owned(),
            |namespace| format!("{}/{}", namespace.as_str(), self.package_name()),
        )
    }
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
        let optional = strict_bool(value, "optional")?.unwrap_or(false);
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
        admit_dependency_rows(rows).map_err(|_| TransportFailure::Protocol)?,
    ))
}

fn npm_dependencies(
    source: &super::PackageCoordinate,
    row: &Value,
    provenance: &[u8],
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
    let mut rows = Vec::new();
    for (field_name, scope, optional) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("optionalDependencies", DependencyScope::Optional, true),
        ("peerDependencies", DependencyScope::Peer, false),
        ("devDependencies", DependencyScope::Development, true),
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
        for (name, requirement) in values {
            let requirement = requirement.as_str().ok_or(TransportFailure::Protocol)?;
            rows.push(dependency_record(
                source,
                backend_semantic::vocabulary::RegistryEcosystem::Npm,
                name,
                requirement,
                scope,
                optional,
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
        admit_dependency_rows(rows).map_err(|_| TransportFailure::Protocol)?,
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
