//! Canonical bounded range messages and the durable semantic-range client.
//!
//! Range messages carry only the semantic segment identity and a byte slice;
//! they do not name a physical pack, object, or storage layout. The receiving
//! client stages each range in its existing sparse CAS, and the semantic
//! cursor advances only after the complete segment has been read back and
//! admitted from durable storage.

use std::fmt;

use backend_semantic::ir::{
    EmbeddingNormalization, EmbeddingPlaneIdentity, GenerationId, LanguageProfile,
    SemanticHydrationCursorToken, SemanticImageIdentity, SemanticIrPlane, SemanticManifestRoot,
    SemanticPlaneCatalog, SemanticPlaneCatalogRoot, SemanticPlaneImageKey, SemanticPlaneKind,
    SemanticPlaneManifest, SemanticRangeRequest, UntrustedSemanticSegmentId,
};

use crate::{
    ByteRange, DurableSemanticSegmentStore, HydrationCredits, IrHydrationCheckpoint,
    IrHydrationCursor, IrHydrationError, IrHydrationPoll, IrHydrationRequest, ReplicationError,
    SelectedGenerationSource, SelectedGenerationStamp, SparseCoverage, SparseSegmentCoverage,
    TransportLimits, VerifiedSemanticSegment,
};

const MAGIC: [u8; 4] = *b"SRNG";
const WIRE_VERSION: u8 = 2;
const GET_TAG: u8 = 1;
const CHUNK_TAG: u8 = 2;
const CHECKPOINT_TAG: u8 = 3;
const CATALOG_GET_TAG: u8 = 4;
const CATALOG_CHUNK_TAG: u8 = 5;
const MANIFEST_GET_TAG: u8 = 6;
const MANIFEST_CHUNK_TAG: u8 = 7;
const SEMANTIC_IMAGE_GET_TAG: u8 = 8;
const SEMANTIC_IMAGE_CHUNK_TAG: u8 = 9;
pub(crate) const MAX_RANGE_BYTES: usize = 16 * 1024;
/// A selected full NXFI image is bounded independently of the smaller metadata pages.
pub const MAX_SEMANTIC_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TARGET_FIELD_BYTES: usize = 4 * 1024;
const MAX_RANGE_MESSAGE_BYTES: usize = 32 * 1024;
pub(crate) const MAX_CAS_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
const MAX_CLIENT_CHECKPOINT_BYTES: usize = MAX_CAS_CHECKPOINT_BYTES + 16 * 1024;
pub(crate) const MAX_SEMANTIC_METADATA_BYTES: u64 = 64 * 1024 * 1024;

/// Package and compiler coordinate needed to reconstruct the live index key.
///
/// The locald adapter must still parse these strings into its canonical
/// package-reference and package-coordinate types, then compare their
/// namespace/coordinate identities with the selected stamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticTargetKey {
    package: Box<str>,
    coordinate: Box<str>,
    profile: LanguageProfile,
}

impl SemanticTargetKey {
    /// Creates a bounded target claim for strict admission by the index side.
    pub fn new(
        package: impl Into<Box<str>>,
        coordinate: impl Into<Box<str>>,
        profile: LanguageProfile,
    ) -> Result<Self, ReplicationError> {
        let package = package.into();
        let coordinate = coordinate.into();
        validate_target_field(&package)?;
        validate_target_field(&coordinate)?;
        Ok(Self {
            package,
            coordinate,
            profile,
        })
    }

    /// Returns the package reference claim.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the exact compiler coordinate claim.
    #[must_use]
    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the closed language profile.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }
}

/// Requests one bounded page of the current authority's semantic image catalog.
/// The first request leaves the expected tuple absent; follow-up pages carry
/// the stamp, catalog root and total length returned by the first page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticCatalogGet {
    /// Caller-generated nonzero correlation identity.
    pub request_id: u64,
    /// Product identity used by the index authority to resolve the selected head.
    pub target: SemanticTargetKey,
    /// Expected current selected frontier on continuation pages.
    pub selected_stamp: Option<SelectedGenerationStamp>,
    /// Expected aggregate catalog root on continuation pages.
    pub catalog_root: Option<SemanticPlaneCatalogRoot>,
    /// Expected full canonical catalog byte length on continuation pages.
    pub total_length: Option<u64>,
    /// Requested half-open byte interval in the canonical catalog encoding.
    pub byte_range: ByteRange,
}

impl SemanticCatalogGet {
    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(CATALOG_GET_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        match (self.selected_stamp, self.catalog_root, self.total_length) {
            (None, None, None) => writer.u8(0)?,
            (Some(stamp), Some(root), Some(total)) => {
                writer.u8(1)?;
                writer.stamp(stamp)?;
                writer.fixed(root.as_bytes())?;
                writer.u64(total)?;
            }
            _ => return Err(ReplicationError::InvalidWire),
        }
        writer.byte_range(self.byte_range)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded wire request.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(CATALOG_GET_TAG)?;
        let request_id = reader.u64()?;
        let target = reader.target()?;
        let (selected_stamp, catalog_root, total_length) = match reader.u8()? {
            0 => (None, None, None),
            1 => (
                Some(reader.stamp()?),
                Some(SemanticPlaneCatalogRoot::from_wire_claim(reader.fixed()?)),
                Some(reader.u64()?),
            ),
            _ => return Err(ReplicationError::InvalidWire),
        };
        let result = Self {
            request_id,
            target,
            selected_stamp,
            catalog_root,
            total_length,
            byte_range: reader.byte_range()?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0 {
            return Err(ReplicationError::InvalidWire);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        match (self.selected_stamp, self.catalog_root, self.total_length) {
            (None, None, None) => validate_metadata_range(None, self.byte_range),
            (Some(stamp), Some(root), Some(total)) => {
                if self.target.profile != stamp.profile() || root.as_bytes() == &[0; 32] {
                    return Err(ReplicationError::IdentityMismatch);
                }
                validate_metadata_range(Some(total), self.byte_range)
            }
            _ => Err(ReplicationError::InvalidWire),
        }
    }
}

/// One bounded page of canonical catalog bytes, bound to its live selected head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticCatalogChunk {
    /// Correlation identity copied from the request.
    pub request_id: u64,
    /// Product identity copied from the request.
    pub target: SemanticTargetKey,
    /// Current selected frontier read by the index authority.
    pub selected_stamp: SelectedGenerationStamp,
    /// Root over the complete canonical catalog byte sequence.
    pub catalog_root: SemanticPlaneCatalogRoot,
    /// Complete canonical catalog length, not this page's payload length.
    pub total_length: u64,
    /// Exact returned half-open byte interval.
    pub byte_range: ByteRange,
    /// At most 16 KiB of canonical catalog bytes.
    pub payload: Vec<u8>,
}

impl SemanticCatalogChunk {
    /// Checks correlation, freshness and range fields against the request.
    pub fn validate_against(&self, get: &SemanticCatalogGet) -> Result<(), ReplicationError> {
        self.validate_shape()?;
        get.validate_shape()?;
        if self.request_id != get.request_id
            || self.target != get.target
            || get
                .selected_stamp
                .is_some_and(|stamp| stamp != self.selected_stamp)
            || get
                .catalog_root
                .is_some_and(|root| root != self.catalog_root)
            || get
                .total_length
                .is_some_and(|length| length != self.total_length)
            || self.byte_range != get.byte_range
            || self.target.profile != self.selected_stamp.profile()
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }

    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(CATALOG_CHUNK_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.fixed(self.catalog_root.as_bytes())?;
        writer.u64(self.total_length)?;
        writer.byte_range(self.byte_range)?;
        writer.sized_payload(&self.payload)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded page.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(CATALOG_CHUNK_TAG)?;
        let result = Self {
            request_id: reader.u64()?,
            target: reader.target()?,
            selected_stamp: reader.stamp()?,
            catalog_root: SemanticPlaneCatalogRoot::from_wire_claim(reader.fixed()?),
            total_length: reader.u64()?,
            byte_range: reader.byte_range()?,
            payload: reader.sized_payload(MAX_RANGE_BYTES)?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.target.profile != self.selected_stamp.profile()
            || self.catalog_root.as_bytes() == &[0; 32]
        {
            return Err(ReplicationError::InvalidWire);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        validate_metadata_range(Some(self.total_length), self.byte_range)?;
        if self.payload.len() as u64 != self.byte_range.len {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}

/// Requests one bounded page of a specific selected image manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticManifestGet {
    /// Caller-generated nonzero correlation identity.
    pub request_id: u64,
    /// Product identity used to re-read the selected head.
    pub target: SemanticTargetKey,
    /// Exact selected authority frontier containing the catalog.
    pub selected_stamp: SelectedGenerationStamp,
    /// Aggregate catalog root from which this image was discovered.
    pub catalog_root: SemanticPlaneCatalogRoot,
    /// Exact image identity and ordinal within that catalog.
    pub image: SemanticPlaneImageKey,
    /// Full canonical manifest length from its catalog entry.
    pub total_length: u64,
    /// Requested half-open byte interval in the manifest encoding.
    pub byte_range: ByteRange,
}

impl SemanticManifestGet {
    /// Confirms that this manifest request names the exact entry learned from
    /// a completely assembled canonical catalog and its authority stamp.
    pub fn validate_against_catalog(
        &self,
        catalog: &SemanticPlaneCatalog,
    ) -> Result<(), ReplicationError> {
        self.validate_shape()?;
        if self.catalog_root != catalog.root()
            || self.selected_stamp.catalog_root() != catalog.root()
            || !catalog.entries().iter().any(|entry| {
                entry.image() == self.image
                    && u64::from(entry.manifest_length()) == self.total_length
            })
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }

    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(MANIFEST_GET_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.fixed(self.catalog_root.as_bytes())?;
        writer.image(self.image)?;
        writer.u64(self.total_length)?;
        writer.byte_range(self.byte_range)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded wire request.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(MANIFEST_GET_TAG)?;
        let result = Self {
            request_id: reader.u64()?,
            target: reader.target()?,
            selected_stamp: reader.stamp()?,
            catalog_root: SemanticPlaneCatalogRoot::from_wire_claim(reader.fixed()?),
            image: reader.image()?,
            total_length: reader.u64()?,
            byte_range: reader.byte_range()?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.target.profile != self.selected_stamp.profile()
            || self.catalog_root.as_bytes() == &[0; 32]
            || self.image.manifest_root().as_bytes() == &[0; 32]
            || self.image.semantic_generation().as_bytes() == &[0; 32]
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        validate_metadata_range(Some(self.total_length), self.byte_range)
    }
}

/// One bounded canonical manifest page, bound to its catalog and image key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticManifestChunk {
    /// Correlation identity copied from the request.
    pub request_id: u64,
    /// Product identity copied from the request.
    pub target: SemanticTargetKey,
    /// Current selected authority frontier.
    pub selected_stamp: SelectedGenerationStamp,
    /// Current aggregate catalog root.
    pub catalog_root: SemanticPlaneCatalogRoot,
    /// Exact image identity returned by the catalog.
    pub image: SemanticPlaneImageKey,
    /// Full canonical manifest byte length.
    pub total_length: u64,
    /// Exact returned half-open byte interval.
    pub byte_range: ByteRange,
    /// At most 16 KiB of canonical manifest bytes.
    pub payload: Vec<u8>,
}

/// Requests one bounded page of a selected complete canonical `NXFI` image.
///
/// The first page omits `image_identity` and `total_length`; its reply supplies
/// both. Every continuation carries that tuple so pages cannot be mixed even
/// when a transport reconnects between requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedSemanticImageGet {
    /// Caller-generated nonzero correlation identity.
    pub request_id: u64,
    /// Product identity used by the index authority to resolve the selected head.
    pub target: SemanticTargetKey,
    /// Exact current authority frontier admitted with the selected catalog.
    pub selected_stamp: SelectedGenerationStamp,
    /// Exact catalog image whose canonical full-image bytes are requested.
    pub image: SemanticPlaneImageKey,
    /// Typed content identity returned by the first page and echoed thereafter.
    pub image_identity: Option<SemanticImageIdentity>,
    /// Exact complete NXFI length returned by the first page and echoed thereafter.
    pub total_length: Option<u64>,
    /// Requested half-open byte interval within the complete NXFI image.
    pub byte_range: ByteRange,
}

impl SelectedSemanticImageGet {
    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(SEMANTIC_IMAGE_GET_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.image(self.image)?;
        match (self.image_identity, self.total_length) {
            (None, None) => writer.u8(0)?,
            (Some(identity), Some(total_length)) => {
                writer.u8(1)?;
                writer.fixed(identity.as_ref())?;
                writer.u64(total_length)?;
            }
            _ => return Err(ReplicationError::InvalidWire),
        }
        writer.byte_range(self.byte_range)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded page request.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(SEMANTIC_IMAGE_GET_TAG)?;
        let request_id = reader.u64()?;
        let target = reader.target()?;
        let selected_stamp = reader.stamp()?;
        let image = reader.image()?;
        let (image_identity, total_length) = match reader.u8()? {
            0 => (None, None),
            1 => {
                let identity = SemanticImageIdentity::try_from(reader.fixed()?)
                    .map_err(|_| ReplicationError::InvalidWire)?;
                (Some(identity), Some(reader.u64()?))
            }
            _ => return Err(ReplicationError::InvalidWire),
        };
        let result = Self {
            request_id,
            target,
            selected_stamp,
            image,
            image_identity,
            total_length,
            byte_range: reader.byte_range()?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.target.profile != self.selected_stamp.profile()
            || self.image.manifest_root().as_bytes() == &[0; 32]
            || self.image.semantic_generation().as_bytes() == &[0; 32]
        {
            return Err(ReplicationError::InvalidWire);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        match (self.image_identity, self.total_length) {
            (None, None) => {
                if self.byte_range.start != 0 || self.byte_range.len != 1 {
                    return Err(ReplicationError::Range);
                }
            }
            (Some(_), Some(total_length)) => {
                if total_length == 0 || total_length > MAX_SEMANTIC_IMAGE_BYTES {
                    return Err(ReplicationError::MessageTooLarge);
                }
                let end = self.byte_range.end()?;
                if self.byte_range.len == 0
                    || self.byte_range.len > MAX_RANGE_BYTES as u64
                    || end > total_length
                {
                    return Err(ReplicationError::Range);
                }
            }
            _ => return Err(ReplicationError::InvalidWire),
        }
        Ok(())
    }
}

/// One bounded canonical-image page bound to an authority stamp and catalog key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedSemanticImageChunk {
    /// Correlation identity copied from the request.
    pub request_id: u64,
    /// Product identity copied from the request.
    pub target: SemanticTargetKey,
    /// Exact authority frontier current while the owner served these bytes.
    pub selected_stamp: SelectedGenerationStamp,
    /// Exact catalog image key returned by selection.
    pub image: SemanticPlaneImageKey,
    /// Typed artifact identity of the complete canonical NXFI bytes.
    pub image_identity: SemanticImageIdentity,
    /// Full canonical NXFI byte length.
    pub total_length: u64,
    /// Exact returned half-open byte interval.
    pub byte_range: ByteRange,
    /// At most 16 KiB of canonical NXFI bytes.
    pub payload: Vec<u8>,
}

impl SelectedSemanticImageChunk {
    /// Checks response correlation, freshness, full-image identity, and byte range.
    pub fn validate_against(&self, get: &SelectedSemanticImageGet) -> Result<(), ReplicationError> {
        self.validate_shape()?;
        get.validate_shape()?;
        if self.request_id != get.request_id
            || self.target != get.target
            || self.selected_stamp != get.selected_stamp
            || self.image != get.image
            || get
                .image_identity
                .is_some_and(|identity| identity != self.image_identity)
            || get
                .total_length
                .is_some_and(|length| length != self.total_length)
            || self.byte_range != get.byte_range
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }

    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(SEMANTIC_IMAGE_CHUNK_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.image(self.image)?;
        writer.fixed(self.image_identity.as_ref())?;
        writer.u64(self.total_length)?;
        writer.byte_range(self.byte_range)?;
        writer.sized_payload(&self.payload)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded page reply.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(SEMANTIC_IMAGE_CHUNK_TAG)?;
        let result = Self {
            request_id: reader.u64()?,
            target: reader.target()?,
            selected_stamp: reader.stamp()?,
            image: reader.image()?,
            image_identity: SemanticImageIdentity::try_from(reader.fixed()?)
                .map_err(|_| ReplicationError::InvalidWire)?,
            total_length: reader.u64()?,
            byte_range: reader.byte_range()?,
            payload: reader.sized_payload(MAX_RANGE_BYTES)?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.target.profile != self.selected_stamp.profile()
            || self.image.manifest_root().as_bytes() == &[0; 32]
            || self.image.semantic_generation().as_bytes() == &[0; 32]
            || self.total_length == 0
            || self.total_length > MAX_SEMANTIC_IMAGE_BYTES
        {
            return Err(ReplicationError::InvalidWire);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        let end = self.byte_range.end()?;
        if self.byte_range.len == 0
            || self.byte_range.len > MAX_RANGE_BYTES as u64
            || end > self.total_length
            || self.payload.len() as u64 != self.byte_range.len
        {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}

impl SemanticManifestChunk {
    /// Checks every response identity against the originating request.
    pub fn validate_against(&self, get: &SemanticManifestGet) -> Result<(), ReplicationError> {
        self.validate_shape()?;
        get.validate_shape()?;
        if self.request_id != get.request_id
            || self.target != get.target
            || self.selected_stamp != get.selected_stamp
            || self.catalog_root != get.catalog_root
            || self.image != get.image
            || self.total_length != get.total_length
            || self.byte_range != get.byte_range
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }

    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(MANIFEST_CHUNK_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.fixed(self.catalog_root.as_bytes())?;
        writer.image(self.image)?;
        writer.u64(self.total_length)?;
        writer.byte_range(self.byte_range)?;
        writer.sized_payload(&self.payload)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded page.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(MANIFEST_CHUNK_TAG)?;
        let result = Self {
            request_id: reader.u64()?,
            target: reader.target()?,
            selected_stamp: reader.stamp()?,
            catalog_root: SemanticPlaneCatalogRoot::from_wire_claim(reader.fixed()?),
            image: reader.image()?,
            total_length: reader.u64()?,
            byte_range: reader.byte_range()?,
            payload: reader.sized_payload(MAX_RANGE_BYTES)?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.target.profile != self.selected_stamp.profile()
            || self.catalog_root.as_bytes() == &[0; 32]
            || self.image.manifest_root().as_bytes() == &[0; 32]
            || self.image.semantic_generation().as_bytes() == &[0; 32]
        {
            return Err(ReplicationError::InvalidWire);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        validate_metadata_range(Some(self.total_length), self.byte_range)?;
        if self.payload.len() as u64 != self.byte_range.len {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}

/// Admits a complete canonical catalog only after its pages have been assembled.
pub fn admit_semantic_catalog(
    expected_root: SemanticPlaneCatalogRoot,
    bytes: &[u8],
) -> Result<SemanticPlaneCatalog, ReplicationError> {
    if bytes.len() as u64 > MAX_SEMANTIC_METADATA_BYTES {
        return Err(ReplicationError::MessageTooLarge);
    }
    let catalog = SemanticPlaneCatalog::decode(bytes).map_err(|_| ReplicationError::InvalidWire)?;
    if catalog.root() != expected_root {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(catalog)
}

/// Admits a complete canonical image manifest against its catalog entry.
pub fn admit_semantic_manifest(
    image: SemanticPlaneImageKey,
    bytes: &[u8],
) -> Result<SemanticPlaneManifest, ReplicationError> {
    if bytes.len() as u64 > MAX_SEMANTIC_METADATA_BYTES {
        return Err(ReplicationError::MessageTooLarge);
    }
    let manifest =
        SemanticPlaneManifest::decode(bytes).map_err(|_| ReplicationError::InvalidWire)?;
    if manifest.root() != image.manifest_root()
        || manifest.semantic_generation() != image.semantic_generation()
    {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(manifest)
}

fn validate_metadata_range(total: Option<u64>, range: ByteRange) -> Result<(), ReplicationError> {
    let end = range.end()?;
    if range.len == 0 || range.len > MAX_RANGE_BYTES as u64 || end > MAX_SEMANTIC_METADATA_BYTES {
        return Err(ReplicationError::Range);
    }
    if let Some(total) = total {
        if total == 0 || total > MAX_SEMANTIC_METADATA_BYTES || end > total {
            return Err(ReplicationError::Range);
        }
    }
    Ok(())
}

/// One exact bounded range request against an authority-selected generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRangeGet {
    /// Caller-generated nonzero correlation identity.
    pub request_id: u64,
    /// Canonical product key needed to re-read the selected frontier.
    pub target: SemanticTargetKey,
    /// Freshness stamp bound to the selected Turso frontier.
    pub selected_stamp: SelectedGenerationStamp,
    /// Exact semantic image tuple in the selected aggregate catalog.
    pub image: SemanticPlaneImageKey,
    /// Exact manifest, plane, segment and stable-key interval claim.
    pub range_request: SemanticRangeRequest,
    /// Exact requested half-open byte interval within the segment.
    pub byte_range: ByteRange,
}

impl SemanticRangeGet {
    /// Creates a range message from a cursor request after live freshness checks.
    pub fn from_request<S: SelectedGenerationSource>(
        request_id: u64,
        target: SemanticTargetKey,
        request: &IrHydrationRequest,
        source: &mut S,
    ) -> Result<Self, IrHydrationError> {
        let range_request = request.segment(source)?;
        let byte_range = request
            .byte_range(source)?
            .ok_or(IrHydrationError::Replication(ReplicationError::Range))?;
        let result = Self {
            request_id,
            target,
            selected_stamp: request.selection().stamp(),
            image: request.selection().image(),
            range_request,
            byte_range,
        };
        result.validate_shape()?;
        Ok(result)
    }

    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(GET_TAG)?;
        writer.u64(self.request_id)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.image(self.image)?;
        writer.range_request(self.range_request)?;
        writer.byte_range(self.byte_range)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded wire request.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(GET_TAG)?;
        let result = Self {
            request_id: reader.u64()?,
            target: reader.target()?,
            selected_stamp: reader.stamp()?,
            image: reader.image()?,
            range_request: reader.range_request()?,
            byte_range: reader.byte_range()?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.target.profile != self.selected_stamp.profile()
            || self.range_request.manifest_root != self.image.manifest_root()
            || self.image.manifest_root().as_bytes() == &[0; 32]
            || self.image.semantic_generation().as_bytes() == &[0; 32]
            || self.range_request.segment_id.as_bytes() == &[0; 32]
            || self.range_request.byte_length == 0
            || self.range_request.byte_length
                > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
            || self.range_request.first_key > self.range_request.last_key
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        validate_target_field(&self.target.package)?;
        validate_target_field(&self.target.coordinate)?;
        validate_segment_range(self.range_request.byte_length, self.byte_range)?;
        if self.byte_range.len > MAX_RANGE_BYTES as u64 {
            return Err(ReplicationError::ChunkTooLarge);
        }
        Ok(())
    }
}

/// One bounded response echoing the exact selected range claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRangeChunk {
    /// Correlation identity copied from the request.
    pub request_id: u64,
    /// Selected-frontier stamp copied from the request.
    pub selected_stamp: SelectedGenerationStamp,
    /// Exact semantic image tuple in the selected aggregate catalog.
    pub image: SemanticPlaneImageKey,
    /// Manifest root carried separately for direct service validation.
    pub manifest_root: SemanticManifestRoot,
    /// Exact IR or embedding plane claim.
    pub plane: SemanticPlaneKind,
    /// Untrusted semantic segment identity from the manifest claim.
    pub segment_id: UntrustedSemanticSegmentId,
    /// Exact byte interval returned from the immutable segment.
    pub byte_range: ByteRange,
    /// Complete segment byte length, not the response payload length.
    pub total_length: u64,
    /// At most 16 KiB of segment payload bytes.
    pub payload: Vec<u8>,
}

impl SemanticRangeChunk {
    /// Constructs a response by echoing the identity of the accepted request.
    pub fn for_request(get: &SemanticRangeGet, payload: Vec<u8>) -> Self {
        Self {
            request_id: get.request_id,
            selected_stamp: get.selected_stamp,
            image: get.image,
            manifest_root: get.range_request.manifest_root,
            plane: get.range_request.plane,
            segment_id: get.range_request.segment_id,
            byte_range: get.byte_range,
            total_length: get.range_request.byte_length,
            payload,
        }
    }

    /// Returns the canonical bounded wire encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        self.validate_shape()?;
        let mut writer = WireWriter::new(MAX_RANGE_MESSAGE_BYTES);
        writer.header(CHUNK_TAG)?;
        writer.u64(self.request_id)?;
        writer.stamp(self.selected_stamp)?;
        writer.image(self.image)?;
        writer.fixed(self.manifest_root.as_bytes())?;
        writer.plane(self.plane)?;
        writer.fixed(self.segment_id.as_bytes())?;
        writer.byte_range(self.byte_range)?;
        writer.u64(self.total_length)?;
        writer.sized_payload(&self.payload)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical bounded response, rejecting payloads over 16 KiB.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_RANGE_MESSAGE_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(CHUNK_TAG)?;
        let result = Self {
            request_id: reader.u64()?,
            selected_stamp: reader.stamp()?,
            image: reader.image()?,
            manifest_root: SemanticManifestRoot::from_wire_claim(reader.fixed()?),
            plane: reader.plane()?,
            segment_id: UntrustedSemanticSegmentId::from_raw(reader.fixed()?),
            byte_range: reader.byte_range()?,
            total_length: reader.u64()?,
            payload: reader.sized_payload(MAX_RANGE_BYTES)?,
        };
        reader.finish()?;
        result.validate_shape()?;
        Ok(result)
    }

    /// Checks every echoed identity and byte bound against the originating get.
    pub fn validate_against(&self, get: &SemanticRangeGet) -> Result<(), ReplicationError> {
        self.validate_shape()?;
        get.validate_shape()?;
        if self.request_id != get.request_id
            || self.selected_stamp != get.selected_stamp
            || self.image != get.image
            || self.manifest_root != get.range_request.manifest_root
            || self.manifest_root != self.image.manifest_root()
            || self.plane != get.range_request.plane
            || self.segment_id != get.range_request.segment_id
            || self.byte_range != get.byte_range
            || self.total_length != get.range_request.byte_length
            || self.byte_range.len
                != u64::try_from(self.payload.len()).map_err(|_| ReplicationError::Overflow)?
        {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), ReplicationError> {
        if self.request_id == 0
            || self.manifest_root != self.image.manifest_root()
            || self.segment_id.as_bytes() == &[0; 32]
        {
            return Err(ReplicationError::InvalidWire);
        }
        if self.payload.len() > MAX_RANGE_BYTES {
            return Err(ReplicationError::ChunkTooLarge);
        }
        validate_segment_range(self.total_length, self.byte_range)?;
        if self.byte_range.len
            != u64::try_from(self.payload.len()).map_err(|_| ReplicationError::Overflow)?
        {
            return Err(ReplicationError::Range);
        }
        Ok(())
    }
}

/// Durable sparse CAS capability for semantic segments.
///
/// `stage_durable_range` must persist bytes before returning, accept identical
/// duplicate ranges idempotently, and reject conflicting overlap. Its returned
/// coverage describes only extents durable in that CAS namespace. The opaque
/// checkpoint bytes must be the CAS's existing byte-free sparse checkpoint;
/// this interface defines no second object or extent encoding.
pub trait DurableSemanticRangeStore: DurableSemanticSegmentStore {
    /// Storage-specific failure reported before cursor acknowledgement.
    type RangeError: fmt::Display;

    /// Persists one exact range under the selected manifest/plane/segment.
    fn stage_durable_range(
        &mut self,
        selection: crate::SelectedSemanticPlane,
        request: SemanticRangeRequest,
        byte_range: ByteRange,
        payload: &[u8],
    ) -> Result<SparseCoverage, Self::RangeError>;

    /// Reads the complete sparse object only when all bytes have been retained.
    fn read_complete_segment(
        &mut self,
        selection: crate::SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<Option<Box<[u8]>>, Self::RangeError>;

    /// Captures the existing CAS sparse-transfer checkpoint after durable staging.
    fn checkpoint_sparse_segment(
        &mut self,
        selection: crate::SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<Vec<u8>, Self::RangeError>;

    /// Reopens sparse extents from the existing CAS checkpoint after process restart.
    fn resume_sparse_segment(
        &mut self,
        selection: crate::SelectedSemanticPlane,
        request: SemanticRangeRequest,
        checkpoint: &[u8],
    ) -> Result<SparseCoverage, Self::RangeError>;

    /// Removes sparse bytes after the complete segment fails semantic
    /// admission, so the next poll requests a clean transfer from offset zero.
    fn discard_sparse_segment(
        &mut self,
        selection: crate::SelectedSemanticPlane,
        request: SemanticRangeRequest,
    ) -> Result<(), Self::RangeError>;
}

/// Result after a durable sparse range write or full segment admission.
#[derive(Debug)]
pub enum SemanticRangeClientProgress {
    /// Bytes and checkpoint are durable; the cursor still requests the missing ranges.
    Staged {
        /// Durable byte extents to pass to `IrHydrationCursor::next_request`.
        coverage: SparseSegmentCoverage,
        /// Root-bound semantic token plus the existing sparse CAS checkpoint.
        checkpoint: SemanticRangeClientCheckpoint,
    },
    /// The full segment passed manifest admission and durable CAS read-back; cursor ACKed.
    Complete(VerifiedSemanticSegment),
}

/// Process-restart checkpoint binding semantic cursor and sparse CAS state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRangeClientCheckpoint {
    target: SemanticTargetKey,
    selected_stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    range_request: SemanticRangeRequest,
    cursor_token: Vec<u8>,
    cas_checkpoint: Vec<u8>,
}

impl SemanticRangeClientCheckpoint {
    fn capture<S: SelectedGenerationSource, C: DurableSemanticRangeStore>(
        cursor: &IrHydrationCursor<'_, '_>,
        source: &mut S,
        get: &SemanticRangeGet,
        store: &mut C,
    ) -> Result<Self, IrHydrationError> {
        let hydration = cursor.checkpoint(source)?;
        if hydration.selection().stamp() != get.selected_stamp
            || hydration.selection().image() != get.image
            || get.range_request.manifest_root != get.image.manifest_root()
        {
            return Err(IrHydrationError::Terminal(
                crate::IrHydrationTerminal::Stale,
            ));
        }
        let cas_checkpoint = store
            .checkpoint_sparse_segment(hydration.selection(), get.range_request)
            .map_err(|error| IrHydrationError::Storage(error.to_string()))?;
        if cas_checkpoint.len() > MAX_CAS_CHECKPOINT_BYTES {
            return Err(IrHydrationError::Replication(
                ReplicationError::MessageTooLarge,
            ));
        }
        if cursor.checkpoint(source)? != hydration {
            return Err(IrHydrationError::Terminal(
                crate::IrHydrationTerminal::Stale,
            ));
        }
        Ok(Self {
            target: get.target.clone(),
            selected_stamp: get.selected_stamp,
            image: get.image,
            range_request: get.range_request,
            cursor_token: hydration.encode_cursor()?,
            cas_checkpoint,
        })
    }

    /// Returns the target required to reconstruct the live index authority key.
    #[must_use]
    pub const fn target(&self) -> &SemanticTargetKey {
        &self.target
    }

    /// Returns the exact selected-frontier stamp bound to this checkpoint.
    #[must_use]
    pub const fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.selected_stamp
    }

    #[must_use]
    /// Returns the exact image tuple bound to this sparse checkpoint.
    pub const fn image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    /// Returns the outstanding semantic segment claim.
    #[must_use]
    pub const fn range_request(&self) -> SemanticRangeRequest {
        self.range_request
    }

    /// Returns the opaque existing sparse-CAS checkpoint bytes.
    #[must_use]
    pub fn cas_checkpoint(&self) -> &[u8] {
        &self.cas_checkpoint
    }

    /// Rebinds the semantic cursor after restart and a fresh authority read.
    pub fn restore_hydration<S: SelectedGenerationSource>(
        &self,
        manifest: &backend_semantic::ir::SemanticPlaneManifest,
        source: &mut S,
    ) -> Result<IrHydrationCheckpoint, IrHydrationError> {
        validate_checkpoint_binding(self)?;
        let cursor = SemanticHydrationCursorToken::decode(&self.cursor_token)?;
        validate_token_binding(cursor, self.range_request)?;
        if cursor.encode()? != self.cursor_token {
            return Err(IrHydrationError::Replication(ReplicationError::InvalidWire));
        }
        IrHydrationCheckpoint::restore(
            manifest,
            source,
            self.selected_stamp,
            self.image,
            self.range_request.plane,
            cursor,
        )
    }

    /// Reopens durable sparse extents from the existing CAS checkpoint.
    pub fn restore_sparse<S: SelectedGenerationSource, C: DurableSemanticRangeStore>(
        &self,
        selection: crate::SelectedSemanticPlane,
        source: &mut S,
        store: &mut C,
        max_ranges: usize,
    ) -> Result<SparseSegmentCoverage, IrHydrationError> {
        validate_checkpoint_binding(self)?;
        if selection.stamp() != self.selected_stamp
            || selection.image() != self.image
            || selection.kind() != self.range_request.plane
        {
            return Err(IrHydrationError::Terminal(
                crate::IrHydrationTerminal::Stale,
            ));
        }
        let current = source
            .current_selected_generation()
            .map_err(|error| IrHydrationError::Frontier(error.to_string()))?;
        if current != self.selected_stamp
            || !source
                .selected_image_is_current(self.selected_stamp, self.image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(
                crate::IrHydrationTerminal::Stale,
            ));
        }
        let coverage = store
            .resume_sparse_segment(selection, self.range_request, &self.cas_checkpoint)
            .map_err(|error| IrHydrationError::Storage(error.to_string()))?;
        validate_durable_coverage(&coverage, self.range_request, None, max_ranges)?;
        Ok(SparseSegmentCoverage::new(
            self.range_request.segment_id,
            coverage,
        ))
    }

    /// Reopens cursor and sparse extents, then checks the outstanding segment
    /// against a newly planned missing-range request.
    pub fn resume<'manifest, 'have, A, C>(
        &self,
        manifest: &'manifest backend_semantic::ir::SemanticPlaneManifest,
        source: &mut A,
        have_ids: &'have [backend_semantic::ir::SemanticSegmentId],
        limits: TransportLimits,
        store: &mut C,
    ) -> Result<
        (
            IrHydrationCursor<'manifest, 'have>,
            SparseSegmentCoverage,
            IrHydrationPoll,
        ),
        IrHydrationError,
    >
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
    {
        let limits = limits.validate()?;
        let hydration = self.restore_hydration(manifest, source)?;
        let selection = hydration.selection();
        let max_ranges = limits.max_ranges;
        let mut cursor = IrHydrationCursor::resume(manifest, hydration, source, have_ids, limits)?;
        let coverage = self.restore_sparse(selection, source, store, max_ranges)?;
        let poll = cursor.next_request(
            source,
            Some(&coverage),
            HydrationCredits::new(1, MAX_RANGE_BYTES as u64),
        )?;
        let next = match &poll {
            IrHydrationPoll::Request(request) | IrHydrationPoll::VerifyLocal(request) => request,
            IrHydrationPoll::NoCredits | IrHydrationPoll::Exhausted => {
                return Err(IrHydrationError::Terminal(
                    crate::IrHydrationTerminal::Stale,
                ));
            }
        };
        if next.segment(source)? != self.range_request {
            return Err(IrHydrationError::Terminal(
                crate::IrHydrationTerminal::Stale,
            ));
        }
        Ok((cursor, coverage, poll))
    }

    /// Returns the canonical bounded checkpoint encoding.
    pub fn encode(&self) -> Result<Vec<u8>, ReplicationError> {
        validate_checkpoint_binding(self)?;
        let token = SemanticHydrationCursorToken::decode(&self.cursor_token)
            .map_err(|_| ReplicationError::InvalidWire)?;
        validate_token_binding(token, self.range_request)?;
        if self.cas_checkpoint.len() > MAX_CAS_CHECKPOINT_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut writer = WireWriter::new(MAX_CLIENT_CHECKPOINT_BYTES);
        writer.header(CHECKPOINT_TAG)?;
        writer.target(&self.target)?;
        writer.stamp(self.selected_stamp)?;
        writer.image(self.image)?;
        writer.range_request(self.range_request)?;
        writer.sized_bytes(&self.cursor_token, 64 * 1024)?;
        writer.sized_bytes(&self.cas_checkpoint, MAX_CAS_CHECKPOINT_BYTES)?;
        Ok(writer.finish())
    }

    /// Decodes one canonical root-bound checkpoint.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplicationError> {
        if bytes.len() > MAX_CLIENT_CHECKPOINT_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let mut reader = WireReader::new(bytes);
        reader.header(CHECKPOINT_TAG)?;
        let checkpoint = Self {
            target: reader.target()?,
            selected_stamp: reader.stamp()?,
            image: reader.image()?,
            range_request: reader.range_request()?,
            cursor_token: reader.sized_bytes(64 * 1024)?,
            cas_checkpoint: reader.sized_bytes(MAX_CAS_CHECKPOINT_BYTES)?,
        };
        reader.finish()?;
        validate_checkpoint_binding(&checkpoint)?;
        let token = SemanticHydrationCursorToken::decode(&checkpoint.cursor_token)
            .map_err(|_| ReplicationError::InvalidWire)?;
        validate_token_binding(token, checkpoint.range_request)?;
        if token.encode().map_err(|_| ReplicationError::InvalidWire)? != checkpoint.cursor_token {
            return Err(ReplicationError::InvalidWire);
        }
        Ok(checkpoint)
    }
}

/// Admits one response only after its bytes have reached durable sparse CAS.
///
/// An incomplete segment returns a durable sparse checkpoint and never ACKs
/// the semantic cursor. A complete segment is read from the CAS, hashed
/// against the selected manifest, committed/read back through the complete
/// segment store, and ACKed only after the final selection recheck.
pub fn accept_semantic_range<A, C>(
    cursor: &mut IrHydrationCursor<'_, '_>,
    source: &mut A,
    request: &IrHydrationRequest,
    get: &SemanticRangeGet,
    chunk: &SemanticRangeChunk,
    store: &mut C,
    limits: TransportLimits,
) -> Result<SemanticRangeClientProgress, IrHydrationError>
where
    A: SelectedGenerationSource,
    C: DurableSemanticRangeStore,
{
    limits.validate()?;
    get.validate_shape()?;
    chunk.validate_against(get)?;
    if get.selected_stamp != request.selection().stamp()
        || get.image != request.selection().image()
        || get.range_request != request.segment(source)?
        || request.byte_range(source)? != Some(get.byte_range)
    {
        return Err(IrHydrationError::Terminal(
            crate::IrHydrationTerminal::Stale,
        ));
    }

    let selection = request.selection();
    let coverage = store
        .stage_durable_range(selection, get.range_request, get.byte_range, &chunk.payload)
        .map_err(|error| IrHydrationError::Storage(error.to_string()))?;
    validate_durable_coverage(
        &coverage,
        get.range_request,
        Some(get.byte_range),
        limits.max_ranges,
    )?;

    // The durable write can race a selected-head change; do not checkpoint or
    // admit bytes under the old selection after that change.
    if request.segment(source)? != get.range_request
        || request.byte_range(source)? != Some(get.byte_range)
    {
        return Err(IrHydrationError::Terminal(
            crate::IrHydrationTerminal::Stale,
        ));
    }

    let complete = ByteRange::new(0, get.range_request.byte_length)?;
    if coverage.covers(complete) {
        let payload = store
            .read_complete_segment(selection, get.range_request)
            .map_err(|error| IrHydrationError::Storage(error.to_string()))?
            .ok_or_else(|| {
                IrHydrationError::Storage(
                    "sparse CAS claims complete coverage but cannot read the segment".to_owned(),
                )
            })?;
        if u64::try_from(payload.len()).map_err(|_| ReplicationError::Overflow)?
            != get.range_request.byte_length
        {
            return Err(IrHydrationError::Replication(ReplicationError::Incomplete));
        }
        let pending = match cursor.verify_payload(source, request, &payload) {
            Ok(pending) => pending,
            Err(error @ IrHydrationError::Semantic(_)) => {
                store
                    .discard_sparse_segment(selection, get.range_request)
                    .map_err(|storage| IrHydrationError::Storage(storage.to_string()))?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let admitted = cursor.commit_admitted_segment(source, request, pending, store)?;
        return Ok(SemanticRangeClientProgress::Complete(admitted));
    }

    let checkpoint = SemanticRangeClientCheckpoint::capture(cursor, source, get, store)?;
    Ok(SemanticRangeClientProgress::Staged {
        coverage: SparseSegmentCoverage::new(get.range_request.segment_id, coverage),
        checkpoint,
    })
}

fn validate_checkpoint_binding(
    checkpoint: &SemanticRangeClientCheckpoint,
) -> Result<(), ReplicationError> {
    validate_target_field(&checkpoint.target.package)?;
    validate_target_field(&checkpoint.target.coordinate)?;
    if checkpoint.target.profile != checkpoint.selected_stamp.profile()
        || checkpoint.range_request.manifest_root != checkpoint.image.manifest_root()
        || checkpoint.image.manifest_root().as_bytes() == &[0; 32]
        || checkpoint.image.semantic_generation().as_bytes() == &[0; 32]
        || checkpoint.range_request.segment_id.as_bytes() == &[0; 32]
        || checkpoint.range_request.byte_length == 0
        || checkpoint.range_request.byte_length
            > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
        || checkpoint.range_request.first_key > checkpoint.range_request.last_key
    {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(())
}

fn validate_target_field(value: &str) -> Result<(), ReplicationError> {
    if value.is_empty()
        || value.len() > MAX_TARGET_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ReplicationError::InvalidWire);
    }
    Ok(())
}

fn validate_segment_range(total_length: u64, range: ByteRange) -> Result<(), ReplicationError> {
    if total_length == 0
        || total_length > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
        || range.len == 0
        || range.end()? > total_length
    {
        return Err(ReplicationError::Range);
    }
    Ok(())
}

fn validate_token_binding(
    token: SemanticHydrationCursorToken,
    request: SemanticRangeRequest,
) -> Result<(), ReplicationError> {
    if token.manifest_root() != request.manifest_root || token.plane_filter() != Some(request.plane)
    {
        return Err(ReplicationError::IdentityMismatch);
    }
    Ok(())
}

fn validate_durable_coverage(
    coverage: &SparseCoverage,
    request: SemanticRangeRequest,
    required_range: Option<ByteRange>,
    max_ranges: usize,
) -> Result<(), IrHydrationError> {
    if coverage.ranges().len() > max_ranges {
        return Err(IrHydrationError::Replication(
            ReplicationError::CoverageLimit,
        ));
    }
    for range in coverage.ranges() {
        if range.len == 0 || range.end()? > request.byte_length {
            return Err(IrHydrationError::Replication(ReplicationError::Range));
        }
    }
    if required_range.is_some_and(|range| !coverage.covers(range)) {
        return Err(IrHydrationError::Storage(
            "durable CAS did not report the newly staged range".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) struct WireWriter {
    bytes: Vec<u8>,
    max: usize,
}

impl WireWriter {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), ReplicationError> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(ReplicationError::Overflow)?;
        if end > self.max {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn header(&mut self, tag: u8) -> Result<(), ReplicationError> {
        self.push(&MAGIC)?;
        self.push(&[WIRE_VERSION, tag])
    }

    fn u8(&mut self, value: u8) -> Result<(), ReplicationError> {
        self.push(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), ReplicationError> {
        self.push(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), ReplicationError> {
        self.push(&value.to_be_bytes())
    }

    pub(crate) fn u64(&mut self, value: u64) -> Result<(), ReplicationError> {
        self.push(&value.to_be_bytes())
    }

    pub(crate) fn fixed(&mut self, value: &[u8; 32]) -> Result<(), ReplicationError> {
        self.push(value)
    }

    pub(crate) fn sized_bytes(
        &mut self,
        value: &[u8],
        maximum: usize,
    ) -> Result<(), ReplicationError> {
        if value.len() > maximum {
            return Err(ReplicationError::MessageTooLarge);
        }
        let len = u32::try_from(value.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
        self.u32(len)?;
        self.push(value)
    }

    fn sized_payload(&mut self, value: &[u8]) -> Result<(), ReplicationError> {
        if value.len() > MAX_RANGE_BYTES {
            return Err(ReplicationError::ChunkTooLarge);
        }
        self.sized_bytes(value, MAX_RANGE_BYTES)
    }

    fn target(&mut self, value: &SemanticTargetKey) -> Result<(), ReplicationError> {
        validate_target_field(&value.package)?;
        validate_target_field(&value.coordinate)?;
        let package_len =
            u16::try_from(value.package.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
        let coordinate_len =
            u16::try_from(value.coordinate.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
        self.u16(package_len)?;
        self.push(value.package.as_bytes())?;
        self.u16(coordinate_len)?;
        self.push(value.coordinate.as_bytes())?;
        self.profile(value.profile)
    }

    fn profile(&mut self, value: LanguageProfile) -> Result<(), ReplicationError> {
        self.push(&<[u8; 2]>::from(value))
    }

    pub(crate) fn stamp(&mut self, value: SelectedGenerationStamp) -> Result<(), ReplicationError> {
        self.push(value.namespace())?;
        self.profile(value.profile())?;
        self.push(value.source_coordinate())?;
        self.u64(value.selection_revision())?;
        self.push(value.selected_root())?;
        self.push(value.closure_id())?;
        self.fixed(value.catalog_root().as_bytes())
    }

    pub(crate) fn image(&mut self, value: SemanticPlaneImageKey) -> Result<(), ReplicationError> {
        self.u32(value.artifact_ordinal())?;
        self.fixed(value.semantic_generation().as_bytes())?;
        self.fixed(value.manifest_root().as_bytes())
    }

    pub(crate) fn range_request(
        &mut self,
        value: SemanticRangeRequest,
    ) -> Result<(), ReplicationError> {
        self.fixed(value.manifest_root.as_bytes())?;
        self.plane(value.plane)?;
        self.push(value.segment_id.as_bytes())?;
        self.fixed(&value.first_key)?;
        self.fixed(&value.last_key)?;
        self.u64(value.byte_length)
    }

    fn byte_range(&mut self, value: ByteRange) -> Result<(), ReplicationError> {
        self.u64(value.start)?;
        self.u64(value.len)
    }

    pub(crate) fn plane(&mut self, value: SemanticPlaneKind) -> Result<(), ReplicationError> {
        match value {
            SemanticPlaneKind::Ir(ir) => {
                self.u8(1)?;
                match ir {
                    SemanticIrPlane::Core => self.u8(1),
                    SemanticIrPlane::Types => self.u8(2),
                    SemanticIrPlane::Relations => self.u8(3),
                    SemanticIrPlane::Occurrences => self.u8(4),
                    SemanticIrPlane::Documentation => self.u8(5),
                    SemanticIrPlane::SourceProvenance => self.u8(6),
                    SemanticIrPlane::LanguageExtensions(profile) => {
                        self.u8(7)?;
                        self.profile(profile)
                    }
                }
            }
            SemanticPlaneKind::Embeddings(value) => {
                self.u8(2)?;
                self.fixed(value.model())?;
                self.fixed(value.model_version())?;
                self.fixed(value.tokenizer())?;
                self.u32(value.dimension())?;
                match value.normalization() {
                    EmbeddingNormalization::None => {
                        self.u8(0)?;
                        self.fixed(&[0; 32])?;
                    }
                    EmbeddingNormalization::L2 => {
                        self.u8(1)?;
                        self.fixed(&[0; 32])?;
                    }
                    EmbeddingNormalization::MeanCenteredL2 => {
                        self.u8(2)?;
                        self.fixed(&[0; 32])?;
                    }
                    EmbeddingNormalization::Custom(identity) => {
                        self.u8(3)?;
                        self.fixed(&identity)?;
                    }
                }
                self.fixed(value.toolchain())?;
                self.fixed(value.recipe())
            }
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) struct WireReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> WireReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ReplicationError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(ReplicationError::Overflow)?;
        if end > self.bytes.len() {
            return Err(ReplicationError::TruncatedFrame);
        }
        let result = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, ReplicationError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(ReplicationError::TruncatedFrame)
    }

    fn u16(&mut self) -> Result<u16, ReplicationError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, ReplicationError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)?,
        ))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, ReplicationError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ReplicationError::TruncatedFrame)?,
        ))
    }

    pub(crate) fn fixed(&mut self) -> Result<[u8; 32], ReplicationError> {
        self.take(32)?
            .try_into()
            .map_err(|_| ReplicationError::TruncatedFrame)
    }

    pub(crate) fn sized_bytes(&mut self, maximum: usize) -> Result<Vec<u8>, ReplicationError> {
        let len = usize::try_from(self.u32()?).map_err(|_| ReplicationError::Overflow)?;
        if len > maximum {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(self.take(len)?.to_vec())
    }

    fn sized_payload(&mut self, maximum: usize) -> Result<Vec<u8>, ReplicationError> {
        let payload = self.sized_bytes(maximum)?;
        if payload.len() > MAX_RANGE_BYTES {
            return Err(ReplicationError::ChunkTooLarge);
        }
        Ok(payload)
    }

    pub(crate) fn header(&mut self, expected_tag: u8) -> Result<(), ReplicationError> {
        if self.take(MAGIC.len())? != MAGIC {
            return Err(ReplicationError::InvalidWire);
        }
        if self.u8()? != WIRE_VERSION {
            return Err(ReplicationError::UnsupportedWireVersion);
        }
        if self.u8()? != expected_tag {
            return Err(ReplicationError::WrongMessage);
        }
        Ok(())
    }

    pub(crate) fn finish(&self) -> Result<(), ReplicationError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ReplicationError::TrailingFrame)
        }
    }

    fn target(&mut self) -> Result<SemanticTargetKey, ReplicationError> {
        let package_len = usize::from(self.u16()?);
        if package_len > MAX_TARGET_FIELD_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let package = String::from_utf8(self.take(package_len)?.to_vec())
            .map_err(|_| ReplicationError::InvalidWire)?;
        let coordinate_len = usize::from(self.u16()?);
        if coordinate_len > MAX_TARGET_FIELD_BYTES {
            return Err(ReplicationError::MessageTooLarge);
        }
        let coordinate = String::from_utf8(self.take(coordinate_len)?.to_vec())
            .map_err(|_| ReplicationError::InvalidWire)?;
        SemanticTargetKey::new(package, coordinate, self.profile()?)
    }

    fn profile(&mut self) -> Result<LanguageProfile, ReplicationError> {
        let profile: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| ReplicationError::TruncatedFrame)?;
        LanguageProfile::try_from(profile).map_err(|_| ReplicationError::InvalidWire)
    }

    pub(crate) fn stamp(&mut self) -> Result<SelectedGenerationStamp, ReplicationError> {
        let namespace: [u8; 16] = self
            .take(16)?
            .try_into()
            .map_err(|_| ReplicationError::TruncatedFrame)?;
        let profile = self.profile()?;
        let source_coordinate = self.fixed()?;
        let selection_revision = self.u64()?;
        let selected_root = self.fixed()?;
        let closure_id = self.fixed()?;
        let catalog_root = SemanticPlaneCatalogRoot::from_wire_claim(self.fixed()?);
        SelectedGenerationStamp::checked(
            namespace,
            profile,
            source_coordinate,
            selection_revision,
            selected_root,
            closure_id,
            catalog_root,
        )
        .map_err(|_| ReplicationError::InvalidWire)
    }

    pub(crate) fn image(&mut self) -> Result<SemanticPlaneImageKey, ReplicationError> {
        let artifact_ordinal = self.u32()?;
        let semantic_generation = GenerationId::from_raw(self.fixed()?);
        let manifest_root = SemanticManifestRoot::from_wire_claim(self.fixed()?);
        Ok(SemanticPlaneImageKey::new(
            artifact_ordinal,
            semantic_generation,
            manifest_root,
        ))
    }

    pub(crate) fn range_request(&mut self) -> Result<SemanticRangeRequest, ReplicationError> {
        Ok(SemanticRangeRequest {
            manifest_root: SemanticManifestRoot::from_wire_claim(self.fixed()?),
            plane: self.plane()?,
            segment_id: UntrustedSemanticSegmentId::from_raw(self.fixed()?),
            first_key: self.fixed()?,
            last_key: self.fixed()?,
            byte_length: self.u64()?,
        })
    }

    fn byte_range(&mut self) -> Result<ByteRange, ReplicationError> {
        ByteRange::new(self.u64()?, self.u64()?)
    }

    pub(crate) fn plane(&mut self) -> Result<SemanticPlaneKind, ReplicationError> {
        match self.u8()? {
            1 => {
                let plane = match self.u8()? {
                    1 => SemanticIrPlane::Core,
                    2 => SemanticIrPlane::Types,
                    3 => SemanticIrPlane::Relations,
                    4 => SemanticIrPlane::Occurrences,
                    5 => SemanticIrPlane::Documentation,
                    6 => SemanticIrPlane::SourceProvenance,
                    7 => SemanticIrPlane::LanguageExtensions(self.profile()?),
                    _ => return Err(ReplicationError::InvalidWire),
                };
                Ok(SemanticPlaneKind::Ir(plane))
            }
            2 => {
                let model = self.fixed()?;
                let model_version = self.fixed()?;
                let tokenizer = self.fixed()?;
                let dimension = self.u32()?;
                let code = self.u8()?;
                let identity = self.fixed()?;
                let normalization = match code {
                    0 if identity == [0; 32] => EmbeddingNormalization::None,
                    1 if identity == [0; 32] => EmbeddingNormalization::L2,
                    2 if identity == [0; 32] => EmbeddingNormalization::MeanCenteredL2,
                    3 => EmbeddingNormalization::Custom(identity),
                    _ => return Err(ReplicationError::InvalidWire),
                };
                let toolchain = self.fixed()?;
                let recipe = self.fixed()?;
                let plane = EmbeddingPlaneIdentity::new(
                    model,
                    model_version,
                    tokenizer,
                    dimension,
                    normalization,
                    toolchain,
                    recipe,
                )
                .map_err(|_| ReplicationError::InvalidWire)?;
                Ok(SemanticPlaneKind::Embeddings(plane))
            }
            _ => Err(ReplicationError::InvalidWire),
        }
    }
}

#[cfg(test)]
mod tests {
    use backend_semantic::ir::{GenerationId, RustEdition, SemanticIrPlane};

    use super::*;

    fn image() -> SemanticPlaneImageKey {
        SemanticPlaneImageKey::new(
            0,
            GenerationId::from_raw([4; 32]),
            SemanticManifestRoot::from_wire_claim([7; 32]),
        )
    }

    fn catalog_root() -> SemanticPlaneCatalogRoot {
        SemanticPlaneCatalog::new(vec![
            backend_semantic::ir::SemanticPlaneCatalogEntry::new(image(), 8)
                .expect("catalog entry"),
        ])
        .expect("catalog")
        .root()
    }

    fn stamp() -> SelectedGenerationStamp {
        SelectedGenerationStamp::checked(
            [1; 16],
            LanguageProfile::Rust(RustEdition::Rust2024),
            [2; 32],
            3,
            [5; 32],
            [6; 32],
            catalog_root(),
        )
        .expect("valid selection stamp")
    }

    fn get() -> SemanticRangeGet {
        SemanticRangeGet {
            request_id: 9,
            target: SemanticTargetKey::new(
                "pkg:cargo/example@1.2.3",
                "pkg:cargo/example@1.2.3",
                LanguageProfile::Rust(RustEdition::Rust2024),
            )
            .expect("target"),
            selected_stamp: stamp(),
            image: image(),
            range_request: SemanticRangeRequest {
                manifest_root: SemanticManifestRoot::from_wire_claim([7; 32]),
                plane: SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                segment_id: UntrustedSemanticSegmentId::from_raw([8; 32]),
                first_key: [10; 32],
                last_key: [11; 32],
                byte_length: 8,
            },
            byte_range: ByteRange::new(2, 3).expect("range"),
        }
    }

    fn target() -> SemanticTargetKey {
        SemanticTargetKey::new(
            "pkg:cargo/example@1.2.3",
            "pkg:cargo/example@1.2.3",
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("target")
    }

    fn catalog() -> SemanticPlaneCatalog {
        SemanticPlaneCatalog::new(vec![
            backend_semantic::ir::SemanticPlaneCatalogEntry::new(image(), 8)
                .expect("catalog entry"),
        ])
        .expect("catalog")
    }

    fn selected_image_get() -> SelectedSemanticImageGet {
        SelectedSemanticImageGet {
            request_id: 41,
            target: target(),
            selected_stamp: stamp(),
            image: image(),
            image_identity: None,
            total_length: None,
            byte_range: ByteRange::new(0, 1).expect("image identity probe"),
        }
    }

    #[test]
    fn get_and_chunk_roundtrip_and_bind_every_response_field() {
        let get = get();
        let decoded_get =
            SemanticRangeGet::decode(&get.encode().expect("get encode")).expect("get decode");
        assert_eq!(decoded_get, get);

        let chunk = SemanticRangeChunk::for_request(&get, b"abc".to_vec());
        let decoded_chunk = SemanticRangeChunk::decode(&chunk.encode().expect("chunk encode"))
            .expect("chunk decode");
        decoded_chunk
            .validate_against(&get)
            .expect("response identity");
        assert_eq!(decoded_chunk, chunk);

        let mut changed = decoded_chunk;
        changed.byte_range = ByteRange::new(3, 3).expect("changed range");
        assert_eq!(
            changed.validate_against(&get),
            Err(ReplicationError::IdentityMismatch)
        );
    }

    #[test]
    fn range_codec_rejects_oversized_or_noncanonical_payloads() {
        let get = get();
        let mut oversized = SemanticRangeChunk::for_request(&get, vec![0; MAX_RANGE_BYTES + 1]);
        oversized.byte_range =
            ByteRange::new(0, MAX_RANGE_BYTES as u64 + 1).expect("oversized range");
        oversized.total_length = MAX_RANGE_BYTES as u64 + 1;
        assert_eq!(oversized.encode(), Err(ReplicationError::ChunkTooLarge));

        let mut noncanonical = get.encode().expect("get encode");
        noncanonical.push(0);
        assert_eq!(
            SemanticRangeGet::decode(&noncanonical),
            Err(ReplicationError::TrailingFrame)
        );
    }

    #[test]
    fn catalog_and_manifest_pages_keep_aggregate_and_image_roots_separate() {
        let catalog = catalog();
        let catalog_bytes = catalog.encode().expect("canonical catalog");
        let stamp = stamp();
        assert_eq!(stamp.catalog_root(), catalog.root());
        assert_ne!(
            stamp.catalog_root().as_bytes(),
            image().manifest_root().as_bytes()
        );
        let catalog_get = SemanticCatalogGet {
            request_id: 31,
            target: target(),
            selected_stamp: None,
            catalog_root: None,
            total_length: None,
            byte_range: ByteRange::new(0, catalog_bytes.len() as u64).expect("catalog page"),
        };
        let decoded_get = SemanticCatalogGet::decode(&catalog_get.encode().expect("catalog get"))
            .expect("decode catalog get");
        assert_eq!(decoded_get, catalog_get);
        let catalog_chunk = SemanticCatalogChunk {
            request_id: catalog_get.request_id,
            target: catalog_get.target.clone(),
            selected_stamp: stamp,
            catalog_root: catalog.root(),
            total_length: catalog_bytes.len() as u64,
            byte_range: catalog_get.byte_range,
            payload: catalog_bytes.to_vec(),
        };
        let decoded_chunk =
            SemanticCatalogChunk::decode(&catalog_chunk.encode().expect("catalog chunk encode"))
                .expect("catalog chunk decode");
        decoded_chunk
            .validate_against(&catalog_get)
            .expect("catalog response identity");
        let reopened = admit_semantic_catalog(decoded_chunk.catalog_root, &decoded_chunk.payload)
            .expect("admit complete canonical catalog");
        assert_eq!(reopened.root(), catalog.root());

        let image_key = reopened.entries()[0].image();
        let manifest_get = SemanticManifestGet {
            request_id: 32,
            target: target(),
            selected_stamp: stamp,
            catalog_root: catalog.root(),
            image: image_key,
            total_length: u64::from(reopened.entries()[0].manifest_length()),
            byte_range: ByteRange::new(2, 3).expect("manifest page"),
        };
        manifest_get
            .validate_against_catalog(&reopened)
            .expect("manifest request matches catalog entry");
        let roundtrip =
            SemanticManifestGet::decode(&manifest_get.encode().expect("manifest get encode"))
                .expect("manifest get decode");
        assert_eq!(roundtrip, manifest_get);
        let manifest_chunk = SemanticManifestChunk {
            request_id: manifest_get.request_id,
            target: manifest_get.target.clone(),
            selected_stamp: stamp,
            catalog_root: catalog.root(),
            image: image_key,
            total_length: manifest_get.total_length,
            byte_range: manifest_get.byte_range,
            payload: b"abc".to_vec(),
        };
        let decoded_manifest_chunk =
            SemanticManifestChunk::decode(&manifest_chunk.encode().expect("manifest chunk encode"))
                .expect("manifest chunk decode");
        decoded_manifest_chunk
            .validate_against(&manifest_get)
            .expect("manifest response identity");

        let mut crossed = decoded_manifest_chunk;
        crossed.image = SemanticPlaneImageKey::new(
            image_key.artifact_ordinal() + 1,
            image_key.semantic_generation(),
            image_key.manifest_root(),
        );
        assert_eq!(
            crossed.validate_against(&manifest_get),
            Err(ReplicationError::IdentityMismatch)
        );
    }

    #[test]
    fn catalog_page_continuation_requires_one_complete_expected_tuple() {
        let request = SemanticCatalogGet {
            request_id: 33,
            target: target(),
            selected_stamp: Some(stamp()),
            catalog_root: Some(catalog_root()),
            total_length: Some(128),
            byte_range: ByteRange::new(16, 16).expect("continuation range"),
        };
        assert_eq!(
            SemanticCatalogGet::decode(&request.encode().expect("continuation encode"))
                .expect("continuation decode"),
            request
        );
        let mut partial_identity = request.clone();
        partial_identity.catalog_root = None;
        assert_eq!(
            partial_identity.encode(),
            Err(ReplicationError::InvalidWire)
        );
    }

    #[test]
    fn selected_full_image_pages_bind_typed_identity_length_and_authority() {
        let first_get = selected_image_get();
        let first_roundtrip =
            SelectedSemanticImageGet::decode(&first_get.encode().expect("first image page encode"))
                .expect("first image page decode");
        assert_eq!(first_roundtrip, first_get);

        let identity = SemanticImageIdentity::from_encoded_bytes(b"canonical NXFI bytes");
        let first_chunk = SelectedSemanticImageChunk {
            request_id: first_get.request_id,
            target: first_get.target.clone(),
            selected_stamp: first_get.selected_stamp,
            image: first_get.image,
            image_identity: identity,
            total_length: 128,
            byte_range: first_get.byte_range,
            payload: vec![0],
        };
        let first_chunk = SelectedSemanticImageChunk::decode(
            &first_chunk.encode().expect("first image reply encode"),
        )
        .expect("first image reply decode");
        first_chunk
            .validate_against(&first_get)
            .expect("first image reply matches selected image");

        let continuation = SelectedSemanticImageGet {
            request_id: 42,
            target: first_get.target.clone(),
            selected_stamp: first_get.selected_stamp,
            image: first_get.image,
            image_identity: Some(identity),
            total_length: Some(128),
            byte_range: ByteRange::new(1, 16).expect("continuation page"),
        };
        let continuation_roundtrip = SelectedSemanticImageGet::decode(
            &continuation
                .encode()
                .expect("continuation image page encode"),
        )
        .expect("continuation image page decode");
        assert_eq!(continuation_roundtrip, continuation);
        let continuation_chunk = SelectedSemanticImageChunk {
            request_id: continuation.request_id,
            target: continuation.target.clone(),
            selected_stamp: continuation.selected_stamp,
            image: continuation.image,
            image_identity: identity,
            total_length: 128,
            byte_range: continuation.byte_range,
            payload: vec![7; 16],
        };
        let continuation_chunk = SelectedSemanticImageChunk::decode(
            &continuation_chunk
                .encode()
                .expect("continuation image reply encode"),
        )
        .expect("continuation image reply decode");
        continuation_chunk
            .validate_against(&continuation)
            .expect("continuation image identity remains exact");

        let mut crossed = continuation_chunk;
        crossed.image_identity = SemanticImageIdentity::from_encoded_bytes(b"another image");
        assert_eq!(
            crossed.validate_against(&continuation),
            Err(ReplicationError::IdentityMismatch)
        );
        let mut partial_tuple = continuation;
        partial_tuple.total_length = None;
        assert_eq!(partial_tuple.encode(), Err(ReplicationError::InvalidWire));
        let mut oversized = first_chunk;
        oversized.total_length = MAX_SEMANTIC_IMAGE_BYTES + 1;
        assert_eq!(oversized.encode(), Err(ReplicationError::InvalidWire));
    }
}
