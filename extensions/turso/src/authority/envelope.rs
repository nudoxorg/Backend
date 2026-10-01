//! Canonical compiler publication envelope stored inside an immutable closure.
//!
//! The envelope commits to the exact compiler attempt, logical generation,
//! physical inner pack, and its sorted object members. It deliberately omits
//! the outer `ClosureId`: that ID is derived from a closure containing this
//! envelope and would create a content-addressing cycle if embedded here.

use super::types::{
    AuthorityHash, AuthorityNamespace, AuthorityPlane, CandidateAttempt, CandidateGeneration,
    ClosureClaim, DurableClosureVerifier, SelectedGeneration,
};
use super::versioned::{
    VERSIONED_PLANE_MANIFEST_SCHEMA, VERSIONED_PLANE_SEGMENT_SCHEMA, VersionedPlaneError,
    VersionedPlaneMetadata,
};
#[cfg(test)]
use super::versioned::{VersionedPlaneArtifactMetadata, VersionedPlanePublication};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, FileStore, TypedObject,
    UntrustedObjectId, VerifiedObjectEnvelope,
};
use backend_version::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, ContentId,
    DependencySetDomain, GenerationId, IrManifestDomain, IrManifestEncoding, IrSemanticImageDomain,
    IrSemanticImageEncoding, ObjectKey, Schema, SchemaIdentity,
};
use std::fmt;

const MAGIC: &[u8] = b"BACKEND_COMPILER_PUBLICATION_ENVELOPE\0";
const ENVELOPE_VERSION: u16 = 4;
const METADATA_MAGIC: &[u8] = b"BACKEND_COMPILER_PUBLICATION_METADATA\0";
const METADATA_VERSION: u16 = 4;
const MAX_PROFILE_BYTES: usize = 256;
const MAX_MEMBER_IDS: usize = 100_000;
const MAX_MANIFEST_BYTES: usize = 16 + MAX_MEMBER_IDS * 440;
const PACK_DOMAIN: &[u8] = b"backend.turso.compiler-inner-pack.v1\0";
const COMPILATION_BINDING_BYTES: usize = 108;
const SEMANTIC_MANIFEST_HEADER_BYTES: usize = 16;
const SEMANTIC_MANIFEST_ENTRY_BYTES: usize = 440;
const SEMANTIC_MANIFEST_VERSION: u16 = 2;
const SEMANTIC_MANIFEST_MAGIC: &[u8; 8] = b"NUDXCPM\0";
const SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET: usize = 404;
const SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET: usize = 436;

type ManifestIdentity = ArtifactId<IrManifestEncoding, IrManifestDomain>;
type BindingIdentity = ArtifactId<CompilePublicationEncoding, CompilePublicationDomain>;
type SemanticImageIdentity = ArtifactId<IrSemanticImageEncoding, IrSemanticImageDomain>;

/// Schema identity for the typed immutable compiler publication envelope.
pub const COMPILER_PUBLICATION_ENVELOPE_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc001, 4);
/// Schema identity for canonical compiler publication metadata.
pub const COMPILER_PUBLICATION_METADATA_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc002, 4);
/// Schema identity for one complete semantic image in the selected publication pack.
pub const COMPILER_SEMANTIC_IMAGE_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc003, 1);

/// One semantic image's FileStore identity and compiler authority facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerImageMember {
    artifact_ordinal: u32,
    object_id: AuthorityHash,
    semantic_image_identity: AuthorityHash,
    byte_length: u32,
}

impl CompilerImageMember {
    /// Wraps a complete semantic image object after checking its typed content identity.
    pub fn from_bytes(
        bytes: &[u8],
        expected_identity: AuthorityHash,
    ) -> Result<(Self, TypedObject), CompilerEnvelopeError> {
        Self::from_bytes_for_ordinal(0, bytes, expected_identity)
    }

    /// Wraps one canonical compiler image at its stable manifest ordinal.
    pub fn from_bytes_for_ordinal(
        artifact_ordinal: u32,
        bytes: &[u8],
        expected_identity: AuthorityHash,
    ) -> Result<(Self, TypedObject), CompilerEnvelopeError> {
        let image_identity = SemanticImageIdentity::from_encoded_bytes(bytes);
        if *image_identity.as_ref() != expected_identity {
            return Err(CompilerEnvelopeError::ImageIdentity);
        }
        let byte_length =
            u32::try_from(bytes.len()).map_err(|_| CompilerEnvelopeError::ByteLengthOverflow)?;
        if byte_length == 0 {
            return Err(CompilerEnvelopeError::ImageLength);
        }
        let key = ObjectKey::<CompilerSemanticImageSchema>::from_value(bytes);
        let object = TypedObject::from_value(&key, bytes);
        Ok((
            Self {
                artifact_ordinal,
                object_id: *object.id().as_bytes(),
                semantic_image_identity: expected_identity,
                byte_length,
            },
            object,
        ))
    }

    /// Records an image already admitted by the local object CAS without
    /// materializing its payload. The compiler image identity remains a
    /// claim here; [`FileStoreCompilerPublicationVerifier`] recomputes it from
    /// the exact closure payload before Turso selection.
    pub fn from_verified_object(
        artifact_ordinal: u32,
        object: VerifiedObjectEnvelope,
        expected_identity: AuthorityHash,
    ) -> Result<Self, CompilerEnvelopeError> {
        if object.schema() != COMPILER_SEMANTIC_IMAGE_SCHEMA {
            return Err(CompilerEnvelopeError::ImageIdentity);
        }
        let byte_length = u32::try_from(object.payload_len())
            .map_err(|_| CompilerEnvelopeError::ByteLengthOverflow)?;
        if byte_length == 0 {
            return Err(CompilerEnvelopeError::ImageLength);
        }
        SemanticImageIdentity::try_from(expected_identity)
            .map_err(|_| CompilerEnvelopeError::ImageIdentity)?;
        Ok(Self {
            artifact_ordinal,
            object_id: *object.id().as_bytes(),
            semantic_image_identity: expected_identity,
            byte_length,
        })
    }

    /// Stable ordinal from the canonical compiler publication manifest.
    #[must_use]
    pub const fn artifact_ordinal(&self) -> u32 {
        self.artifact_ordinal
    }

    /// FileStore content-addressed object ID.
    #[must_use]
    pub const fn object_id(&self) -> &AuthorityHash {
        &self.object_id
    }

    /// Compiler's typed identity for the complete canonical semantic image.
    #[must_use]
    pub const fn semantic_image_identity(&self) -> &AuthorityHash {
        &self.semantic_image_identity
    }

    /// Exact canonical semantic image byte length.
    #[must_use]
    pub const fn byte_length(&self) -> u32 {
        self.byte_length
    }
}

/// Exact semantic-image inventory entry and owned bytes reopened from a closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReopenedCompilerImage {
    member: CompilerImageMember,
    bytes: Box<[u8]>,
}

impl ReopenedCompilerImage {
    /// Exact metadata inventory entry for these bytes.
    #[must_use]
    pub const fn member(&self) -> &CompilerImageMember {
        &self.member
    }

    /// Exact canonical image bytes copied from the verified FileStore closure.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Compiler facts needed to reconstruct a semantic publication after restart.
///
/// This is a preservation record for facts already admitted by the compiler
/// owner. It carries the exact canonical semantic manifest and binding bytes,
/// plus the exact FileStore identities of each semantic image, so a cold reader
/// can rebuild compiler publication facts without the old ProductState.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerPublicationMetadata {
    manifest_bytes: Box<[u8]>,
    manifest_identity: AuthorityHash,
    manifest_fragment_count: u32,
    manifest_byte_length: u32,
    binding_bytes: [u8; COMPILATION_BINDING_BYTES],
    binding_identity: AuthorityHash,
    pinned_root: AuthorityHash,
    dependency_set: AuthorityHash,
    images: Box<[CompilerImageMember]>,
    versioned_planes: Option<VersionedPlaneMetadata>,
    semantic_catalog_root: Option<AuthorityHash>,
}

impl CompilerPublicationMetadata {
    /// Admits the exact semantic-v2 manifest, canonical binding bytes, and image members.
    pub fn new(
        manifest_bytes: &[u8],
        binding_bytes: [u8; COMPILATION_BINDING_BYTES],
        images: Vec<CompilerImageMember>,
    ) -> Result<Self, CompilerEnvelopeError> {
        Self::new_with_versioned_planes(manifest_bytes, binding_bytes, images, None)
    }

    /// Admits the compiler manifest, image inventory, and optional complete
    /// versioned semantic-plane metadata. Each listed auxiliary segment is
    /// independently rechecked against its exact closure payload before the
    /// candidate may be selected.
    pub fn new_with_versioned_planes(
        manifest_bytes: &[u8],
        binding_bytes: [u8; COMPILATION_BINDING_BYTES],
        mut images: Vec<CompilerImageMember>,
        versioned_planes: Option<VersionedPlaneMetadata>,
    ) -> Result<Self, CompilerEnvelopeError> {
        let (manifest_identity, manifest_fragment_count, manifest_byte_length) =
            inspect_semantic_manifest(manifest_bytes)?;
        let binding_identity = validate_binding(&binding_bytes, &manifest_identity)?;
        for image in &images {
            SemanticImageIdentity::try_from(image.semantic_image_identity)
                .map_err(|_| CompilerEnvelopeError::ImageIdentity)?;
            if image.byte_length == 0 {
                return Err(CompilerEnvelopeError::ImageLength);
            }
        }
        images.sort_unstable_by_key(|image| image.object_id);
        if images
            .windows(2)
            .any(|pair| pair[0].object_id == pair[1].object_id)
        {
            return Err(CompilerEnvelopeError::DuplicateMember);
        }
        let mut ordinals = images
            .iter()
            .map(|image| image.artifact_ordinal)
            .collect::<Vec<_>>();
        ordinals.sort_unstable();
        if ordinals
            .iter()
            .enumerate()
            .any(|(expected, actual)| usize::try_from(*actual).ok() != Some(expected))
        {
            return Err(CompilerEnvelopeError::MemberOrder);
        }
        if metadata_member_count(images.len(), versioned_planes.as_ref())? > MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        verify_manifest_image_inventory(manifest_bytes, &images)?;
        if let Some(planes) = &versioned_planes {
            if planes.artifacts().len() != images.len()
                || planes
                    .artifacts()
                    .iter()
                    .enumerate()
                    .any(|(ordinal, artifact)| {
                        usize::try_from(artifact.image_key().artifact_ordinal()).ok()
                            != Some(ordinal)
                    })
            {
                return Err(CompilerEnvelopeError::VersionedPlaneMetadata);
            }
        }
        let pinned_root = binding_bytes[12..44]
            .try_into()
            .map_err(|_| CompilerEnvelopeError::Binding)?;
        let dependency_set = binding_bytes[44..76]
            .try_into()
            .map_err(|_| CompilerEnvelopeError::Binding)?;
        GenerationId::try_from(pinned_root).map_err(|_| CompilerEnvelopeError::Binding)?;
        ContentId::<DependencySetDomain>::try_from(dependency_set)
            .map_err(|_| CompilerEnvelopeError::Binding)?;
        Ok(Self {
            manifest_bytes: manifest_bytes.to_vec().into_boxed_slice(),
            manifest_identity,
            manifest_fragment_count,
            manifest_byte_length,
            binding_bytes,
            binding_identity,
            pinned_root,
            dependency_set,
            images: images.into_boxed_slice(),
            semantic_catalog_root: versioned_planes
                .as_ref()
                .map(|planes| *planes.catalog_root().as_bytes()),
            versioned_planes,
        })
    }

    /// Typed compiler manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> &AuthorityHash {
        &self.manifest_identity
    }

    /// Semantic-v2 manifest format version.
    #[must_use]
    pub const fn manifest_version(&self) -> u16 {
        SEMANTIC_MANIFEST_VERSION
    }

    /// Exact compiler artifact count in the semantic manifest.
    #[must_use]
    pub const fn manifest_fragment_count(&self) -> u32 {
        self.manifest_fragment_count
    }

    /// Exact canonical manifest byte length.
    #[must_use]
    pub const fn manifest_byte_length(&self) -> u32 {
        self.manifest_byte_length
    }

    /// Canonical generation-to-manifest binding bytes.
    #[must_use]
    pub const fn binding_bytes(&self) -> &[u8; COMPILATION_BINDING_BYTES] {
        &self.binding_bytes
    }

    /// Typed compiler generation binding identity.
    #[must_use]
    pub const fn binding_identity(&self) -> &AuthorityHash {
        &self.binding_identity
    }

    /// Exact pinned compiler root named by the generation binding.
    #[must_use]
    pub const fn pinned_root(&self) -> &AuthorityHash {
        &self.pinned_root
    }

    /// Exact dependency-set identity named by the generation binding.
    #[must_use]
    pub const fn dependency_set(&self) -> &AuthorityHash {
        &self.dependency_set
    }

    /// Exact semantic images covered by the compiler manifest.
    #[must_use]
    pub fn images(&self) -> &[CompilerImageMember] {
        &self.images
    }

    /// Canonical versioned IR/embedding manifest and physical segment refs,
    /// when this compiler generation publishes independent planes.
    #[must_use]
    pub fn versioned_planes(&self) -> Option<&VersionedPlaneMetadata> {
        self.versioned_planes.as_ref()
    }

    /// Aggregate catalog root for every exact image manifest in this record.
    #[must_use]
    pub const fn semantic_catalog_root(&self) -> Option<&AuthorityHash> {
        self.semantic_catalog_root.as_ref()
    }

    /// Compatibility alias for the old singular field name; the value is the
    /// aggregate catalog root, never an individual image manifest root.
    #[must_use]
    pub const fn semantic_manifest_root(&self) -> Option<&AuthorityHash> {
        self.semantic_catalog_root()
    }

    /// Exact canonical semantic-v2 manifest bytes bound by this publication.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// Returns canonical, versioned metadata bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(
            METADATA_MAGIC.len()
                + 2
                + 4
                + self.manifest_bytes.len()
                + COMPILATION_BINDING_BYTES
                + 4
                + self.images.len() * 72
                + 1
                + self
                    .versioned_planes
                    .as_ref()
                    .map_or(0, |planes| 4 + planes.canonical_bytes().len()),
        );
        output.extend_from_slice(METADATA_MAGIC);
        output.extend_from_slice(&METADATA_VERSION.to_le_bytes());
        let manifest_length = u32::try_from(self.manifest_bytes.len()).unwrap_or(u32::MAX);
        output.extend_from_slice(&manifest_length.to_le_bytes());
        output.extend_from_slice(&self.manifest_bytes);
        output.extend_from_slice(&self.binding_bytes);
        let count = u32::try_from(self.images.len()).unwrap_or(u32::MAX);
        output.extend_from_slice(&count.to_le_bytes());
        for image in &self.images {
            output.extend_from_slice(&image.artifact_ordinal.to_le_bytes());
            output.extend_from_slice(&image.object_id);
            output.extend_from_slice(&image.semantic_image_identity);
            output.extend_from_slice(&image.byte_length.to_le_bytes());
        }
        match &self.versioned_planes {
            None => output.push(0),
            Some(planes) => {
                output.push(1);
                let bytes = planes.canonical_bytes();
                let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                output.extend_from_slice(&length.to_le_bytes());
                output.extend_from_slice(&bytes);
            }
        }
        output
    }

    /// Returns the typed FileStore metadata object.
    #[must_use]
    pub fn typed_object(&self) -> TypedObject {
        let key = ObjectKey::<CompilerPublicationMetadataSchema>::from_value(self);
        TypedObject::from_value(&key, self)
    }

    /// Returns the content-addressed metadata object ID.
    #[must_use]
    pub fn object_id(&self) -> AuthorityHash {
        *self.typed_object().id().as_bytes()
    }

    /// Builds the exact artifact claim and payload for a FileStore artifact plan.
    pub fn artifact_object(&self) -> Result<(ArtifactObjectClaim, Vec<u8>), CompilerEnvelopeError> {
        artifact_claim(self.typed_object())
    }

    /// Parses and admits one complete canonical metadata record.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerEnvelopeError> {
        let mut input = Decoder::new(bytes);
        if input.take(METADATA_MAGIC.len())? != METADATA_MAGIC {
            return Err(CompilerEnvelopeError::MetadataMagic);
        }
        if input.u16()? != METADATA_VERSION {
            return Err(CompilerEnvelopeError::MetadataVersion);
        }
        let manifest_length =
            usize::try_from(input.u32()?).map_err(|_| CompilerEnvelopeError::ManifestFacts)?;
        if manifest_length < SEMANTIC_MANIFEST_HEADER_BYTES || manifest_length > MAX_MANIFEST_BYTES
        {
            return Err(CompilerEnvelopeError::ManifestFacts);
        }
        let manifest_bytes = input.take(manifest_length)?.to_vec();
        let binding_bytes = input.array::<COMPILATION_BINDING_BYTES>()?;
        let count =
            usize::try_from(input.u32()?).map_err(|_| CompilerEnvelopeError::MemberCount)?;
        if count == 0 || count > MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        let mut images = Vec::with_capacity(count);
        for _ in 0..count {
            images.push(CompilerImageMember {
                artifact_ordinal: input.u32()?,
                object_id: input.array::<32>()?,
                semantic_image_identity: input.array::<32>()?,
                byte_length: input.u32()?,
            });
        }
        let versioned_planes = match input.u8()? {
            0 => None,
            1 => {
                let length = usize::try_from(input.u32()?)
                    .map_err(|_| CompilerEnvelopeError::VersionedPlaneMetadata)?;
                let bytes = input.take(length)?;
                Some(
                    VersionedPlaneMetadata::decode(bytes)
                        .map_err(|error| CompilerEnvelopeError::VersionedPlane(error))?,
                )
            }
            _ => return Err(CompilerEnvelopeError::VersionedPlaneMetadata),
        };
        if !input.is_empty() {
            return Err(CompilerEnvelopeError::TrailingBytes);
        }
        if images
            .windows(2)
            .any(|pair| pair[0].object_id >= pair[1].object_id)
        {
            return Err(CompilerEnvelopeError::MemberOrder);
        }
        let metadata = Self::new_with_versioned_planes(
            &manifest_bytes,
            binding_bytes,
            images,
            versioned_planes,
        )?;
        if metadata.canonical_bytes() != bytes {
            return Err(CompilerEnvelopeError::NonCanonical);
        }
        Ok(metadata)
    }
}

/// Canonical package compiler output identity admitted before head selection.
///
/// `member_ids` contains sorted identities of the typed metadata and semantic
/// image objects; it excludes this envelope object, and `physical_pack_id`
/// commits to that exact ordered set. The envelope's own object ID becomes the authority's
/// candidate ID. Its outer FileStore closure ID is only known after the
/// envelope and members have been admitted together.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerPublicationEnvelope {
    namespace: AuthorityNamespace,
    profile: Box<str>,
    attempt_id: [u8; 16],
    attempt_epoch: u64,
    attempt_fence: AuthorityHash,
    input_digest: AuthorityHash,
    logical_generation: AuthorityHash,
    metadata_id: AuthorityHash,
    semantic_manifest_root: Option<AuthorityHash>,
    physical_pack_id: AuthorityHash,
    member_ids: Box<[AuthorityHash]>,
}

impl CompilerPublicationEnvelope {
    /// Creates a profile-scoped envelope from the exact Turso-minted attempt and compiler outputs.
    pub fn new(
        attempt: &CandidateAttempt,
        metadata: &CompilerPublicationMetadata,
    ) -> Result<Self, CompilerEnvelopeError> {
        let Some(profile) = attempt.namespace().plane().profile() else {
            return Err(CompilerEnvelopeError::Profile);
        };
        validate_profile(profile)?;
        if metadata.images.is_empty() {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        let metadata_id = metadata.object_id();
        let mut member_ids = metadata_member_ids(metadata);
        if member_ids.len() > MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        member_ids.sort_unstable();
        if member_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(CompilerEnvelopeError::DuplicateMember);
        }
        let envelope = Self {
            namespace: attempt.namespace().clone(),
            profile: profile.into(),
            attempt_id: *attempt.attempt_nonce(),
            attempt_epoch: attempt.epoch(),
            attempt_fence: attempt.fence_bytes(),
            input_digest: *attempt.input_digest(),
            logical_generation: metadata.binding_identity,
            metadata_id,
            semantic_manifest_root: metadata.semantic_manifest_root().copied(),
            physical_pack_id: physical_pack_id(&member_ids),
            member_ids: member_ids.into_boxed_slice(),
        };
        if envelope
            .member_ids
            .binary_search(&envelope.object_id())
            .is_ok()
        {
            return Err(CompilerEnvelopeError::SelfMember);
        }
        Ok(envelope)
    }

    /// Exact package/source/branch/environment/profile authority namespace.
    #[must_use]
    pub fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Stable compiler profile identifier, such as `rust/2024/lower-ir`.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Turso-minted random attempt capability ID and epoch.
    #[must_use]
    pub const fn attempt(&self) -> (&[u8; 16], u64) {
        (&self.attempt_id, self.attempt_epoch)
    }

    /// Exact scheduler attempt ordinal and persisted publication fence.
    #[must_use]
    pub const fn scheduler_fence(&self) -> (u64, AuthorityHash) {
        (self.attempt_epoch, self.attempt_fence)
    }

    /// Exact canonical compiler input digest.
    #[must_use]
    pub const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    /// Logical generation identity, stable across physical repacking.
    #[must_use]
    pub const fn logical_generation(&self) -> &AuthorityHash {
        &self.logical_generation
    }

    /// FileStore identity of the typed metadata needed for cold semantic activation.
    #[must_use]
    pub const fn metadata_id(&self) -> &AuthorityHash {
        &self.metadata_id
    }

    /// Exact semantic-plane manifest root committed by this publication, if
    /// versioned planes are present.
    #[must_use]
    pub const fn semantic_manifest_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    /// Aggregate catalog root bound by the selected compiler envelope.
    #[must_use]
    pub const fn semantic_catalog_root(&self) -> Option<&AuthorityHash> {
        self.semantic_manifest_root.as_ref()
    }

    /// Physical identity of the exact sorted inner member set.
    #[must_use]
    pub const fn physical_pack_id(&self) -> &AuthorityHash {
        &self.physical_pack_id
    }

    /// Exact sorted compiler output member identities, excluding this envelope.
    #[must_use]
    pub fn member_ids(&self) -> &[AuthorityHash] {
        &self.member_ids
    }

    /// Checks that the envelope names the exact Turso-selected immutable tuple.
    #[must_use]
    pub fn matches_selected(&self, selected: &SelectedGeneration) -> bool {
        let (attempt_id, attempt_epoch) = selected.attempt();
        self.namespace == *selected.namespace()
            && self.attempt_id == *attempt_id
            && self.attempt_epoch == attempt_epoch
            && self.scheduler_fence() == selected.scheduler_fence()
            && self.input_digest == *selected.input_digest()
            && self.logical_generation == *selected.target_root()
            && self.physical_pack_id == *selected.pack_id()
            && self.semantic_manifest_root == selected.semantic_manifest_root().copied()
            && self.object_id() == *selected.candidate_id()
    }

    /// Confirms metadata identity, logical root, and exact compiler member set.
    pub fn validate_metadata(
        &self,
        metadata: &CompilerPublicationMetadata,
    ) -> Result<(), CompilerEnvelopeError> {
        if self.metadata_id != metadata.object_id()
            || self.logical_generation != metadata.binding_identity
            || self.semantic_manifest_root != metadata.semantic_manifest_root().copied()
            || self.member_ids.as_ref() != metadata_member_ids(metadata).as_slice()
        {
            return Err(CompilerEnvelopeError::MetadataMismatch);
        }
        Ok(())
    }

    /// Returns the canonical, versioned envelope bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(
            MAGIC.len() + 2 + 16 * 4 + 4 * 5 + 16 + 32 * 6 + 1 + self.member_ids.len() * 32,
        );
        self.encode_into(&mut output);
        output
    }

    /// Returns the typed FileStore object used as the candidate ID.
    #[must_use]
    pub fn typed_object(&self) -> TypedObject {
        let key = ObjectKey::<CompilerPublicationEnvelopeSchema>::from_value(self);
        TypedObject::from_value(&key, self)
    }

    /// Returns the content-addressed object identity of this envelope.
    #[must_use]
    pub fn object_id(&self) -> AuthorityHash {
        *self.typed_object().id().as_bytes()
    }

    /// Creates the authority candidate tuple from this exact envelope and outer closure ID.
    pub fn candidate(
        &self,
        attempt: CandidateAttempt,
        outer_closure_id: AuthorityHash,
    ) -> Result<CandidateGeneration, CompilerEnvelopeError> {
        if attempt.namespace() != &self.namespace
            || attempt.attempt_nonce() != &self.attempt_id
            || attempt.epoch() != self.attempt_epoch
            || attempt.fence_bytes() != self.attempt_fence
            || attempt.input_digest() != &self.input_digest
        {
            return Err(CompilerEnvelopeError::AttemptMismatch);
        }
        Ok(attempt
            .candidate(
                self.object_id(),
                self.logical_generation,
                self.physical_pack_id,
                outer_closure_id,
            )
            .with_semantic_manifest_root(self.semantic_manifest_root))
    }

    /// Builds the exact artifact claim and payload for a FileStore artifact plan.
    pub fn artifact_object(&self) -> Result<(ArtifactObjectClaim, Vec<u8>), CompilerEnvelopeError> {
        let object = self.typed_object();
        let claim = ArtifactObjectClaim::new(
            object.schema(),
            *object.key(),
            *object.version(),
            u64::try_from(object.bytes().len())
                .map_err(|_| CompilerEnvelopeError::ByteLengthOverflow)?,
        )
        .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()));
        Ok((claim, object.bytes().to_vec()))
    }

    /// Parses and admits one complete canonical envelope value.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerEnvelopeError> {
        let mut input = Decoder::new(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(CompilerEnvelopeError::Magic);
        }
        if input.u16()? != ENVELOPE_VERSION {
            return Err(CompilerEnvelopeError::Version);
        }
        let package = input.string(4096)?;
        let source = input.string(4096)?;
        let branch = input.string(4096)?;
        let environment = input.string(4096)?;
        let plane = match input.u8()? {
            1 => {
                let profile = input.string(MAX_PROFILE_BYTES)?;
                validate_profile(&profile)?;
                AuthorityPlane::semantic_profile(profile)
                    .map_err(|_| CompilerEnvelopeError::Profile)?
            }
            _ => return Err(CompilerEnvelopeError::Profile),
        };
        let namespace = AuthorityNamespace::with_plane(package, source, branch, environment, plane)
            .map_err(|_| CompilerEnvelopeError::Namespace)?;
        let profile = namespace
            .plane()
            .profile()
            .ok_or(CompilerEnvelopeError::Profile)?
            .to_owned();
        let attempt_id = input.array::<16>()?;
        let attempt_epoch = input.u64()?;
        let attempt_fence = input.array::<32>()?;
        let input_digest = input.array::<32>()?;
        let logical_generation = input.array::<32>()?;
        let metadata_id = input.array::<32>()?;
        let semantic_manifest_root = decode_optional_hash(&mut input)?;
        let expected_physical_pack_id = input.array::<32>()?;
        let member_count =
            usize::try_from(input.u32()?).map_err(|_| CompilerEnvelopeError::MemberCount)?;
        if member_count == 0 || member_count > MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        let mut member_ids = Vec::with_capacity(member_count);
        for _ in 0..member_count {
            member_ids.push(input.array::<32>()?);
        }
        if !input.is_empty() {
            return Err(CompilerEnvelopeError::TrailingBytes);
        }
        if member_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(CompilerEnvelopeError::MemberOrder);
        }
        if physical_pack_id(&member_ids) != expected_physical_pack_id {
            return Err(CompilerEnvelopeError::PackIdentity);
        }
        if member_ids.binary_search(&metadata_id).is_err() {
            return Err(CompilerEnvelopeError::MetadataMissing);
        }
        let envelope = Self {
            namespace,
            profile: profile.into_boxed_str(),
            attempt_id,
            attempt_epoch,
            attempt_fence,
            input_digest,
            logical_generation,
            metadata_id,
            semantic_manifest_root,
            physical_pack_id: expected_physical_pack_id,
            member_ids: member_ids.into_boxed_slice(),
        };
        if envelope
            .member_ids
            .binary_search(&envelope.object_id())
            .is_ok()
        {
            return Err(CompilerEnvelopeError::SelfMember);
        }
        if envelope.canonical_bytes() != bytes {
            return Err(CompilerEnvelopeError::NonCanonical);
        }
        Ok(envelope)
    }

    fn encode_into(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&ENVELOPE_VERSION.to_le_bytes());
        put_string(output, self.namespace.package());
        put_string(output, self.namespace.source());
        put_string(output, self.namespace.branch());
        put_string(output, self.namespace.environment());
        output.push(1);
        put_string(output, self.profile());
        output.extend_from_slice(&self.attempt_id);
        output.extend_from_slice(&self.attempt_epoch.to_le_bytes());
        output.extend_from_slice(&self.attempt_fence);
        output.extend_from_slice(&self.input_digest);
        output.extend_from_slice(&self.logical_generation);
        output.extend_from_slice(&self.metadata_id);
        encode_optional_hash(self.semantic_manifest_root, output);
        output.extend_from_slice(&self.physical_pack_id);
        let count = u32::try_from(self.member_ids.len()).unwrap_or(u32::MAX);
        output.extend_from_slice(&count.to_le_bytes());
        for member in &self.member_ids {
            output.extend_from_slice(member);
        }
    }

    fn matches_claim(&self, claim: &ClosureClaim) -> bool {
        let (attempt_id, attempt_epoch) = claim.attempt();
        self.namespace == *claim.namespace()
            && self.attempt_id == *attempt_id
            && self.attempt_epoch == attempt_epoch
            && self.scheduler_fence() == claim.scheduler_fence()
            && self.input_digest == *claim.input_digest()
            && self.logical_generation == *claim.target_root()
            && self.physical_pack_id == *claim.pack_id()
            && self.semantic_manifest_root == claim.semantic_manifest_root().copied()
    }
}

/// Verifies a persisted outer closure and its exact embedded compiler envelope.
pub(super) struct FileStoreCompilerPublicationVerifier<'store, 'envelope, 'metadata> {
    store: &'store FileStore,
    budget: ArtifactBudget,
    expected: &'envelope CompilerPublicationEnvelope,
    expected_metadata: &'metadata CompilerPublicationMetadata,
}

impl<'store, 'envelope, 'metadata>
    FileStoreCompilerPublicationVerifier<'store, 'envelope, 'metadata>
{
    /// Uses a bounded closure reopen and exact expected envelope and metadata.
    #[must_use]
    pub(super) const fn new(
        store: &'store FileStore,
        budget: ArtifactBudget,
        expected: &'envelope CompilerPublicationEnvelope,
        expected_metadata: &'metadata CompilerPublicationMetadata,
    ) -> Self {
        Self {
            store,
            budget,
            expected,
            expected_metadata,
        }
    }
}

/// Cold-reopens one Turso-selected envelope and metadata from its verified closure.
pub fn reopen_selected_compiler_publication(
    store: &FileStore,
    budget: ArtifactBudget,
    selected: &SelectedGeneration,
) -> Result<ReopenedCompilerPublication, String> {
    let stored = store
        .reopen_stored_closure(
            ArtifactClosureClaim::from_bytes(*selected.closure_id()),
            budget,
        )
        .map_err(|error| format!("{error:?}"))?;
    if stored.closure().as_bytes() != selected.closure_id() {
        return Err("reopened selected closure identity mismatch".to_owned());
    }
    let manifest = store
        .open_closure(stored.closure())
        .map_err(|error| format!("{error:?}"))?;
    let envelope_id = manifest
        .admit_claim(UntrustedObjectId::from_bytes(*selected.candidate_id()))
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "selected compiler envelope is absent from its closure".to_owned())?;
    let envelope_object = manifest
        .get(envelope_id)
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "selected compiler envelope disappeared from its closure".to_owned())?;
    if envelope_object.schema() != COMPILER_PUBLICATION_ENVELOPE_SCHEMA {
        return Err("selected candidate object has the wrong envelope schema".to_owned());
    }
    let envelope = CompilerPublicationEnvelope::decode(envelope_object.bytes())
        .map_err(|error| error.to_string())?;
    if !envelope.matches_selected(selected) {
        return Err("selected compiler envelope differs from the Turso head".to_owned());
    }
    let metadata_id = manifest
        .admit_claim(UntrustedObjectId::from_bytes(*envelope.metadata_id()))
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "selected compiler metadata is absent from its closure".to_owned())?;
    let metadata_object = manifest
        .get(metadata_id)
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "selected compiler metadata disappeared from its closure".to_owned())?;
    if metadata_object.schema() != COMPILER_PUBLICATION_METADATA_SCHEMA {
        return Err("selected compiler metadata has the wrong schema".to_owned());
    }
    let metadata = CompilerPublicationMetadata::decode(metadata_object.bytes())
        .map_err(|error| error.to_string())?;
    envelope
        .validate_metadata(&metadata)
        .map_err(|error| error.to_string())?;
    if selected.semantic_manifest_root().copied() != metadata.semantic_manifest_root().copied() {
        return Err("selected semantic-plane root differs from compiler metadata".to_owned());
    }
    let images = copy_semantic_image_members(&manifest, &metadata)?;
    verify_versioned_plane_members(&manifest, &metadata)?;
    verify_exact_member_set(
        &manifest,
        envelope_id,
        envelope.member_ids(),
        stored.object_count(),
    )?;
    Ok(ReopenedCompilerPublication {
        envelope,
        metadata,
        images,
    })
}

/// Cold-reopens only the small envelope and semantic metadata for one
/// selected closure. The durable manifest is opened by its untrusted closure
/// claim and only metadata/member-index paths are traversed; CAS output bytes
/// are read lazily by the consumer that requests an image or plane range.
///
/// This is the production selector path for bounded versioned-plane reads.
/// It relies on the closure having passed [`FileStoreCompilerPublicationVerifier`]
/// before Turso selected it, then rechecks the authority tuple and exact
/// immutable closure membership without rescanning every output payload.
pub fn reopen_selected_compiler_metadata(
    store: &FileStore,
    selected: &SelectedGeneration,
) -> Result<ReopenedCompilerMetadata, String> {
    let manifest = store
        .open_closure_claim(ArtifactClosureClaim::from_bytes(*selected.closure_id()))
        .map_err(|error| format!("open selected compiler closure index: {error:?}"))?;
    let envelope_id = manifest
        .admit_claim(UntrustedObjectId::from_bytes(*selected.candidate_id()))
        .map_err(|error| format!("admit selected compiler envelope: {error:?}"))?
        .ok_or_else(|| "selected compiler envelope is absent from its closure".to_owned())?;
    let envelope_object = manifest
        .get(envelope_id)
        .map_err(|error| format!("read selected compiler envelope: {error:?}"))?
        .ok_or_else(|| "selected compiler envelope disappeared from its closure".to_owned())?;
    if envelope_object.schema() != COMPILER_PUBLICATION_ENVELOPE_SCHEMA {
        return Err("selected candidate object has the wrong envelope schema".to_owned());
    }
    let envelope = CompilerPublicationEnvelope::decode(envelope_object.bytes())
        .map_err(|error| error.to_string())?;
    if !envelope.matches_selected(selected) {
        return Err("selected compiler envelope differs from the Turso head".to_owned());
    }
    let metadata_id = manifest
        .admit_claim(UntrustedObjectId::from_bytes(*envelope.metadata_id()))
        .map_err(|error| format!("admit selected compiler metadata: {error:?}"))?
        .ok_or_else(|| "selected compiler metadata is absent from its closure".to_owned())?;
    let metadata_object = manifest
        .get(metadata_id)
        .map_err(|error| format!("read selected compiler metadata: {error:?}"))?
        .ok_or_else(|| "selected compiler metadata disappeared from its closure".to_owned())?;
    if metadata_object.schema() != COMPILER_PUBLICATION_METADATA_SCHEMA {
        return Err("selected compiler metadata has the wrong schema".to_owned());
    }
    let metadata = CompilerPublicationMetadata::decode(metadata_object.bytes())
        .map_err(|error| error.to_string())?;
    envelope
        .validate_metadata(&metadata)
        .map_err(|error| error.to_string())?;
    if selected.semantic_manifest_root().copied() != metadata.semantic_manifest_root().copied() {
        return Err("selected semantic-plane root differs from compiler metadata".to_owned());
    }
    let count = manifest
        .entry_count()
        .ok_or_else(|| "selected compiler closure count is unavailable".to_owned())?;
    verify_exact_member_set(
        &manifest,
        envelope_id,
        envelope.member_ids(),
        u64::try_from(count).map_err(|_| "selected compiler closure count overflow".to_owned())?,
    )?;
    Ok(ReopenedCompilerMetadata { envelope, metadata })
}

/// Exact envelope and typed metadata reopened without loading output payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReopenedCompilerMetadata {
    envelope: CompilerPublicationEnvelope,
    metadata: CompilerPublicationMetadata,
}

impl ReopenedCompilerMetadata {
    /// Selected immutable envelope.
    #[must_use]
    pub const fn envelope(&self) -> &CompilerPublicationEnvelope {
        &self.envelope
    }

    /// Exact compiler metadata and versioned-plane reference inventory.
    #[must_use]
    pub const fn metadata(&self) -> &CompilerPublicationMetadata {
        &self.metadata
    }
}

/// Exact typed envelope and compiler facts reopened from a selected closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReopenedCompilerPublication {
    envelope: CompilerPublicationEnvelope,
    metadata: CompilerPublicationMetadata,
    images: Box<[ReopenedCompilerImage]>,
}

impl ReopenedCompilerPublication {
    /// Selected immutable envelope.
    #[must_use]
    pub const fn envelope(&self) -> &CompilerPublicationEnvelope {
        &self.envelope
    }

    /// Exact compiler manifest, binding, and image facts needed for cold projection recovery.
    #[must_use]
    pub const fn metadata(&self) -> &CompilerPublicationMetadata {
        &self.metadata
    }

    /// Semantic image bytes in the exact order of the metadata inventory.
    #[must_use]
    pub fn images(&self) -> &[ReopenedCompilerImage] {
        &self.images
    }
}

impl DurableClosureVerifier for FileStoreCompilerPublicationVerifier<'_, '_, '_> {
    type Error = String;

    fn verify_closure(&self, claim: &ClosureClaim) -> Result<(), Self::Error> {
        if !self.expected.matches_claim(claim) || self.expected.object_id() != *claim.candidate_id()
        {
            return Err("compiler envelope differs from the exact candidate claim".to_owned());
        }
        let stored = self
            .store
            .reopen_stored_closure(
                ArtifactClosureClaim::from_bytes(*claim.closure_id()),
                self.budget,
            )
            .map_err(|error| format!("{error:?}"))?;
        if stored.closure().as_bytes() != claim.closure_id() {
            return Err("reopened outer closure identity mismatch".to_owned());
        }
        let manifest = self
            .store
            .open_closure(stored.closure())
            .map_err(|error| format!("{error:?}"))?;
        let envelope_claim = UntrustedObjectId::from_bytes(*claim.candidate_id());
        let envelope_id = manifest
            .admit_claim(envelope_claim)
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "publication envelope is absent from its closure".to_owned())?;
        let object = manifest
            .get(envelope_id)
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "publication envelope disappeared from its closure".to_owned())?;
        if object.schema() != COMPILER_PUBLICATION_ENVELOPE_SCHEMA {
            return Err("candidate object has the wrong envelope schema".to_owned());
        }
        let observed = CompilerPublicationEnvelope::decode(object.bytes())
            .map_err(|error| error.to_string())?;
        if observed != *self.expected || !observed.matches_claim(claim) {
            return Err("reopened envelope differs from the exact candidate claim".to_owned());
        }
        observed
            .validate_metadata(self.expected_metadata)
            .map_err(|error| error.to_string())?;
        let metadata_id = manifest
            .admit_claim(UntrustedObjectId::from_bytes(observed.metadata_id))
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "compiler publication metadata is absent from its closure".to_owned())?;
        let metadata_object = manifest
            .get(metadata_id)
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| {
                "compiler publication metadata disappeared from its closure".to_owned()
            })?;
        if metadata_object.schema() != COMPILER_PUBLICATION_METADATA_SCHEMA {
            return Err("compiler metadata object has the wrong schema".to_owned());
        }
        let observed_metadata = CompilerPublicationMetadata::decode(metadata_object.bytes())
            .map_err(|error| error.to_string())?;
        if observed_metadata != *self.expected_metadata
            || observed_metadata.binding_identity != observed.logical_generation
            || metadata_member_ids(&observed_metadata).as_slice() != observed.member_ids.as_ref()
        {
            return Err(
                "reopened compiler metadata differs from the exact envelope pack".to_owned(),
            );
        }
        verify_semantic_image_members(&manifest, &observed_metadata)?;
        verify_versioned_plane_members(&manifest, &observed_metadata)?;
        verify_exact_member_set(
            &manifest,
            envelope_id,
            observed.member_ids(),
            stored.object_count(),
        )?;
        Ok(())
    }
}

fn metadata_member_ids(metadata: &CompilerPublicationMetadata) -> Vec<AuthorityHash> {
    let plane_count = metadata
        .versioned_planes
        .as_ref()
        .map_or(0, VersionedPlaneMetadata::segment_count);
    let mut members = Vec::with_capacity(metadata.images.len() + plane_count + 1);
    members.push(metadata.object_id());
    members.extend(metadata.images.iter().map(|image| image.object_id));
    if let Some(planes) = &metadata.versioned_planes {
        members.extend(
            planes
                .artifacts()
                .iter()
                .map(|artifact| *artifact.manifest_object_id()),
        );
        members.extend(planes.members().map(|member| *member.object_id()));
    }
    members.sort_unstable();
    members.dedup();
    members
}

fn metadata_member_count(
    image_count: usize,
    versioned_planes: Option<&VersionedPlaneMetadata>,
) -> Result<usize, CompilerEnvelopeError> {
    let plane_count = versioned_planes.map_or(0, |planes| planes.object_ids().len());
    image_count
        .checked_add(plane_count)
        .and_then(|count| count.checked_add(1))
        .ok_or(CompilerEnvelopeError::MemberCount)
}

fn verify_semantic_image_members(
    manifest: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<(), String> {
    for expected in metadata.images() {
        let object_id = manifest
            .admit_claim(UntrustedObjectId::from_bytes(*expected.object_id()))
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "compiler semantic image is absent from its closure".to_owned())?;
        let object = manifest
            .get(object_id)
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "compiler semantic image disappeared from its closure".to_owned())?;
        if object.schema() != COMPILER_SEMANTIC_IMAGE_SCHEMA
            || u32::try_from(object.bytes().len()).ok() != Some(expected.byte_length())
        {
            return Err(
                "compiler semantic image schema or length differs from metadata".to_owned(),
            );
        }
        let (observed, typed) = CompilerImageMember::from_bytes_for_ordinal(
            expected.artifact_ordinal(),
            object.bytes(),
            *expected.semantic_image_identity(),
        )
        .map_err(|error| error.to_string())?;
        if observed != *expected || typed.id() != object.id() {
            return Err("compiler semantic image identity differs from metadata".to_owned());
        }
    }
    Ok(())
}

fn verify_versioned_plane_members(
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<(), String> {
    let Some(versioned) = metadata.versioned_planes() else {
        return Ok(());
    };
    if versioned.artifacts().len() != metadata.images().len() {
        return Err("semantic image and plane catalog counts differ".to_owned());
    }
    let mut images_by_ordinal = vec![None; metadata.images().len()];
    for image in metadata.images() {
        let ordinal = usize::try_from(image.artifact_ordinal())
            .map_err(|_| "semantic image ordinal is invalid".to_owned())?;
        let slot = images_by_ordinal
            .get_mut(ordinal)
            .ok_or_else(|| "semantic image ordinal is outside the catalog".to_owned())?;
        if slot.replace(image).is_some() {
            return Err("semantic image ordinal is duplicated".to_owned());
        }
    }
    for artifact in versioned.artifacts() {
        let image_key = artifact.image_key();
        let ordinal = usize::try_from(image_key.artifact_ordinal())
            .map_err(|_| "plane catalog ordinal is invalid".to_owned())?;
        let expected_image = images_by_ordinal
            .get(ordinal)
            .copied()
            .flatten()
            .ok_or_else(|| "plane catalog has no corresponding compiler image".to_owned())?;
        let semantic_manifest =
            backend_semantic::ir::SemanticPlaneManifest::decode(artifact.manifest_bytes())
                .map_err(|error| format!("decode versioned semantic manifest: {error}"))?;
        if semantic_manifest.root() != image_key.manifest_root()
            || semantic_manifest.semantic_generation() != image_key.semantic_generation()
            || semantic_manifest.root() != artifact.manifest_root()
        {
            return Err("versioned semantic image key differs from its manifest".to_owned());
        }
        let manifest_object_id = closure
            .admit_claim(UntrustedObjectId::from_bytes(
                *artifact.manifest_object_id(),
            ))
            .map_err(|error| format!("admit semantic manifest object: {error:?}"))?
            .ok_or_else(|| "compiler closure is missing a semantic manifest object".to_owned())?;
        let manifest_object = closure
            .get(manifest_object_id)
            .map_err(|error| format!("read semantic manifest object: {error:?}"))?
            .ok_or_else(|| "semantic manifest disappeared from closure".to_owned())?;
        if manifest_object.schema() != VERSIONED_PLANE_MANIFEST_SCHEMA
            || manifest_object.bytes() != artifact.manifest_bytes()
        {
            return Err("semantic manifest object differs from its selected bytes".to_owned());
        }
        let image_id = closure
            .admit_claim(UntrustedObjectId::from_bytes(*expected_image.object_id()))
            .map_err(|error| format!("admit semantic image for plane generation: {error:?}"))?
            .ok_or_else(|| "compiler closure is missing a catalog image".to_owned())?;
        let image = closure
            .get(image_id)
            .map_err(|error| format!("read semantic image for plane generation: {error:?}"))?
            .ok_or_else(|| {
                "semantic image disappeared while checking plane generation".to_owned()
            })?;
        if backend_semantic::ir::GenerationId::from_canonical_bytes(image.bytes())
            != image_key.semantic_generation()
        {
            return Err(
                "plane catalog generation differs from its exact compiler image".to_owned(),
            );
        }

        let segment_index = semantic_segment_index(&semantic_manifest)?;
        if segment_index.len() != artifact.members().len() {
            return Err("semantic segment inventory differs from its manifest".to_owned());
        }
        for expected in artifact.members() {
            let expected_segment_id = *expected.segment_id().as_bytes();
            let segment_position = segment_index
                .binary_search_by_key(&expected_segment_id, |(segment_id, _, _)| *segment_id)
                .map_err(|_| "semantic segment ref is absent from its manifest".to_owned())?;
            let (_, plane_kind, segment) = segment_index
                .get(segment_position)
                .ok_or_else(|| "semantic segment index changed during verification".to_owned())?;
            let object_id = closure
                .admit_claim(UntrustedObjectId::from_bytes(*expected.object_id()))
                .map_err(|error| format!("admit semantic segment object: {error:?}"))?
                .ok_or_else(|| {
                    "compiler closure is missing a semantic segment object".to_owned()
                })?;
            let object = closure
                .get(object_id)
                .map_err(|error| format!("read semantic segment object: {error:?}"))?
                .ok_or_else(|| "semantic segment object disappeared from closure".to_owned())?;
            if object.schema() != VERSIONED_PLANE_SEGMENT_SCHEMA
                || u64::try_from(object.bytes().len()).ok() != Some(expected.byte_length())
            {
                return Err(
                    "semantic segment object schema or length differs from inventory".to_owned(),
                );
            }
            segment
                .admit(*plane_kind, object.bytes())
                .map_err(|error| format!("verify semantic segment payload: {error}"))?;
        }
    }
    Ok(())
}

/// Indexes every manifest segment once so closure members can be verified in
/// O(n log n) rather than rescanning all planes for each physical member.
fn semantic_segment_index(
    manifest: &backend_semantic::ir::SemanticPlaneManifest,
) -> Result<
    Vec<(
        [u8; 32],
        backend_semantic::ir::SemanticPlaneKind,
        backend_semantic::ir::SemanticPlaneSegment,
    )>,
    String,
> {
    let segment_count = manifest
        .planes()
        .iter()
        .try_fold(0usize, |count, plane| {
            count.checked_add(plane.segments().len())
        })
        .ok_or_else(|| "semantic segment count overflow".to_owned())?;
    if segment_count > MAX_MEMBER_IDS {
        return Err("semantic segment count exceeds publication bound".to_owned());
    }
    let mut index = Vec::new();
    index
        .try_reserve_exact(segment_count)
        .map_err(|_| "semantic segment index allocation failed".to_owned())?;
    for plane in manifest.planes() {
        for segment in plane.segments() {
            index.push((*segment.id_claim().as_bytes(), plane.kind(), *segment));
        }
    }
    index.sort_unstable_by_key(|(segment_id, _, _)| *segment_id);
    if index.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("semantic manifest contains a duplicate segment ID".to_owned());
    }
    Ok(index)
}

fn copy_semantic_image_members(
    manifest: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<Box<[ReopenedCompilerImage]>, String> {
    let mut images = Vec::new();
    images
        .try_reserve_exact(metadata.images().len())
        .map_err(|_| "compiler image inventory allocation failed".to_owned())?;
    for expected in metadata.images() {
        let object_id = manifest
            .admit_claim(UntrustedObjectId::from_bytes(*expected.object_id()))
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "compiler semantic image is absent from its closure".to_owned())?;
        let object = manifest
            .get(object_id)
            .map_err(|error| format!("{error:?}"))?
            .ok_or_else(|| "compiler semantic image disappeared from its closure".to_owned())?;
        if object.schema() != COMPILER_SEMANTIC_IMAGE_SCHEMA
            || u32::try_from(object.bytes().len()).ok() != Some(expected.byte_length())
        {
            return Err(
                "compiler semantic image schema or length differs from metadata".to_owned(),
            );
        }
        let (observed, typed) = CompilerImageMember::from_bytes_for_ordinal(
            expected.artifact_ordinal(),
            object.bytes(),
            *expected.semantic_image_identity(),
        )
        .map_err(|error| error.to_string())?;
        if observed != *expected || typed.id() != object.id() {
            return Err("compiler semantic image identity differs from metadata".to_owned());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(object.bytes().len())
            .map_err(|_| "compiler semantic image allocation failed".to_owned())?;
        bytes.extend_from_slice(object.bytes());
        images.push(ReopenedCompilerImage {
            member: *expected,
            bytes: bytes.into_boxed_slice(),
        });
    }
    Ok(images.into_boxed_slice())
}

fn verify_exact_member_set(
    manifest: &backend_store::DurableManifest,
    envelope_id: backend_store::ObjectId,
    members: &[AuthorityHash],
    closure_object_count: u64,
) -> Result<(), String> {
    let expected_count = u64::try_from(members.len())
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| "compiler member count overflow".to_owned())?;
    if closure_object_count != expected_count
        || manifest
            .entry_count()
            .is_some_and(|count| u64::try_from(count).ok() != Some(expected_count))
    {
        return Err("closure contains an unlisted or missing compiler member".to_owned());
    }
    let envelope_bytes = *envelope_id.as_bytes();
    if members.binary_search(&envelope_bytes).is_ok() {
        return Err("publication envelope is also listed as a compiler member".to_owned());
    }
    for member in members {
        if manifest
            .admit_claim(UntrustedObjectId::from_bytes(*member))
            .map_err(|error| format!("{error:?}"))?
            .is_none()
        {
            return Err("closure is missing a listed compiler member".to_owned());
        }
    }
    Ok(())
}

/// Canonical envelope parsing, identity, or member-set failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerEnvelopeError {
    /// Envelope magic is wrong.
    Magic,
    /// Envelope version is unsupported.
    Version,
    /// Namespace fields are invalid.
    Namespace,
    /// Compiler profile is empty or outside its canonical bounds.
    Profile,
    /// Envelope member set is empty or exceeds its bound.
    MemberCount,
    /// Envelope members contain a duplicate ID.
    DuplicateMember,
    /// Envelope member IDs are not in strict canonical order.
    MemberOrder,
    /// Envelope member set does not match its physical pack identity.
    PackIdentity,
    /// Member list attempts to contain the envelope itself.
    SelfMember,
    /// Input has bytes after the complete canonical value.
    TrailingBytes,
    /// Input has a valid structure but is not the unique canonical encoding.
    NonCanonical,
    /// Input is truncated or a length exceeds the lane limit.
    Truncated,
    /// The canonical envelope payload length cannot fit the storage protocol.
    ByteLengthOverflow,
    /// The supplied Turso attempt differs from the envelope's exact attempt.
    AttemptMismatch,
    /// Compiler metadata magic is wrong.
    MetadataMagic,
    /// Compiler metadata version is unsupported.
    MetadataVersion,
    /// Metadata does not preserve a valid semantic-v2 manifest fact tuple.
    ManifestFacts,
    /// Compiler manifest identity has the wrong typed authority bytes.
    ManifestIdentity,
    /// Canonical compiler generation binding is malformed or names another manifest.
    Binding,
    /// One semantic image identity has the wrong typed authority bytes.
    ImageIdentity,
    /// One semantic image has no canonical bytes.
    ImageLength,
    /// Envelope does not name its metadata member.
    MetadataMissing,
    /// Compiler metadata does not match the envelope's root or member set.
    MetadataMismatch,
    /// The optional versioned-plane metadata flag or length is malformed.
    VersionedPlaneMetadata,
    /// The canonical versioned-plane metadata is invalid.
    VersionedPlane(VersionedPlaneError),
}

impl fmt::Display for CompilerEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid compiler publication envelope: {self:?}")
    }
}

impl std::error::Error for CompilerEnvelopeError {}

fn validate_binding(
    binding_bytes: &[u8; COMPILATION_BINDING_BYTES],
    manifest_identity: &AuthorityHash,
) -> Result<AuthorityHash, CompilerEnvelopeError> {
    if &binding_bytes[..8] != b"NUDXCPB\0"
        || u16::from_le_bytes([binding_bytes[8], binding_bytes[9]]) != 1
        || binding_bytes[10..12] != [0, 0]
        || binding_bytes[76..108] != manifest_identity[..]
    {
        return Err(CompilerEnvelopeError::Binding);
    }
    ManifestIdentity::try_from(*manifest_identity)
        .map_err(|_| CompilerEnvelopeError::ManifestIdentity)?;
    let identity = BindingIdentity::from_encoded_bytes(binding_bytes);
    Ok(*identity.as_ref())
}

fn inspect_semantic_manifest(
    bytes: &[u8],
) -> Result<(AuthorityHash, u32, u32), CompilerEnvelopeError> {
    if bytes.len() < SEMANTIC_MANIFEST_HEADER_BYTES
        || bytes.len() > MAX_MANIFEST_BYTES
        || &bytes[..8] != SEMANTIC_MANIFEST_MAGIC
        || u16::from_le_bytes([bytes[8], bytes[9]]) != SEMANTIC_MANIFEST_VERSION
        || bytes[10..12] != [0, 0]
    {
        return Err(CompilerEnvelopeError::ManifestFacts);
    }
    let fragment_count = u32::from_le_bytes(
        bytes[12..16]
            .try_into()
            .map_err(|_| CompilerEnvelopeError::ManifestFacts)?,
    );
    let count =
        usize::try_from(fragment_count).map_err(|_| CompilerEnvelopeError::ManifestFacts)?;
    if count == 0 || count > MAX_MEMBER_IDS {
        return Err(CompilerEnvelopeError::ManifestFacts);
    }
    let expected_length = SEMANTIC_MANIFEST_HEADER_BYTES
        .checked_add(
            count
                .checked_mul(SEMANTIC_MANIFEST_ENTRY_BYTES)
                .ok_or(CompilerEnvelopeError::ManifestFacts)?,
        )
        .ok_or(CompilerEnvelopeError::ManifestFacts)?;
    if bytes.len() != expected_length {
        return Err(CompilerEnvelopeError::ManifestFacts);
    }
    let manifest_byte_length =
        u32::try_from(bytes.len()).map_err(|_| CompilerEnvelopeError::ManifestFacts)?;
    let identity = ManifestIdentity::from_encoded_bytes(bytes);
    let identity = *identity.as_ref();
    ManifestIdentity::try_from(identity).map_err(|_| CompilerEnvelopeError::ManifestIdentity)?;
    Ok((identity, fragment_count, manifest_byte_length))
}

fn verify_manifest_image_inventory(
    manifest_bytes: &[u8],
    images: &[CompilerImageMember],
) -> Result<(), CompilerEnvelopeError> {
    let (_, fragment_count, _) = inspect_semantic_manifest(manifest_bytes)?;
    if usize::try_from(fragment_count).ok() != Some(images.len()) {
        return Err(CompilerEnvelopeError::ManifestFacts);
    }
    let mut manifest_images = Vec::with_capacity(images.len());
    for ordinal in 0..images.len() {
        let entry_start = SEMANTIC_MANIFEST_HEADER_BYTES + ordinal * SEMANTIC_MANIFEST_ENTRY_BYTES;
        let image_start = entry_start + SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET;
        let identity: AuthorityHash = manifest_bytes[image_start..image_start + 32]
            .try_into()
            .map_err(|_| CompilerEnvelopeError::ManifestFacts)?;
        SemanticImageIdentity::try_from(identity)
            .map_err(|_| CompilerEnvelopeError::ImageIdentity)?;
        let length_start = entry_start + SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET;
        let length = u32::from_le_bytes(
            manifest_bytes[length_start..length_start + 4]
                .try_into()
                .map_err(|_| CompilerEnvelopeError::ManifestFacts)?,
        );
        if length == 0 {
            return Err(CompilerEnvelopeError::ImageLength);
        }
        manifest_images.push((identity, length));
    }
    let mut metadata_images = vec![None; images.len()];
    for image in images {
        let ordinal = usize::try_from(image.artifact_ordinal)
            .map_err(|_| CompilerEnvelopeError::MemberOrder)?;
        let slot = metadata_images
            .get_mut(ordinal)
            .ok_or(CompilerEnvelopeError::MemberOrder)?;
        if slot
            .replace((image.semantic_image_identity, image.byte_length))
            .is_some()
        {
            return Err(CompilerEnvelopeError::MemberOrder);
        }
    }
    if metadata_images.iter().any(Option::is_none) {
        return Err(CompilerEnvelopeError::MemberOrder);
    }
    if manifest_images
        .iter()
        .copied()
        .zip(metadata_images.into_iter().flatten())
        .any(|(manifest, metadata)| manifest != metadata)
    {
        return Err(CompilerEnvelopeError::MetadataMismatch);
    }
    Ok(())
}

fn artifact_claim(
    object: TypedObject,
) -> Result<(ArtifactObjectClaim, Vec<u8>), CompilerEnvelopeError> {
    let claim = ArtifactObjectClaim::new(
        object.schema(),
        *object.key(),
        *object.version(),
        u64::try_from(object.bytes().len())
            .map_err(|_| CompilerEnvelopeError::ByteLengthOverflow)?,
    )
    .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()));
    Ok((claim, object.bytes().to_vec()))
}

struct CompilerPublicationMetadataSchema;

impl Schema for CompilerPublicationMetadataSchema {
    const DOMAIN: u8 = COMPILER_PUBLICATION_METADATA_SCHEMA.domain();
    const TYPE: u16 = COMPILER_PUBLICATION_METADATA_SCHEMA.ty();
    const VERSION: u8 = COMPILER_PUBLICATION_METADATA_SCHEMA.version();

    type Value = CompilerPublicationMetadata;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.canonical_bytes());
    }
}

struct CompilerSemanticImageSchema;

impl Schema for CompilerSemanticImageSchema {
    const DOMAIN: u8 = COMPILER_SEMANTIC_IMAGE_SCHEMA.domain();
    const TYPE: u16 = COMPILER_SEMANTIC_IMAGE_SCHEMA.ty();
    const VERSION: u8 = COMPILER_SEMANTIC_IMAGE_SCHEMA.version();

    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

struct CompilerPublicationEnvelopeSchema;

impl Schema for CompilerPublicationEnvelopeSchema {
    const DOMAIN: u8 = COMPILER_PUBLICATION_ENVELOPE_SCHEMA.domain();
    const TYPE: u16 = COMPILER_PUBLICATION_ENVELOPE_SCHEMA.ty();
    const VERSION: u8 = COMPILER_PUBLICATION_ENVELOPE_SCHEMA.version();

    type Value = CompilerPublicationEnvelope;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        value.encode_into(output);
    }
}

fn validate_profile(profile: &str) -> Result<(), CompilerEnvelopeError> {
    if profile.trim().is_empty()
        || profile.len() > MAX_PROFILE_BYTES
        || !profile.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(CompilerEnvelopeError::Profile);
    }
    Ok(())
}

fn put_string(output: &mut Vec<u8>, value: &str) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value.as_bytes());
}

fn encode_optional_hash(value: Option<AuthorityHash>, output: &mut Vec<u8>) {
    match value {
        None => output.push(0),
        Some(hash) => {
            output.push(1);
            output.extend_from_slice(&hash);
        }
    }
}

fn decode_optional_hash(
    input: &mut Decoder<'_>,
) -> Result<Option<AuthorityHash>, CompilerEnvelopeError> {
    match input.u8()? {
        0 => Ok(None),
        1 => Ok(Some(input.array::<32>()?)),
        _ => Err(CompilerEnvelopeError::VersionedPlaneMetadata),
    }
}

fn physical_pack_id(members: &[AuthorityHash]) -> AuthorityHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PACK_DOMAIN);
    hasher.update(
        &u64::try_from(members.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for member in members {
        hasher.update(member);
    }
    *hasher.finalize().as_bytes()
}

struct Decoder<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Decoder<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], CompilerEnvelopeError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CompilerEnvelopeError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(CompilerEnvelopeError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CompilerEnvelopeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CompilerEnvelopeError::Truncated)
    }

    fn u16(&mut self) -> Result<u16, CompilerEnvelopeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u8(&mut self) -> Result<u8, CompilerEnvelopeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, CompilerEnvelopeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, CompilerEnvelopeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn string(&mut self, maximum: usize) -> Result<String, CompilerEnvelopeError> {
        let length = usize::try_from(self.u32()?).map_err(|_| CompilerEnvelopeError::Truncated)?;
        if length == 0 || length > maximum {
            return Err(CompilerEnvelopeError::Truncated);
        }
        std::str::from_utf8(self.take(length)?)
            .map(str::to_owned)
            .map_err(|_| CompilerEnvelopeError::Truncated)
    }

    const fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use backend_store::{ArtifactPlan, ClosureManifest};
    use backend_version::ObjectKey;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STORE: AtomicU64 = AtomicU64::new(0);

    struct MemberSchema;

    impl Schema for MemberSchema {
        const DOMAIN: u8 = 0x7b;
        const TYPE: u16 = 0xc002;

        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    fn attempt() -> CandidateAttempt {
        let namespace = AuthorityNamespace::semantic_profile(
            "pkg:cargo/widget",
            "registry:crates-io",
            "main",
            "stable",
            "rust/2024/lower-ir",
        )
        .expect("valid namespace");
        let observation = super::super::types::SourceObservation::new(
            namespace.clone(),
            Some([9; 32]),
            1,
            super::super::types::SourceObservationValue::KnownCount(1),
        )
        .expect("valid observation");
        CandidateAttempt::new(
            namespace,
            4,
            [5; 16],
            [6; 32],
            [7; 32],
            0,
            None,
            super::super::types::SourceObservationReceipt::new(observation, 1),
        )
    }

    fn member(bytes: &[u8]) -> TypedObject {
        let key = ObjectKey::<MemberSchema>::from_value(bytes);
        TypedObject::from_value(&key, bytes)
    }

    fn compiler_image() -> (CompilerImageMember, TypedObject) {
        compiler_image_from_bytes(b"verified compiler image")
    }

    fn compiler_image_from_bytes(bytes: &[u8]) -> (CompilerImageMember, TypedObject) {
        let identity = SemanticImageIdentity::from_encoded_bytes(bytes);
        CompilerImageMember::from_bytes(bytes, *identity.as_ref()).expect("valid image member")
    }

    #[test]
    fn verified_image_envelope_builds_metadata_without_materializing_payload() {
        let (expected, object) = compiler_image();
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("open image CAS");
        store.write_object(&object).expect("write image object");
        let verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
            .expect("authenticate image object envelope");
        let observed = CompilerImageMember::from_verified_object(
            expected.artifact_ordinal(),
            verified,
            *expected.semantic_image_identity(),
        )
        .expect("build image metadata from checked envelope");
        assert_eq!(observed, expected);

        let unrelated = member(b"not a semantic image");
        store
            .write_object(&unrelated)
            .expect("write unrelated object");
        let unrelated_verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(*unrelated.id().as_bytes()))
            .expect("authenticate unrelated object envelope");
        assert_eq!(
            CompilerImageMember::from_verified_object(
                expected.artifact_ordinal(),
                unrelated_verified,
                *expected.semantic_image_identity(),
            ),
            Err(CompilerEnvelopeError::ImageIdentity),
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove image CAS");
    }

    fn metadata(image: CompilerImageMember) -> CompilerPublicationMetadata {
        metadata_with_images(vec![image], None)
    }

    fn metadata_with_versioned_planes(
        image: CompilerImageMember,
        versioned_planes: Option<VersionedPlaneMetadata>,
    ) -> CompilerPublicationMetadata {
        metadata_with_images(vec![image], versioned_planes)
    }

    fn metadata_with_images(
        mut images: Vec<CompilerImageMember>,
        versioned_planes: Option<VersionedPlaneMetadata>,
    ) -> CompilerPublicationMetadata {
        for (ordinal, image) in images.iter_mut().enumerate() {
            image.artifact_ordinal = u32::try_from(ordinal).expect("fixture image count fits u32");
        }
        let image_count = u32::try_from(images.len()).expect("bounded image count");
        let mut manifest_bytes = vec![
            0_u8;
            SEMANTIC_MANIFEST_HEADER_BYTES
                + images.len() * SEMANTIC_MANIFEST_ENTRY_BYTES
        ];
        manifest_bytes[..8].copy_from_slice(SEMANTIC_MANIFEST_MAGIC);
        manifest_bytes[8..10].copy_from_slice(&SEMANTIC_MANIFEST_VERSION.to_le_bytes());
        manifest_bytes[12..16].copy_from_slice(&image_count.to_le_bytes());
        for (ordinal, image) in images.iter().enumerate() {
            let entry_start =
                SEMANTIC_MANIFEST_HEADER_BYTES + ordinal * SEMANTIC_MANIFEST_ENTRY_BYTES;
            let image_start = entry_start + SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET;
            manifest_bytes[image_start..image_start + 32]
                .copy_from_slice(image.semantic_image_identity());
            let image_length_start = entry_start + SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET;
            manifest_bytes[image_length_start..image_length_start + 4]
                .copy_from_slice(&image.byte_length().to_le_bytes());
        }
        let manifest = ManifestIdentity::from_encoded_bytes(&manifest_bytes);
        let root = GenerationId::from_canonical_bytes(b"compiler package root");
        let dependencies =
            ContentId::<DependencySetDomain>::from_canonical_bytes(b"dependency set");
        let mut binding = [0_u8; COMPILATION_BINDING_BYTES];
        binding[..8].copy_from_slice(b"NUDXCPB\0");
        binding[8..10].copy_from_slice(&1_u16.to_le_bytes());
        binding[12..44].copy_from_slice(root.as_ref());
        binding[44..76].copy_from_slice(dependencies.as_ref());
        binding[76..108].copy_from_slice(manifest.as_ref());
        CompilerPublicationMetadata::new_with_versioned_planes(
            &manifest_bytes,
            binding,
            images,
            versioned_planes,
        )
        .expect("valid compiler metadata")
    }

    fn semantic_planes(
        generation: backend_semantic::ir::GenerationId,
    ) -> VersionedPlanePublication {
        semantic_planes_for_image(generation, b"typed plane bytes")
    }

    fn semantic_planes_for_image(
        generation: backend_semantic::ir::GenerationId,
        payload: &[u8],
    ) -> VersionedPlanePublication {
        use backend_semantic::ir::{
            LanguageProfile, RustEdition, SemanticBuildIdentity, SemanticInputWitness,
            SemanticIrPlane, SemanticPlane, SemanticPlaneKind, SemanticPlaneManifest,
            SemanticPlaneSegment,
        };
        use backend_semantic::vocabulary::Stage;
        use backend_version::{Coverage, ScopeRoot};

        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segment = SemanticPlaneSegment::from_payload(kind, [0x31; 32], [0x32; 32], 1, payload)
            .expect("checked semantic segment");
        let manifest = SemanticPlaneManifest::new(
            generation,
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
                SemanticPlane::claimed(kind, vec![segment], Coverage::Partial)
                    .expect("claim-only semantic plane"),
            ],
        )
        .expect("canonical semantic plane manifest");
        let manifest_bytes = manifest.encode().expect("encode semantic plane manifest");
        let id = segment
            .admit(kind, payload)
            .expect("admit exact semantic plane bytes");
        VersionedPlanePublication::from_payloads(&manifest_bytes, [(id, payload)])
            .expect("build exact plane publication")
    }

    fn plane_artifact_at(
        publication: &VersionedPlanePublication,
        artifact_ordinal: u32,
    ) -> VersionedPlaneArtifactMetadata {
        let artifact = &publication.metadata().artifacts()[0];
        VersionedPlaneArtifactMetadata::new(
            artifact_ordinal,
            artifact.manifest_bytes(),
            artifact.members().to_vec(),
        )
        .expect("remap fixture manifest to its canonical artifact ordinal")
    }

    fn combine_plane_objects(publications: &[&VersionedPlanePublication]) -> Vec<TypedObject> {
        let mut objects = publications
            .iter()
            .flat_map(|publication| publication.objects().iter().cloned())
            .collect::<Vec<_>>();
        objects.sort_unstable_by_key(|object| *object.id().as_bytes());
        objects.dedup_by_key(|object| *object.id().as_bytes());
        objects
    }

    fn artifact_claim(object: &TypedObject) -> ArtifactObjectClaim {
        ArtifactObjectClaim::new(
            object.schema(),
            *object.key(),
            *object.version(),
            u64::try_from(object.bytes().len()).expect("bounded member"),
        )
        .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
    }

    fn selected_generation(
        attempt: &CandidateAttempt,
        candidate: &CandidateGeneration,
        generation: u64,
    ) -> SelectedGeneration {
        SelectedGeneration::new(
            attempt.namespace().clone(),
            generation,
            *candidate.candidate_id(),
            *attempt.attempt_id(),
            attempt.epoch(),
            attempt.fence_bytes(),
            *attempt.input_digest(),
            *candidate.target_root(),
            *candidate.pack_id(),
            *candidate.closure_id(),
            candidate.semantic_manifest_root().copied(),
            attempt.observation().clone(),
            super::super::types::SelectionOrigin::CompilerAttempt,
        )
    }

    fn scratch_store() -> std::path::PathBuf {
        let sequence = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-compiler-envelope-{}-{sequence}",
            std::process::id()
        ))
    }

    fn publish_envelope_closure(
        directory: &std::path::Path,
        envelope: &CompilerPublicationEnvelope,
        metadata: &CompilerPublicationMetadata,
        include_extra: bool,
    ) -> (FileStore, backend_store::StoredClosureReceipt) {
        publish_envelope_closure_with_objects(directory, envelope, metadata, &[], include_extra)
    }

    fn publish_envelope_closure_with_objects(
        directory: &std::path::Path,
        envelope: &CompilerPublicationEnvelope,
        metadata: &CompilerPublicationMetadata,
        additional_objects: &[TypedObject],
        include_extra: bool,
    ) -> (FileStore, backend_store::StoredClosureReceipt) {
        let (_, image_object) = compiler_image();
        publish_envelope_closure_with_images(
            directory,
            envelope,
            metadata,
            &[image_object],
            additional_objects,
            include_extra,
        )
    }

    fn publish_envelope_closure_with_images(
        directory: &std::path::Path,
        envelope: &CompilerPublicationEnvelope,
        metadata: &CompilerPublicationMetadata,
        image_objects: &[TypedObject],
        additional_objects: &[TypedObject],
        include_extra: bool,
    ) -> (FileStore, backend_store::StoredClosureReceipt) {
        let mut member_objects = image_objects.to_vec();
        member_objects.push(metadata.typed_object());
        member_objects.extend_from_slice(additional_objects);
        if include_extra {
            member_objects.push(member(b"unlisted compiler image"));
        }
        let envelope_object = envelope.typed_object();
        let mut all_objects = member_objects.clone();
        all_objects.push(envelope_object.clone());
        all_objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        let complete_manifest =
            ClosureManifest::new(all_objects).expect("valid complete compiler object set");
        let target = ArtifactClosureClaim::from_id(complete_manifest.id());

        let mut artifacts: Vec<_> = member_objects
            .iter()
            .cloned()
            .map(|object| (artifact_claim(&object), object.bytes().to_vec()))
            .collect();
        let (envelope_claim, envelope_bytes) =
            envelope.artifact_object().expect("bounded envelope");
        artifacts.push((envelope_claim, envelope_bytes));
        artifacts.sort_by_key(|(claim, _)| (claim.schema(), *claim.key(), *claim.version()));
        let plan = ArtifactPlan::new(
            None,
            target,
            artifacts.iter().map(|(claim, _)| *claim).collect(),
            Vec::new(),
        );
        let store = FileStore::open(directory, 1024 * 1024).expect("open FileStore");
        let mut session = store
            .artifact_sink(ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8))
            .begin(plan)
            .expect("begin artifact closure");
        for (index, (_, bytes)) in artifacts.iter().enumerate() {
            session.put(index, 0, bytes).expect("write artifact member");
        }
        let receipt = session.finish().expect("finish complete closure");
        (store, receipt)
    }

    #[test]
    fn envelope_encoding_is_canonical_and_binds_the_attempt_and_member_set() {
        let attempt = attempt();
        let (image, _) = compiler_image();
        let metadata = metadata(image);
        assert_eq!(
            CompilerPublicationMetadata::decode(&metadata.canonical_bytes())
                .expect("metadata round trip"),
            metadata
        );
        let envelope =
            CompilerPublicationEnvelope::new(&attempt, &metadata).expect("valid envelope");
        assert_eq!(
            envelope.member_ids(),
            metadata_member_ids(&metadata).as_slice()
        );
        assert_eq!(
            CompilerPublicationEnvelope::decode(&envelope.canonical_bytes())
                .expect("canonical round trip"),
            envelope
        );
        let candidate = envelope
            .candidate(attempt.clone(), [10; 32])
            .expect("candidate uses exact envelope fields");
        assert_eq!(candidate.candidate_id(), &envelope.object_id());
        assert_eq!(candidate.target_root(), metadata.binding_identity());
        assert_eq!(candidate.pack_id(), envelope.physical_pack_id());
        assert!(matches!(envelope.candidate(attempt, [11; 32]), Ok(_)));
        let mut trailing = envelope.canonical_bytes();
        trailing.push(0);
        assert_eq!(
            CompilerPublicationEnvelope::decode(&trailing),
            Err(CompilerEnvelopeError::TrailingBytes)
        );
        let mut wrong_members = envelope.canonical_bytes();
        let last = wrong_members.len() - 1;
        wrong_members[last] ^= 1;
        assert_eq!(
            CompilerPublicationEnvelope::decode(&wrong_members),
            Err(CompilerEnvelopeError::PackIdentity)
        );
    }

    #[test]
    fn file_store_verifier_reopens_exact_envelope_and_rejects_extra_member() {
        let attempt = attempt();
        let (image, image_object) = compiler_image();
        let metadata = metadata(image);
        let envelope =
            CompilerPublicationEnvelope::new(&attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let (store, receipt) = publish_envelope_closure(&directory, &envelope, &metadata, false);
        let candidate = envelope
            .candidate(attempt.clone(), *receipt.closure().as_bytes())
            .expect("candidate from stored envelope");
        let claim = ClosureClaim::for_candidate(&candidate);
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        verifier
            .verify_closure(&claim)
            .expect("verify exact stored envelope and members");
        let selected = SelectedGeneration::new(
            attempt.namespace().clone(),
            1,
            *candidate.candidate_id(),
            *attempt.attempt_id(),
            attempt.epoch(),
            attempt.fence_bytes(),
            *attempt.input_digest(),
            *candidate.target_root(),
            *candidate.pack_id(),
            *candidate.closure_id(),
            candidate.semantic_manifest_root().copied(),
            attempt.observation().clone(),
            super::super::types::SelectionOrigin::CompilerAttempt,
        );
        let reopened = reopen_selected_compiler_publication(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &selected,
        )
        .expect("cold reopen selected compiler metadata");
        assert_eq!(reopened.envelope(), &envelope);
        assert_eq!(reopened.metadata(), &metadata);
        assert_eq!(reopened.images().len(), 1);
        assert_eq!(reopened.images()[0].member(), &image);
        assert_eq!(reopened.images()[0].bytes(), image_object.bytes());
        drop(store);

        let extra_directory = scratch_store();
        let (extra_store, extra_receipt) =
            publish_envelope_closure(&extra_directory, &envelope, &metadata, true);
        let extra_candidate = envelope
            .candidate(attempt, *extra_receipt.closure().as_bytes())
            .expect("candidate with extra closure object");
        let extra_claim = ClosureClaim::for_candidate(&extra_candidate);
        let extra_verifier = FileStoreCompilerPublicationVerifier::new(
            &extra_store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        assert!(extra_verifier.verify_closure(&extra_claim).is_err());
        drop(extra_store);
        std::fs::remove_dir_all(directory).expect("remove test store");
        std::fs::remove_dir_all(extra_directory).expect("remove extra test store");
    }

    #[test]
    fn typed_verifier_rejects_unrelated_valid_closure_for_compiler_candidate() {
        let attempt = attempt();
        let (image, _) = compiler_image();
        let metadata = metadata(image);
        let envelope =
            CompilerPublicationEnvelope::new(&attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("open unrelated store");
        let unrelated_object = member(b"valid but unrelated closure member");
        let unrelated_closure =
            ClosureManifest::new(vec![unrelated_object]).expect("valid unrelated closure");
        let unrelated_id = store
            .write_closure(&unrelated_closure)
            .expect("persist unrelated valid closure");
        let candidate = envelope
            .candidate(attempt, *unrelated_id.as_bytes())
            .expect("candidate claims unrelated outer closure");
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        assert!(
            verifier
                .verify_closure(&ClosureClaim::for_candidate(&candidate))
                .is_err()
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove unrelated store");
    }

    #[test]
    fn public_verifier_rejects_valid_output_from_another_admitted_attempt() {
        let admitted_attempt = attempt();
        let (image, _) = compiler_image();
        let metadata = metadata(image);
        let envelope = CompilerPublicationEnvelope::new(&admitted_attempt, &metadata)
            .expect("valid admitted output envelope");
        let directory = scratch_store();
        let (store, receipt) = publish_envelope_closure(&directory, &envelope, &metadata, false);
        let candidate = envelope
            .candidate(admitted_attempt.clone(), *receipt.closure().as_bytes())
            .expect("valid output candidate for admitted attempt");
        let other_attempt = CandidateAttempt::new(
            admitted_attempt.namespace().clone(),
            admitted_attempt.epoch() + 1,
            [0x91; 16],
            [0x92; 32],
            [0x93; 32],
            admitted_attempt.base_generation(),
            admitted_attempt.base_root(),
            admitted_attempt.observation().clone(),
        );
        let authority_path = scratch_store().with_extension("db");
        let authority =
            futures_executor::block_on(super::super::TursoAuthority::open(&authority_path))
                .expect("open authority for typed verification");
        let budget = ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8);
        authority
            .verify_compiler_publication(
                &candidate,
                &admitted_attempt,
                &store,
                budget,
                &envelope,
                &metadata,
            )
            .expect("valid output passes exact admitted attempt");
        assert!(matches!(
            authority.verify_compiler_publication(
                &candidate,
                &other_attempt,
                &store,
                budget,
                &envelope,
                &metadata,
            ),
            Err(super::super::AuthorityError::AdmittedInputMismatch),
        ));
        drop(authority);
        drop(store);
        std::fs::remove_file(authority_path).expect("remove authority database");
        std::fs::remove_dir_all(directory).expect("remove compiler store");
    }

    #[test]
    fn typed_publication_binds_plane_generation_to_exact_image_bytes() {
        let initial_attempt = attempt();
        let (image, image_object) = compiler_image();
        let expected_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(image_object.bytes());
        let plane_publication = semantic_planes(expected_generation);
        let metadata =
            metadata_with_versioned_planes(image, Some(plane_publication.metadata().clone()));
        let envelope =
            CompilerPublicationEnvelope::new(&initial_attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let (store, receipt) = publish_envelope_closure_with_objects(
            &directory,
            &envelope,
            &metadata,
            plane_publication.objects(),
            false,
        );
        let candidate = envelope
            .candidate(initial_attempt.clone(), *receipt.closure().as_bytes())
            .expect("candidate from stored envelope");
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        verifier
            .verify_closure(&ClosureClaim::for_candidate(&candidate))
            .expect("plane manifest names exact canonical image generation");
        let selected = selected_generation(&initial_attempt, &candidate, 1);
        let reopened = reopen_selected_compiler_metadata(&store, &selected)
            .expect("cold reopen selected versioned plane metadata");
        let reopened_manifest = backend_semantic::ir::SemanticPlaneManifest::decode(
            reopened
                .metadata()
                .versioned_planes()
                .expect("reopened plane metadata")
                .artifacts()[0]
                .manifest_bytes(),
        )
        .expect("cold manifest decode");
        assert_eq!(reopened_manifest.semantic_generation(), expected_generation);
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove valid plane store");

        let second_attempt = attempt();
        let (image, image_object) = compiler_image();
        let wrong_generation = backend_semantic::ir::GenerationId::from_canonical_bytes(
            b"not the selected compiler image bytes",
        );
        assert_ne!(
            wrong_generation,
            backend_semantic::ir::GenerationId::from_canonical_bytes(image_object.bytes()),
        );
        let plane_publication = semantic_planes(wrong_generation);
        let metadata =
            metadata_with_versioned_planes(image, Some(plane_publication.metadata().clone()));
        let envelope =
            CompilerPublicationEnvelope::new(&second_attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let (store, receipt) = publish_envelope_closure_with_objects(
            &directory,
            &envelope,
            &metadata,
            plane_publication.objects(),
            false,
        );
        let candidate = envelope
            .candidate(second_attempt, *receipt.closure().as_bytes())
            .expect("candidate with mismatched plane generation");
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        assert!(
            verifier
                .verify_closure(&ClosureClaim::for_candidate(&candidate))
                .is_err()
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove mismatched plane store");
    }

    #[test]
    fn plane_generation_scopes_to_one_image_in_multi_image_publications() {
        let initial_attempt = attempt();
        let (first_image, first_object) = compiler_image_from_bytes(b"first compiler image");
        let (second_image, second_object) = compiler_image_from_bytes(b"second compiler image");
        let first_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(first_object.bytes());
        let second_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(second_object.bytes());
        let first_planes =
            semantic_planes_for_image(first_generation, b"first image independent plane");
        let second_planes =
            semantic_planes_for_image(second_generation, b"second image independent plane");
        let planes = VersionedPlaneMetadata::from_artifacts(vec![
            plane_artifact_at(&first_planes, 0),
            plane_artifact_at(&second_planes, 1),
        ])
        .expect("exact catalog for both image manifests");
        let metadata = metadata_with_images(vec![first_image, second_image], Some(planes));
        let envelope =
            CompilerPublicationEnvelope::new(&initial_attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let image_objects = [first_object, second_object];
        let plane_objects = combine_plane_objects(&[&first_planes, &second_planes]);
        let (store, receipt) = publish_envelope_closure_with_images(
            &directory,
            &envelope,
            &metadata,
            &image_objects,
            &plane_objects,
            false,
        );
        let candidate = envelope
            .candidate(initial_attempt.clone(), *receipt.closure().as_bytes())
            .expect("candidate from stored multi-image closure");
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        verifier
            .verify_closure(&ClosureClaim::for_candidate(&candidate))
            .expect("each catalog image key identifies its exact compiler image");
        let selected = selected_generation(&initial_attempt, &candidate, 1);
        let reopened = reopen_selected_compiler_metadata(&store, &selected)
            .expect("cold reopen multi-image plane publication");
        assert_eq!(reopened.metadata().images().len(), 2);
        assert_eq!(
            reopened
                .metadata()
                .versioned_planes()
                .expect("selected plane catalog")
                .artifacts()
                .iter()
                .map(|artifact| artifact.image_key().semantic_generation())
                .collect::<Vec<_>>(),
            [first_generation, second_generation],
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove multi-image store");

        let swapped_attempt = attempt();
        let (first_image, first_object) = compiler_image_from_bytes(b"first compiler image");
        let (second_image, second_object) = compiler_image_from_bytes(b"second compiler image");
        let first_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(first_object.bytes());
        let second_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(second_object.bytes());
        let first_planes =
            semantic_planes_for_image(second_generation, b"swapped first image plane");
        let second_planes =
            semantic_planes_for_image(first_generation, b"swapped second image plane");
        let swapped_planes = VersionedPlaneMetadata::from_artifacts(vec![
            plane_artifact_at(&first_planes, 0),
            plane_artifact_at(&second_planes, 1),
        ])
        .expect("internally canonical swapped catalog");
        let metadata = metadata_with_images(vec![first_image, second_image], Some(swapped_planes));
        let envelope =
            CompilerPublicationEnvelope::new(&swapped_attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let plane_objects = combine_plane_objects(&[&first_planes, &second_planes]);
        let (store, receipt) = publish_envelope_closure_with_images(
            &directory,
            &envelope,
            &metadata,
            &[first_object, second_object],
            &plane_objects,
            false,
        );
        let candidate = envelope
            .candidate(swapped_attempt, *receipt.closure().as_bytes())
            .expect("candidate with swapped image associations");
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        assert!(
            verifier
                .verify_closure(&ClosureClaim::for_candidate(&candidate))
                .is_err()
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove swapped-image store");

        let omitted_attempt = attempt();
        let (first_image, first_object) = compiler_image_from_bytes(b"first compiler image");
        let (second_image, second_object) = compiler_image_from_bytes(b"second compiler image");
        let first_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(first_object.bytes());
        let second_generation =
            backend_semantic::ir::GenerationId::from_canonical_bytes(second_object.bytes());
        let first_planes =
            semantic_planes_for_image(first_generation, b"first image independent plane");
        let second_planes =
            semantic_planes_for_image(second_generation, b"second image independent plane");
        let planes = VersionedPlaneMetadata::from_artifacts(vec![
            plane_artifact_at(&first_planes, 0),
            plane_artifact_at(&second_planes, 1),
        ])
        .expect("exact catalog for both image manifests");
        let metadata = metadata_with_images(vec![first_image, second_image], Some(planes));
        let envelope =
            CompilerPublicationEnvelope::new(&omitted_attempt, &metadata).expect("valid envelope");
        let directory = scratch_store();
        let plane_objects = combine_plane_objects(&[&first_planes, &second_planes]);
        let (store, receipt) = publish_envelope_closure_with_images(
            &directory,
            &envelope,
            &metadata,
            &[first_object],
            &plane_objects,
            false,
        );
        let candidate = envelope
            .candidate(omitted_attempt, *receipt.closure().as_bytes())
            .expect("candidate with omitted second compiler image");
        let verifier = FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        );
        assert!(
            verifier
                .verify_closure(&ClosureClaim::for_candidate(&candidate))
                .is_err()
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("remove omitted-image store");
    }

    #[test]
    fn large_asymmetric_plane_manifest_builds_one_sorted_segment_index() {
        use backend_semantic::ir::{
            LanguageProfile, RustEdition, SemanticBuildIdentity, SemanticInputWitness,
            SemanticIrPlane, SemanticPlane, SemanticPlaneKind, SemanticPlaneManifest,
            SemanticPlaneSegment,
        };
        use backend_version::{Coverage, ScopeRoot};

        const SEGMENTS: usize = 20_000;
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut segments = Vec::with_capacity(SEGMENTS);
        for ordinal in 0..SEGMENTS {
            let ordinal = u64::try_from(ordinal).expect("ordinal bounded");
            let mut key = [0; 32];
            key[..8].copy_from_slice(&ordinal.to_be_bytes());
            let payload = ordinal.to_be_bytes();
            segments.push(
                SemanticPlaneSegment::from_payload(kind, key, key, 1, &payload)
                    .expect("checked ordered segment"),
            );
        }
        let manifest = SemanticPlaneManifest::new(
            backend_semantic::ir::GenerationId::from_canonical_bytes(b"large semantic image"),
            SemanticBuildIdentity::new(
                [1; 32],
                [2; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                backend_semantic::vocabulary::Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
                [6; 32],
            ),
            SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32])),
            vec![
                SemanticPlane::claimed(kind, segments, Coverage::Partial)
                    .expect("asymmetric claim-only IR plane"),
            ],
        )
        .expect("large bounded manifest");

        let index = semantic_segment_index(&manifest).expect("one-pass sorted index");
        assert_eq!(index.len(), SEGMENTS);
        let first_id = *manifest.planes()[0].segments()[0].id_claim().as_bytes();
        let middle_id = *manifest.planes()[0].segments()[SEGMENTS / 2]
            .id_claim()
            .as_bytes();
        let last_id = *manifest.planes()[0].segments()[SEGMENTS - 1]
            .id_claim()
            .as_bytes();
        for expected_id in [first_id, middle_id, last_id] {
            let position = index
                .binary_search_by_key(&expected_id, |(segment_id, _, _)| *segment_id)
                .expect("segment resolves through the sorted index");
            assert_eq!(index[position].0, expected_id);
        }
    }
}
