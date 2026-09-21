//! Parsers for the native package-registry protocols.
//!
//! The wire shapes here are the public upstream protocols themselves. The
//! transport owns follow-up requests for protocols whose listing omits an
//! authenticated archive digest (Maven and Go); this module stays pure and
//! never turns a missing claim into a fabricated checksum.

use std::collections::BTreeSet;

use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductText,
    admit_dependency_rows,
};
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
            let mut release = self.release_from_checksum(
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
            )?;
            release.dependency_facts =
                cargo_dependencies(&release.coordinate, &row, line.as_bytes())?;
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
                let mut release = self.release_from_checksum(
                    version,
                    field(dist, "tarball")?.to_owned(),
                    checksum,
                    &encoded,
                    facts(
                        standing,
                        DownloadCount::NotReported(DownloadCountGap::Unsupported),
                    ),
                )?;
                release.dependency_facts = npm_dependencies(&release.coordinate, row, &encoded)?;
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
                    dependency_groups: entry.get("dependencyGroups").cloned(),
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
                let dependency_groups = release.dependency_groups.clone();
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
    let Some(values) = row.get("deps").and_then(Value::as_array) else {
        return Ok(DependencyFacts::Unknown(
            ProductText::new("Cargo index row omits dependency metadata")
                .map_err(|_| TransportFailure::Protocol)?,
        ));
    };
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
        rows.push(dependency_record(
            source,
            backend_semantic::vocabulary::RegistryEcosystem::Cargo,
            name,
            requirement,
            scope,
            value
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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
        let Some(values) = row.get(field_name).and_then(Value::as_object) else {
            continue;
        };
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
        let dependencies = group
            .get("dependencies")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        for dependency in dependencies {
            let name = field(dependency, "id")?;
            let requirement = dependency
                .get("range")
                .and_then(Value::as_str)
                .unwrap_or("*");
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
