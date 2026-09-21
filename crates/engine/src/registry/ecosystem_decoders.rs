//! Parsers for the native package-registry protocols.
//!
//! The wire shapes here are the public upstream protocols themselves. The
//! transport owns follow-up requests for protocols whose listing omits an
//! authenticated archive digest (Maven and Go); this module stays pure and
//! never turns a missing claim into a fabricated checksum.

use serde_json::Value;

use super::super::transport::ArchiveIntegrity;
use super::{EcosystemAdapter, RegistryChecksum, TransportFailure, component};
use crate::registry::{
    DownloadCount, DownloadCountGap, ReleaseFacts, ReleaseStanding, SecurityStanding,
};

impl EcosystemAdapter {
    pub(super) fn decode_cargo(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut releases = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).map_err(|_| TransportFailure::Protocol)?;
            if field(&row, "name")? != self.package_name() {
                return Err(TransportFailure::Protocol);
            }
            let version = field(&row, "vers")?;
            let checksum = RegistryChecksum::sha256_hex(field(&row, "cksum")?)?;
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
            releases.push(self.release_from_checksum(
                version,
                url,
                checksum,
                line.as_bytes(),
                facts(
                    if row.get("yanked").and_then(Value::as_bool).unwrap_or(false) {
                        ReleaseStanding::Yanked
                    } else {
                        ReleaseStanding::Available
                    },
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                ),
            )?);
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
        let expected_name = self.namespace_name().map_or_else(
            || self.package_name().to_owned(),
            |namespace| format!("{namespace}/{}", self.package_name()),
        );
        if root.get("name").and_then(Value::as_str) != Some(expected_name.as_str()) {
            return Err(TransportFailure::Protocol);
        }
        versions
            .iter()
            .map(|(version, row)| {
                let dist = row.get("dist").ok_or(TransportFailure::Protocol)?;
                let integrity = field(dist, "integrity")?;
                let checksum = RegistryChecksum::sha512_base64(
                    integrity
                        .strip_prefix("sha512-")
                        .ok_or(TransportFailure::Protocol)?,
                )?;
                let standing = row
                    .get("deprecated")
                    .and_then(Value::as_str)
                    .filter(|message| !message.is_empty())
                    .map_or(ReleaseStanding::Available, |_| ReleaseStanding::Deprecated);
                let encoded = serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?;
                self.release_from_checksum(
                    version,
                    field(dist, "tarball")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(
                        standing,
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    ),
                )
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
        let files = root
            .get("files")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        let mut selected = std::collections::BTreeMap::<String, &Value>::new();
        for file in files {
            let filename = field(file, "filename")?;
            let version = python_version(filename, self.package_name())?;
            let hash = file
                .get("hashes")
                .and_then(|hashes| hashes.get("sha256"))
                .and_then(Value::as_str)
                .ok_or(TransportFailure::Protocol)?;
            let rank = if is_sdist(filename) {
                0
            } else if is_wheel(filename) {
                1
            } else {
                continue;
            };
            if !file
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|url| !url.is_empty())
            {
                return Err(TransportFailure::Protocol);
            }
            let replace = selected.get(&version).is_none_or(|old| {
                let old_name = old.get("filename").and_then(Value::as_str).unwrap_or("");
                let old_rank = if is_sdist(old_name) { 0 } else { 1 };
                (rank, filename) < (old_rank, old_name)
            });
            let _ = RegistryChecksum::sha256_hex(hash)?;
            if replace {
                selected.insert(version, file);
            }
        }
        selected
            .into_iter()
            .map(|(version, file)| {
                let checksum = RegistryChecksum::sha256_hex(
                    file.get("hashes")
                        .and_then(|hashes| hashes.get("sha256"))
                        .and_then(Value::as_str)
                        .ok_or(TransportFailure::Protocol)?,
                )?;
                let standing = match file.get("yanked") {
                    Some(Value::Bool(true)) => ReleaseStanding::Yanked,
                    Some(Value::String(reason)) if !reason.is_empty() => ReleaseStanding::Yanked,
                    Some(Value::Bool(false)) | None => ReleaseStanding::Available,
                    Some(_) => return Err(TransportFailure::Protocol),
                };
                let encoded = serde_json::to_vec(file).map_err(|_| TransportFailure::Protocol)?;
                self.release_from_checksum(
                    &version,
                    field(file, "url")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(
                        standing,
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn maven_versions(&self, bytes: &[u8]) -> Result<Vec<String>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let group = xml_text(text, "groupId")?;
        let artifact = xml_text(text, "artifactId")?;
        if group != self.namespace_name().ok_or(TransportFailure::Protocol)?
            || artifact != self.package_name()
        {
            return Err(TransportFailure::Protocol);
        }
        let mut versions = xml_values(xml_block(text, "versions")?, "version")?;
        versions.sort();
        versions.dedup();
        Ok(versions)
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

    pub(crate) fn maven_archive_url(&self, version: &str) -> String {
        self.maven_archive_url_with_classifier(version, "")
    }

    pub(crate) fn maven_source_archive_url(&self, version: &str) -> String {
        self.maven_archive_url_with_classifier(version, "-sources")
    }

    fn maven_archive_url_with_classifier(&self, version: &str, classifier: &str) -> String {
        let namespace = self
            .namespace_name()
            .map(|value| value.replace('.', "/"))
            .unwrap_or_default();
        format!(
            "{}/{}/{}/{}/{}-{}{}.jar",
            self.endpoint_url(),
            namespace,
            component(self.package_name()),
            component(version),
            component(self.package_name()),
            component(version),
            classifier
        )
    }

    pub(crate) fn conan_revision(&self, bytes: &[u8]) -> Result<String, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let reference = field(&root, "reference")?;
        let expected_prefix = format!("{}/{}@", self.package_name(), self.conan_recipe_version());
        if !reference.starts_with(&expected_prefix) {
            return Err(TransportFailure::Protocol);
        }
        root.get("revisions")
            .and_then(Value::as_array)
            .and_then(|revisions| revisions.first())
            .and_then(|revision| revision.get("revision"))
            .and_then(Value::as_str)
            .filter(|revision| !revision.is_empty())
            .map(str::to_owned)
            .ok_or(TransportFailure::Protocol)
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
            component(self.conan_recipe_version())
        )
    }

    pub(crate) fn conan_archive_name(&self, bytes: &[u8]) -> Result<String, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let files = root
            .get("files")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        ["conan_export.tgz", "conan_sources.tgz"]
            .iter()
            .find(|name| files.contains_key(**name))
            .map(|name| (*name).to_owned())
            .ok_or(TransportFailure::DownloadUnavailable)
    }

    pub(crate) fn nuget_metadata(
        &self,
        bytes: &[u8],
        package_base: Option<&str>,
    ) -> Result<Vec<NugetReleaseMetadata>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut leaves = Vec::new();
        collect_registration_leaves(&root, &mut leaves)?;
        leaves
            .into_iter()
            .map(|row| {
                let entry = row.get("catalogEntry").ok_or(TransportFailure::Protocol)?;
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
                } else {
                    ReleaseStanding::Available
                };
                let facts = facts(
                    standing,
                    DownloadCount::NotReported(DownloadCountGap::Unsupported),
                )
                .with_security(nuget_security(entry)?);
                Ok(NugetReleaseMetadata {
                    version,
                    archive_url,
                    checksum,
                    provenance: serde_json::to_vec(row).map_err(|_| TransportFailure::Protocol)?,
                    facts,
                })
            })
            .collect()
    }

    pub(super) fn decode_nuget(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::NativeRelease>, TransportFailure> {
        self.nuget_metadata(bytes, None)?
            .into_iter()
            .map(|release| {
                let checksum = release
                    .checksum
                    .ok_or(TransportFailure::DownloadUnavailable)?;
                self.release_from_checksum(
                    &release.version,
                    release.archive_url,
                    checksum,
                    &release.provenance,
                    release.facts,
                )
            })
            .collect()
    }

    pub(crate) fn go_versions(&self, bytes: &[u8]) -> Result<Vec<String>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut versions = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::trim)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if versions
            .iter()
            .any(|version| version.split_whitespace().count() != 1)
        {
            return Err(TransportFailure::Protocol);
        }
        versions.sort();
        versions.dedup();
        Ok(versions)
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
}

pub(crate) struct NugetReleaseMetadata {
    pub(crate) version: String,
    pub(crate) archive_url: String,
    pub(crate) checksum: Option<RegistryChecksum>,
    pub(crate) provenance: Vec<u8>,
    pub(crate) facts: ReleaseFacts,
}

fn facts(standing: ReleaseStanding, downloads: DownloadCount) -> ReleaseFacts {
    ReleaseFacts::new(standing, downloads, SecurityStanding::Unassessed)
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, TransportFailure> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or(TransportFailure::Protocol)
}

fn is_sdist(filename: &str) -> bool {
    filename.ends_with(".tar.gz") || filename.ends_with(".zip")
}

fn is_wheel(filename: &str) -> bool {
    filename.ends_with(".whl")
}

fn python_version(filename: &str, package: &str) -> Result<String, TransportFailure> {
    let stem = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".zip"))
        .or_else(|| filename.strip_suffix(".whl"))
        .ok_or(TransportFailure::Protocol)?;
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
    let version = if is_wheel(filename) {
        // Wheel names are distribution-version-build-python-abi-platform.
        tail.split('-').next().unwrap_or_default()
    } else {
        tail
    };
    (!version.is_empty())
        .then(|| version.to_owned())
        .ok_or(TransportFailure::Protocol)
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
    let vulnerabilities = vulnerabilities
        .as_array()
        .ok_or(TransportFailure::Protocol)?;
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

fn xml_block<'a>(text: &'a str, tag: &str) -> Result<&'a str, TransportFailure> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open).ok_or(TransportFailure::Protocol)?;
    let content_start = start + open.len();
    let end = text[content_start..]
        .find(&close)
        .map(|offset| content_start + offset)
        .ok_or(TransportFailure::Protocol)?;
    Ok(&text[content_start..end])
}

fn xml_text<'a>(text: &'a str, tag: &str) -> Result<&'a str, TransportFailure> {
    let block = xml_block(text, tag)?;
    if block.contains('<') {
        return Err(TransportFailure::Protocol);
    }
    Ok(block.trim())
}

fn xml_values(text: &str, tag: &str) -> Result<Vec<String>, TransportFailure> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut rest = text;
    let mut values = Vec::new();
    while let Some(start) = rest.find(&open) {
        let content = &rest[start + open.len()..];
        let end = content.find(&close).ok_or(TransportFailure::Protocol)?;
        let value = content[..end].trim();
        if value.is_empty() || value.contains('<') || value.contains('>') {
            return Err(TransportFailure::Protocol);
        }
        values.push(value.to_owned());
        rest = &content[end + close.len()..];
    }
    if values.is_empty() {
        return Err(TransportFailure::Protocol);
    }
    Ok(values)
}
