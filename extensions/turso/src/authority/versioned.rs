//! Canonical metadata linking storage-neutral semantic segment identities to
//! the physical objects admitted in one compiler publication closure.

use backend_semantic::ir::{
    GenerationId, SemanticManifestError, SemanticManifestRoot, SemanticPlaneCatalog,
    SemanticPlaneCatalogEntry, SemanticPlaneCatalogRoot, SemanticPlaneImageKey,
    SemanticPlaneManifest, SemanticSegmentId, UntrustedSemanticSegmentId,
};
pub use backend_semantic::ir::{
    VERSIONED_PLANE_MANIFEST_SCHEMA, VERSIONED_PLANE_SEGMENT_SCHEMA, VersionedPlaneManifestSchema,
    VersionedPlaneSegmentSchema,
};
use backend_store::TypedObject;
use backend_version::ObjectKey;
use std::fmt;

const MAGIC: &[u8; 8] = b"TURVPM\0\0";
const VERSION: u16 = 3;
const MAX_SEGMENTS: usize = 100_000;
const MAX_ARTIFACTS: usize = 50_000;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_SEGMENT_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_CATALOG_RANGE_BYTES: usize = 16 * 1024;
const MEMBER_BYTES: usize = 32 + 32 + 8;

/// Physical CAS reference for one untrusted semantic segment claim reopened
/// from canonical metadata. The semantic claim becomes trusted only after its
/// payload has been checked with `SemanticPlaneSegment::admit`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionedPlaneMember {
    segment_id: UntrustedSemanticSegmentId,
    object_id: [u8; 32],
    byte_length: u64,
}

impl VersionedPlaneMember {
    /// Creates a reference from a checked logical ID and the exact typed CAS
    /// object which stores its bytes.
    pub fn from_object(
        segment_id: SemanticSegmentId,
        object: &TypedObject,
    ) -> Result<Self, VersionedPlaneError> {
        if object.schema() != VERSIONED_PLANE_SEGMENT_SCHEMA
            || object.bytes().is_empty()
            || object.bytes().len() > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES
        {
            return Err(VersionedPlaneError::SegmentObject);
        }
        Ok(Self {
            segment_id: UntrustedSemanticSegmentId::from_raw(*segment_id.as_bytes()),
            object_id: *object.id().as_bytes(),
            byte_length: u64::try_from(object.bytes().len())
                .map_err(|_| VersionedPlaneError::SegmentObject)?,
        })
    }

    /// Records a segment object already authenticated from the immutable CAS.
    pub fn from_verified_object(
        segment_id: SemanticSegmentId,
        object: backend_store::VerifiedObjectEnvelope,
    ) -> Result<Self, VersionedPlaneError> {
        if object.schema() != VERSIONED_PLANE_SEGMENT_SCHEMA
            || object.payload_len() == 0
            || object.payload_len() > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
        {
            return Err(VersionedPlaneError::SegmentObject);
        }
        Ok(Self {
            segment_id: UntrustedSemanticSegmentId::from_raw(*segment_id.as_bytes()),
            object_id: *object.id().as_bytes(),
            byte_length: object.payload_len(),
        })
    }

    /// Untrusted logical ID claim from the canonical plane manifest.
    #[must_use]
    pub const fn segment_id(&self) -> UntrustedSemanticSegmentId {
        self.segment_id
    }

    /// Physical CAS object ID. This value is local to the selected closure.
    #[must_use]
    pub const fn object_id(&self) -> &[u8; 32] {
        &self.object_id
    }

    /// Exact full segment payload length.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

/// Canonical per-image semantic manifest and its exact physical segment
/// references. Coverage bytes remain claims; decoding this record never
/// creates or restores an authority witness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedPlaneArtifactMetadata {
    image: SemanticPlaneImageKey,
    manifest_bytes: Box<[u8]>,
    manifest_root: [u8; 32],
    manifest_object_id: [u8; 32],
    members: Box<[VersionedPlaneMember]>,
}

impl VersionedPlaneArtifactMetadata {
    /// Validates that every manifest segment has exactly one logical-to-CAS
    /// reference with the same payload length. Payload identities are checked
    /// again from the actual closure bytes before the publication is selected.
    pub fn new(
        artifact_ordinal: u32,
        manifest_bytes: &[u8],
        mut members: Vec<VersionedPlaneMember>,
    ) -> Result<Self, VersionedPlaneError> {
        let manifest_object = manifest_object(manifest_bytes);
        Self::from_parts(
            artifact_ordinal,
            manifest_bytes,
            *manifest_object.id().as_bytes(),
            &mut members,
        )
    }

    /// Creates metadata from a manifest object already admitted by the CAS.
    /// The caller must keep the closure pin alive through outer publication.
    pub fn from_verified_parts(
        artifact_ordinal: u32,
        manifest_bytes: &[u8],
        verified_manifest_object: backend_store::VerifiedObjectEnvelope,
        mut members: Vec<VersionedPlaneMember>,
    ) -> Result<Self, VersionedPlaneError> {
        if verified_manifest_object.schema() != VERSIONED_PLANE_MANIFEST_SCHEMA
            || verified_manifest_object.payload_len()
                != u64::try_from(manifest_bytes.len())
                    .map_err(|_| VersionedPlaneError::ManifestLength)?
        {
            return Err(VersionedPlaneError::ManifestObject);
        }
        let expected = manifest_object(manifest_bytes);
        if verified_manifest_object.key() != expected.key()
            || verified_manifest_object.id().as_bytes() != expected.id().as_bytes()
        {
            return Err(VersionedPlaneError::ManifestObject);
        }
        Self::from_parts(
            artifact_ordinal,
            manifest_bytes,
            *verified_manifest_object.id().as_bytes(),
            &mut members,
        )
    }

    fn from_parts(
        artifact_ordinal: u32,
        manifest_bytes: &[u8],
        manifest_object_id: [u8; 32],
        members: &mut Vec<VersionedPlaneMember>,
    ) -> Result<Self, VersionedPlaneError> {
        let manifest = decode_manifest(manifest_bytes)?;
        let mut expected = Vec::new();
        for plane in manifest.planes() {
            for segment in plane.segments() {
                expected.push((segment.id_claim(), segment.byte_length()));
            }
        }
        if expected.is_empty() || expected.len() > MAX_SEGMENTS || expected.len() != members.len() {
            return Err(VersionedPlaneError::MemberCount);
        }
        members.sort_unstable_by_key(|member| *member.segment_id.as_bytes());
        if members
            .windows(2)
            .any(|pair| pair[0].segment_id == pair[1].segment_id)
        {
            return Err(VersionedPlaneError::DuplicateSegment);
        }
        expected.sort_unstable_by_key(|(id, _)| *id.as_bytes());
        for ((expected_id, expected_length), member) in expected.iter().zip(members.iter()) {
            if *expected_id != member.segment_id || *expected_length != member.byte_length {
                return Err(VersionedPlaneError::ManifestMemberMismatch);
            }
            if member.object_id == [0; 32] {
                return Err(VersionedPlaneError::SegmentObject);
            }
        }
        let manifest_root = *manifest.root().as_bytes();
        if manifest_object_id == [0; 32] {
            return Err(VersionedPlaneError::ManifestObject);
        }
        let image = SemanticPlaneImageKey::new(
            artifact_ordinal,
            manifest.semantic_generation(),
            manifest.root(),
        );
        Ok(Self {
            image,
            manifest_bytes: manifest_bytes.to_vec().into_boxed_slice(),
            manifest_root,
            manifest_object_id,
            members: std::mem::take(members).into_boxed_slice(),
        })
    }

    /// Exact storage-neutral image scope and manifest root.
    #[must_use]
    pub const fn image_key(&self) -> SemanticPlaneImageKey {
        self.image
    }

    /// Exact canonical semantic manifest bytes for this image.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// Canonical per-image manifest root.
    #[must_use]
    pub const fn manifest_root(&self) -> SemanticManifestRoot {
        SemanticManifestRoot::from_wire_claim(self.manifest_root)
    }

    /// Physical c005 manifest object included in the selected closure.
    #[must_use]
    pub const fn manifest_object_id(&self) -> &[u8; 32] {
        &self.manifest_object_id
    }

    /// Logical segment ID claims and their physical closure references in
    /// canonical logical-ID order.
    #[must_use]
    pub fn members(&self) -> &[VersionedPlaneMember] {
        &self.members
    }
}

/// Canonical set of per-image plane manifests selected by one compiler
/// publication. The catalog root is independent of physical CAS IDs and binds
/// each artifact ordinal, semantic generation, and per-image manifest root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedPlaneMetadata {
    artifacts: Box<[VersionedPlaneArtifactMetadata]>,
    catalog_root: SemanticPlaneCatalogRoot,
    catalog_bytes: Box<[u8]>,
    catalog_len: u64,
    segment_count: usize,
}

impl VersionedPlaneMetadata {
    /// Creates a one-image plane catalog for compatibility with single-image producers.
    pub fn new(
        manifest_bytes: &[u8],
        members: Vec<VersionedPlaneMember>,
    ) -> Result<Self, VersionedPlaneError> {
        Self::from_artifacts(vec![VersionedPlaneArtifactMetadata::new(
            0,
            manifest_bytes,
            members,
        )?])
    }

    /// Validates and stores every image's exact manifest and member refs.
    pub fn from_artifacts(
        mut artifacts: Vec<VersionedPlaneArtifactMetadata>,
    ) -> Result<Self, VersionedPlaneError> {
        if artifacts.is_empty() || artifacts.len() > MAX_ARTIFACTS {
            return Err(VersionedPlaneError::ArtifactCount);
        }
        artifacts.sort_unstable_by_key(|artifact| artifact.image.artifact_ordinal());
        if artifacts
            .windows(2)
            .any(|pair| pair[0].image.artifact_ordinal() == pair[1].image.artifact_ordinal())
        {
            return Err(VersionedPlaneError::DuplicateArtifactOrdinal);
        }
        let mut total_segments = 0usize;
        let mut total_manifest_bytes = 0usize;
        let mut total_segment_bytes = 0u64;
        let mut total_output_members = artifacts.len();
        let mut catalog_entries = Vec::with_capacity(artifacts.len());
        for (index, artifact) in artifacts.iter().enumerate() {
            if usize::try_from(artifact.image.artifact_ordinal()).ok() != Some(index) {
                return Err(VersionedPlaneError::NonContiguousArtifactOrdinal);
            }
            total_segments = total_segments
                .checked_add(artifact.members.len())
                .ok_or(VersionedPlaneError::MemberCount)?;
            if total_segments > MAX_SEGMENTS {
                return Err(VersionedPlaneError::MemberCount);
            }
            total_segment_bytes = artifact
                .members
                .iter()
                .try_fold(total_segment_bytes, |total, member| {
                    total.checked_add(member.byte_length)
                })
                .ok_or(VersionedPlaneError::PayloadBudget)?;
            if total_segment_bytes > MAX_TOTAL_SEGMENT_PAYLOAD_BYTES as u64 {
                return Err(VersionedPlaneError::PayloadBudget);
            }
            total_output_members = total_output_members
                .checked_add(artifact.members.len())
                .ok_or(VersionedPlaneError::MemberCount)?;
            if total_output_members > MAX_SEGMENTS {
                return Err(VersionedPlaneError::MemberCount);
            }
            total_manifest_bytes = total_manifest_bytes
                .checked_add(artifact.manifest_bytes.len())
                .ok_or(VersionedPlaneError::CatalogLength)?;
            if total_manifest_bytes > MAX_TOTAL_MANIFEST_BYTES {
                return Err(VersionedPlaneError::ManifestLength);
            }
            catalog_entries.push(SemanticPlaneCatalogEntry::new(
                artifact.image,
                u32::try_from(artifact.manifest_bytes.len())
                    .map_err(|_| VersionedPlaneError::ManifestLength)?,
            )?);
        }
        let catalog = SemanticPlaneCatalog::new(catalog_entries)?;
        let catalog_bytes = catalog.encode()?;
        let total_catalog_metadata = total_manifest_bytes
            .checked_add(catalog_bytes.len())
            .ok_or(VersionedPlaneError::CatalogLength)?;
        if catalog_bytes.len() > MAX_TOTAL_MANIFEST_BYTES
            || total_catalog_metadata > MAX_TOTAL_MANIFEST_BYTES
        {
            return Err(VersionedPlaneError::CatalogLength);
        }
        Ok(Self {
            artifacts: artifacts.into_boxed_slice(),
            catalog_root: catalog.root(),
            catalog_len: u64::try_from(catalog_bytes.len())
                .map_err(|_| VersionedPlaneError::CatalogLength)?,
            catalog_bytes,
            segment_count: total_segments,
        })
    }

    /// Reopens a complete canonical metadata record.
    pub fn decode(bytes: &[u8]) -> Result<Self, VersionedPlaneError> {
        let mut decoder = Decoder::new(bytes);
        if decoder.take(MAGIC.len())? != MAGIC {
            return Err(VersionedPlaneError::Magic);
        }
        if decoder.u16()? != VERSION {
            return Err(VersionedPlaneError::Version);
        }
        let catalog_root = SemanticPlaneCatalogRoot::from_wire_claim(decoder.array::<32>()?);
        let artifact_count =
            usize::try_from(decoder.u32()?).map_err(|_| VersionedPlaneError::ArtifactCount)?;
        if artifact_count == 0 || artifact_count > MAX_ARTIFACTS {
            return Err(VersionedPlaneError::ArtifactCount);
        }
        let mut artifacts = Vec::new();
        artifacts
            .try_reserve_exact(artifact_count)
            .map_err(|_| VersionedPlaneError::Allocation)?;
        let mut total_manifest_bytes = 0usize;
        let mut total_segments = 0usize;
        let mut total_output_members = artifact_count;
        let mut total_segment_bytes = 0u64;
        for _ in 0..artifact_count {
            let ordinal = decoder.u32()?;
            let generation = GenerationId::from_raw(decoder.array::<32>()?);
            let manifest_root = SemanticManifestRoot::from_wire_claim(decoder.array::<32>()?);
            let manifest_object_id = decoder.array::<32>()?;
            let manifest_length =
                usize::try_from(decoder.u32()?).map_err(|_| VersionedPlaneError::ManifestLength)?;
            if manifest_length == 0 || manifest_length > MAX_MANIFEST_BYTES {
                return Err(VersionedPlaneError::ManifestLength);
            }
            total_manifest_bytes = total_manifest_bytes
                .checked_add(manifest_length)
                .ok_or(VersionedPlaneError::ManifestLength)?;
            if total_manifest_bytes > MAX_TOTAL_MANIFEST_BYTES {
                return Err(VersionedPlaneError::ManifestLength);
            }
            let manifest_bytes = decoder.take(manifest_length)?.to_vec();
            let member_count =
                usize::try_from(decoder.u32()?).map_err(|_| VersionedPlaneError::MemberCount)?;
            if member_count == 0 || member_count > MAX_SEGMENTS {
                return Err(VersionedPlaneError::MemberCount);
            }
            total_segments = total_segments
                .checked_add(member_count)
                .ok_or(VersionedPlaneError::MemberCount)?;
            total_output_members = total_output_members
                .checked_add(member_count)
                .ok_or(VersionedPlaneError::MemberCount)?;
            if total_segments > MAX_SEGMENTS || total_output_members > MAX_SEGMENTS {
                return Err(VersionedPlaneError::MemberCount);
            }
            let mut members = Vec::new();
            members
                .try_reserve_exact(member_count)
                .map_err(|_| VersionedPlaneError::Allocation)?;
            for _ in 0..member_count {
                let member = VersionedPlaneMember {
                    segment_id: UntrustedSemanticSegmentId::from_raw(decoder.array::<32>()?),
                    object_id: decoder.array::<32>()?,
                    byte_length: decoder.u64()?,
                };
                if member.byte_length == 0
                    || member.byte_length > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
                {
                    return Err(VersionedPlaneError::SegmentObject);
                }
                total_segment_bytes = total_segment_bytes
                    .checked_add(member.byte_length)
                    .ok_or(VersionedPlaneError::PayloadBudget)?;
                if total_segment_bytes > MAX_TOTAL_SEGMENT_PAYLOAD_BYTES as u64 {
                    return Err(VersionedPlaneError::PayloadBudget);
                }
                members.push(member);
            }
            let artifact = VersionedPlaneArtifactMetadata::new(ordinal, &manifest_bytes, members)?;
            if artifact.image.semantic_generation() != generation
                || artifact.manifest_root() != manifest_root
            {
                return Err(VersionedPlaneError::ArtifactImage);
            }
            if artifact.manifest_object_id != manifest_object_id {
                return Err(VersionedPlaneError::ManifestObject);
            }
            artifacts.push(artifact);
        }
        if !decoder.is_empty() {
            return Err(VersionedPlaneError::TrailingBytes);
        }
        let metadata = Self::from_artifacts(artifacts)?;
        if metadata.catalog_root != catalog_root {
            return Err(VersionedPlaneError::CatalogRoot);
        }
        if metadata.canonical_bytes() != bytes {
            return Err(VersionedPlaneError::NonCanonical);
        }
        Ok(metadata)
    }

    /// Storage-neutral root of the full selected image-manifest catalog.
    #[must_use]
    pub const fn catalog_root(&self) -> SemanticPlaneCatalogRoot {
        self.catalog_root
    }

    /// Canonical page length of the logical image-manifest catalog.
    #[must_use]
    pub const fn catalog_len(&self) -> u64 {
        self.catalog_len
    }

    /// Per-image plane manifests in canonical artifact-ordinal order.
    #[must_use]
    pub fn artifacts(&self) -> &[VersionedPlaneArtifactMetadata] {
        &self.artifacts
    }

    /// Returns the exact selected per-image manifest and segment refs.
    #[must_use]
    pub fn artifact_for_image(
        &self,
        image: SemanticPlaneImageKey,
    ) -> Option<&VersionedPlaneArtifactMetadata> {
        let index = self
            .artifacts
            .binary_search_by_key(&image.artifact_ordinal(), |artifact| {
                artifact.image.artifact_ordinal()
            })
            .ok()?;
        let artifact = self.artifacts.get(index)?;
        (artifact.image == image).then_some(artifact)
    }

    /// Every logical-to-physical segment reference in canonical image/ID order.
    pub fn members(&self) -> impl Iterator<Item = &VersionedPlaneMember> {
        self.artifacts
            .iter()
            .flat_map(|artifact| artifact.members.iter())
    }

    /// Number of logical segment references in the selected catalog.
    #[must_use]
    pub const fn segment_count(&self) -> usize {
        self.segment_count
    }

    /// Streams a bounded range of a logical catalog. Physical CAS object IDs
    /// never appear in this client-facing representation.
    pub fn write_catalog_range(
        &self,
        offset: u64,
        output: &mut [u8],
    ) -> Result<(), VersionedPlaneError> {
        if output.is_empty() || output.len() > MAX_CATALOG_RANGE_BYTES {
            return Err(VersionedPlaneError::CatalogLength);
        }
        let end = offset
            .checked_add(
                u64::try_from(output.len()).map_err(|_| VersionedPlaneError::CatalogLength)?,
            )
            .ok_or(VersionedPlaneError::CatalogLength)?;
        if end > self.catalog_len {
            return Err(VersionedPlaneError::CatalogLength);
        }
        let start = usize::try_from(offset).map_err(|_| VersionedPlaneError::CatalogLength)?;
        let end = usize::try_from(end).map_err(|_| VersionedPlaneError::CatalogLength)?;
        output.copy_from_slice(
            self.catalog_bytes
                .get(start..end)
                .ok_or(VersionedPlaneError::CatalogLength)?,
        );
        Ok(())
    }

    /// Streams a bounded range of one exact image's canonical manifest.
    pub fn write_manifest_range(
        &self,
        image: SemanticPlaneImageKey,
        offset: u64,
        output: &mut [u8],
    ) -> Result<(), VersionedPlaneError> {
        if output.is_empty() || output.len() > MAX_CATALOG_RANGE_BYTES {
            return Err(VersionedPlaneError::CatalogLength);
        }
        let artifact = self
            .artifact_for_image(image)
            .ok_or(VersionedPlaneError::ArtifactImage)?;
        let end = offset
            .checked_add(
                u64::try_from(output.len()).map_err(|_| VersionedPlaneError::ManifestLength)?,
            )
            .ok_or(VersionedPlaneError::ManifestLength)?;
        if end
            > u64::try_from(artifact.manifest_bytes.len())
                .map_err(|_| VersionedPlaneError::ManifestLength)?
        {
            return Err(VersionedPlaneError::ManifestLength);
        }
        let start = usize::try_from(offset).map_err(|_| VersionedPlaneError::ManifestLength)?;
        let end = usize::try_from(end).map_err(|_| VersionedPlaneError::ManifestLength)?;
        output.copy_from_slice(
            artifact
                .manifest_bytes
                .get(start..end)
                .ok_or(VersionedPlaneError::ManifestLength)?,
        );
        Ok(())
    }

    /// Serializes the canonical owner-side metadata form, including CAS refs.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(
            MAGIC.len()
                + 2
                + 32
                + 4
                + self
                    .artifacts
                    .iter()
                    .map(artifact_encoded_len)
                    .sum::<usize>(),
        );
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&VERSION.to_le_bytes());
        output.extend_from_slice(self.catalog_root.as_bytes());
        output.extend_from_slice(
            &u32::try_from(self.artifacts.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for artifact in &self.artifacts {
            output.extend_from_slice(&artifact.image.artifact_ordinal().to_le_bytes());
            output.extend_from_slice(artifact.image.semantic_generation().as_bytes());
            output.extend_from_slice(artifact.image.manifest_root().as_bytes());
            output.extend_from_slice(&artifact.manifest_object_id);
            output.extend_from_slice(
                &u32::try_from(artifact.manifest_bytes.len())
                    .unwrap_or(u32::MAX)
                    .to_le_bytes(),
            );
            output.extend_from_slice(&artifact.manifest_bytes);
            output.extend_from_slice(
                &u32::try_from(artifact.members.len())
                    .unwrap_or(u32::MAX)
                    .to_le_bytes(),
            );
            for member in &artifact.members {
                encode_member(member, &mut output);
            }
        }
        output
    }

    /// Distinct physical member identities that the outer compiler envelope
    /// must list. Several logical segments may share identical payload bytes.
    #[must_use]
    pub fn object_ids(&self) -> Vec<[u8; 32]> {
        let mut ids = Vec::with_capacity(self.segment_count + self.artifacts.len());
        ids.extend(
            self.artifacts
                .iter()
                .map(|artifact| artifact.manifest_object_id),
        );
        ids.extend(self.members().map(|member| member.object_id));
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

fn artifact_encoded_len(artifact: &VersionedPlaneArtifactMetadata) -> usize {
    4 + 32 + 32 + 32 + 4 + artifact.manifest_bytes.len() + 4 + artifact.members.len() * MEMBER_BYTES
}

fn manifest_object(manifest_bytes: &[u8]) -> TypedObject {
    let key = ObjectKey::<VersionedPlaneManifestSchema>::from_value(manifest_bytes);
    TypedObject::from_value(&key, manifest_bytes)
}

fn encode_member(member: &VersionedPlaneMember, output: &mut Vec<u8>) {
    output.extend_from_slice(member.segment_id.as_bytes());
    output.extend_from_slice(&member.object_id);
    output.extend_from_slice(&member.byte_length.to_le_bytes());
}

/// Publication-side bundle of validated metadata and the segment CAS objects
/// it references. It is not an authority capability and carries no coverage
/// witness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedPlanePublication {
    metadata: VersionedPlaneMetadata,
    objects: Box<[TypedObject]>,
}

impl VersionedPlanePublication {
    /// Checks every producer payload against the canonical manifest and builds
    /// its typed CAS object without changing the logical segment identity.
    pub fn from_payloads<'a>(
        manifest_bytes: &[u8],
        payloads: impl IntoIterator<Item = (SemanticSegmentId, &'a [u8])>,
    ) -> Result<Self, VersionedPlaneError> {
        Self::from_payloads_with_budget(manifest_bytes, payloads, MAX_TOTAL_SEGMENT_PAYLOAD_BYTES)
    }

    fn from_payloads_with_budget<'a>(
        manifest_bytes: &[u8],
        payloads: impl IntoIterator<Item = (SemanticSegmentId, &'a [u8])>,
        max_total_payload_bytes: usize,
    ) -> Result<Self, VersionedPlaneError> {
        let manifest = decode_manifest(manifest_bytes)?;
        let mut expected = std::collections::BTreeMap::new();
        for plane in manifest.planes() {
            for segment in plane.segments() {
                expected.insert(*segment.id_claim().as_bytes(), (plane.kind(), *segment));
            }
        }
        if expected.is_empty() || expected.len() > MAX_SEGMENTS {
            return Err(VersionedPlaneError::MemberCount);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut members = Vec::with_capacity(expected.len());
        let mut objects = Vec::with_capacity(expected.len().saturating_add(1));
        objects.push(manifest_object(manifest_bytes));
        let mut total_payload_bytes = 0usize;
        for (id, payload) in payloads {
            if payload.is_empty()
                || payload.len() > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES
            {
                return Err(VersionedPlaneError::SegmentObject);
            }
            let id_bytes = *id.as_bytes();
            let (kind, segment) = expected
                .get(&id_bytes)
                .copied()
                .ok_or(VersionedPlaneError::UnexpectedSegment)?;
            if !seen.insert(id_bytes) || segment.admit(kind, payload)? != id {
                return Err(VersionedPlaneError::DuplicateSegment);
            }
            total_payload_bytes = total_payload_bytes
                .checked_add(payload.len())
                .filter(|total| *total <= max_total_payload_bytes)
                .ok_or(VersionedPlaneError::PayloadBudget)?;
            let value = payload.to_vec();
            let key = ObjectKey::<VersionedPlaneSegmentSchema>::from_value(&value);
            let object = TypedObject::from_value(&key, &value);
            members.push(VersionedPlaneMember::from_object(id, &object)?);
            objects.push(object);
        }
        if seen.len() != expected.len() {
            return Err(VersionedPlaneError::MissingSegment);
        }
        let metadata = VersionedPlaneMetadata::new(manifest_bytes, members)?;
        objects.sort_by_key(|object| *object.id().as_bytes());
        objects.dedup_by_key(|object| *object.id().as_bytes());
        Ok(Self {
            metadata,
            objects: objects.into_boxed_slice(),
        })
    }

    /// Builds a publication from members whose typed CAS envelopes were
    /// already authenticated by a bounded FileStore stream. Segment bytes are
    /// not cloned or retained in this wrapper.
    pub fn from_verified_parts(
        artifacts: Vec<VersionedPlaneArtifactMetadata>,
    ) -> Result<Self, VersionedPlaneError> {
        let metadata = VersionedPlaneMetadata::from_artifacts(artifacts)?;
        Ok(Self {
            metadata,
            objects: Box::new([]),
        })
    }

    /// Canonical logical-to-physical refs included in compiler metadata.
    #[must_use]
    pub const fn metadata(&self) -> &VersionedPlaneMetadata {
        &self.metadata
    }

    /// Distinct typed segment payload objects to include in the exact outer
    /// closure.
    #[must_use]
    pub fn objects(&self) -> &[TypedObject] {
        &self.objects
    }
}

fn decode_manifest(bytes: &[u8]) -> Result<SemanticPlaneManifest, VersionedPlaneError> {
    if bytes.is_empty() || bytes.len() > MAX_MANIFEST_BYTES {
        return Err(VersionedPlaneError::ManifestLength);
    }
    let manifest = SemanticPlaneManifest::decode(bytes)?;
    if manifest.encode()? != bytes {
        return Err(VersionedPlaneError::NonCanonical);
    }
    Ok(manifest)
}

/// Malformed versioned-plane metadata or object inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionedPlaneError {
    /// Manifest wire is malformed or violates the semantic manifest grammar.
    Manifest(SemanticManifestError),
    /// Metadata magic is wrong.
    Magic,
    /// Metadata version is unsupported.
    Version,
    /// Canonical manifest length is absent or exceeds its bound.
    ManifestLength,
    /// Embedded root does not match the decoded manifest.
    ManifestRoot,
    /// Segment list is empty, incomplete, or exceeds its bound.
    MemberCount,
    /// Artifact image catalog is empty or exceeds its aggregate bound.
    ArtifactCount,
    /// Artifact ordinals are duplicated.
    DuplicateArtifactOrdinal,
    /// Image ordinal, generation, or manifest root differs from canonical bytes.
    ArtifactImage,
    /// Canonical compiler publication ordinals must be exactly `0..image_count`.
    NonContiguousArtifactOrdinal,
    /// Aggregate catalog root differs from its canonical image records.
    CatalogRoot,
    /// Catalog range or canonical catalog length is invalid.
    CatalogLength,
    /// Segment logical IDs are duplicated.
    DuplicateSegment,
    /// Member ref does not exactly match one manifest segment claim.
    ManifestMemberMismatch,
    /// Segment object has the wrong schema or byte length.
    SegmentObject,
    /// Canonical manifest CAS object is absent or differs from its bytes.
    ManifestObject,
    /// Payload ID is not declared by the selected manifest.
    UnexpectedSegment,
    /// One manifest segment has no payload object.
    MissingSegment,
    /// Retained segment payload objects exceed the bounded publication budget.
    PayloadBudget,
    /// Input has bytes after the canonical value.
    TrailingBytes,
    /// Input is truncated or a length exceeds its lane limit.
    Truncated,
    /// Allocation could not be bounded.
    Allocation,
    /// Valid fields do not use their unique canonical encoding.
    NonCanonical,
}

impl From<SemanticManifestError> for VersionedPlaneError {
    fn from(error: SemanticManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl fmt::Display for VersionedPlaneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid versioned semantic plane metadata: {self:?}"
        )
    }
}

impl std::error::Error for VersionedPlaneError {}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], VersionedPlaneError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(VersionedPlaneError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(VersionedPlaneError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16, VersionedPlaneError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| VersionedPlaneError::Truncated)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, VersionedPlaneError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| VersionedPlaneError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, VersionedPlaneError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| VersionedPlaneError::Truncated)?,
        ))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], VersionedPlaneError> {
        self.take(N)?
            .try_into()
            .map_err(|_| VersionedPlaneError::Truncated)
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use backend_semantic::ir::{
        EmbeddingNormalization, EmbeddingPlaneIdentity, GenerationId, LanguageProfile, RustEdition,
        SemanticBuildIdentity, SemanticInputWitness, SemanticIrPlane, SemanticPlane,
        SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneSegment,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_version::{Coverage, ScopeRoot};

    fn manifest_and_payloads() -> (Vec<u8>, Vec<(SemanticSegmentId, Vec<u8>)>) {
        let ir_kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let embedding_kind = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [21; 32],
                [22; 32],
                [25; 32],
                3,
                EmbeddingNormalization::L2,
                [23; 32],
                [24; 32],
            )
            .expect("embedding dimension is nonzero"),
        );
        let ir_bytes = b"ir-segment:asymmetric-core".to_vec();
        let embedding_bytes = b"embedding-segment:asymmetric-vector".to_vec();
        let ir_segment =
            SemanticPlaneSegment::from_payload(ir_kind, [0x31; 32], [0x32; 32], 1, &ir_bytes)
                .expect("IR segment");
        let embedding_segment = SemanticPlaneSegment::from_payload(
            embedding_kind,
            [0x41; 32],
            [0x42; 32],
            1,
            &embedding_bytes,
        )
        .expect("embedding segment");
        let manifest = SemanticPlaneManifest::new(
            GenerationId::from_canonical_bytes(b"fixture canonical semantic image"),
            SemanticBuildIdentity::new(
                [1; 32],
                [2; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
                [6; 32],
            ),
            SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32])),
            vec![
                SemanticPlane::claimed(ir_kind, vec![ir_segment], Coverage::Partial)
                    .expect("claim-only IR plane"),
                SemanticPlane::claimed(embedding_kind, vec![embedding_segment], Coverage::Partial)
                    .expect("claim-only embedding plane"),
            ],
        )
        .expect("canonical manifest");
        let manifest_bytes = manifest.encode().expect("manifest encoding");
        let payloads = vec![
            (
                ir_segment.admit(ir_kind, &ir_bytes).expect("IR ID"),
                ir_bytes,
            ),
            (
                embedding_segment
                    .admit(embedding_kind, &embedding_bytes)
                    .expect("embedding ID"),
                embedding_bytes,
            ),
        ];
        (manifest_bytes, payloads)
    }

    #[test]
    fn publication_keeps_embedding_independent_and_reopens_exact_refs() {
        let (manifest_bytes, payloads) = manifest_and_payloads();
        let borrowed = payloads
            .iter()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
            .collect::<Vec<_>>();
        let publication = VersionedPlanePublication::from_payloads(&manifest_bytes, borrowed)
            .expect("both plane payloads admitted");
        let manifest =
            SemanticPlaneManifest::decode(&manifest_bytes).expect("canonical manifest reopens");
        assert_eq!(manifest.planes().len(), 2);
        assert!(
            manifest
                .planes()
                .iter()
                .any(|plane| matches!(plane.kind(), SemanticPlaneKind::Embeddings(_)))
        );
        assert!(!manifest.claims_admitted());
        assert_eq!(publication.objects().len(), 3);

        let reopened = VersionedPlaneMetadata::decode(&publication.metadata().canonical_bytes())
            .expect("canonical physical refs reopen");
        assert_eq!(reopened, *publication.metadata());
        assert_eq!(reopened.members().count(), 2);
        assert_eq!(reopened.object_ids().len(), 3);
        let catalog = SemanticPlaneCatalog::decode(&reopened.catalog_bytes)
            .expect("canonical aggregate catalog reopens");
        assert_eq!(catalog.root(), reopened.catalog_root());
        assert_eq!(
            catalog.encode().expect("catalog bytes"),
            reopened.catalog_bytes
        );
        assert_eq!(catalog.entries().len(), 1);
        assert_eq!(
            catalog.entries()[0].image(),
            reopened.artifacts()[0].image_key()
        );
        let canonical_metadata = publication.metadata().canonical_bytes();
        assert_eq!(
            reopened.canonical_bytes(),
            canonical_metadata,
            "metadata must reopen byte-for-byte",
        );
        assert_eq!(
            VersionedPlaneMetadata::decode(&canonical_metadata)
                .expect("canonical physical refs roundtrip")
                .canonical_bytes(),
            canonical_metadata,
        );
    }

    #[test]
    fn catalog_rejects_a_missing_canonical_artifact_ordinal() {
        let (manifest_bytes, payloads) = manifest_and_payloads();
        let members = payloads
            .iter()
            .map(|(id, bytes)| {
                let object = TypedObject::from_value(
                    &ObjectKey::<VersionedPlaneSegmentSchema>::from_value(bytes),
                    bytes,
                );
                VersionedPlaneMember::from_object(*id, &object).expect("typed segment member")
            })
            .collect::<Vec<_>>();
        let first = VersionedPlaneArtifactMetadata::new(0, &manifest_bytes, members.clone())
            .expect("ordinal zero metadata");
        let third = VersionedPlaneArtifactMetadata::new(2, &manifest_bytes, members)
            .expect("ordinal two metadata");
        assert_eq!(
            VersionedPlaneMetadata::from_artifacts(vec![first, third]),
            Err(VersionedPlaneError::NonContiguousArtifactOrdinal),
        );
    }

    #[test]
    fn artifact_metadata_rejects_duplicate_logical_segment_ids() {
        let (manifest_bytes, payloads) = manifest_and_payloads();
        let members = payloads
            .iter()
            .map(|(id, bytes)| {
                let object = TypedObject::from_value(
                    &ObjectKey::<VersionedPlaneSegmentSchema>::from_value(bytes),
                    bytes,
                );
                VersionedPlaneMember::from_object(*id, &object).expect("typed segment member")
            })
            .collect::<Vec<_>>();
        let duplicate_members = vec![members[0], members[0]];
        assert_eq!(
            VersionedPlaneArtifactMetadata::new(0, &manifest_bytes, duplicate_members),
            Err(VersionedPlaneError::DuplicateSegment),
        );

        let borrowed = payloads
            .iter()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
            .collect::<Vec<_>>();
        let mut metadata = VersionedPlanePublication::from_payloads(&manifest_bytes, borrowed)
            .expect("valid two-segment publication")
            .metadata()
            .canonical_bytes();
        let member_count_offset = 46 + 4 + 32 + 32 + 32 + 4 + manifest_bytes.len();
        assert_eq!(
            u32::from_le_bytes(
                metadata[member_count_offset..member_count_offset + 4]
                    .try_into()
                    .expect("member count bytes"),
            ),
            2,
        );
        let members_offset = member_count_offset + 4;
        let first_id = metadata[members_offset..members_offset + 32].to_vec();
        let second_id_offset = members_offset + MEMBER_BYTES;
        metadata[second_id_offset..second_id_offset + 32].copy_from_slice(&first_id);
        assert_eq!(
            VersionedPlaneMetadata::decode(&metadata),
            Err(VersionedPlaneError::DuplicateSegment),
        );
    }

    #[test]
    fn catalog_bounds_many_images_and_aggregate_member_inventory() {
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let payload = b"one bounded segment";
        let segment = SemanticPlaneSegment::from_payload(kind, [0x71; 32], [0x72; 32], 1, payload)
            .expect("one checked segment");
        let manifest = SemanticPlaneManifest::new(
            GenerationId::from_canonical_bytes(b"one image for catalog bound"),
            SemanticBuildIdentity::new(
                [0x11; 32],
                [0x12; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                Stage::LowerIr,
                [0x13; 32],
                [0x14; 32],
                [0x15; 32],
                [0x16; 32],
            ),
            SemanticInputWitness::claimed([0x17; 32], ScopeRoot::from_bytes([0x18; 32])),
            vec![
                SemanticPlane::claimed(kind, vec![segment], Coverage::Partial)
                    .expect("one partial plane"),
            ],
        )
        .expect("one canonical image manifest");
        let manifest_bytes = manifest.encode().expect("encode bounded manifest");
        let segment_id = segment.admit(kind, payload).expect("exact segment ID");
        let publication = VersionedPlanePublication::from_payloads(
            &manifest_bytes,
            [(segment_id, payload.as_slice())],
        )
        .expect("one exact plane artifact");
        let prototype = publication.metadata().artifacts()[0].clone();
        let mut artifacts = Vec::with_capacity(MAX_ARTIFACTS);
        for ordinal in 0..MAX_ARTIFACTS {
            let mut artifact = prototype.clone();
            artifact.image = SemanticPlaneImageKey::new(
                u32::try_from(ordinal).expect("bounded image ordinal"),
                prototype.image.semantic_generation(),
                prototype.image.manifest_root(),
            );
            artifacts.push(artifact);
        }
        let metadata = VersionedPlaneMetadata::from_artifacts(artifacts)
            .expect("maximum bounded image catalog remains representable");
        assert_eq!(metadata.artifacts().len(), MAX_ARTIFACTS);
        assert_eq!(metadata.segment_count(), MAX_ARTIFACTS);
        assert_eq!(metadata.object_ids().len(), 2);
        assert!(metadata.catalog_len() <= MAX_TOTAL_MANIFEST_BYTES as u64);
        assert!(metadata.canonical_bytes().len() <= 80 * 1024 * 1024);
    }

    #[test]
    fn publication_rejects_missing_or_corrupt_auxiliary_plane_payload() {
        let (manifest_bytes, payloads) = manifest_and_payloads();
        assert_eq!(
            VersionedPlanePublication::from_payloads(
                &manifest_bytes,
                [(payloads[0].0, payloads[0].1.as_slice())],
            ),
            Err(VersionedPlaneError::MissingSegment),
        );
        let corrupt = b"corrupt-embedding";
        assert!(matches!(
            VersionedPlanePublication::from_payloads(
                &manifest_bytes,
                [
                    (payloads[0].0, payloads[0].1.as_slice()),
                    (payloads[1].0, corrupt.as_slice()),
                ],
            ),
            Err(VersionedPlaneError::Manifest(_)),
        ));
    }

    #[test]
    fn publication_bounds_total_retained_segment_payloads() {
        let (manifest_bytes, payloads) = manifest_and_payloads();
        let borrowed = payloads
            .iter()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
            .collect::<Vec<_>>();
        let total = payloads.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
        assert_eq!(
            VersionedPlanePublication::from_payloads_with_budget(
                &manifest_bytes,
                borrowed,
                total - 1,
            ),
            Err(VersionedPlaneError::PayloadBudget),
        );
    }

    #[test]
    fn manifest_metadata_rejects_dropped_member_on_cold_decode() {
        let (manifest_bytes, payloads) = manifest_and_payloads();
        let borrowed = payloads
            .iter()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
            .collect::<Vec<_>>();
        let publication = VersionedPlanePublication::from_payloads(&manifest_bytes, borrowed)
            .expect("both plane payloads admitted");
        let mut bytes = publication.metadata().canonical_bytes();
        bytes.truncate(bytes.len() - MEMBER_BYTES);
        assert_eq!(
            VersionedPlaneMetadata::decode(&bytes),
            Err(VersionedPlaneError::Truncated),
        );
    }
}
