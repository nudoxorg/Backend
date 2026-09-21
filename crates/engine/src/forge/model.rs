use super::*;

/// Closed result algebra for forge acquisition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgeAcquisitionOutcome {
    /// Exact source tree, metadata, and package manifests were admitted.
    Hit(Arc<ForgeAcquisitionResult>),
    /// The source was absent from the local cache while offline.
    Offline,
    /// The source could not be reached.
    Unavailable,
    /// The source asked for a later retry.
    RetryAfter(u64),
    /// The request or response was rejected by policy/protocol.
    Rejected(ForgeRejectReason),
    /// A local cache object or journal failed integrity checks.
    Corrupt,
}

/// Why forge acquisition was rejected locally.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ForgeRejectReason {
    /// A bound was exceeded.
    Bounds,
    /// The selected revision did not match the requested identity.
    RevisionMismatch,
    /// Archive path or format was unsafe.
    Archive,
    /// A package manifest was malformed.
    Manifest,
    /// Metadata response was malformed.
    Metadata,
    /// A delegated object failed local verification.
    Integrity,
    /// Network policy denied the operation.
    Policy,
}

/// One discovered package manifest inside an admitted forge tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageManifest {
    /// Manifest path relative to the selected repository subdirectory.
    pub path: Arc<str>,
    /// Ecosystem grammar selected by the manifest filename.
    pub ecosystem: RegistryEcosystem,
    /// Package name when the manifest records one.
    pub name: ForgeFact<ProductText>,
    /// Package version when the manifest records one. It remains absent when
    /// a workspace inherits or otherwise omits its version.
    pub version: ForgeFact<ProductText>,
    /// Dependency graph facts emitted by the manifest parser.
    pub dependencies: DependencyFacts<Box<[PackageDependencyRecord]>>,
}

/// Immutable forge result consumed by package pages and compiler adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeAcquisitionResult {
    /// Canonical coordinate requested by the caller.
    pub coordinate: ForgeCoordinate,
    /// Exact remote commit/tree resolution.
    pub resolution: ForgeResolution,
    /// Content identity of the original archive bytes.
    pub archive: RawArchiveObjectId,
    /// Shared content-addressed tree manifest.
    pub tree: Arc<TreeManifest>,
    /// Repository metadata retained with typed availability.
    pub metadata: ForgeRepositoryMetadata,
    /// Package manifests discovered under the optional subdirectory.
    pub manifests: Box<[ForgePackageManifest]>,
    /// Shared canonical source snapshot for the selected tree.
    pub snapshot: Arc<SourceSnapshot>,
    /// Root-bound delta from the forge source genesis snapshot.
    pub delta: Arc<AcquisitionDelta>,
    /// Immutable receipt binding coordinate, commit, tree, and archive.
    pub receipt: ForgeReceipt,
}

impl ForgeAcquisitionResult {
    /// Projects an admitted forge result into the GUI/product package DTO.
    /// The projection preserves every recorded/unavailable metadata fact and
    /// uses the same manifest paths and dependency rows admitted by the
    /// shared package graph.
    pub fn product_record(
        &self,
    ) -> Result<backend_library::ForgePackageRecord, ForgeProtocolError> {
        fn text(value: &str) -> Result<ProductText, ForgeProtocolError> {
            ProductText::new(value).map_err(|_| ForgeProtocolError::Malformed)
        }
        fn fact<T, U, F>(
            value: &ForgeFact<T>,
            map: F,
        ) -> Result<backend_library::ForgeFact<U>, ForgeProtocolError>
        where
            F: FnOnce(&T) -> Result<U, ForgeProtocolError>,
        {
            match value {
                ForgeFact::Recorded(value) => map(value).map(backend_library::ForgeFact::Recorded),
                ForgeFact::Unavailable(reason) => Ok(backend_library::ForgeFact::Unavailable(
                    ProductText::from_static(reason.as_str()),
                )),
            }
        }
        let metadata = backend_library::ForgeRepositoryMetadataRecord {
            owner: fact(&self.metadata.owner, |value| Ok(value.clone()))?,
            description: fact(&self.metadata.description, |value| Ok(value.clone()))?,
            license: fact(&self.metadata.license, |value| Ok(value.clone()))?,
            readme: fact(&self.metadata.readme, |value| Ok(value.clone()))?,
            topics: fact(&self.metadata.topics, |value| Ok(value.clone()))?,
            stars: fact(&self.metadata.stars, |value| Ok(*value))?,
            forks: fact(&self.metadata.forks, |value| Ok(*value))?,
        };
        let manifests = self
            .manifests
            .iter()
            .map(|manifest| {
                let (name, version) = match (&manifest.name, &manifest.version) {
                    (ForgeFact::Recorded(name), ForgeFact::Recorded(version)) => {
                        (Some(name.clone()), Some(version.clone()))
                    }
                    (ForgeFact::Recorded(name), _) => (Some(name.clone()), None),
                    (_, ForgeFact::Recorded(version)) => (None, Some(version.clone())),
                    _ => (None, None),
                };
                let dependency_count = match &manifest.dependencies {
                    DependencyFacts::Known(rows) => {
                        u16::try_from(rows.len()).map_err(|_| ForgeProtocolError::Malformed)?
                    }
                    DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => 0,
                };
                Ok(backend_library::ForgeManifestRecord {
                    path: text(manifest.path.as_ref())?,
                    ecosystem: text(manifest.ecosystem.as_str())?,
                    name,
                    version,
                    dependency_count,
                })
            })
            .collect::<Result<Vec<_>, ForgeProtocolError>>()?
            .into_boxed_slice();
        let revision =
            String::from_utf8_lossy(&self.coordinate.revision().canonical_bytes()).into_owned();
        let commit = backend_library::ForgeFact::Recorded(text(&self.resolution.commit.as_hex())?);
        let tree = match &self.resolution.tree {
            Some(tree) => backend_library::ForgeFact::Recorded(text(&tree.as_hex())?),
            None => backend_library::ForgeFact::Unavailable(ProductText::from_static(
                ForgeUnavailableReason::AuthorityOmitted.as_str(),
            )),
        };
        Ok(backend_library::ForgePackageRecord {
            coordinate: text(&self.coordinate.canonical())?,
            provider: text(self.coordinate.provider().as_str())?,
            owner: text(self.coordinate.owner())?,
            repository: text(self.coordinate.repository())?,
            revision: text(&revision)?,
            subdir: self.coordinate.subdir().map(text).transpose()?,
            commit,
            tree,
            metadata,
            manifests,
            source: backend_library::ForgeFact::Recorded(text(&hex(&self.tree.id().to_bytes()))?),
        })
    }
}

/// Durable forge publication receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeReceipt {
    /// Canonical coordinate identity.
    pub coordinate: [u8; ID_BYTES],
    /// Exact resolved commit identity.
    pub commit: ForgeObjectId,
    /// Exact remote tree identity, if reported.
    pub remote_tree: Option<ForgeObjectId>,
    /// Shared admitted content tree identity.
    pub tree: [u8; ID_BYTES],
    /// Original archive content identity.
    pub archive: [u8; ID_BYTES],
    /// Shared source snapshot root.
    pub snapshot: [u8; ID_BYTES],
    /// Shared acquisition delta identity.
    pub delta: [u8; ID_BYTES],
    /// Diagnostic observation time; it is not part of any source or snapshot
    /// identity and never selects a journal record.
    pub observed_at_millis: u64,
}
