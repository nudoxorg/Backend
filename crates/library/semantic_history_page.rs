//! Snapshot-bound, lossless pages of retained compiler versions and history images.

use crate::{
    PackageReference, ProductAdmissionError, SemanticHistoryImagePublicationProof,
    SemanticHistoryInputReplayStatus, SemanticHistoryPublicationProof,
    SemanticHistoryPublicationStatus, SemanticHistorySelectionStamp, SemanticVersionRecord,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

/// Default number of whole image proofs (or versions without a published catalog) per page.
pub const DEFAULT_SEMANTIC_HISTORY_PAGE_ITEMS: u16 = 8;
/// Maximum number of whole items requested from one history page.
pub const MAX_SEMANTIC_HISTORY_PAGE_ITEMS: u16 = 16;
/// Bound on the complete typed page, leaving room for presentation and the transport envelope.
pub const MAX_SEMANTIC_HISTORY_PAGE_BYTES: usize = 12 * 1024;

/// A history-page refusal preserves why the complete read could not be served.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticHistoryPageError {
    /// The requested item count is outside the maintained bounds.
    InvalidLimit,
    /// The supplied continuation is malformed or outside its catalog position.
    InvalidCursor,
    /// The saved semantic/source/version/history basis no longer matches this owner.
    StaleBasis,
    /// An owner record, catalog or page failed strict structural admission.
    InvalidCatalog,
    /// One whole version/header/image item cannot fit the fixed response allowance.
    WholeItemOversized,
}

impl std::fmt::Display for SemanticHistoryPageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLimit => "history page limit must be between 1 and 16",
            Self::InvalidCursor => "invalid semantic history continuation; copy semantic_continue or start a fresh semantic-versions page",
            Self::StaleBasis => "semantic history continuation has a different source or catalog basis; start a fresh semantic-versions page",
            Self::InvalidCatalog => "retained semantic history failed catalog admission",
            Self::WholeItemOversized => "one complete semantic history item exceeds the fixed page budget; lowering limit cannot make this item fit",
        })
    }
}
impl std::error::Error for SemanticHistoryPageError {}
impl From<ProductAdmissionError> for SemanticHistoryPageError {
    fn from(_: ProductAdmissionError) -> Self {
        Self::InvalidCatalog
    }
}

/// Exact publication header; the complete ordered image proofs are read through history pages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticHistoryCatalog {
    /// Exact committed selection authenticated by the publication fence.
    pub selection: SemanticHistorySelectionStamp,
    /// Package name used by the native semantic target.
    pub target_package: String,
    /// Exact compiler source/version coordinate.
    pub target_coordinate: String,
    /// Commitment to the complete ordered image set, never just the returned page.
    pub package_identity: [u8; 32],
    /// Total number of images in that set.
    pub image_count: u32,
    /// Public branch tip verified by the publication proof.
    pub reference_tip: [u8; 32],
    /// Package commit proven reachable from the reference tip.
    pub reachable_commit: [u8; 32],
    /// Persisted source replay authority; paging does not create new authority.
    pub input_replay_status: SemanticHistoryInputReplayStatus,
}

impl SemanticHistoryCatalog {
    fn from_proof(proof: &SemanticHistoryPublicationProof) -> Result<Self, ProductAdmissionError> {
        Ok(Self {
            selection: proof.selection.clone(),
            target_package: proof.target_package.clone(),
            target_coordinate: proof.target_coordinate.clone(),
            package_identity: proof.package_identity,
            image_count: u32::try_from(proof.images.len())
                .map_err(|_| ProductAdmissionError::SemanticVersionShape)?,
            reference_tip: proof.reference_tip,
            reachable_commit: proof.reachable_commit,
            input_replay_status: proof.input_replay_status,
        })
    }

    pub(crate) fn admit(
        &self,
        version: &SemanticVersionRecord,
        commit: [u8; 32],
    ) -> Result<(), ProductAdmissionError> {
        if self.image_count == 0
            || self.image_count != version.artifacts
            || self.selection.profile != version.profile
            || self.reference_tip != commit
            || self.reachable_commit != commit
            || self.target_package.is_empty()
            || self.target_package.len() > crate::MAX_PRODUCT_TEXT_BYTES
            || self.target_coordinate.is_empty()
            || self.target_coordinate.len() > crate::MAX_PRODUCT_TEXT_BYTES
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }

    /// Reassembles the original full publication proof after collecting every image in order.
    /// No incomplete catalog is admitted as a complete proof.
    ///
    /// # Errors
    /// Refuses missing, reordered, duplicated or changed image proofs and parent commitments.
    pub fn assemble(
        &self,
        images: Box<[SemanticHistoryImagePublicationProof]>,
    ) -> Result<SemanticHistoryPublicationProof, ProductAdmissionError> {
        let proof = SemanticHistoryPublicationProof {
            selection: self.selection.clone(),
            target_package: self.target_package.clone(),
            target_coordinate: self.target_coordinate.clone(),
            package_identity: self.package_identity,
            images,
            reference_tip: self.reference_tip,
            reachable_commit: self.reachable_commit,
            input_replay_status: self.input_replay_status,
        };
        if proof.images.len() != self.image_count as usize
            || proof.package_identity != proof.recompute_package_identity()
            || proof.images.iter().enumerate().any(|(ordinal, image)| {
                image.image.artifact_ordinal as usize != ordinal
                    || image.parent_commits.len() > 1
                    || (ordinal > 0
                        && image.parent_commits.as_ref()
                            != [proof.images[ordinal - 1].history_commit])
            })
            || proof.images.last().map(|image| image.history_commit) != Some(proof.reachable_commit)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(proof)
    }
}

/// Exact immutable product roots and observed history catalog shared by a page chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionsBasis {
    /// Root of the retained semantic-publication relation.
    pub semantic_relation_root: [u8; 32],
    /// Root of the answering selected Project source relation.
    pub source_relation_root: [u8; 32],
    /// Commitment to every version/source field, excluding the reconstructible history sidecar.
    pub versions_metadata_identity: [u8; 32],
    /// Commitment to the complete observed versions and their exact history statuses.
    pub versions_identity: [u8; 32],
}

/// Bounded opaque continuation. Owner admission checks its complete package and basis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SemanticVersionsCursor(String);

impl SemanticVersionsCursor {
    /// Admits a bounded hex-encoded cursor without interpreting it as authority.
    ///
    /// # Errors
    /// Refuses tokens outside the exact hex-encoded position bound.
    pub fn new(value: impl Into<String>) -> Result<Self, ProductAdmissionError> {
        let value = value.into();
        if value.len() != 340 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(Self(value))
    }
    /// Returns the exact token for the next request.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    fn decode(&self) -> Result<Position, ProductAdmissionError> {
        let mut bytes = [0u8; 170];
        for (byte, pair) in bytes.iter_mut().zip(self.0.as_bytes().chunks_exact(2)) {
            let text = std::str::from_utf8(pair)
                .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
            *byte = u8::from_str_radix(text, 16)
                .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        }
        let id = |offset| {
            let mut id = [0u8; 32];
            id.copy_from_slice(&bytes[offset..offset + 32]);
            id
        };
        Ok(Position {
            schema: u16::from_be_bytes([bytes[0], bytes[1]]),
            package_identity: id(2),
            basis: SemanticVersionsBasis {
                semantic_relation_root: id(34),
                source_relation_root: id(66),
                versions_metadata_identity: id(98),
                versions_identity: id(130),
            },
            version: u32::from_be_bytes(
                bytes[162..166]
                    .try_into()
                    .map_err(|_| ProductAdmissionError::SemanticVersionShape)?,
            ),
            image: u32::from_be_bytes(
                bytes[166..170]
                    .try_into()
                    .map_err(|_| ProductAdmissionError::SemanticVersionShape)?,
            ),
        })
    }
}
impl TryFrom<String> for SemanticVersionsCursor {
    type Error = ProductAdmissionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<SemanticVersionsCursor> for String {
    fn from(value: SemanticVersionsCursor) -> Self {
        value.0
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    schema: u16,
    package_identity: [u8; 32],
    basis: SemanticVersionsBasis,
    version: u32,
    image: u32,
}
impl Position {
    fn encode(&self) -> Result<SemanticVersionsCursor, ProductAdmissionError> {
        let mut bytes = [0u8; 170];
        bytes[..2].copy_from_slice(&self.schema.to_be_bytes());
        for (offset, id) in [
            (2, self.package_identity),
            (34, self.basis.semantic_relation_root),
            (66, self.basis.source_relation_root),
            (98, self.basis.versions_metadata_identity),
            (130, self.basis.versions_identity),
        ] {
            bytes[offset..offset + 32].copy_from_slice(&id);
        }
        bytes[162..166].copy_from_slice(&self.version.to_be_bytes());
        bytes[166..170].copy_from_slice(&self.image.to_be_bytes());
        let mut text = String::with_capacity(340);
        for byte in bytes {
            use std::fmt::Write;
            write!(&mut text, "{byte:02x}")
                .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        }
        SemanticVersionsCursor::new(text)
    }
}

/// One version with a whole contiguous segment of its published image catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionPageEntry {
    /// Exact selected source operand; PublishedCatalog makes the separately paged proof explicit.
    pub version: SemanticVersionRecord,
    /// Position in the immutable retained-version order.
    pub version_ordinal: u32,
    /// False on continuation segments of an already returned version.
    pub first_segment: bool,
    /// First catalog ordinal returned in this segment.
    pub start_ordinal: u32,
    /// Exact parent immediately before this segment, if it starts inside a catalog.
    pub previous_commit: Option<[u8; 32]>,
    /// Whole image proofs, in canonical catalog order.
    pub images: Box<[SemanticHistoryImagePublicationProof]>,
}

/// Availability of the requested exact history-page basis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticVersionsPageState {
    /// The returned segments belong to the admitted page basis.
    Ready,
    /// The cold owner is reconstructing the same committed history; the cursor has not advanced.
    Preparing,
}

/// A complete bounded page. Continuation is explicit even for a single large generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticVersionsPage {
    /// Whether the exact requested history is available; Preparing never emits image segments.
    pub state: SemanticVersionsPageState,
    /// Exact requested package.
    pub package: PackageReference,
    /// Snapshot and catalog commitments pinned by every continuation.
    pub basis: SemanticVersionsBasis,
    /// Original bounded item limit, retained by the copyable next action.
    pub limit: u16,
    /// Exact number of retained versions in the pinned observation, including later pages.
    pub version_count: u16,
    /// Whole version/catalog segments, never a truncated proof or field.
    pub entries: Box<[SemanticVersionPageEntry]>,
    /// Exact continuation, absent only when every version and image was returned.
    pub next_cursor: Option<SemanticVersionsCursor>,
}

struct HashWriter {
    hash: blake3::Hasher,
    bytes: u64,
}
impl HashWriter {
    fn new(domain: &[u8]) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(domain);
        Self { hash, bytes: 0 }
    }
}
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hash.update(bytes);
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Clone only metadata, never an entire image catalog merely to replace its status.
fn with_history(
    version: &SemanticVersionRecord,
    history_status: SemanticHistoryPublicationStatus,
) -> SemanticVersionRecord {
    SemanticVersionRecord {
        package: version.package.clone(),
        coordinate: version.coordinate.clone(),
        profile: version.profile,
        generation: version.generation,
        generation_root: version.generation_root,
        dependency_set: version.dependency_set,
        manifest: version.manifest,
        artifacts: version.artifacts,
        semantic_bytes: version.semantic_bytes,
        complete: version.complete,
        selected: version.selected,
        freshness: version.freshness,
        history_status,
        selected_source_frontier: version.selected_source_frontier.clone(),
    }
}

/// Structural work accounted while preparing a catalog or emitting one page.
/// These are operation counts/bytes, not elapsed-time or compiler-performance claims.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticHistoryReadWork {
    /// Complete version records admitted when constructing the immutable catalog.
    pub admitted_versions: u64,
    /// Complete image proofs admitted through those version records.
    pub admitted_images: u64,
    /// Full version and metadata bytes sent to the catalog commitment hashers.
    pub catalog_hashed_bytes: u64,
    /// Whole image proofs copied into bounded page candidates, including retries.
    pub copied_images: u64,
    /// Complete bounded page candidates measured before egress admission.
    pub measured_page_bytes: u64,
}

fn add_owned_bytes(total: &mut usize, bytes: usize) -> Option<()> {
    *total = total.checked_add(bytes)?;
    Some(())
}

fn package_owned_bytes(package: &PackageReference) -> usize {
    match package {
        PackageReference::Local(text) => text.retained_capacity(),
        // PackageCoordinate stores its spelling in Box<str>; facts are inline.
        PackageReference::Purl(coordinate) => coordinate.as_str().len(),
    }
}

fn version_owned_bytes(version: &SemanticVersionRecord) -> Option<usize> {
    // Exhaustive destructuring makes new record/variant fields require an
    // accounting decision rather than silently excluding nested storage.
    let SemanticVersionRecord {
        package,
        coordinate,
        profile: _,
        generation: _,
        generation_root: _,
        dependency_set: _,
        manifest: _,
        artifacts: _,
        semantic_bytes: _,
        complete: _,
        selected: _,
        freshness: _,
        history_status,
        selected_source_frontier,
    } = version;
    let mut bytes = package_owned_bytes(package);
    add_owned_bytes(&mut bytes, coordinate.as_str().len())?;
    if let Some(crate::SelectedProjectSourceFrontier {
        package,
        source_relation_root: _,
        source_version: _,
        file_count: _,
    }) = selected_source_frontier
    {
        add_owned_bytes(&mut bytes, package_owned_bytes(package))?;
    }
    match history_status {
        SemanticHistoryPublicationStatus::Published {
            selection_id: _,
            commit: _,
            reference,
            proof,
        } => {
            add_owned_bytes(&mut bytes, reference.capacity())?;
            let SemanticHistoryPublicationProof {
                selection: _,
                target_package,
                target_coordinate,
                package_identity: _,
                images,
                reference_tip: _,
                reachable_commit: _,
                input_replay_status: _,
            } = proof;
            add_owned_bytes(&mut bytes, target_package.capacity())?;
            add_owned_bytes(&mut bytes, target_coordinate.capacity())?;
            add_owned_bytes(&mut bytes, std::mem::size_of_val(images.as_ref()))?;
            for SemanticHistoryImagePublicationProof {
                image: _,
                history_commit: _,
                parent_commits,
            } in images.iter()
            {
                add_owned_bytes(&mut bytes, std::mem::size_of_val(parent_commits.as_ref()))?;
            }
        }
        SemanticHistoryPublicationStatus::PublishedCatalog {
            selection_id: _,
            commit: _,
            reference,
            catalog,
        } => {
            add_owned_bytes(&mut bytes, reference.capacity())?;
            let SemanticHistoryCatalog {
                selection: _,
                target_package,
                target_coordinate,
                package_identity: _,
                image_count: _,
                reference_tip: _,
                reachable_commit: _,
                input_replay_status: _,
            } = catalog;
            add_owned_bytes(&mut bytes, target_package.capacity())?;
            add_owned_bytes(&mut bytes, target_coordinate.capacity())?;
        }
        SemanticHistoryPublicationStatus::Deferred {
            selection_id: _,
            reason,
        }
        | SemanticHistoryPublicationStatus::Refused {
            selection_id: _,
            reason,
        } => {
            add_owned_bytes(&mut bytes, reason.capacity())?;
        }
        SemanticHistoryPublicationStatus::NotSelected
        | SemanticHistoryPublicationStatus::NotRequested { selection_id: _ }
        | SemanticHistoryPublicationStatus::Pending { selection_id: _ }
        | SemanticHistoryPublicationStatus::Superseded { selection_id: _ } => {}
    }
    Some(bytes)
}

/// Immutable, strictly admitted owner observation reused by an exact page chain.
/// Construction consumes the full records once. Paging touches only metadata
/// headers, requested images, their preceding edge and the next position.
pub struct SemanticVersionsCatalog {
    package: PackageReference,
    package_identity: [u8; 32],
    basis: SemanticVersionsBasis,
    versions: Box<[SemanticVersionRecord]>,
    page_versions: Box<[SemanticVersionRecord]>,
    reconstructing: bool,
    work: SemanticHistoryReadWork,
}

impl SemanticVersionsCatalog {
    /// Requested storage owned by this immutable catalog, including its inline
    /// value, both record buffers and every transitive owned allocation.
    ///
    /// String capacity is charged rather than encoded size. This excludes Arc
    /// control storage, allocator overhead and transient preparation/page work;
    /// the retaining owner must account for its own control/key allocations.
    /// `None` means arithmetic overflow and permits only uncached use.
    #[must_use]
    pub fn retained_allocation_bytes(&self) -> Option<usize> {
        let Self {
            package,
            package_identity: _,
            basis: _,
            versions,
            page_versions,
            reconstructing: _,
            work: _,
        } = self;
        let mut bytes = std::mem::size_of::<Self>();
        add_owned_bytes(&mut bytes, package_owned_bytes(package))?;
        add_owned_bytes(&mut bytes, std::mem::size_of_val(versions.as_ref()))?;
        add_owned_bytes(&mut bytes, std::mem::size_of_val(page_versions.as_ref()))?;
        for version in versions.iter().chain(page_versions.iter()) {
            add_owned_bytes(&mut bytes, version_owned_bytes(version)?)?;
        }
        Some(bytes)
    }

    /// Admits and commits one immutable owner observation. Roots come from its
    /// captured workspace, never from a caller continuation.
    ///
    /// # Errors
    /// Refuses inconsistent full records, package/source roots, or catalog-only inputs.
    pub fn new(
        package: PackageReference,
        semantic_relation_root: [u8; 32],
        source_relation_root: [u8; 32],
        versions: Box<[SemanticVersionRecord]>,
    ) -> Result<Self, SemanticHistoryPageError> {
        if versions.len() > crate::MAX_PRODUCT_ROWS {
            return Err(SemanticHistoryPageError::InvalidCatalog);
        }
        for version in versions.iter() {
            version.admit()?;
            if matches!(
                version.history_status,
                SemanticHistoryPublicationStatus::PublishedCatalog { .. }
            ) || version.package != package
                || version
                    .selected_source_frontier
                    .as_ref()
                    .is_some_and(|frontier| frontier.source_relation_root != source_relation_root)
            {
                return Err(SemanticHistoryPageError::InvalidCatalog);
            }
        }
        let mut hash = HashWriter::new(b"backend.semantic-versions-page.v1\0");
        serde_json::to_writer(&mut hash, &versions)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        let mut metadata_hash = HashWriter::new(b"backend.semantic-versions-metadata.v1\0");
        for version in versions.iter() {
            serde_json::to_writer(
                &mut metadata_hash,
                &with_history(version, SemanticHistoryPublicationStatus::NotSelected),
            )
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        }
        let basis = SemanticVersionsBasis {
            semantic_relation_root,
            source_relation_root,
            versions_metadata_identity: *metadata_hash.hash.finalize().as_bytes(),
            versions_identity: *hash.hash.finalize().as_bytes(),
        };
        let package_identity = *blake3::hash(package.as_str().as_bytes()).as_bytes();
        let page_versions = versions
            .iter()
            .map(|version| {
                let status = match &version.history_status {
                    SemanticHistoryPublicationStatus::Published {
                        selection_id,
                        commit,
                        reference,
                        proof,
                    } => SemanticHistoryPublicationStatus::PublishedCatalog {
                        selection_id: *selection_id,
                        commit: *commit,
                        reference: reference.clone(),
                        catalog: SemanticHistoryCatalog::from_proof(proof)?,
                    },
                    other => other.clone(),
                };
                Ok(with_history(version, status))
            })
            .collect::<Result<Vec<_>, SemanticHistoryPageError>>()?
            .into_boxed_slice();
        let reconstructing = versions.iter().any(|version| {
            version.selected
                && matches!(
                    &version.history_status,
                    SemanticHistoryPublicationStatus::Pending { .. }
                        | SemanticHistoryPublicationStatus::NotRequested { .. }
                        | SemanticHistoryPublicationStatus::Deferred { .. }
                )
        });
        let work = SemanticHistoryReadWork {
            admitted_versions: versions.len() as u64,
            admitted_images: versions
                .iter()
                .map(|version| match &version.history_status {
                    SemanticHistoryPublicationStatus::Published { proof, .. } => {
                        proof.images.len() as u64
                    }
                    _ => 0,
                })
                .sum(),
            catalog_hashed_bytes: hash.bytes.saturating_add(metadata_hash.bytes),
            ..SemanticHistoryReadWork::default()
        };
        Ok(Self {
            package,
            package_identity,
            basis,
            versions,
            page_versions,
            reconstructing,
            work,
        })
    }

    /// Exact requested package held by this observation.
    #[must_use]
    pub fn package(&self) -> &PackageReference {
        &self.package
    }

    /// Exact semantic/source and full-metadata/history commitments of this observation.
    #[must_use]
    pub fn basis(&self) -> &SemanticVersionsBasis {
        &self.basis
    }

    /// One-time complete catalog work. Repeated pages never repeat these counts.
    #[must_use]
    pub fn preparation_work(&self) -> SemanticHistoryReadWork {
        self.work
    }

    /// Emits a bounded page without re-admitting or rehashing the complete catalog.
    ///
    /// # Errors
    /// Refuses invalid bounds/cursors, stale saved bases, or an unrenderable whole item.
    pub fn page(
        &self,
        limit: u16,
        cursor: Option<&SemanticVersionsCursor>,
    ) -> Result<SemanticVersionsPage, SemanticHistoryPageError> {
        self.page_with_work(limit, cursor).0
    }

    /// Emits a page and retains structural counts even for refused requests.
    #[must_use]
    pub fn page_with_work(
        &self,
        limit: u16,
        cursor: Option<&SemanticVersionsCursor>,
    ) -> (
        Result<SemanticVersionsPage, SemanticHistoryPageError>,
        SemanticHistoryReadWork,
    ) {
        let mut work = SemanticHistoryReadWork::default();
        let result = self.page_inner(limit, cursor, &mut work);
        (result, work)
    }

    fn page_inner(
        &self,
        limit: u16,
        cursor: Option<&SemanticVersionsCursor>,
        work: &mut SemanticHistoryReadWork,
    ) -> Result<SemanticVersionsPage, SemanticHistoryPageError> {
        if limit == 0 || limit > MAX_SEMANTIC_HISTORY_PAGE_ITEMS {
            return Err(SemanticHistoryPageError::InvalidLimit);
        }
        let versions = &self.versions;
        let basis = &self.basis;
        let package = self.package.clone();
        let package_identity = self.package_identity;
        let mut position = match cursor {
            Some(cursor) => cursor
                .decode()
                .map_err(|_| SemanticHistoryPageError::InvalidCursor)?,
            None => Position {
                schema: 1,
                package_identity,
                basis: basis.clone(),
                version: 0,
                image: 0,
            },
        };
        if position.schema != 1
            || position.package_identity != package_identity
            || position.basis.semantic_relation_root != basis.semantic_relation_root
            || position.basis.source_relation_root != basis.source_relation_root
            || position.basis.versions_metadata_identity != basis.versions_metadata_identity
        {
            return Err(SemanticHistoryPageError::StaleBasis);
        }
        if position.version as usize > versions.len()
            || (cursor.is_some() && position.version as usize == versions.len())
        {
            return Err(SemanticHistoryPageError::InvalidCursor);
        }
        if versions
            .get(position.version as usize)
            .is_some_and(|version| position.image >= version.artifacts)
        {
            return Err(SemanticHistoryPageError::InvalidCursor);
        }
        if position.basis.versions_identity != basis.versions_identity {
            if self.reconstructing
                && let Some(cursor) = cursor
            {
                let page = SemanticVersionsPage {
                    state: SemanticVersionsPageState::Preparing,
                    package,
                    basis: position.basis.clone(),
                    limit,
                    entries: Box::new([]),
                    version_count: versions.len() as u16,
                    next_cursor: Some(cursor.clone()),
                };
                page.admit()?;
                return Ok(page);
            }
            return Err(SemanticHistoryPageError::StaleBasis);
        }
        let mut page = SemanticVersionsPage {
            state: SemanticVersionsPageState::Ready,
            package,
            basis: basis.clone(),
            limit,
            entries: Box::new([]),
            version_count: versions.len() as u16,
            next_cursor: None,
        };
        let mut entries = Vec::new();
        let mut used = 0usize;
        while let Some(original) = versions.get(position.version as usize) {
            if used >= usize::from(limit) {
                break;
            }
            let images = match &original.history_status {
                SemanticHistoryPublicationStatus::Published { proof, .. } => proof.images.as_ref(),
                _ => &[][..],
            };
            let start = position.image as usize;
            if (images.is_empty() && start != 0) || (!images.is_empty() && start >= images.len()) {
                return Err(SemanticHistoryPageError::InvalidCursor);
            }
            let mut count = if images.is_empty() {
                0
            } else {
                (usize::from(limit) - used).min(images.len() - start)
            };
            loop {
                let version = self.page_versions[position.version as usize].clone();
                work.copied_images = work.copied_images.saturating_add(count as u64);
                let entry = SemanticVersionPageEntry {
                    version,
                    version_ordinal: position.version,
                    first_segment: start == 0,
                    start_ordinal: position.image,
                    previous_commit: start
                        .checked_sub(1)
                        .map(|prior| images[prior].history_commit),
                    images: images[start..start + count].into(),
                };
                let finished = images.is_empty() || start + count == images.len();
                let next = Position {
                    schema: 1,
                    package_identity,
                    basis: page.basis.clone(),
                    version: position.version + u32::from(finished),
                    image: if finished { 0 } else { (start + count) as u32 },
                };
                entries.push(entry);
                work.copied_images = work.copied_images.saturating_add(
                    entries
                        .iter()
                        .map(|entry| entry.images.len() as u64)
                        .sum::<u64>(),
                );
                page.entries = entries.clone().into_boxed_slice();
                page.next_cursor = if next.version as usize == versions.len() {
                    None
                } else {
                    Some(next.encode()?)
                };
                let bytes = crate::surface::serialized_json_size(&page);
                work.measured_page_bytes = work.measured_page_bytes.saturating_add(bytes as u64);
                if bytes <= MAX_SEMANTIC_HISTORY_PAGE_BYTES {
                    position = next;
                    used += count.max(1);
                    break;
                }
                entries.pop();
                if count > 1 {
                    count -= 1;
                    continue;
                }
                if entries.is_empty() {
                    return Err(SemanticHistoryPageError::WholeItemOversized);
                }
                page.entries = entries.into_boxed_slice();
                page.next_cursor = Some(position.encode()?);
                page.admit()?;
                return Ok(page);
            }
        }
        page.admit()?;
        Ok(page)
    }
}

impl SemanticVersionsPage {
    /// Compatibility constructor for callers without a retained catalog. This
    /// performs complete admission/hashing once for this call. Owner page routes
    /// should keep a `SemanticVersionsCatalog` for their exact immutable snapshot.
    ///
    /// # Errors
    /// Refuses bounds, inconsistent records, stale cursors or unrenderable whole items.
    pub fn from_versions(
        package: PackageReference,
        semantic_relation_root: [u8; 32],
        source_relation_root: [u8; 32],
        versions: &[SemanticVersionRecord],
        limit: u16,
        cursor: Option<&SemanticVersionsCursor>,
    ) -> Result<Self, SemanticHistoryPageError> {
        if limit == 0 || limit > MAX_SEMANTIC_HISTORY_PAGE_ITEMS {
            return Err(SemanticHistoryPageError::InvalidLimit);
        }
        if versions.len() > crate::MAX_PRODUCT_ROWS {
            return Err(SemanticHistoryPageError::InvalidCatalog);
        }
        SemanticVersionsCatalog::new(
            package,
            semantic_relation_root,
            source_relation_root,
            versions.to_vec().into_boxed_slice(),
        )?
        .page(limit, cursor)
    }

    /// Checks page framing, exact version/catalog binding, ordinals, parent edges and byte budget.
    ///
    /// # Errors
    /// Refuses a page with incomplete framing, changed proof edges or an invalid continuation.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.state == SemanticVersionsPageState::Preparing {
            return if self.entries.is_empty()
                && self.next_cursor.as_ref().is_some_and(|cursor| {
                    cursor.decode().is_ok_and(|position| {
                        position.schema == 1
                            && position.version < u32::from(self.version_count)
                            && position.basis == self.basis
                            && position.package_identity
                                == *blake3::hash(self.package.as_str().as_bytes()).as_bytes()
                    })
                })
                && self.version_count as usize <= crate::MAX_PRODUCT_ROWS
                && self.limit > 0
                && self.limit <= MAX_SEMANTIC_HISTORY_PAGE_ITEMS
                && crate::surface::serialized_json_size(self) <= MAX_SEMANTIC_HISTORY_PAGE_BYTES
            {
                Ok(())
            } else {
                Err(ProductAdmissionError::SemanticVersionShape)
            };
        }
        if self.limit == 0
            || self.limit > MAX_SEMANTIC_HISTORY_PAGE_ITEMS
            || self.entries.len() > usize::from(self.limit)
            || crate::surface::serialized_json_size(self) > MAX_SEMANTIC_HISTORY_PAGE_BYTES
            || (self.entries.is_empty() && self.next_cursor.is_some())
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        if self.version_count as usize > crate::MAX_PRODUCT_ROWS
            || (self.entries.is_empty() && self.version_count != 0)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let mut items = 0usize;
        let mut next_position = None;
        let mut previous_entry: Option<&SemanticVersionPageEntry> = None;
        for entry in &self.entries {
            if entry.version_ordinal >= u32::from(self.version_count)
                || next_position.is_some_and(|(version, image)| {
                    (entry.version_ordinal, entry.start_ordinal) != (version, image)
                })
            {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }

            if previous_entry.is_some_and(|previous| {
                previous.version_ordinal == entry.version_ordinal
                    && (previous.version != entry.version
                        || previous.images.last().map(|image| image.history_commit)
                            != entry.previous_commit)
            }) {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
            entry.version.admit()?;
            if entry.version.package != self.package
                || entry
                    .version
                    .selected_source_frontier
                    .as_ref()
                    .is_some_and(|frontier| {
                        frontier.source_relation_root != self.basis.source_relation_root
                    })
                || entry.first_segment != (entry.start_ordinal == 0)
            {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
            match &entry.version.history_status {
                SemanticHistoryPublicationStatus::PublishedCatalog { catalog, .. } => {
                    if entry.images.is_empty()
                        || entry.start_ordinal as usize + entry.images.len()
                            > catalog.image_count as usize
                        || (entry.start_ordinal == 0 && entry.previous_commit.is_some())
                        || (entry.start_ordinal != 0 && entry.previous_commit.is_none())
                    {
                        return Err(ProductAdmissionError::SemanticVersionShape);
                    }
                    for (offset, image) in entry.images.iter().enumerate() {
                        let ordinal = entry.start_ordinal as usize + offset;
                        if image.image.artifact_ordinal as usize != ordinal
                            || image.parent_commits.len() > 1
                            || (ordinal > 0
                                && image.parent_commits.as_ref()
                                    != [if offset == 0 {
                                        entry.previous_commit.unwrap_or([0; 32])
                                    } else {
                                        entry.images[offset - 1].history_commit
                                    }])
                        {
                            return Err(ProductAdmissionError::SemanticVersionShape);
                        }
                    }
                    if entry.start_ordinal as usize + entry.images.len()
                        == catalog.image_count as usize
                        && entry.images.last().map(|image| image.history_commit)
                            != Some(catalog.reachable_commit)
                    {
                        return Err(ProductAdmissionError::SemanticVersionShape);
                    }
                }
                SemanticHistoryPublicationStatus::Published { .. } => {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                _ if !entry.images.is_empty()
                    || entry.start_ordinal != 0
                    || entry.previous_commit.is_some() =>
                {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                _ => {}
            }
            let end = entry.start_ordinal as usize + entry.images.len();
            let finished = match &entry.version.history_status {
                SemanticHistoryPublicationStatus::PublishedCatalog { catalog, .. } => {
                    end == catalog.image_count as usize
                }
                _ => true,
            };
            next_position = Some((
                entry.version_ordinal + u32::from(finished),
                if finished { 0 } else { end as u32 },
            ));
            items += entry.images.len().max(1);
            previous_entry = Some(entry);
        }
        if let Some((version, image)) = next_position {
            match &self.next_cursor {
                None if version != u32::from(self.version_count) || image != 0 => {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                Some(cursor) => {
                    let position = cursor.decode()?;
                    if version >= u32::from(self.version_count)
                        || position.schema != 1
                        || position.package_identity
                            != *blake3::hash(self.package.as_str().as_bytes()).as_bytes()
                        || position.basis != self.basis
                        || position.version != version
                        || position.image != image
                    {
                        return Err(ProductAdmissionError::SemanticVersionShape);
                    }
                }
                None => {}
            }
        }
        if items > usize::from(self.limit) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(())
    }

    /// Checks that this page belongs to the exact dispatched package and continuation.
    pub fn admits_request(
        &self,
        package: &PackageReference,
        limit: u16,
        cursor: Option<&SemanticVersionsCursor>,
    ) -> bool {
        if self.package != *package || self.limit != limit || self.admit().is_err() {
            return false;
        }
        if self.state == SemanticVersionsPageState::Preparing {
            return cursor.is_some() && self.next_cursor.as_ref() == cursor;
        }
        match cursor {
            None => self
                .entries
                .first()
                .is_none_or(|entry| entry.version_ordinal == 0 && entry.start_ordinal == 0),
            Some(cursor) => cursor.decode().is_ok_and(|position| {
                position.schema == 1
                    && position.basis == self.basis
                    && position.package_identity
                        == *blake3::hash(package.as_str().as_bytes()).as_bytes()
                    && self.entries.first().is_some_and(|entry| {
                        entry.version_ordinal == position.version
                            && entry.start_ordinal == position.image
                    })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, CommandDto, CommandReply, ReplyDto, SurfaceCommand, SurfaceReply};
    use std::collections::BTreeSet;

    // Exact selected frontier captured in the retained public DTO. The scaled
    // image catalog below changes no selected-source field.
    const CAPTURED_SOURCE_ROOT: [u8; 32] = [
        2, 50, 167, 209, 110, 146, 255, 176, 60, 124, 126, 30, 167, 190, 176, 3, 205, 247, 221, 70,
        33, 231, 162, 84, 85, 223, 230, 22, 114, 23, 117, 253,
    ];

    fn retained_fixture(images: u32) -> SemanticVersionRecord {
        // The captured 42-image DTO supplies the public schema. Scaling its
        // catalog is a deterministic unit fixture, not native compiler proof.
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../present/fixtures/semantic-versions-semver-public.json"
        ))
        .expect("captured public DTO");
        let SurfaceReply::SemanticVersions(records) =
            serde_json::from_value(packet["surface"].clone()).expect("full typed DTO")
        else {
            panic!("versions");
        };
        let mut record = records[0].clone();
        record.artifacts = images;
        assert_eq!(
            record
                .selected_source_frontier
                .as_ref()
                .expect("captured selected source")
                .source_relation_root,
            CAPTURED_SOURCE_ROOT,
            "retain the captured source identity while scaling only history images"
        );
        let SemanticHistoryPublicationStatus::Published { commit, proof, .. } =
            &mut record.history_status
        else {
            panic!("published fixture");
        };
        let source = proof.images.clone();
        let mut scaled: Vec<SemanticHistoryImagePublicationProof> = Vec::new();
        for ordinal in 0..images {
            let mut image = source[ordinal as usize % source.len()].clone();
            image.image.artifact_ordinal = ordinal;
            image.image.image_identity = *blake3::hash(&ordinal.to_be_bytes()).as_bytes();
            image.history_commit =
                *blake3::hash(&[b"test-history".as_slice(), &ordinal.to_be_bytes()].concat())
                    .as_bytes();
            image.parent_commits = scaled.last().map_or_else(
                || Vec::new().into_boxed_slice(),
                |prior| vec![prior.history_commit].into_boxed_slice(),
            );
            scaled.push(image);
        }
        proof.images = scaled.into_boxed_slice();
        *commit = proof
            .images
            .last()
            .expect("nonempty fixture")
            .history_commit;
        proof.reference_tip = *commit;
        proof.reachable_commit = *commit;
        proof.package_identity = proof.recompute_package_identity();
        record
            .admit()
            .expect("complete consistent publication fixture");
        record
    }

    #[test]
    fn large_history_pages_preserve_every_image_parent_and_exact_source() {
        let record = retained_fixture(133);
        assert!(
            crate::surface::serialized_json_size(&record) > 49_152,
            "this is the oversized single-version shape; generation-row paging alone cannot fix it"
        );
        let SemanticHistoryPublicationStatus::Published {
            proof: original, ..
        } = &record.history_status
        else {
            panic!("full proof");
        };
        let records = [record.clone()];
        assert_eq!(
            SemanticVersionsPage::from_versions(
                record.package.clone(),
                [17; 32],
                [29; 32],
                &records,
                8,
                None,
            ),
            Err(SemanticHistoryPageError::InvalidCatalog),
            "a real source-root mismatch must still refuse before paging"
        );
        let mut cursor = None;
        let mut images = Vec::new();
        let mut seen = BTreeSet::new();
        let mut starts = 0;
        let mut pages = 0;
        let mut catalog = None;
        loop {
            let page = SemanticVersionsPage::from_versions(
                record.package.clone(),
                [17; 32],
                CAPTURED_SOURCE_ROOT,
                &records,
                8,
                cursor.as_ref(),
            )
            .expect("whole bounded page");
            assert!(page.admits_request(&record.package, 8, cursor.as_ref()));
            assert_eq!(page.state, SemanticVersionsPageState::Ready);
            assert_eq!(page.version_count, 1);
            assert!(crate::surface::serialized_json_size(&page) <= MAX_SEMANTIC_HISTORY_PAGE_BYTES);
            for entry in &page.entries {
                starts += usize::from(entry.first_segment);
                assert_eq!(entry.start_ordinal as usize, images.len());
                assert_eq!(entry.version.generation, record.generation);
                assert_eq!(
                    entry.version.selected_source_frontier,
                    record.selected_source_frontier
                );
                assert_eq!(
                    crate::SemanticShapeSelection::from_selected(&entry.version),
                    crate::SemanticShapeSelection::from_selected(&record),
                    "a copyable selected source has exactly the original shape authority"
                );
                let SemanticHistoryPublicationStatus::PublishedCatalog {
                    catalog: header, ..
                } = &entry.version.history_status
                else {
                    panic!("explicit paged catalog");
                };
                if let Some(previous) = &catalog {
                    assert_eq!(previous, header);
                }
                catalog = Some(header.clone());
                for image in &entry.images {
                    assert!(
                        seen.insert(image.image.artifact_ordinal),
                        "no duplicate image"
                    );
                    images.push(image.clone());
                }
            }
            pages += 1;
            assert!(pages <= 133, "no nonadvancing Ready cursor");
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert!(pages > 1);
        assert_eq!(
            starts, 1,
            "continuation segments are not new retained generations"
        );
        assert_eq!(seen.len(), 133);
        assert_eq!(images.as_slice(), original.images.as_ref());
        let header = catalog.expect("header");
        assert_eq!(
            header
                .assemble(images.clone().into_boxed_slice())
                .expect("complete authenticated catalog"),
            *original
        );
        images.remove(7);
        assert!(
            header.assemble(images.into_boxed_slice()).is_err(),
            "a missing image cannot become a complete proof"
        );
    }

    #[test]
    fn immutable_catalog_allocation_census_counts_full_proofs_headers_and_capacity() {
        let record = retained_fixture(133);
        let package = record.package.clone();
        let mut catalog = SemanticVersionsCatalog::new(
            package,
            [17; 32],
            CAPTURED_SOURCE_ROOT,
            vec![record].into_boxed_slice(),
        )
        .expect("admitted full observation");
        let initial = catalog.retained_allocation_bytes().expect("finite census");
        let original_page = catalog.page(8, None).expect("original exact page");
        let SemanticHistoryPublicationStatus::Published {
            proof, reference, ..
        } = &mut catalog.versions[0].history_status
        else {
            panic!("full proof");
        };
        // Retained string capacity changes without changing any committed byte.
        let old_capacity = reference.capacity();
        reference.reserve_exact(4096);
        let reference_delta = reference.capacity() - old_capacity;
        let old_target_capacity = proof.target_package.capacity();
        proof.target_package.reserve_exact(8192);
        let target_delta = proof.target_package.capacity() - old_target_capacity;
        let images = proof.images.len();
        let parents = proof
            .images
            .iter()
            .map(|image| image.parent_commits.len())
            .sum::<usize>();
        assert_eq!(images, 133);
        assert_eq!(parents, 132);
        assert_eq!(
            catalog.retained_allocation_bytes(),
            Some(initial + reference_delta + target_delta)
        );
        assert!(
            initial
                >= std::mem::size_of::<SemanticVersionsCatalog>()
                    + 2 * std::mem::size_of::<SemanticVersionRecord>()
                    + images * std::mem::size_of::<SemanticHistoryImagePublicationProof>()
                    + parents * std::mem::size_of::<[u8; 32]>()
        );
        let before_header = catalog
            .retained_allocation_bytes()
            .expect("full and header storage");
        let SemanticHistoryPublicationStatus::PublishedCatalog {
            catalog: header, ..
        } = &mut catalog.page_versions[0].history_status
        else {
            panic!("derived whole header");
        };
        let old_capacity = header.target_coordinate.capacity();
        header.target_coordinate.reserve_exact(16_384);
        let header_delta = header.target_coordinate.capacity() - old_capacity;
        assert_eq!(
            catalog.retained_allocation_bytes(),
            Some(before_header + header_delta)
        );
        let page = catalog
            .page(8, None)
            .expect("capacity cannot change exact proof/basis");
        assert_eq!(
            page, original_page,
            "spare capacities change no wire/proof field"
        );
        let cursor = page.next_cursor.expect("whole history continuation");
        assert!(catalog.page(8, Some(&cursor)).is_ok());
        let mut overflow = usize::MAX;
        assert!(add_owned_bytes(&mut overflow, 1).is_none());
        assert_eq!(overflow, usize::MAX, "accounting never wraps");
    }

    #[test]
    fn immutable_catalog_walk_admits_and_hashes_16384_images_once() {
        // This synthetic public-schema volume uses the package-member constant,
        // not a global semantic-image limit. It proves structural work only.
        let record = retained_fixture(crate::MAX_PROJECT_MEMBERS as u32);
        let package = record.package.clone();
        let SemanticHistoryPublicationStatus::Published { proof, .. } = &record.history_status
        else {
            panic!("full original proof");
        };
        let proof = proof.clone();
        let catalog = SemanticVersionsCatalog::new(
            package.clone(),
            [17; 32],
            CAPTURED_SOURCE_ROOT,
            vec![record].into_boxed_slice(),
        )
        .expect("one exact observation");
        let construction = catalog.preparation_work();
        assert_eq!(construction.admitted_versions, 1);
        assert_eq!(construction.admitted_images, 16_384);
        assert!(construction.catalog_hashed_bytes > 49_152);
        let mut cursor = None;
        let mut collected = Vec::new();
        let mut pages = 0;
        let mut copied = 0;
        let mut measured = 0;
        let mut header = None;
        loop {
            let (page, work) = catalog.page_with_work(8, cursor.as_ref());
            let page = page.expect("bounded indexed page");
            assert!(page.admits_request(&package, 8, cursor.as_ref()));
            assert_eq!(work.admitted_versions, 0);
            assert_eq!(work.admitted_images, 0);
            assert_eq!(
                work.catalog_hashed_bytes, 0,
                "no complete-catalog rehash on a page"
            );
            for entry in &page.entries {
                assert_eq!(entry.start_ordinal as usize, collected.len());
                let SemanticHistoryPublicationStatus::PublishedCatalog { catalog, .. } =
                    &entry.version.history_status
                else {
                    panic!("header");
                };
                if let Some(prior) = &header {
                    assert_eq!(prior, catalog);
                }
                header = Some(catalog.clone());
                collected.extend_from_slice(&entry.images);
            }
            copied += work.copied_images;
            measured += work.measured_page_bytes;
            pages += 1;
            assert!(pages <= 16_384, "every Ready page advances");
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert!(pages >= 2048);
        assert_eq!(collected.as_slice(), proof.images.as_ref());
        assert_eq!(
            header
                .expect("catalog")
                .assemble(collected.into_boxed_slice())
                .expect("whole proof"),
            proof
        );
        assert!(
            copied <= pages * 8 * 9 * 2,
            "candidate copies are bounded by page limit, not catalog length"
        );
        assert!(
            measured <= pages * 8 * (MAX_SEMANTIC_HISTORY_PAGE_BYTES as u64 + 16_384),
            "only bounded page candidates are measured"
        );
        assert_eq!(catalog.preparation_work(), construction);
        let (bad, refused_work) = catalog.page_with_work(0, None);
        assert_eq!(bad, Err(SemanticHistoryPageError::InvalidLimit));
        assert_eq!(refused_work, SemanticHistoryReadWork::default());
        let cold_records =
            serde_json::to_vec(&catalog.versions).expect("unchanged retained observation");
        let cold = SemanticVersionsCatalog::new(
            package,
            [17; 32],
            CAPTURED_SOURCE_ROOT,
            serde_json::from_slice(&cold_records).expect("new stateless catalog"),
        )
        .expect("cold rebuild");
        let saved = catalog
            .page(8, None)
            .expect("first")
            .next_cursor
            .expect("continuation");
        assert_eq!(
            cold.page(8, Some(&saved)),
            catalog.page(8, Some(&saved)),
            "cold rebuild retains exact basis and next position"
        );
        assert_eq!(
            cold.preparation_work(),
            construction,
            "one complete cold rebuild, never per page"
        );
    }

    #[test]
    fn history_pages_cross_version_boundaries_without_inventing_generations() {
        let selected = retained_fixture(133);
        let mut before = with_history(&selected, SemanticHistoryPublicationStatus::NotSelected);
        before.selected = false;
        before.selected_source_frontier = None;
        before.generation = crate::object_version(b"older");
        let mut after = before.clone();
        after.generation = crate::object_version(b"another-retained-generation");
        let versions = [before, selected.clone(), after];
        let mut cursor = None;
        let mut starts = Vec::new();
        let mut image_ordinals = Vec::new();
        let mut pages = 0;
        loop {
            let page = SemanticVersionsPage::from_versions(
                selected.package.clone(),
                [1; 32],
                CAPTURED_SOURCE_ROOT,
                &versions,
                3,
                cursor.as_ref(),
            )
            .expect("mixed history page");
            assert_eq!(page.version_count, 3);
            assert!(page.admits_request(&selected.package, 3, cursor.as_ref()));
            for entry in &page.entries {
                let original = &versions[entry.version_ordinal as usize];
                assert_eq!(entry.version.generation, original.generation);
                assert_eq!(entry.version.selected, original.selected);
                assert_eq!(
                    entry.version.selected_source_frontier,
                    original.selected_source_frontier
                );
                if entry.first_segment {
                    starts.push(entry.version_ordinal);
                }
                for image in &entry.images {
                    image_ordinals.push(image.image.artifact_ordinal);
                }
            }
            pages += 1;
            assert!(pages <= 135);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(starts, [0, 1, 2]);
        assert_eq!(image_ordinals, (0..133).collect::<Vec<_>>());
    }

    #[test]
    fn history_page_refusal_preserves_stale_basis_vs_unrenderable_whole_item() {
        let mut record = retained_fixture(1);
        let package =
            PackageReference::parse(format!("/{}", "a".repeat(4000))).expect("bounded package");
        record.package = package.clone();
        record.selected_source_frontier = None;
        let SemanticHistoryPublicationStatus::Published { proof, .. } = &mut record.history_status
        else {
            panic!("proof");
        };
        proof.target_package = "p".repeat(crate::MAX_PRODUCT_TEXT_BYTES);
        proof.target_coordinate = "c".repeat(crate::MAX_PRODUCT_TEXT_BYTES);
        proof.package_identity = proof.recompute_package_identity();
        record
            .admit()
            .expect("a valid complete item is not an invalid catalog");
        let error = SemanticVersionsPage::from_versions(
            package,
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[record],
            1,
            None,
        )
        .expect_err("a whole item cannot fit");
        assert_eq!(error, SemanticHistoryPageError::WholeItemOversized);
        assert!(error.to_string().contains("lowering limit cannot"));
        let record = retained_fixture(133);
        let first = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[record.clone()],
            8,
            None,
        )
        .expect("first page");
        assert_eq!(
            SemanticVersionsPage::from_versions(
                record.package.clone(),
                [9; 32],
                CAPTURED_SOURCE_ROOT,
                &[record],
                8,
                first.next_cursor.as_ref()
            ),
            Err(SemanticHistoryPageError::StaleBasis)
        );
    }

    #[test]
    fn stateless_history_cursor_retains_position_during_cold_preparation() {
        let record = retained_fixture(133);
        let first = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[record.clone()],
            8,
            None,
        )
        .expect("first page");
        let saved = first.next_cursor.expect("continuation");
        let bytes = serde_json::to_vec(&record).expect("retained immutable fixture");
        let reopened: SemanticVersionRecord =
            serde_json::from_slice(&bytes).expect("new stateless reader");
        let mut pending = reopened.clone();
        let SemanticHistoryPublicationStatus::Published { selection_id, .. } =
            pending.history_status
        else {
            panic!("publication");
        };
        pending.history_status = SemanticHistoryPublicationStatus::Pending { selection_id };
        let preparing = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[pending.clone()],
            8,
            Some(&saved),
        )
        .expect("same committed source still preparing");
        assert_eq!(preparing.state, SemanticVersionsPageState::Preparing);
        assert!(preparing.entries.is_empty());
        assert_eq!(preparing.next_cursor.as_ref(), Some(&saved));
        assert!(preparing.admits_request(&record.package, 8, Some(&saved)));
        let resumed = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[reopened.clone()],
            8,
            Some(&saved),
        )
        .expect("same exact publication restored");
        assert_eq!(resumed.entries[0].start_ordinal, 8);
        assert!(!resumed.entries[0].first_segment);
        assert_ne!(resumed.next_cursor.as_ref(), Some(&saved));
        assert!(
            SemanticVersionsPage::from_versions(
                record.package.clone(),
                [9; 32],
                CAPTURED_SOURCE_ROOT,
                &[reopened.clone()],
                8,
                Some(&saved)
            )
            .is_err()
        );
        let mut changed_source = reopened;
        changed_source
            .selected_source_frontier
            .as_mut()
            .expect("selected source")
            .source_relation_root = [9; 32];
        assert!(
            SemanticVersionsPage::from_versions(
                record.package.clone(),
                [1; 32],
                [9; 32],
                &[changed_source],
                8,
                Some(&saved)
            )
            .is_err()
        );
        pending.manifest = [99; 32];
        assert!(
            SemanticVersionsPage::from_versions(
                record.package.clone(),
                [1; 32],
                CAPTURED_SOURCE_ROOT,
                &[pending],
                8,
                Some(&saved)
            )
            .is_err(),
            "Preparing cannot hide changed version metadata"
        );
    }

    #[test]
    fn adjacent_history_segments_require_identical_version_and_parent_edge() {
        let record = retained_fixture(133);
        let mut page = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[record.clone()],
            2,
            None,
        )
        .expect("owner-produced contiguous proof pair");
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].images.len(), 2);
        // Independently repartition the real factory output into two whole,
        // contiguous segments. This is a strict-codec control, not a claim
        // about the factory's chosen partition or native runtime authority.
        let mut first = page.entries[0].clone();
        let mut second = first.clone();
        second.first_segment = false;
        second.start_ordinal = 1;
        second.previous_commit = Some(first.images[0].history_commit);
        second.images = vec![first.images[1].clone()].into_boxed_slice();
        first.images = vec![first.images[0].clone()].into_boxed_slice();
        page.entries = vec![first, second].into_boxed_slice();
        page.admit()
            .expect("legitimate identical split-version records");
        let command = SurfaceCommand::SemanticVersionsPage {
            package: record.package,
            limit: 2,
            cursor: None,
        };
        crate::admit_surface_reply(&command, &SurfaceReply::SemanticVersionsPage(page.clone()))
            .expect("exact split page admitted");
        let mut changed_source = page.clone();
        changed_source.entries[1].version.semantic_bytes += 1;
        changed_source.entries[1]
            .version
            .admit()
            .expect("source field remains individually valid");
        assert!(
            crate::admit_surface_reply(
                &command,
                &SurfaceReply::SemanticVersionsPage(changed_source)
            )
            .is_err(),
            "one version cannot expose contradictory source metadata"
        );
        let mut changed_edge = page;
        changed_edge.entries[1].previous_commit = Some([77; 32]);
        changed_edge.entries[1].images[0].parent_commits = Box::new([[77; 32]]);
        assert!(
            changed_edge.admit().is_err(),
            "an internally consistent segment cannot detach from its preceding segment"
        );
    }

    #[test]
    fn history_page_admission_refuses_gaps_wrong_scope_and_cursor_jumps() {
        let record = retained_fixture(133);
        let page = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            CAPTURED_SOURCE_ROOT,
            &[record.clone()],
            8,
            None,
        )
        .expect("page");
        let command = SurfaceCommand::SemanticVersionsPage {
            package: record.package.clone(),
            limit: 8,
            cursor: None,
        };
        let request = CommandDto::new(41, Command::Surface(command.clone()));
        let reply = ReplyDto::new(
            41,
            CommandReply::Surface(SurfaceReply::SemanticVersionsPage(page.clone())),
        );
        crate::admit_reply(&request, &reply).expect("exact typed route");
        let legacy = SurfaceCommand::SemanticVersions {
            package: record.package.clone(),
        };
        assert!(
            crate::admit_surface_reply(&legacy, &SurfaceReply::SemanticVersionsPage(page.clone()))
                .is_err(),
            "shared command ID cannot substitute the bounded reply family for the legacy contract"
        );
        assert!(
            crate::admit_surface_reply(
                &command,
                &SurfaceReply::SemanticVersions(Box::new([record.clone()]))
            )
            .is_err(),
            "page requests cannot accept an unbounded full history"
        );
        crate::admit_surface_reply(
            &legacy,
            &SurfaceReply::SemanticVersions(Box::new([record.clone()])),
        )
        .expect("original full API contract remains admitted");
        let mut foreign_version = record.clone();
        foreign_version.package =
            PackageReference::parse("/other/project").expect("foreign project");
        assert!(
            crate::admit_surface_reply(
                &legacy,
                &SurfaceReply::SemanticVersions(Box::new([foreign_version]))
            )
            .is_err()
        );
        let catalog_only = page.entries[0].version.clone();
        catalog_only
            .admit()
            .expect("page entry remains a usable selected source");
        assert!(
            crate::admit_surface_reply(
                &legacy,
                &SurfaceReply::SemanticVersions(Box::new([catalog_only.clone()]))
            )
            .is_err(),
            "legacy full-history contract cannot claim completeness with only a catalog header"
        );
        assert!(
            SurfaceReply::SemanticVersionSelected(catalog_only)
                .admit(crate::CommandId::SelectSemanticVersion)
                .is_err()
        );
        let mut foreign_source = page.clone();
        foreign_source.entries[0]
            .version
            .selected_source_frontier
            .as_mut()
            .expect("selected frontier")
            .source_relation_root = [99; 32];
        assert!(
            foreign_source.admit().is_err(),
            "source operand cannot claim a different snapshot root than its page"
        );
        let mut gap = page.clone();
        gap.entries[0].images[2].image.artifact_ordinal += 1;
        assert!(gap.admit().is_err());
        let mut parent = page.clone();
        parent.entries[0].images[2].parent_commits = Box::new([[33; 32]]);
        assert!(parent.admit().is_err());
        let mut absent = page.clone();
        absent.next_cursor = None;
        assert!(
            absent.admit().is_err(),
            "a partial catalog needs continuation"
        );
        let mut jump = page.clone();
        let mut position = jump
            .next_cursor
            .as_ref()
            .expect("cursor")
            .decode()
            .expect("position");
        position.image += 1;
        jump.next_cursor = Some(position.encode().expect("encoded changed position"));
        assert!(jump.admit().is_err());
        let foreign = PackageReference::parse("/other/project").expect("foreign project");
        assert!(!page.admits_request(&foreign, 8, None));
        assert!(!page.admits_request(&record.package, 7, None));
        let wrong = SurfaceCommand::SemanticVersionsPage {
            package: foreign,
            limit: 8,
            cursor: None,
        };
        assert!(
            crate::admit_surface_reply(&wrong, &SurfaceReply::SemanticVersionsPage(page)).is_err()
        );
        for limit in [0, MAX_SEMANTIC_HISTORY_PAGE_ITEMS + 1] {
            assert!(
                SemanticVersionsPage::from_versions(
                    record.package.clone(),
                    [1; 32],
                    CAPTURED_SOURCE_ROOT,
                    &[record.clone()],
                    limit,
                    None
                )
                .is_err()
            );
        }
    }
}
