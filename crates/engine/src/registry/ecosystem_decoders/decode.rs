//! Decodes native registry payloads into release and feed rows.

use super::*;

impl EcosystemAdapter {
    pub(in crate::registry::ecosystem) fn decode_cargo(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::super::NativeRelease>, TransportFailure> {
        let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
        let mut releases = Vec::new();
        for (line_index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > 1024 * 1024 || releases.len() >= super::super::MAX_NATIVE_RELEASES {
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

    pub(in crate::registry::ecosystem) fn decode_npm(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::super::NativeRelease>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let versions = root
            .get("versions")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        if versions.len() > super::super::MAX_NATIVE_RELEASES {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(versions.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(super::super::MAX_NATIVE_RELEASES)
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

    pub(in crate::registry::ecosystem) fn decode_python(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::super::NativeRelease>, TransportFailure> {
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
            if super::super::normalized_pypi_name(name)
                != super::super::normalized_pypi_name(self.package_name())
            {
                return Err(TransportFailure::Protocol);
            }
        }
        let files = root
            .get("files")
            .and_then(Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        if files.len() > super::super::MAX_NATIVE_RELEASES {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(files.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(super::super::MAX_NATIVE_RELEASES)
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
    ) -> Result<super::super::NativeRelease, TransportFailure> {
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
    ) -> Result<super::super::NativeRelease, TransportFailure> {
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
    ) -> Result<super::super::PackageCoordinate, TransportFailure> {
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

    /// JSON API document for one simple-index version.
    ///
    /// The simple index authenticates files. `requires_dist` lives on the
    /// per-version JSON API, so dependency admission fetches only the versions
    /// on the current page.
    pub(crate) fn pypi_json_url(&self, version: &str) -> String {
        format!(
            "{}/pypi/{}/{}/json",
            self.endpoint.url().trim_end_matches('/'),
            super::super::normalized_pypi_name(self.package_name()),
            component(version),
        )
    }

    /// Admits `info.requires_dist` from one PyPI JSON API document.
    ///
    /// A missing or null field is unknown metadata. An array is the complete
    /// declared set, including an empty set. Extras markers are optional
    /// edges; the requirement text keeps the original PEP 508 spelling.
    pub(crate) fn pypi_requires_dist(
        &self,
        bytes: &[u8],
        source: &super::super::PackageCoordinate,
        provenance: &[u8],
    ) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
        let root: Value = serde_json::from_slice(bytes).map_err(|_| TransportFailure::Protocol)?;
        let info = root
            .get("info")
            .and_then(Value::as_object)
            .ok_or(TransportFailure::Protocol)?;
        let name = info
            .get("name")
            .and_then(Value::as_str)
            .ok_or(TransportFailure::Protocol)?;
        if super::super::normalized_pypi_name(name)
            != super::super::normalized_pypi_name(self.package_name())
        {
            return Err(TransportFailure::Protocol);
        }
        let version = info
            .get("version")
            .and_then(Value::as_str)
            .ok_or(TransportFailure::Protocol)?;
        if version != source.version() {
            return Err(TransportFailure::Protocol);
        }
        let Some(requires) = info.get("requires_dist") else {
            return Ok(DependencyFacts::Unknown(
                ProductText::new("PyPI JSON API omits requires_dist")
                    .map_err(|_| TransportFailure::Protocol)?,
            ));
        };
        if requires.is_null() {
            return Ok(DependencyFacts::Unknown(
                ProductText::new("PyPI JSON API omits requires_dist")
                    .map_err(|_| TransportFailure::Protocol)?,
            ));
        }
        let requires = requires.as_array().ok_or(TransportFailure::Protocol)?;
        let limit = backend_library::MAX_PACKAGE_GRAPH_ROWS;
        if requires.len() > limit {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(requires.len()).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(limit).map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let mut rows = Vec::with_capacity(requires.len());
        let mut seen = BTreeSet::new();
        for requirement in requires {
            let requirement = requirement
                .as_str()
                .ok_or(TransportFailure::Protocol)?
                .trim();
            if requirement.is_empty() {
                return Err(TransportFailure::Protocol);
            }
            let name = pypi_requirement_name(requirement)?;
            let extra = pypi_requirement_is_extra(requirement);
            let row = dependency_record(
                source,
                backend_semantic::vocabulary::RegistryEcosystem::Pypi,
                name,
                requirement,
                if extra {
                    DependencyScope::Optional
                } else {
                    DependencyScope::Runtime
                },
                extra,
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

    pub(crate) fn maven_dependencies(
        &self,
        bytes: &[u8],
        source: &super::super::PackageCoordinate,
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

    pub(in crate::registry::ecosystem) fn decode_nuget(
        &self,
        bytes: &[u8],
    ) -> Result<Vec<super::super::NativeRelease>, TransportFailure> {
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
        source: &super::super::PackageCoordinate,
        module: &GoMod,
        provenance: &[u8],
    ) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, TransportFailure> {
        let source = PackageReference::parse(source.as_str().to_owned())
            .map_err(|_| TransportFailure::Protocol)?;
        let digest = *blake3::hash(provenance).as_bytes();
        let mut rows = Vec::with_capacity(module.requires.len());
        for requirement in &module.requires {
            let scope = if requirement.indirect {
                DependencyScope::Development
            } else {
                DependencyScope::Runtime
            };
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
                scope,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: digest,
                    provenance: digest,
                },
            ));
        }
        Ok(DependencyFacts::Known(
            admit_dependency_rows(collapse_dependency_rows(rows))
                .map_err(|_| TransportFailure::Protocol)?,
        ))
    }

    pub(crate) fn go_release(
        &self,
        version: &str,
        checksum: RegistryChecksum,
        provenance: &[u8],
        facts: ReleaseFacts,
    ) -> Result<super::super::NativeRelease, TransportFailure> {
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
        request: super::super::FeedRequest,
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
        releases: Vec<super::super::NativeRelease>,
        request: super::super::FeedRequest,
        start: usize,
        prefix: [u8; 24],
        total: usize,
    ) -> Result<super::super::FeedPage, TransportFailure> {
        let end = start.saturating_add(releases.len());
        if end > total || releases.len() > request.max_items {
            return Err(TransportFailure::Bounds);
        }
        let packages = releases
            .iter()
            .map(|release| super::super::RemotePackage {
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
            return Ok(super::super::FeedPage {
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
        Ok(super::super::FeedPage {
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
