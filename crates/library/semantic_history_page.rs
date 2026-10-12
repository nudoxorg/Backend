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

struct HashWriter(blake3::Hasher);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
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

impl SemanticVersionsPage {
    /// Pages already owner-admitted immutable versions without changing any retained fact.
    /// The roots must come from the answering owner snapshot, never from caller input.
    ///
    /// # Errors
    /// Distinguishes invalid bounds/cursors/catalogs, changed saved bases, and a whole
    /// retained item that cannot fit without losing fields.
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
        for version in versions {
            version.admit()?;
            if version.package != package
                || version
                    .selected_source_frontier
                    .as_ref()
                    .is_some_and(|frontier| frontier.source_relation_root != source_relation_root)
            {
                return Err(SemanticHistoryPageError::InvalidCatalog);
            }
        }
        let mut hash = HashWriter(blake3::Hasher::new());
        hash.0.update(b"backend.semantic-versions-page.v1\0");
        serde_json::to_writer(&mut hash, versions)
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        let mut metadata_hash = HashWriter(blake3::Hasher::new());
        metadata_hash
            .0
            .update(b"backend.semantic-versions-metadata.v1\0");
        for version in versions {
            serde_json::to_writer(
                &mut metadata_hash,
                &with_history(version, SemanticHistoryPublicationStatus::NotSelected),
            )
            .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
        }
        let basis = SemanticVersionsBasis {
            semantic_relation_root,
            source_relation_root,
            versions_metadata_identity: *metadata_hash.0.finalize().as_bytes(),
            versions_identity: *hash.0.finalize().as_bytes(),
        };
        let package_identity = *blake3::hash(package.as_str().as_bytes()).as_bytes();
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
            let reconstructing = versions.iter().any(|version| {
                version.selected
                    && matches!(
                        &version.history_status,
                        SemanticHistoryPublicationStatus::Pending { .. }
                            | SemanticHistoryPublicationStatus::NotRequested { .. }
                            | SemanticHistoryPublicationStatus::Deferred { .. }
                    )
            });
            if reconstructing && let Some(cursor) = cursor {
                let page = Self {
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
        let mut page = Self {
            state: SemanticVersionsPageState::Ready,
            package,
            basis,
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
            let (catalog, images) = match &original.history_status {
                SemanticHistoryPublicationStatus::Published { proof, .. } => (
                    Some(SemanticHistoryCatalog::from_proof(proof)?),
                    proof.images.as_ref(),
                ),
                // Only complete owner records can be used to build the public page.
                SemanticHistoryPublicationStatus::PublishedCatalog { .. } => {
                    return Err(SemanticHistoryPageError::InvalidCatalog);
                }
                _ => (None, &[][..]),
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
                let status = if let (
                    Some(catalog),
                    SemanticHistoryPublicationStatus::Published {
                        selection_id,
                        commit,
                        reference,
                        ..
                    },
                ) = (catalog.as_ref(), &original.history_status)
                {
                    SemanticHistoryPublicationStatus::PublishedCatalog {
                        selection_id: *selection_id,
                        commit: *commit,
                        reference: reference.clone(),
                        catalog: catalog.clone(),
                    }
                } else {
                    original.history_status.clone()
                };
                let version = with_history(original, status);
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
                page.entries = entries.clone().into_boxed_slice();
                page.next_cursor = if next.version as usize == versions.len() {
                    None
                } else {
                    Some(next.encode()?)
                };
                if crate::surface::serialized_json_size(&page) <= MAX_SEMANTIC_HISTORY_PAGE_BYTES {
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
        // The unit owner fixture has its own source-root identity; the captured
        // packet supplies the schema, not authority for this synthetic catalog.
        record
            .selected_source_frontier
            .as_mut()
            .expect("selected source")
            .source_relation_root = [2; 32];
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
                [29; 32],
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
                [2; 32],
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
        let error =
            SemanticVersionsPage::from_versions(package, [1; 32], [2; 32], &[record], 1, None)
                .expect_err("a whole item cannot fit");
        assert_eq!(error, SemanticHistoryPageError::WholeItemOversized);
        assert!(error.to_string().contains("lowering limit cannot"));
        let record = retained_fixture(133);
        let first = SemanticVersionsPage::from_versions(
            record.package.clone(),
            [1; 32],
            [2; 32],
            &[record.clone()],
            8,
            None,
        )
        .expect("first page");
        assert_eq!(
            SemanticVersionsPage::from_versions(
                record.package.clone(),
                [9; 32],
                [2; 32],
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
            [2; 32],
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
            [2; 32],
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
            [2; 32],
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
                [2; 32],
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
                [2; 32],
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
            [2; 32],
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
            [2; 32],
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
                    [2; 32],
                    &[record.clone()],
                    limit,
                    None
                )
                .is_err()
            );
        }
    }
}
