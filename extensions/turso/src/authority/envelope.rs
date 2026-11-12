//! Canonical compiler publication envelope stored inside an immutable closure.
//!
//! The envelope commits to the exact compiler attempt, logical generation,
//! physical inner pack, and its sorted object members. It deliberately omits
//! the outer `ClosureId`: that ID is derived from a closure containing this
//! envelope and would create a content-addressing cycle if embedded here.

#[path = "borrowed_metadata.rs"]
mod borrowed_metadata;
#[path = "borrowed_publication.rs"]
mod borrowed_publication;
pub use borrowed_publication::{visit_selected_publication_wire, SelectedPublicationWire};

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
use backend_semantic::ir::{
    COMPILATION_MANIFEST_HEADER_BYTES as SEMANTIC_MANIFEST_HEADER_BYTES,
    COMPILATION_MANIFEST_MAGIC,
    COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES as SEMANTIC_MANIFEST_ENTRY_BYTES,
    CompilationManifestFormat,
};
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
const PROGRAM_METADATA_VERSION: u16 = 5;
const COVERAGE_METADATA_VERSION: u16 = 6;
const TYPESCRIPT_SNAPSHOT_METADATA_VERSION: u16 = 7;
const CSHARP_EVIDENCE_METADATA_VERSION: u16 = 8;
const NATIVE_HEADERS_METADATA_VERSION: u16 = 9;
const NATIVE_LINES_METADATA_VERSION: u16 = 10;
const MAX_PROFILE_BYTES: usize = 256;
/// Maximum number of members in one authenticated compiler publication closure.
pub const MAX_COMPILER_PUBLICATION_MEMBER_IDS: usize = 100_000;
const MAX_MEMBER_IDS: usize = MAX_COMPILER_PUBLICATION_MEMBER_IDS;
const MAX_MANIFEST_BYTES: usize =
    SEMANTIC_MANIFEST_HEADER_BYTES + MAX_MEMBER_IDS * SEMANTIC_MANIFEST_ENTRY_BYTES;
const PACK_DOMAIN: &[u8] = b"backend.turso.compiler-inner-pack.v1\0";
const COMPILATION_BINDING_BYTES: usize = 108;
const SEMANTIC_MANIFEST_MAGIC: &[u8; 8] = &COMPILATION_MANIFEST_MAGIC;
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

/// Schema of witness-bearing metadata. Legacy metadata remains byte-identical v4.
pub(super) const COMPILER_PROGRAM_METADATA_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc002, 5);
/// Metadata with an authenticated compiler-observed coverage member.
pub(super) const COMPILER_COVERAGE_METADATA_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc002, 6);
/// Metadata with a generation-bound TypeScript read-closure certificate.
pub const COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc002, 7);
/// Metadata with one bounded C# captured invocation evidence aggregate.
pub const COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc002, 8);
/// Metadata and typed object schemas for compiler-attested native TypeScript headers.
pub const COMPILER_NATIVE_HEADERS_METADATA_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc002, 9);
/// Metadata binding an exact native source line aggregate.
pub const COMPILER_NATIVE_LINES_METADATA_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc002, 10);
pub const COMPILER_NATIVE_LINES_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc00f, 1);
const MAX_COMPILER_NATIVE_LINES_BYTES: usize = 64 * 1024 * 1024;

/// A typed, bounded source-line member; selected closure admission supplies authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerNativeLinesMember { object_id: AuthorityHash, byte_length: u32 }
impl CompilerNativeLinesMember {
    /// Admits a bounded, authenticated view without allocating another copy.
    pub fn from_verified_view(view: backend_store::VerifiedObjectView<'_>, checkpoint: &mut impl FnMut(usize) -> Result<(), CompilerEnvelopeError>) -> Result<Self, CompilerEnvelopeError> {
        let bytes = view.bytes();
        if bytes.is_empty() || bytes.len() > MAX_COMPILER_NATIVE_LINES_BYTES || view.schema() != COMPILER_NATIVE_LINES_SCHEMA { return Err(CompilerEnvelopeError::NativeHeaders); }
        backend_semantic::native_source_lines::NativeSourceLineAggregateView::parse(bytes, checkpoint).map_err(|e| match e {
        backend_semantic::native_source_lines::BorrowedSourceLinesError::Invalid => CompilerEnvelopeError::NativeHeaders,
        backend_semantic::native_source_lines::BorrowedSourceLinesError::Stopped(e) => e,
    })?;
        let mut key = backend_version::ObjectKeyHasher::new(COMPILER_NATIVE_LINES_SCHEMA, bytes.len()).map_err(|_| CompilerEnvelopeError::NativeHeaders)?;
        for chunk in bytes.chunks(64 * 1024) { checkpoint(chunk.len())?; key.update(chunk).map_err(|_| CompilerEnvelopeError::NativeHeaders)?; }
        if &key.finish().map_err(|_| CompilerEnvelopeError::NativeHeaders)? != view.key() { return Err(CompilerEnvelopeError::NativeHeaders); }
        Ok(Self { object_id: *view.id().as_bytes(), byte_length: u32::try_from(bytes.len()).map_err(|_| CompilerEnvelopeError::NativeHeaders)? })
    }
    pub const fn object_id(&self) -> &AuthorityHash { &self.object_id }
    pub const fn byte_length(&self) -> u32 { self.byte_length }
}

pub const COMPILER_NATIVE_HEADERS_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc00e, 1);
pub const MAX_COMPILER_NATIVE_HEADERS_BYTES: usize = 32 * 1024 * 1024;

/// Bounded object member; its contents become authoritative only via selected closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerNativeHeadersMember { object_id: AuthorityHash, byte_length: u32 }
impl CompilerNativeHeadersMember {
    pub fn from_verified_object(object: VerifiedObjectEnvelope, bytes: &[u8], budget: &mut usize) -> Result<Self, CompilerEnvelopeError> {
        let _aggregate = backend_semantic::native_ts_headers::NativeTsHeaderAggregate::decode_canonical_budgeted(bytes, budget).map_err(|_| CompilerEnvelopeError::NativeHeaders)?;
        let key = native_headers_key(bytes).map_err(|_| CompilerEnvelopeError::NativeHeaders)?;
        if bytes.is_empty() || bytes.len() > MAX_COMPILER_NATIVE_HEADERS_BYTES || object.schema() != COMPILER_NATIVE_HEADERS_SCHEMA || object.key() != &key || object.payload_len() != bytes.len() as u64 { return Err(CompilerEnvelopeError::NativeHeaders); }
        Ok(Self { object_id: *object.id().as_bytes(), byte_length: u32::try_from(bytes.len()).map_err(|_| CompilerEnvelopeError::NativeHeaders)? })
    }
    pub const fn object_id(&self) -> &AuthorityHash { &self.object_id }
    pub const fn byte_length(&self) -> u32 { self.byte_length }
}
/// Constructs the typed aggregate object; the canonical aggregate decoder must validate its contents.
pub fn compiler_native_headers_object(bytes: &[u8], budget: &mut usize) -> Result<TypedObject, CompilerEnvelopeError> {
    if bytes.is_empty() || bytes.len() > MAX_COMPILER_NATIVE_HEADERS_BYTES { return Err(CompilerEnvelopeError::NativeHeaders); }
    // from_value retains one encoded Vec while ObjectVersion encodes another;
    // converting the Vec into an Arc can temporarily hold a third copy and its header.
    let copies = bytes.len().checked_mul(3).and_then(|n| n.checked_add(2 * std::mem::size_of::<usize>())).ok_or(CompilerEnvelopeError::NativeHeaders)?;
    *budget = budget.checked_sub(copies).ok_or(CompilerEnvelopeError::NativeHeaders)?;
    let _aggregate = backend_semantic::native_ts_headers::NativeTsHeaderAggregate::decode_canonical_budgeted(bytes, budget).map_err(|_| CompilerEnvelopeError::NativeHeaders)?;
    Ok(TypedObject::from_value(&ObjectKey::<CompilerNativeHeadersBytesSchema>::from_value(bytes), bytes))
}
/// Bytes authenticated by the selected compiler closure; source/image admission remains separate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedNativeHeadersWitness { bytes: Box<[u8]>, object_id: AuthorityHash }
impl VerifiedNativeHeadersWitness {
    pub fn as_bytes(&self) -> &[u8] { &self.bytes }
    pub const fn object_id(&self) -> &AuthorityHash { &self.object_id }
}

/// Typed CAS schema for the C# invocation evidence aggregate.
pub const COMPILER_CSHARP_EVIDENCE_SCHEMA: SchemaIdentity = SchemaIdentity::new(0x7a, 0xc00c, 1);

/// Claim to an immutable C# evidence aggregate. This is not source authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerCSharpEvidenceMember {
    object_id: AuthorityHash,
    byte_length: u32,
}
impl CompilerCSharpEvidenceMember {
    /// Checks the typed object, content-derived key and bounded extent.
    pub fn from_verified_object(object: VerifiedObjectEnvelope, bytes: &[u8]) -> Result<Self, CompilerEnvelopeError> {
        let key = ObjectKey::<backend_semantic::ir::CSharpInvocationEvidenceSchema>::from_value(bytes);
        if object.schema() != COMPILER_CSHARP_EVIDENCE_SCHEMA || object.key() != key.as_bytes()
            || object.payload_len() != bytes.len() as u64 || bytes.is_empty()
            || bytes.len() > backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES {
            return Err(CompilerEnvelopeError::CSharpInvocationEvidence);
        }
        Ok(Self { object_id: *object.id().as_bytes(), byte_length: u32::try_from(bytes.len()).map_err(|_| CompilerEnvelopeError::CSharpInvocationEvidence)? })
    }
    /// Exact immutable object identity.
    pub const fn object_id(&self) -> &AuthorityHash { &self.object_id }
    /// Exact encoded payload length.
    pub const fn byte_length(&self) -> u32 { self.byte_length }
}

/// Schema of the canonical immutable coverage catalog.
pub(super) const COMPILER_SOURCE_COVERAGE_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc00a, 1);
/// Schema of the bounded TypeScript snapshot certificate payload.
pub const COMPILER_TYPESCRIPT_SNAPSHOT_WITNESS_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc00b, 1);

/// Opaque certificate bytes admitted from selected metadata and closure membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedTypeScriptSnapshotWitness(Box<[u8]>);

impl VerifiedTypeScriptSnapshotWitness {
    /// Exact bytes whose typed object and selected-closure membership were verified.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Bounded pointer to a complete TypeScript compiler observation ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerTypeScriptSnapshotMember {
    object_id: AuthorityHash,
    byte_length: u32,
}

impl CompilerTypeScriptSnapshotMember {
    /// Admits a verified typed ledger object of the exact declared extent.
    pub fn from_verified_object(
        object: VerifiedObjectEnvelope,
        bytes: &[u8],
    ) -> Result<Self, CompilerEnvelopeError> {
        let key =
            ObjectKey::<backend_semantic::ir::TypeScriptSnapshotWitnessSchema>::from_value(bytes);
        if object.schema() != COMPILER_TYPESCRIPT_SNAPSHOT_WITNESS_SCHEMA
            || object.key() != key.as_bytes()
            || object.payload_len() != bytes.len() as u64
            || bytes.is_empty()
            || bytes.len() > backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES
        {
            return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness);
        }
        Ok(Self {
            object_id: *object.id().as_bytes(),
            byte_length: u32::try_from(bytes.len())
                .map_err(|_| CompilerEnvelopeError::TypeScriptSnapshotWitness)?,
        })
    }

    /// Exact immutable object identity.
    pub const fn object_id(&self) -> &AuthorityHash {
        &self.object_id
    }

    /// Exact serialized ledger extent.
    pub const fn byte_length(&self) -> u32 {
        self.byte_length
    }
}

/// A bounded pointer to compiler-observed coverage, stored once in the selected closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerSourceCoverageMember {
    object_id: AuthorityHash,
    identity: AuthorityHash,
    byte_length: u32,
}
impl CompilerSourceCoverageMember {
    /// Admits a verified typed object and its canonical coverage catalog.
    pub fn from_verified_object(
        object: VerifiedObjectEnvelope,
        coverage: &backend_semantic::ir::SourceCoverageObservation,
    ) -> Result<Self, CompilerEnvelopeError> {
        let bytes = coverage.encode();
        let key =
            ObjectKey::<backend_semantic::ir::SourceCoverageSchema>::from_value(bytes.as_slice());
        if object.schema() != COMPILER_SOURCE_COVERAGE_SCHEMA
            || object.key() != key.as_bytes()
            || object.payload_len() != bytes.len() as u64
        {
            return Err(CompilerEnvelopeError::SourceCoverage);
        }
        Ok(Self {
            object_id: *object.id().as_bytes(),
            identity: coverage.identity(),
            byte_length: bytes.len() as u32,
        })
    }
    /// Exact immutable object identity.
    pub const fn object_id(&self) -> &AuthorityHash {
        &self.object_id
    }
    /// Exact canonical catalog digest.
    pub const fn identity(&self) -> &AuthorityHash {
        &self.identity
    }
    /// Exact payload extent.
    pub const fn byte_length(&self) -> u32 {
        self.byte_length
    }
}

/// Exact schema of one full native program source-membership object.
pub(super) const NATIVE_PROGRAM_SOURCE_MANIFEST_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc009, 1);

/// Exact schema of a reference forest containing independently verified leaves.
pub(super) const NATIVE_PROGRAM_FOREST_SCHEMA: SchemaIdentity =
    SchemaIdentity::new(0x7a, 0xc009, 2);

/// A small immutable CAS reference, never a repeated source list in metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerProgramSourceMember {
    object_id: AuthorityHash,
    program: AuthorityHash,
    byte_length: u32,
}
impl CompilerProgramSourceMember {
    /// Binds an already admitted CAS object to its validated canonical membership.
    pub fn from_verified_object(
        object: VerifiedObjectEnvelope,
        sources: &backend_semantic::ir::NativeProgramSourceManifest,
    ) -> Result<Self, CompilerEnvelopeError> {
        Self::from_verified_program_sources(object, &sources.clone().into())
    }
    /// Binds a typed single/forest object. The stored digest is an envelope
    /// identity, not a synthetic union-program coordinate.
    pub fn from_verified_program_sources(
        object: VerifiedObjectEnvelope,
        sources: &backend_semantic::ir::NativeProgramSources,
    ) -> Result<Self, CompilerEnvelopeError> {
        let (schema, key) = if sources.is_forest() {
            (
                NATIVE_PROGRAM_FOREST_SCHEMA,
                *ObjectKey::<backend_semantic::ir::NativeProgramForestSchema>::from_value(
                    sources.as_bytes(),
                )
                .as_bytes(),
            )
        } else {
            (
                NATIVE_PROGRAM_SOURCE_MANIFEST_SCHEMA,
                *ObjectKey::<backend_semantic::ir::NativeProgramSourceManifestSchema>::from_value(
                    sources.as_bytes(),
                )
                .as_bytes(),
            )
        };
        if object.schema() != schema
            || object.key() != &key
            || object.payload_len() != sources.as_bytes().len() as u64
        {
            return Err(CompilerEnvelopeError::NativeProgramSources);
        }
        Ok(Self {
            object_id: *object.id().as_bytes(),
            program: sources.membership_identity(),
            byte_length: sources.as_bytes().len() as u32,
        })
    }
    /// Exact CAS member identity.
    #[must_use]
    pub const fn object_id(&self) -> &AuthorityHash {
        &self.object_id
    }
    /// Single membership ID or typed forest envelope digest, never a union program.
    #[must_use]
    pub const fn membership_identity(&self) -> &AuthorityHash {
        &self.program
    }
    /// Bounded canonical payload length.
    #[must_use]
    pub const fn byte_length(&self) -> u32 {
        self.byte_length
    }
}

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
    manifest_format: CompilationManifestFormat,
    manifest_fragment_count: u32,
    manifest_byte_length: u32,
    binding_bytes: [u8; COMPILATION_BINDING_BYTES],
    binding_identity: AuthorityHash,
    pinned_root: AuthorityHash,
    dependency_set: AuthorityHash,
    images: Box<[CompilerImageMember]>,
    versioned_planes: Option<VersionedPlaneMetadata>,
    semantic_catalog_root: Option<AuthorityHash>,
    native_program_sources: Option<CompilerProgramSourceMember>,
    source_coverage: Option<CompilerSourceCoverageMember>,
    typescript_snapshot: Option<CompilerTypeScriptSnapshotMember>,
    csharp_evidence: Option<CompilerCSharpEvidenceMember>,
    native_headers: Option<CompilerNativeHeadersMember>,
    native_lines: Option<CompilerNativeLinesMember>,
}

impl CompilerPublicationMetadata {
    /// Admits the exact semantic manifest, canonical binding bytes, and image members.
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
        let (manifest_identity, manifest_format, manifest_fragment_count, manifest_byte_length) =
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
            manifest_format,
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
            native_program_sources: None,
            source_coverage: None,
            typescript_snapshot: None,
            csharp_evidence: None,
            native_headers: None,
            native_lines: None,
        })
    }

    /// Adds an exact bounded program object to this compiler closure.
    pub fn with_native_program_sources(
        mut self,
        member: CompilerProgramSourceMember,
    ) -> Result<Self, CompilerEnvelopeError> {
        if self.csharp_evidence.is_some() || metadata_member_ids_checked(&self)?.len() >= MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        self.native_program_sources = Some(member);
        Ok(self)
    }

    /// Optional full native program proof; absence grants no additional authority.
    #[must_use]
    pub const fn native_program_sources(&self) -> Option<&CompilerProgramSourceMember> {
        self.native_program_sources.as_ref()
    }

    /// Attaches the exact compiler coverage object before publication is committed.
    pub fn with_source_coverage(
        mut self,
        member: CompilerSourceCoverageMember,
    ) -> Result<Self, CompilerEnvelopeError> {
        if self.native_program_sources.is_none() || self.csharp_evidence.is_some() {
            return Err(CompilerEnvelopeError::SourceCoverage);
        }
        if self.source_coverage.is_some() || metadata_member_ids_checked(&self)?.len() >= MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        self.source_coverage = Some(member);
        Ok(self)
    }
    /// Legacy metadata records no coverage explanations.
    pub const fn source_coverage(&self) -> Option<&CompilerSourceCoverageMember> {
        self.source_coverage.as_ref()
    }

    /// Adds the complete cold-replay certificate for this exact native program generation.
    pub fn with_typescript_snapshot_witness(
        mut self,
        member: CompilerTypeScriptSnapshotMember,
    ) -> Result<Self, CompilerEnvelopeError> {
        if self.native_program_sources.is_none()
            || self.csharp_evidence.is_some()
            || self.typescript_snapshot.is_some()
            || member.byte_length == 0
            || member.byte_length as usize
                > backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES
            || metadata_member_ids_checked(&self)?.len() >= MAX_MEMBER_IDS
        {
            return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness);
        }
        self.typescript_snapshot = Some(member);
        Ok(self)
    }

    /// Optional complete native TypeScript read-closure certificate.
    #[must_use]
    pub const fn typescript_snapshot_witness(&self) -> Option<&CompilerTypeScriptSnapshotMember> {
        self.typescript_snapshot.as_ref()
    }

    /// Adds one C# invocation aggregate; no TS program/coverage may coexist.
    pub fn with_csharp_evidence(mut self, member: CompilerCSharpEvidenceMember) -> Result<Self, CompilerEnvelopeError> {
        if self.csharp_evidence.is_some() || self.native_program_sources.is_some()
            || self.source_coverage.is_some() || self.typescript_snapshot.is_some()
            || member.byte_length == 0 || member.byte_length as usize > backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES
            || metadata_member_ids_checked(&self)?.len() >= MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::CSharpInvocationEvidence);
        }
        self.csharp_evidence = Some(member);
        Ok(self)
    }
    /// Optional typed C# evidence object; this alone is not source authority.
    pub const fn csharp_evidence(&self) -> Option<&CompilerCSharpEvidenceMember> { self.csharp_evidence.as_ref() }

    /// Binds the aggregate to this exact compiler publication before sealing.
    pub fn with_native_headers(mut self, member: CompilerNativeHeadersMember) -> Result<Self, CompilerEnvelopeError> {
        if self.native_headers.is_some() || self.native_program_sources.is_none() || self.csharp_evidence.is_some() || metadata_member_ids_checked(&self)?.len() >= MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::NativeHeaders); }
        self.native_headers = Some(member);
        Ok(self)
    }
    pub const fn native_headers(&self) -> Option<&CompilerNativeHeadersMember> { self.native_headers.as_ref() }

    /// Binds the exact line-map member to the selected compiler attempt.
    pub fn with_native_lines(mut self, member: CompilerNativeLinesMember) -> Result<Self, CompilerEnvelopeError> {
        if self.native_lines.is_some() || self.native_program_sources.is_none() || self.csharp_evidence.is_some()
            || member.byte_length == 0 || member.byte_length as usize > MAX_COMPILER_NATIVE_LINES_BYTES
            || metadata_member_ids_checked(&self)?.len() >= MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::NativeHeaders); }
        self.native_lines = Some(member);
        Ok(self)
    }
    pub const fn native_lines(&self) -> Option<&CompilerNativeLinesMember> { self.native_lines.as_ref() }

    fn schema(&self) -> SchemaIdentity {
        if self.native_lines.is_some() {
            COMPILER_NATIVE_LINES_METADATA_SCHEMA
        } else if self.native_headers.is_some() {
            COMPILER_NATIVE_HEADERS_METADATA_SCHEMA
        } else if self.csharp_evidence.is_some() {
            COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA
        } else if self.typescript_snapshot.is_some() {
            COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA
        } else if self.source_coverage.is_some() {
            COMPILER_COVERAGE_METADATA_SCHEMA
        } else if self.native_program_sources.is_some() {
            COMPILER_PROGRAM_METADATA_SCHEMA
        } else {
            COMPILER_PUBLICATION_METADATA_SCHEMA
        }
    }

    /// Typed compiler manifest identity.
    #[must_use]
    pub const fn manifest_identity(&self) -> &AuthorityHash {
        &self.manifest_identity
    }

    /// Exact admitted semantic manifest wire generation.
    #[must_use]
    pub const fn manifest_version(&self) -> u16 {
        self.manifest_format.wire_version()
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

    /// Exact canonical semantic manifest bytes bound by this publication.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// Returns canonical, versioned metadata bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut capacity = METADATA_MAGIC.len() + 2 + 4 + self.manifest_bytes.len()
            + COMPILATION_BINDING_BYTES + 4 + self.images.len() * 72 + 1;
        if let Some(planes) = &self.versioned_planes { capacity += 4 + planes.canonical_bytes().len(); }
        if self.typescript_snapshot.is_some() || self.native_headers.is_some() || self.native_lines.is_some() {
            capacity += 2 + usize::from(self.native_program_sources.is_some()) * 68
                + usize::from(self.source_coverage.is_some()) * 68
                + usize::from(self.typescript_snapshot.is_some()) * 36
                + usize::from(self.native_headers.is_some()) * 37;
        } else {
            capacity += usize::from(self.source_coverage.is_some())
                + usize::from(self.native_program_sources.is_some()) * 68
                + usize::from(self.source_coverage.is_some()) * 68
                + usize::from(self.csharp_evidence.is_some()) * 36;
        }
        if self.native_lines.is_some() { capacity += 38; }
        let mut output = Vec::with_capacity(capacity);
        output.extend_from_slice(METADATA_MAGIC);
        output.extend_from_slice(
            &if self.native_lines.is_some() {
                NATIVE_LINES_METADATA_VERSION
            } else if self.native_headers.is_some() {
                NATIVE_HEADERS_METADATA_VERSION
            } else if self.csharp_evidence.is_some() {
                CSHARP_EVIDENCE_METADATA_VERSION
            } else if self.typescript_snapshot.is_some() {
                TYPESCRIPT_SNAPSHOT_METADATA_VERSION
            } else if self.source_coverage.is_some() {
                COVERAGE_METADATA_VERSION
            } else if self.native_program_sources.is_some() {
                PROGRAM_METADATA_VERSION
            } else {
                METADATA_VERSION
            }
            .to_le_bytes(),
        );
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
        if self.typescript_snapshot.is_some() || self.native_headers.is_some() || self.native_lines.is_some() {
            output.push(u8::from(self.native_program_sources.is_some()));
            if let Some(member) = self.native_program_sources {
                output.extend_from_slice(&member.object_id);
                output.extend_from_slice(&member.program);
                output.extend_from_slice(&member.byte_length.to_le_bytes());
            }
            output.push(u8::from(self.source_coverage.is_some()));
            if let Some(member) = self.source_coverage {
                output.extend_from_slice(&member.object_id);
                output.extend_from_slice(&member.identity);
                output.extend_from_slice(&member.byte_length.to_le_bytes());
            }
            if self.native_headers.is_some() || self.native_lines.is_some() { output.push(u8::from(self.typescript_snapshot.is_some())); }
            if let Some(member) = self.typescript_snapshot {
                output.extend_from_slice(&member.object_id);
                output.extend_from_slice(&member.byte_length.to_le_bytes());
            }
        } else {
            if self.source_coverage.is_some() {
                output.push(u8::from(self.native_program_sources.is_some()));
            }
            if let Some(member) = self.native_program_sources {
                output.extend_from_slice(&member.object_id);
                output.extend_from_slice(&member.program);
                output.extend_from_slice(&member.byte_length.to_le_bytes());
            }
            if let Some(member) = self.source_coverage {
                output.extend_from_slice(&member.object_id);
                output.extend_from_slice(&member.identity);
                output.extend_from_slice(&member.byte_length.to_le_bytes());
            }
        }
        if let Some(member) = self.csharp_evidence {
            output.extend_from_slice(&member.object_id);
            output.extend_from_slice(&member.byte_length.to_le_bytes());
        }
        if self.native_lines.is_some() { output.push(u8::from(self.native_headers.is_some())); }
        if let Some(member) = self.native_headers {
            output.extend_from_slice(&member.object_id);
            output.extend_from_slice(&member.byte_length.to_le_bytes());
        }
        if let Some(member) = self.native_lines {
            output.extend_from_slice(&member.object_id);
            output.extend_from_slice(&member.byte_length.to_le_bytes());
        }
        output
    }

    /// Returns the typed FileStore metadata object.
    #[must_use]
    pub fn typed_object(&self) -> TypedObject {
        if self.native_lines.is_some() {
            let key = ObjectKey::<CompilerNativeLinesMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        } else if self.native_headers.is_some() {
            let key = ObjectKey::<CompilerNativeHeadersMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        } else if self.csharp_evidence.is_some() {
            let key = ObjectKey::<CompilerCSharpEvidenceMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        } else if self.typescript_snapshot.is_some() {
            let key = ObjectKey::<CompilerTypeScriptSnapshotMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        } else if self.source_coverage.is_some() {
            let key = ObjectKey::<CompilerCoverageMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        } else if self.native_program_sources.is_some() {
            let key = ObjectKey::<CompilerProgramMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        } else {
            let key = ObjectKey::<CompilerPublicationMetadataSchema>::from_value(self);
            TypedObject::from_value(&key, self)
        }
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
        let version = input.u16()?;
        if !matches!(
            version,
            METADATA_VERSION
                | PROGRAM_METADATA_VERSION
                | COVERAGE_METADATA_VERSION
                | TYPESCRIPT_SNAPSHOT_METADATA_VERSION
                | CSHARP_EVIDENCE_METADATA_VERSION
                | NATIVE_HEADERS_METADATA_VERSION
                | NATIVE_LINES_METADATA_VERSION
        ) {
            return Err(CompilerEnvelopeError::MetadataVersion);
        }
        let manifest_length =
            usize::try_from(input.u32()?).map_err(|_| CompilerEnvelopeError::ManifestFacts)?;
        if manifest_length < SEMANTIC_MANIFEST_HEADER_BYTES || manifest_length > MAX_MANIFEST_BYTES
        {
            return Err(CompilerEnvelopeError::ManifestFacts);
        }
        let manifest_bytes = input.take(manifest_length)?;
        let binding_bytes = input.array::<COMPILATION_BINDING_BYTES>()?;
        let count =
            usize::try_from(input.u32()?).map_err(|_| CompilerEnvelopeError::MemberCount)?;
        if count == 0 || count > MAX_MEMBER_IDS {
            return Err(CompilerEnvelopeError::MemberCount);
        }
        let required = count.checked_mul(72).and_then(|n| n.checked_add(1)).ok_or(CompilerEnvelopeError::Truncated)?;
        if input.bytes.len().saturating_sub(input.offset) < required { return Err(CompilerEnvelopeError::Truncated); }
        // Preflight optional framing without allocating image rows or manifests.
        let mut scan = Decoder { bytes: input.bytes, offset: input.offset };
        scan.take(count.checked_mul(72).ok_or(CompilerEnvelopeError::Truncated)?)?;
        match scan.u8()? {
            0 => {},
            1 => {
                let n = scan.u32()? as usize;
                let nested = scan.take(n)?;
                super::versioned::preflight_versioned_framing(nested).map_err(CompilerEnvelopeError::VersionedPlane)?;
            },
            _ => return Err(CompilerEnvelopeError::VersionedPlaneMetadata),
        }
        let has_program = if matches!(version, COVERAGE_METADATA_VERSION | TYPESCRIPT_SNAPSHOT_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION) {
            match scan.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::NativeProgramSources) }
        } else { version == PROGRAM_METADATA_VERSION };
        if has_program { scan.take(68)?; }
        let has_coverage = match version {
            COVERAGE_METADATA_VERSION => true,
            TYPESCRIPT_SNAPSHOT_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION => match scan.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::SourceCoverage) },
            _ => false,
        };
        if has_coverage { scan.take(68)?; }
        let has_snapshot = match version {
            TYPESCRIPT_SNAPSHOT_METADATA_VERSION => true,
            NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION => match scan.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness) },
            _ => false,
        };
        if has_snapshot { scan.take(36)?; }
        if version == CSHARP_EVIDENCE_METADATA_VERSION || version == NATIVE_HEADERS_METADATA_VERSION { scan.take(36)?; }
        if version == NATIVE_LINES_METADATA_VERSION { match scan.u8()? { 0 => {}, 1 => { scan.take(36)?; }, _ => return Err(CompilerEnvelopeError::NativeHeaders) }; scan.take(36)?; }
        if !scan.is_empty() { return Err(CompilerEnvelopeError::TrailingBytes); }
        let mut images = Vec::new();
        images.try_reserve_exact(count).map_err(|_| CompilerEnvelopeError::MemberCount)?;
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
        let has_program = if matches!(
            version,
            COVERAGE_METADATA_VERSION | TYPESCRIPT_SNAPSHOT_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION
        ) {
            match input.u8()? {
                0 => false,
                1 => true,
                _ => return Err(CompilerEnvelopeError::NativeProgramSources),
            }
        } else {
            version == PROGRAM_METADATA_VERSION
        };
        let native_program_sources = if has_program {
            let member = CompilerProgramSourceMember {
                object_id: input.array::<32>()?,
                program: input.array::<32>()?,
                byte_length: input.u32()?,
            };
            if member.byte_length == 0
                || member.byte_length as usize
                    > backend_semantic::ir::MAX_NATIVE_PROGRAM_MANIFEST_BYTES
            {
                return Err(CompilerEnvelopeError::NativeProgramSources);
            }
            Some(member)
        } else {
            None
        };
        let has_coverage = match version {
            COVERAGE_METADATA_VERSION => true,
            TYPESCRIPT_SNAPSHOT_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION => match input.u8()? {
                0 => false,
                1 => true,
                _ => return Err(CompilerEnvelopeError::SourceCoverage),
            },
            _ => false,
        };
        let source_coverage = if has_coverage {
            let member = CompilerSourceCoverageMember {
                object_id: input.array::<32>()?,
                identity: input.array::<32>()?,
                byte_length: input.u32()?,
            };
            if member.byte_length == 0
                || member.byte_length as usize > backend_semantic::ir::MAX_SOURCE_COVERAGE_BYTES
            {
                return Err(CompilerEnvelopeError::SourceCoverage);
            }
            Some(member)
        } else {
            None
        };
        let has_snapshot = match version {
            TYPESCRIPT_SNAPSHOT_METADATA_VERSION => true,
            NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION => match input.u8()? {
                0 => false, 1 => true,
                _ => return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness),
            },
            _ => false,
        };
        let typescript_snapshot = if has_snapshot {
            let member = CompilerTypeScriptSnapshotMember {
                object_id: input.array::<32>()?,
                byte_length: input.u32()?,
            };
            if member.byte_length == 0
                || member.byte_length as usize
                    > backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES
            {
                return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness);
            }
            Some(member)
        } else {
            None
        };
        let csharp_evidence = if version == CSHARP_EVIDENCE_METADATA_VERSION {
            let member = CompilerCSharpEvidenceMember { object_id: input.array::<32>()?, byte_length: input.u32()? };
            if member.byte_length == 0 || member.byte_length as usize > backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES {
                return Err(CompilerEnvelopeError::CSharpInvocationEvidence);
            }
            Some(member)
        } else { None };
        let has_headers = if version == NATIVE_LINES_METADATA_VERSION { match input.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::NativeHeaders) } } else { version == NATIVE_HEADERS_METADATA_VERSION };
        let native_headers = if has_headers {
            let member = CompilerNativeHeadersMember { object_id: input.array::<32>()?, byte_length: input.u32()? };
            if member.byte_length == 0 || member.byte_length as usize > MAX_COMPILER_NATIVE_HEADERS_BYTES { return Err(CompilerEnvelopeError::NativeHeaders); }
            Some(member)
        } else { None };
        let native_lines = if version == NATIVE_LINES_METADATA_VERSION {
            let member = CompilerNativeLinesMember { object_id: input.array::<32>()?, byte_length: input.u32()? };
            if member.byte_length == 0 || member.byte_length as usize > MAX_COMPILER_NATIVE_LINES_BYTES { return Err(CompilerEnvelopeError::NativeHeaders); }
            Some(member)
        } else { None };
        if !input.is_empty() {
            return Err(CompilerEnvelopeError::TrailingBytes);
        }
        if images
            .windows(2)
            .any(|pair| pair[0].object_id >= pair[1].object_id)
        {
            return Err(CompilerEnvelopeError::MemberOrder);
        }
        let mut metadata = Self::new_with_versioned_planes(
            manifest_bytes,
            binding_bytes,
            images,
            versioned_planes,
        )?;
        if let Some(member) = native_program_sources {
            metadata = metadata.with_native_program_sources(member)?;
        }
        if let Some(member) = source_coverage {
            metadata = metadata.with_source_coverage(member)?;
        }
        if let Some(member) = typescript_snapshot {
            metadata = metadata.with_typescript_snapshot_witness(member)?;
        }
        if let Some(member) = csharp_evidence { metadata = metadata.with_csharp_evidence(member)?; }
        if let Some(member) = native_headers { metadata = metadata.with_native_headers(member)?; }
        if let Some(member) = native_lines { metadata = metadata.with_native_lines(member)?; }
        if metadata.canonical_bytes() != bytes {
            return Err(CompilerEnvelopeError::NonCanonical);
        }
        Ok(metadata)
    }
}

/// Requested retained and temporary allocations of metadata decoding. The
/// authenticated input buffer and closure/member traversal are separate charges.
fn compiler_metadata_decode_bound(bytes: &[u8]) -> Result<usize, CompilerEnvelopeError> {
    let mut input = Decoder::new(bytes);
    if input.take(METADATA_MAGIC.len())? != METADATA_MAGIC { return Err(CompilerEnvelopeError::MetadataMagic); }
    let version = input.u16()?;
    if !matches!(version, METADATA_VERSION | PROGRAM_METADATA_VERSION | COVERAGE_METADATA_VERSION | TYPESCRIPT_SNAPSHOT_METADATA_VERSION | CSHARP_EVIDENCE_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION) { return Err(CompilerEnvelopeError::MetadataVersion); }
    let manifest_len = input.u32()? as usize;
    if manifest_len < SEMANTIC_MANIFEST_HEADER_BYTES || manifest_len > MAX_MANIFEST_BYTES { return Err(CompilerEnvelopeError::ManifestFacts); }
    input.take(manifest_len)?;
    input.take(COMPILATION_BINDING_BYTES)?;
    let count = input.u32()? as usize;
    if count == 0 || count > MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::MemberCount); }
    input.take(count.checked_mul(72).ok_or(CompilerEnvelopeError::Truncated)?)?;
    let mut bound = bytes.len().checked_add(manifest_len).ok_or(CompilerEnvelopeError::MemberCount)?; // canonical output + retained manifest
    let per_image = std::mem::size_of::<CompilerImageMember>().checked_mul(2).ok_or(CompilerEnvelopeError::MemberCount)?;
    let bitmap = count.checked_add(7).ok_or(CompilerEnvelopeError::MemberCount)? / 8;
    bound = bound.checked_add(count.checked_mul(per_image).ok_or(CompilerEnvelopeError::MemberCount)?)
        .and_then(|n| n.checked_add(bitmap)).ok_or(CompilerEnvelopeError::MemberCount)?;
    let mut nested_wire_len = 0usize;
    let mut nested_physical_slots = 0usize;
    match input.u8()? {
        0 => {},
        1 => {
            let n = input.u32()? as usize;
            nested_wire_len = n;
            let nested = input.take(n)?;
            let (nested_bound, slots) = super::versioned::versioned_decode_allocation_bound(nested).map_err(CompilerEnvelopeError::VersionedPlane)?;
            nested_physical_slots = slots;
            // Outer canonical encoding asks for the versioned wire once when
            // sizing and once when emitting it.
            bound = bound.checked_add(nested_bound).and_then(|v| v.checked_add(n.checked_mul(2)?)).ok_or(CompilerEnvelopeError::MemberCount)?;
        },
        _ => return Err(CompilerEnvelopeError::VersionedPlaneMetadata),
    }
    // The ordinary decoder performs the full optional-tail preflight before
    // materialization. This scan checks the same flags and extents here.
    let has_program = if matches!(version, COVERAGE_METADATA_VERSION | TYPESCRIPT_SNAPSHOT_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION) {
        match input.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::NativeProgramSources) }
    } else { version == PROGRAM_METADATA_VERSION };
    if has_program { input.take(68)?; }
    let has_coverage = match version {
        COVERAGE_METADATA_VERSION => true,
        TYPESCRIPT_SNAPSHOT_METADATA_VERSION | NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION => match input.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::SourceCoverage) },
        _ => false,
    };
    if has_coverage { input.take(68)?; }
    let has_snapshot = match version {
        TYPESCRIPT_SNAPSHOT_METADATA_VERSION => true,
        NATIVE_HEADERS_METADATA_VERSION | NATIVE_LINES_METADATA_VERSION => match input.u8()? { 0 => false, 1 => true, _ => return Err(CompilerEnvelopeError::TypeScriptSnapshotWitness) },
        _ => false,
    };
    if has_snapshot { input.take(36)?; }
    if version == CSHARP_EVIDENCE_METADATA_VERSION || version == NATIVE_HEADERS_METADATA_VERSION { input.take(36)?; }
    if version == NATIVE_LINES_METADATA_VERSION { match input.u8()? { 0 => {}, 1 => { input.take(36)?; }, _ => return Err(CompilerEnvelopeError::NativeHeaders) }; input.take(36)?; }
    if !input.is_empty() { return Err(CompilerEnvelopeError::TrailingBytes); }
    // Each optional setter checks an exact member inventory. Its canonical
    // metadata object ID is streamed from one temporary canonical encoding;
    // The inventory reserves raw image/plane/optional slots before deduping.
    let setters = usize::from(has_program) + usize::from(has_coverage) + usize::from(has_snapshot)
        + usize::from(version == CSHARP_EVIDENCE_METADATA_VERSION)
        + usize::from(version == NATIVE_HEADERS_METADATA_VERSION) + 2 * usize::from(version == NATIVE_LINES_METADATA_VERSION);
    let inventory_slots = count.checked_add(nested_physical_slots).and_then(|v| v.checked_add(7)).ok_or(CompilerEnvelopeError::MemberCount)?;
    let per_setter = bytes.len().checked_add(nested_wire_len.checked_mul(2).ok_or(CompilerEnvelopeError::MemberCount)?)
        .and_then(|v| v.checked_add(inventory_slots.checked_mul(32)?)).ok_or(CompilerEnvelopeError::MemberCount)?;
    bound = bound.checked_add(setters.checked_mul(per_setter).ok_or(CompilerEnvelopeError::MemberCount)?).ok_or(CompilerEnvelopeError::MemberCount)?;
    Ok(bound)
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
        self.matches_selected_with_verified_id(selected, &self.object_id())
    }

    fn matches_selected_with_verified_id(&self, selected: &SelectedGeneration, verified_id: &AuthorityHash) -> bool {
        let (attempt_id, attempt_epoch) = selected.attempt();
        self.namespace == *selected.namespace()
            && self.attempt_id == *attempt_id
            && self.attempt_epoch == attempt_epoch
            && self.scheduler_fence() == selected.scheduler_fence()
            && self.input_digest == *selected.input_digest()
            && self.logical_generation == *selected.target_root()
            && self.physical_pack_id == *selected.pack_id()
            && self.semantic_manifest_root == selected.semantic_manifest_root().copied()
            && verified_id == selected.candidate_id()
    }

    /// Confirms metadata identity, logical root, and exact compiler member set.
    pub fn validate_metadata(
        &self,
        metadata: &CompilerPublicationMetadata,
    ) -> Result<(), CompilerEnvelopeError> {
        self.validate_metadata_with_verified_id(metadata, &metadata.object_id())
    }

    fn validate_metadata_with_verified_id(&self, metadata: &CompilerPublicationMetadata, verified_id: &AuthorityHash) -> Result<(), CompilerEnvelopeError> {
        if &self.metadata_id != verified_id
            || self.logical_generation != metadata.binding_identity
            || self.semantic_manifest_root != metadata.semantic_manifest_root().copied()
            || self.member_ids.as_ref() != metadata_member_ids_with_id(metadata, *verified_id).as_slice()
        {
            return Err(CompilerEnvelopeError::MetadataMismatch);
        }
        Ok(())
    }

    /// Returns the canonical, versioned envelope bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let capacity = MAGIC.len() + 2 + 5 * 4
            + self.namespace.package().len() + self.namespace.source().len()
            + self.namespace.branch().len() + self.namespace.environment().len() + self.profile().len()
            + 1 + 16 + 8 + 32 * 4 + 1
            + usize::from(self.semantic_manifest_root.is_some()) * 32
            + 32 + 4 + self.member_ids.len() * 32;
        let mut output = Vec::with_capacity(capacity);
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
        let required = member_count.checked_mul(32).ok_or(CompilerEnvelopeError::Truncated)?;
        if input.bytes.len().saturating_sub(input.offset) < required { return Err(CompilerEnvelopeError::Truncated); }
        let mut member_ids = Vec::new();
        member_ids.try_reserve_exact(member_count).map_err(|_| CompilerEnvelopeError::MemberCount)?;
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
        if envelope.canonical_bytes() != bytes {
            return Err(CompilerEnvelopeError::NonCanonical);
        }
        // Canonicality is established above. Derive the physical identity
        // directly from those bytes without rebuilding key/version payloads.
        let object_id = backend_store::untrusted_object_id_for_encoded_value(COMPILER_PUBLICATION_ENVELOPE_SCHEMA, bytes)
            .map_err(|_| CompilerEnvelopeError::ByteLengthOverflow)?;
        if envelope.member_ids.binary_search(object_id.as_bytes()).is_ok() {
            return Err(CompilerEnvelopeError::SelfMember);
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
    stopped: Option<&'store dyn Fn() -> bool>,
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
            stopped: None,
        }
    }

    pub(super) fn with_stop(mut self, stopped: &'store dyn Fn() -> bool) -> Self {
        self.stopped = Some(stopped);
        self
    }
}

/// Requested allocations for envelope decode, excluding authenticated input
/// and closure-index traversal. The complete row extent is checked first.
fn compiler_envelope_decode_bound(bytes: &[u8]) -> Result<usize, CompilerEnvelopeError> {
    let mut input = Decoder::new(bytes);
    if input.take(MAGIC.len())? != MAGIC { return Err(CompilerEnvelopeError::Magic); }
    if input.u16()? != ENVELOPE_VERSION { return Err(CompilerEnvelopeError::Version); }
    let mut names = 0usize;
    let mut profile_len = 0usize;
    for index in 0..5 {
        if index == 4 && input.u8()? != 1 { return Err(CompilerEnvelopeError::Profile); }
        let n = input.u32()? as usize;
        if n == 0 || n > if index == 4 { MAX_PROFILE_BYTES } else { 4096 } { return Err(CompilerEnvelopeError::Truncated); }
        let name = input.take(n)?;
        std::str::from_utf8(name).map_err(|_| CompilerEnvelopeError::Truncated)?;
        names = names.checked_add(n).ok_or(CompilerEnvelopeError::MemberCount)?;
        if index == 4 { profile_len = n; }
    }
    input.take(16 + 8 + 32 * 4)?;
    match input.u8()? { 0 => {}, 1 => { input.take(32)?; }, _ => return Err(CompilerEnvelopeError::VersionedPlaneMetadata) }
    input.take(32)?;
    let count = input.u32()? as usize;
    if count == 0 || count > MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::MemberCount); }
    let row_bytes = count.checked_mul(32).ok_or(CompilerEnvelopeError::MemberCount)?;
    input.take(row_bytes)?;
    if !input.is_empty() { return Err(CompilerEnvelopeError::TrailingBytes); }
    bytes.len().checked_add(names.checked_mul(2).ok_or(CompilerEnvelopeError::MemberCount)?)
        .and_then(|n| n.checked_add(profile_len))
        .and_then(|n| n.checked_add(row_bytes.checked_mul(2)?))
        .ok_or(CompilerEnvelopeError::MemberCount)
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
    if !matches!(
        metadata_object.schema(),
        COMPILER_PUBLICATION_METADATA_SCHEMA
            | COMPILER_PROGRAM_METADATA_SCHEMA
            | COMPILER_COVERAGE_METADATA_SCHEMA
            | COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA
            | COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA
            | COMPILER_NATIVE_HEADERS_METADATA_SCHEMA
            | COMPILER_NATIVE_LINES_METADATA_SCHEMA
    ) {
        return Err("selected compiler metadata has the wrong schema".to_owned());
    }
    let metadata = CompilerPublicationMetadata::decode(metadata_object.bytes())
        .map_err(|error| error.to_string())?;
    if metadata_object.schema() != metadata.schema()
        || metadata.object_id() != *metadata_object.id().as_bytes()
    {
        return Err("compiler metadata version differs from its typed object".to_owned());
    }
    envelope
        .validate_metadata(&metadata)
        .map_err(|error| error.to_string())?;
    let _ = reopen_typescript_snapshot_witness_bounded(store, &manifest, &metadata)?;
    let _ = reopen_csharp_evidence_bounded(store, &manifest, &metadata)?;
    if selected.semantic_manifest_root().copied() != metadata.semantic_manifest_root().copied() {
        return Err("selected semantic-plane root differs from compiler metadata".to_owned());
    }
    if reopen_source_coverage(&manifest, &metadata)?
        .as_ref()
        .is_some_and(|coverage| coverage.input_root() != selected.input_digest())
    {
        return Err("compiler coverage input differs from selected attempt".to_owned());
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
    if !matches!(
        metadata_object.schema(),
        COMPILER_PUBLICATION_METADATA_SCHEMA
            | COMPILER_PROGRAM_METADATA_SCHEMA
            | COMPILER_COVERAGE_METADATA_SCHEMA
            | COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA
            | COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA
            | COMPILER_NATIVE_HEADERS_METADATA_SCHEMA
            | COMPILER_NATIVE_LINES_METADATA_SCHEMA
    ) {
        return Err("selected compiler metadata has the wrong schema".to_owned());
    }
    let metadata = CompilerPublicationMetadata::decode(metadata_object.bytes())
        .map_err(|error| error.to_string())?;
    if metadata_object.schema() != metadata.schema()
        || metadata.object_id() != *metadata_object.id().as_bytes()
    {
        return Err("compiler metadata version differs from its typed object".to_owned());
    }
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

/// Reopens the cold TypeScript witness only from the selected immutable
/// generation's authenticated metadata and exact closure.
pub fn reopen_selected_typescript_snapshot_witness(
    store: &FileStore,
    selected: &SelectedGeneration,
) -> Result<Option<VerifiedTypeScriptSnapshotWitness>, String> {
    let reopened = reopen_selected_compiler_metadata(store, selected)?;
    let manifest = store
        .open_closure_claim(ArtifactClosureClaim::from_bytes(*selected.closure_id()))
        .map_err(|error| format!("open selected TypeScript snapshot closure: {error:?}"))?;
    if manifest.id().as_bytes() != selected.closure_id() {
        return Err("selected TypeScript snapshot closure identity mismatch".to_owned());
    }
    reopen_typescript_snapshot_witness_bounded(store, &manifest, reopened.metadata())
        .map(|bytes| bytes.map(|bytes| VerifiedTypeScriptSnapshotWitness(bytes.into_boxed_slice())))
}

/// Borrowed, fully qualified selected envelope. The object bytes must have
/// already passed physical, typed-version, and key verification.
struct BorrowedSelectedEnvelope<'a> {
    metadata_id: AuthorityHash,
    logical_generation: AuthorityHash,
    semantic_root: Option<AuthorityHash>,
    member_wire: &'a [u8],
}
impl BorrowedSelectedEnvelope<'_> {
    fn members(&self) -> impl Iterator<Item = AuthorityHash> + '_ {
        self.member_wire.chunks_exact(32).map(|chunk| { let mut id = [0; 32]; id.copy_from_slice(chunk); id })
    }
}
fn borrowed_selected_envelope<'a>(bytes: &'a [u8], selected: &SelectedGeneration, id: &AuthorityHash, checkpoint: &dyn Fn() -> Result<(), CompilerEnvelopeError>) -> Result<BorrowedSelectedEnvelope<'a>, CompilerEnvelopeError> {
    let mut input = Decoder::new(bytes);
    if input.take(MAGIC.len())? != MAGIC { return Err(CompilerEnvelopeError::Magic); }
    if input.u16()? != ENVELOPE_VERSION { return Err(CompilerEnvelopeError::Version); }
    let namespace = selected.namespace();
    for expected in [namespace.package(), namespace.source(), namespace.branch(), namespace.environment()] {
        if input.borrowed_string(4096)? != expected { return Err(CompilerEnvelopeError::Namespace); }
    }
    if input.u8()? != 1 { return Err(CompilerEnvelopeError::Profile); }
    if Some(input.borrowed_string(MAX_PROFILE_BYTES)?) != namespace.plane().profile() { return Err(CompilerEnvelopeError::Profile); }
    let attempt_id = input.array::<16>()?;
    let epoch = input.u64()?;
    let fence = input.array::<32>()?;
    let digest = input.array::<32>()?;
    let logical_generation = input.array::<32>()?;
    let metadata_id = input.array::<32>()?;
    let semantic_root = decode_optional_hash(&mut input)?;
    let pack = input.array::<32>()?;
    let count = input.u32()? as usize;
    if count == 0 || count > MAX_MEMBER_IDS { return Err(CompilerEnvelopeError::MemberCount); }
    let rows = input.take(count.checked_mul(32).ok_or(CompilerEnvelopeError::Truncated)?)?;
    if !input.is_empty() { return Err(CompilerEnvelopeError::TrailingBytes); }
    let (selected_attempt, selected_epoch) = selected.attempt();
    if &attempt_id != selected_attempt || epoch != selected_epoch || (epoch, fence) != selected.scheduler_fence()
        || &digest != selected.input_digest() || &logical_generation != selected.target_root()
        || semantic_root != selected.semantic_manifest_root().copied() || &pack != selected.pack_id() || id != selected.candidate_id()
    { return Err(CompilerEnvelopeError::AttemptMismatch); }
    let mut hasher = blake3::Hasher::new();
    hasher.update(PACK_DOMAIN); hasher.update(&(count as u64).to_le_bytes());
    let mut previous: Option<&[u8]> = None;
    let mut has_metadata = false;
    for row in rows.chunks_exact(32) {
        checkpoint()?;
        if previous.is_some_and(|prior| prior >= row) { return Err(CompilerEnvelopeError::MemberOrder); }
        if row == id.as_slice() { return Err(CompilerEnvelopeError::SelfMember); }
        if row == metadata_id.as_slice() { has_metadata = true; }
        hasher.update(row); previous = Some(row);
    }
    if !has_metadata { return Err(CompilerEnvelopeError::MetadataMissing); }
    if hasher.finalize().as_bytes() != &pack { return Err(CompilerEnvelopeError::PackIdentity); }
    Ok(BorrowedSelectedEnvelope { metadata_id, logical_generation, semantic_root, member_wire: rows })
}

/// Caller-owned peak-memory credit and finite selected-admission work tokens.
/// A work token admits one schema-bounded node operation, object chunk, or
/// metadata decode; callers must set an operation-appropriate finite limit.
#[derive(Debug)]
pub struct SelectedNativeHeaderBudget { memory: usize, work: std::cell::Cell<u64> }
impl SelectedNativeHeaderBudget {
    pub fn new(memory_bytes: usize, work_units: u64) -> Self { Self { memory: memory_bytes, work: std::cell::Cell::new(work_units) } }
    pub const fn remaining_memory(&self) -> usize { self.memory }
    /// Reserves retained caller allocations on the same admission ledger.
    /// Credit remains spent while the allocation or any derived owner is alive.
    pub fn reserve_retained(&mut self, bytes: usize) -> Result<(), backend_store::StoreError> {
        self.memory = self.memory.checked_sub(bytes).ok_or(backend_store::StoreError::Bounds)?;
        Ok(())
    }
    pub fn remaining_work(&self) -> u64 { self.work.get() }
    /// Debits bounded consumer operations against the same selected-admission ledger.
    /// The callback must represent the live operation's cancellation or deadline.
    pub fn checkpoint(&self, units: u64, cancelled: &dyn Fn() -> bool) -> Result<(), backend_store::StoreError> {
        if cancelled() { return Err(backend_store::StoreError::Bounds); }
        self.work.set(self.work.get().checked_sub(units).ok_or(backend_store::StoreError::Bounds)?);
        Ok(())
    }
}
fn verify_metadata_member_bitmap(
    envelope: &BorrowedSelectedEnvelope<'_>, metadata: &CompilerPublicationMetadata,
    metadata_id: AuthorityHash, budget: &mut SelectedNativeHeaderBudget, cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let count = envelope.member_wire.len() / 32;
    let bytes = count.checked_add(7).ok_or("member bitmap overflow")? / 8;
    let before = budget.memory;
    let result = (|| {
        let mut seen = backend_store::try_allocate_exact_zeroed_bytes_budgeted(bytes, &mut budget.memory)
            .map_err(|e| format!("member bitmap: {e:?}"))?;
        let ids = std::iter::once(metadata_id)
            .chain(metadata.native_program_sources.iter().map(|x| *x.object_id()))
            .chain(metadata.source_coverage.iter().map(|x| *x.object_id()))
            .chain(metadata.typescript_snapshot.iter().map(|x| *x.object_id()))
            .chain(metadata.csharp_evidence.iter().map(|x| *x.object_id()))
            .chain(metadata.native_headers.iter().map(|x| *x.object_id()))
            .chain(metadata.images.iter().map(|x| *x.object_id()))
            .chain(metadata.versioned_planes.iter().flat_map(|p| p.artifacts().iter().map(|a| *a.manifest_object_id())))
            .chain(metadata.versioned_planes.iter().flat_map(|p| p.members().map(|m| *m.object_id())));
        for id in ids {
            let mut low = 0usize; let mut high = count;
            while low < high {
                header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected inventory stopped: {e:?}"))?;
                let mid = low + (high - low) / 2;
                let cell = &envelope.member_wire[mid * 32..mid * 32 + 32];
                if cell < id.as_slice() { low = mid + 1; } else { high = mid; }
            }
            if low == count || envelope.member_wire[low * 32..low * 32 + 32] != id {
                return Err("selected compiler metadata names a foreign member".into());
            }
            seen[low / 8] |= 1 << (low % 8);
        }
        for index in 0..count {
            header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected inventory stopped: {e:?}"))?;
            if seen[index / 8] & (1 << (index % 8)) == 0 { return Err("selected compiler envelope has an unconsumed member".into()); }
        }
        Ok(())
    })();
    budget.memory = before;
    result
}

fn header_checkpoint(work: &std::cell::Cell<u64>, cancelled: &dyn Fn() -> bool) -> Result<(), backend_store::StoreError> {
    if cancelled() { return Err(backend_store::StoreError::Bounds); }
    work.set(work.get().checked_sub(1).ok_or(backend_store::StoreError::Bounds)?);
    Ok(())
}
fn header_reserve(memory: &mut usize, bytes: usize) -> Result<(), String> {
    *memory = memory.checked_sub(bytes).ok_or("selected header memory budget exceeded")?; Ok(())
}
fn header_member_bounded(store: &FileStore, pin: &backend_store::GcPinGuard, closure: backend_store::ClosureId, id: AuthorityHash,
    budget: &mut SelectedNativeHeaderBudget, cancelled: &dyn Fn() -> bool) -> Result<u64, String> {
    let before = budget.memory;
    let work = &budget.work;
    let result = store.contains_closure_member_bounded(pin, closure, UntrustedObjectId::from_bytes(id), 32, &mut budget.memory,
        |_| header_checkpoint(work, cancelled),
        |_, _| header_checkpoint(work, cancelled));
    // Membership returns only scalars; its node and locator reservations are temporary.
    budget.memory = before;
    let (found, count) = result.map_err(|e| format!("bounded selected member: {e:?}"))?;
    if !found { return Err("selected compiler member absent from closure".into()); }
    Ok(count)
}
fn header_object_bounded(store: &FileStore, pin: &backend_store::GcPinGuard, id: AuthorityHash,
    schemas: &[SchemaIdentity], maximum: usize, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool) -> Result<(Box<[u8]>, SchemaIdentity, AuthorityHash), String> {
    let work = &budget.work;
    let reader = store.open_bounded_nonrelation_object(pin, UntrustedObjectId::from_bytes(id), schemas, None, maximum,
        &mut budget.memory, |_| header_checkpoint(work, cancelled)).map_err(|e| format!("open bounded compiler object: {e:?}"))?;
    let len = reader.payload_len();
    // Exact requested-layout, fallible allocation; no intermediate Vec.
    let mut bytes = backend_store::try_allocate_exact_zeroed_bytes_budgeted(len, &mut budget.memory)
        .map_err(|e| format!("compiler payload allocation: {e:?}"))?;
    let (schema, key, checked_id) = reader.read_into(&mut bytes, &mut budget.memory,
        |_, _| header_checkpoint(work, cancelled),
        |view, _| Ok((view.schema(), *view.key(), *view.id().as_bytes())))
        .map_err(|e| format!("read bounded compiler object: {e:?}"))?;
    if checked_id != id { return Err("verified compiler object identity differs".into()); }
    Ok((bytes, schema, key))
}

fn selected_borrowed_metadata_member(
    wire: &[u8], schema: SchemaIdentity, metadata_id: AuthorityHash,
    envelope: &BorrowedSelectedEnvelope<'_>, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Option<CompilerNativeHeadersMember>, Option<CompilerNativeLinesMember>), String> {
    let version_at = METADATA_MAGIC.len();
    let version = wire.get(version_at..version_at + 2).ok_or("truncated metadata version")?;
    let version = u16::from_le_bytes([version[0], version[1]]);
    let expected = match version {
        METADATA_VERSION => COMPILER_PUBLICATION_METADATA_SCHEMA,
        PROGRAM_METADATA_VERSION => COMPILER_PROGRAM_METADATA_SCHEMA,
        COVERAGE_METADATA_VERSION => COMPILER_COVERAGE_METADATA_SCHEMA,
        TYPESCRIPT_SNAPSHOT_METADATA_VERSION => COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA,
        CSHARP_EVIDENCE_METADATA_VERSION => COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA,
        NATIVE_HEADERS_METADATA_VERSION => COMPILER_NATIVE_HEADERS_METADATA_SCHEMA,
        NATIVE_LINES_METADATA_VERSION => COMPILER_NATIVE_LINES_METADATA_SCHEMA,
        _ => return Err("invalid compiler metadata version".into()),
    };
    if schema != expected { return Err("compiler metadata schema differs".into()); }
    let bitmap_len = (MAX_MEMBER_IDS + 7) / 8;
    let mut bitmap = backend_store::try_allocate_exact_zeroed_bytes_budgeted(bitmap_len, &mut budget.memory)
        .map_err(|e| format!("allocate metadata bitmap: {e:?}"))?;
    let metadata = borrowed_metadata::BorrowedCompilerMetadata::parse(wire, metadata_id, envelope.member_wire,
        &mut bitmap, &mut |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("selected metadata validation: {e:?}"))?;
    if envelope.logical_generation != metadata.binding_identity || envelope.semantic_root != metadata.semantic_catalog_root {
        return Err("selected compiler metadata differs from envelope".into());
    }
    let member = (metadata.native_headers, metadata.native_lines);
    drop(bitmap);
    budget.memory = budget.memory.checked_add(bitmap_len).ok_or("metadata bitmap refund overflow")?;
    Ok(member)
}

/// Reopens selected header evidence while charging metadata, member traversal,
/// payload decoding, and work to one caller-owned budget. Cancellation is
/// checked before each bounded store operation and before/after decoding.
/// Metadata validation checks cancellation throughout borrowed traversal.
pub fn reopen_selected_native_headers_budgeted(
    store: &FileStore, selected: &SelectedGeneration, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<VerifiedNativeHeadersWitness>, String> {
    let initial = budget.memory;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let pin = store.pin_garbage_collection_bounded(&mut budget.memory, |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("pin selected compiler closure: {e:?}"))?;
    let closure = backend_store::ClosureId::from_bytes(*selected.closure_id());
    let count = header_member_bounded(store, &pin, closure, *selected.candidate_id(), budget, cancelled)?;
    if count == 0 || count > (MAX_MEMBER_IDS as u64 + 1) { return Err("selected compiler closure count exceeds limit".into()); }
    let envelope_max = MAGIC.len() + 2 + 5 * 4 + 4 * 4096 + MAX_PROFILE_BYTES + 1 + 16 + 8 + 32 * 6 + 1 + 4 + MAX_MEMBER_IDS * 32;
    let (envelope_wire, schema, key) = header_object_bounded(store, &pin, *selected.candidate_id(), &[COMPILER_PUBLICATION_ENVELOPE_SCHEMA], envelope_max, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected envelope stopped: {e:?}"))?;
    verified_payload_key_checkpointed(schema, &envelope_wire, &key, budget, cancelled)?;
    let envelope = borrowed_selected_envelope(&envelope_wire, selected, selected.candidate_id(), &|| {
        header_checkpoint(&budget.work, cancelled).map_err(|_| CompilerEnvelopeError::Stopped)
    }).map_err(|e| e.to_string())?;
    if count != u64::try_from(envelope.member_wire.len() / 32).map_err(|_| "member count overflow")? + 1 { return Err("selected compiler closure count differs".into()); }
    let metadata_id = envelope.metadata_id;
    header_member_bounded(store, &pin, closure, metadata_id, budget, cancelled)?;
    let metadata_max = MAX_MANIFEST_BYTES + 64 * 1024 * 1024 + MAX_MEMBER_IDS * 72 + 1024;
    let schemas = [COMPILER_PUBLICATION_METADATA_SCHEMA, COMPILER_PROGRAM_METADATA_SCHEMA, COMPILER_COVERAGE_METADATA_SCHEMA,
        COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA, COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA, COMPILER_NATIVE_HEADERS_METADATA_SCHEMA, COMPILER_NATIVE_LINES_METADATA_SCHEMA];
    let (wire, schema, key) = header_object_bounded(store, &pin, metadata_id, &schemas, metadata_max, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected metadata stopped: {e:?}"))?;
    verified_payload_key_checkpointed(schema, &wire, &key, budget, cancelled)?;
    let (member, _) = selected_borrowed_metadata_member(&wire, schema, metadata_id, &envelope, budget, cancelled)?;
    for id in envelope.members() {
        if header_member_bounded(store, &pin, closure, id, budget, cancelled)? != count { return Err("selected closure count changed".into()); }
    }
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let Some(member) = member else { budget.memory = initial; return Ok(None) };
    let id = *member.object_id();
    let length = member.byte_length() as usize;
    let (wire, schema, key) = header_object_bounded(store, &pin, id, &[COMPILER_NATIVE_HEADERS_SCHEMA], length, budget, cancelled)?;
    if schema != COMPILER_NATIVE_HEADERS_SCHEMA || wire.len() != length { return Err("selected header extent differs".into()); }
    verified_payload_key_checkpointed(schema, &wire, &key, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    // Validate all canonical NHA1 framing without materializing decoded rows.
    let _view = backend_semantic::native_ts_headers::NativeTsHeaderAggregateView::parse_canonical_checkpointed(&wire, &mut |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("selected header scan: {e:?}"))?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let retained = wire.len().checked_add(2 * std::mem::size_of::<usize>()).ok_or("header retained budget overflow")?;
    let bytes = wire;
    budget.memory = initial.checked_sub(retained).ok_or("header retained budget exceeded")?;
    Ok(Some(VerifiedNativeHeadersWitness { bytes, object_id: id }))
}

/// Line-map wire authenticated by the exact selected compiler closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedNativeLinesWitness { bytes: Box<[u8]>, object_id: AuthorityHash }
impl VerifiedNativeLinesWitness {
    pub fn as_bytes(&self) -> &[u8] { &self.bytes }
    pub const fn object_id(&self) -> &AuthorityHash { &self.object_id }
}

pub fn reopen_selected_native_lines_budgeted(
    store: &FileStore, selected: &SelectedGeneration, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<VerifiedNativeLinesWitness>, String> {
    let initial = budget.memory;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let pin = store.pin_garbage_collection_bounded(&mut budget.memory, |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("pin selected compiler closure: {e:?}"))?;
    let closure = backend_store::ClosureId::from_bytes(*selected.closure_id());
    let count = header_member_bounded(store, &pin, closure, *selected.candidate_id(), budget, cancelled)?;
    if count == 0 || count > (MAX_MEMBER_IDS as u64 + 1) { return Err("selected compiler closure count exceeds limit".into()); }
    let envelope_max = MAGIC.len() + 2 + 5 * 4 + 4 * 4096 + MAX_PROFILE_BYTES + 1 + 16 + 8 + 32 * 6 + 1 + 4 + MAX_MEMBER_IDS * 32;
    let (envelope_wire, schema, key) = header_object_bounded(store, &pin, *selected.candidate_id(), &[COMPILER_PUBLICATION_ENVELOPE_SCHEMA], envelope_max, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected envelope stopped: {e:?}"))?;
    verified_payload_key_checkpointed(schema, &envelope_wire, &key, budget, cancelled)?;
    let envelope = borrowed_selected_envelope(&envelope_wire, selected, selected.candidate_id(), &|| {
        header_checkpoint(&budget.work, cancelled).map_err(|_| CompilerEnvelopeError::Stopped)
    }).map_err(|e| e.to_string())?;
    if count != u64::try_from(envelope.member_wire.len() / 32).map_err(|_| "member count overflow")? + 1 { return Err("selected compiler closure count differs".into()); }
    let metadata_id = envelope.metadata_id;
    header_member_bounded(store, &pin, closure, metadata_id, budget, cancelled)?;
    let metadata_max = MAX_MANIFEST_BYTES + 64 * 1024 * 1024 + MAX_MEMBER_IDS * 72 + 1024;
    let schemas = [COMPILER_PUBLICATION_METADATA_SCHEMA, COMPILER_PROGRAM_METADATA_SCHEMA, COMPILER_COVERAGE_METADATA_SCHEMA,
        COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA, COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA, COMPILER_NATIVE_HEADERS_METADATA_SCHEMA, COMPILER_NATIVE_LINES_METADATA_SCHEMA];
    let (wire, schema, key) = header_object_bounded(store, &pin, metadata_id, &schemas, metadata_max, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected metadata stopped: {e:?}"))?;
    verified_payload_key_checkpointed(schema, &wire, &key, budget, cancelled)?;
    let (_, member) = selected_borrowed_metadata_member(&wire, schema, metadata_id, &envelope, budget, cancelled)?;
    for id in envelope.members() {
        if header_member_bounded(store, &pin, closure, id, budget, cancelled)? != count { return Err("selected closure count changed".into()); }
    }
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let Some(member) = member else { budget.memory = initial; return Ok(None) };
    let id = *member.object_id();
    let length = member.byte_length() as usize;
    let (wire, schema, key) = header_object_bounded(store, &pin, id, &[COMPILER_NATIVE_LINES_SCHEMA], length, budget, cancelled)?;
    if schema != COMPILER_NATIVE_LINES_SCHEMA || wire.len() != length { return Err("selected header extent differs".into()); }
    verified_payload_key_checkpointed(schema, &wire, &key, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    // Validate all canonical NHA1 framing without materializing decoded rows.
    let _view = backend_semantic::native_source_lines::NativeSourceLineAggregateView::parse(&wire, &mut |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("selected header scan: {e:?}"))?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let retained = wire.len().checked_add(2 * std::mem::size_of::<usize>()).ok_or("header retained budget overflow")?;
    let bytes = wire;
    budget.memory = initial.checked_sub(retained).ok_or("header retained budget exceeded")?;
    Ok(Some(VerifiedNativeLinesWitness { bytes, object_id: id }))
}

/// Selected-closure authenticated header claims borrowed from caller scratch.
pub struct VerifiedNativeHeadersView<'a> { view: backend_semantic::native_ts_headers::NativeTsHeaderAggregateView<'a>, object_id: AuthorityHash }
impl<'a> VerifiedNativeHeadersView<'a> {
    pub const fn object_id(&self) -> &AuthorityHash { &self.object_id }
    pub fn view(&self) -> &backend_semantic::native_ts_headers::NativeTsHeaderAggregateView<'a> { &self.view }
}
fn header_object_into_scratch<'a>(store: &FileStore, pin: &backend_store::GcPinGuard, id: AuthorityHash,
    schemas: &[SchemaIdentity], maximum: usize, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool, scratch: &'a mut [u8]) -> Result<(&'a [u8], &'a mut [u8], SchemaIdentity, AuthorityHash), String> {
    let work = &budget.work;
    let reader = store.open_bounded_nonrelation_object(pin, UntrustedObjectId::from_bytes(id), schemas, None, maximum,
        &mut budget.memory, |_| header_checkpoint(work, cancelled)).map_err(|e| format!("open bounded compiler object: {e:?}"))?;
    let len = reader.payload_len();
    if len > scratch.len() { return Err("selected compiler scratch capacity exceeded".into()); }
    let (payload, remaining) = scratch.split_at_mut(len);
    let (schema, key, checked_id) = reader.read_into(payload, &mut budget.memory,
        |_, _| header_checkpoint(work, cancelled), |view, _| Ok((view.schema(), *view.key(), *view.id().as_bytes())))
        .map_err(|e| format!("read bounded compiler object: {e:?}"))?;
    if checked_id != id { return Err("verified compiler object identity differs".into()); }
    Ok((payload, remaining, schema, key))
}

/// Reopens selected claims in caller-provisioned, operation-charged scratch.
/// The returned borrowed view cannot outlive that scratch. Metadata decoding
/// is validated without allocating owned metadata vectors.
pub fn reopen_selected_native_headers_in_scratch<'a>(
    store: &FileStore, selected: &SelectedGeneration, budget: &mut SelectedNativeHeaderBudget,
    cancelled: &dyn Fn() -> bool, scratch: &'a mut [u8],
) -> Result<Option<VerifiedNativeHeadersView<'a>>, String> {
    let initial = budget.memory;
    let scratch_charge = scratch.len();
    header_reserve(&mut budget.memory, scratch_charge)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let pin = store.pin_garbage_collection_bounded(&mut budget.memory, |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("pin selected compiler closure: {e:?}"))?;
    let closure = backend_store::ClosureId::from_bytes(*selected.closure_id());
    let count = header_member_bounded(store, &pin, closure, *selected.candidate_id(), budget, cancelled)?;
    if count == 0 || count > (MAX_MEMBER_IDS as u64 + 1) { return Err("selected compiler closure count exceeds limit".into()); }
    let envelope_max = MAGIC.len() + 2 + 5 * 4 + 4 * 4096 + MAX_PROFILE_BYTES + 1 + 16 + 8 + 32 * 6 + 1 + 4 + MAX_MEMBER_IDS * 32;
    let (envelope_wire, remaining_scratch, schema, key) = header_object_into_scratch(store, &pin, *selected.candidate_id(), &[COMPILER_PUBLICATION_ENVELOPE_SCHEMA], envelope_max, budget, cancelled, scratch)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected envelope stopped: {e:?}"))?;
    verified_payload_key_checkpointed(schema, &envelope_wire, &key, budget, cancelled)?;
    let envelope = borrowed_selected_envelope(&envelope_wire, selected, selected.candidate_id(), &|| {
        header_checkpoint(&budget.work, cancelled).map_err(|_| CompilerEnvelopeError::Stopped)
    }).map_err(|e| e.to_string())?;
    if count != u64::try_from(envelope.member_wire.len() / 32).map_err(|_| "member count overflow")? + 1 { return Err("selected compiler closure count differs".into()); }
    let metadata_id = envelope.metadata_id;
    header_member_bounded(store, &pin, closure, metadata_id, budget, cancelled)?;
    let metadata_max = MAX_MANIFEST_BYTES + 64 * 1024 * 1024 + MAX_MEMBER_IDS * 72 + 1024;
    let schemas = [COMPILER_PUBLICATION_METADATA_SCHEMA, COMPILER_PROGRAM_METADATA_SCHEMA, COMPILER_COVERAGE_METADATA_SCHEMA,
        COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA, COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA, COMPILER_NATIVE_HEADERS_METADATA_SCHEMA, COMPILER_NATIVE_LINES_METADATA_SCHEMA];
    let (wire, _, schema, key) = header_object_into_scratch(store, &pin, metadata_id, &schemas, metadata_max, budget, cancelled, &mut *remaining_scratch)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected metadata stopped: {e:?}"))?;
    verified_payload_key_checkpointed(schema, &wire, &key, budget, cancelled)?;
    let (member, _) = selected_borrowed_metadata_member(&wire, schema, metadata_id, &envelope, budget, cancelled)?;
    for id in envelope.members() {
        if header_member_bounded(store, &pin, closure, id, budget, cancelled)? != count { return Err("selected closure count changed".into()); }
    }
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    let Some(member) = member else { budget.memory = initial.checked_sub(scratch_charge).ok_or("scratch budget exceeded")?; return Ok(None) };
    let id = *member.object_id();
    let length = member.byte_length() as usize;
    let (wire, _, schema, key) = header_object_into_scratch(store, &pin, id, &[COMPILER_NATIVE_HEADERS_SCHEMA], length, budget, cancelled, &mut *remaining_scratch)?;
    if schema != COMPILER_NATIVE_HEADERS_SCHEMA || wire.len() != length { return Err("selected header extent differs".into()); }
    verified_payload_key_checkpointed(schema, &wire, &key, budget, cancelled)?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    // Validate all canonical NHA1 framing without materializing decoded rows.
    let view = backend_semantic::native_ts_headers::NativeTsHeaderAggregateView::parse_canonical_checkpointed(wire, &mut |_| header_checkpoint(&budget.work, cancelled))
        .map_err(|e| format!("selected header scan: {e:?}"))?;
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("selected header stopped: {e:?}"))?;
    budget.memory = initial.checked_sub(scratch_charge).ok_or("scratch budget exceeded")?;
    Ok(Some(VerifiedNativeHeadersView { view, object_id: id }))
}

/// Reopens the aggregate only from the selected authenticated compiler closure.
pub fn reopen_selected_native_headers(store: &FileStore, selected: &SelectedGeneration, max_bytes: usize) -> Result<Option<VerifiedNativeHeadersWitness>, String> {
    let reopened = reopen_selected_compiler_metadata(store, selected)?;
    let closure = store.open_closure_claim(ArtifactClosureClaim::from_bytes(*selected.closure_id()))
        .map_err(|error| format!("open selected header closure: {error:?}"))?;
    if closure.id().as_bytes() != selected.closure_id() { return Err("header closure identity mismatch".into()); }
    let Some(member) = reopened.metadata().native_headers() else { return Ok(None) };
    if member.byte_length() as usize > max_bytes { return Err("header aggregate exceeds caller budget".into()); }
    reopen_native_headers_bounded(store, &closure, reopened.metadata(), max_bytes).map(|value| value.map(|bytes| VerifiedNativeHeadersWitness { bytes: bytes.into_boxed_slice(), object_id: *member.object_id() }))
}

fn reopen_native_headers_bounded(store: &FileStore, closure: &backend_store::DurableManifest, metadata: &CompilerPublicationMetadata, max_bytes: usize) -> Result<Option<Vec<u8>>, String> {
    let Some(member) = metadata.native_headers() else { return Ok(None) };
    let len = member.byte_length() as usize;
    if len == 0 || len > MAX_COMPILER_NATIVE_HEADERS_BYTES || len > max_bytes { return Err("header aggregate extent invalid".into()); }
    let encoded_and_key = len.checked_mul(2).ok_or("header aggregate budget overflow")?;
    let mut decoded_budget = max_bytes.checked_sub(encoded_and_key).ok_or("header aggregate exceeds caller budget")?;
    let claim = UntrustedObjectId::from_bytes(*member.object_id());
    closure.admit_claim(claim).map_err(|e| format!("admit header aggregate: {e:?}"))?.ok_or("header aggregate absent from selected closure")?;
    // Allocate the declared bounded extent once; incremental Vec growth can
    // otherwise temporarily retain both old and new backing allocations.
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| "header aggregate allocation failed".to_owned())?;
    let verified = store.write_verified_object_payload(claim, u64::from(member.byte_length()), &mut bytes).map_err(|e| format!("read header aggregate: {e:?}"))?;
    let _aggregate = backend_semantic::native_ts_headers::NativeTsHeaderAggregate::decode_canonical_budgeted(&bytes, &mut decoded_budget).map_err(|e| e.to_string())?;
    let key = native_headers_key(&bytes)?;
    if verified.id().as_bytes() != member.object_id() || verified.schema() != COMPILER_NATIVE_HEADERS_SCHEMA || verified.payload_len() != u64::from(member.byte_length()) || verified.key() != &key || bytes.len() != len { return Err("header aggregate schema, key, identity, or extent mismatch".into()); }
    Ok(Some(bytes))
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
        if self.stopped.is_some_and(|stop| stop()) { return Err("compiler candidate verification stopped".into()); }
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
        if !matches!(
            metadata_object.schema(),
            COMPILER_PUBLICATION_METADATA_SCHEMA
                | COMPILER_PROGRAM_METADATA_SCHEMA
                | COMPILER_COVERAGE_METADATA_SCHEMA
                | COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA
                | COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA
            | COMPILER_NATIVE_HEADERS_METADATA_SCHEMA
            | COMPILER_NATIVE_LINES_METADATA_SCHEMA
        ) {
            return Err("compiler metadata object has the wrong schema".to_owned());
        }
        let observed_metadata = CompilerPublicationMetadata::decode(metadata_object.bytes())
            .map_err(|error| error.to_string())?;
        if metadata_object.schema() != observed_metadata.schema()
            || observed_metadata.object_id() != *metadata_object.id().as_bytes()
            || observed_metadata != *self.expected_metadata
            || observed_metadata.binding_identity != observed.logical_generation
            || metadata_member_ids(&observed_metadata).as_slice() != observed.member_ids.as_ref()
        {
            return Err(
                "reopened compiler metadata differs from the exact envelope pack".to_owned(),
            );
        }
        if reopen_source_coverage(&manifest, &observed_metadata)?
            .as_ref()
            .is_some_and(|coverage| coverage.input_root() != observed.input_digest())
        {
            return Err("compiler coverage input differs from admitted attempt".to_owned());
        }
        verify_native_lines_for_admission(self.store, &manifest, &observed_metadata, self.budget, self.stopped)?;
        verify_semantic_image_members(self.store, &manifest, &observed_metadata)?;
        verify_versioned_plane_members(&manifest, &observed_metadata)?;
        verify_exact_member_set(
            &manifest,
            envelope_id,
            observed.member_ids(),
            stored.object_count(),
        )?;
        if self.stopped.is_some_and(|stop| stop()) { return Err("compiler candidate verification stopped".into()); }
        Ok(())
    }
}

/// The candidate verifier admits c00f as a typed member before selection.
/// This operation is distinct from metadata-only reopen and does not make
/// line claims authoritative for a host source.
fn verify_native_lines_for_admission(
    store: &FileStore, manifest: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata, policy: ArtifactBudget,
    external_stop: Option<&dyn Fn() -> bool>,
) -> Result<(), String> {
    let Some(member) = metadata.native_lines() else { return Ok(()) };
    let length = member.byte_length() as usize;
    if length == 0 || length > MAX_COMPILER_NATIVE_LINES_BYTES || length as u64 > policy.max_payload_bytes {
        return Err("native line member extent exceeds candidate budget".into());
    }
    manifest.admit_claim(UntrustedObjectId::from_bytes(*member.object_id()))
        .map_err(|e| format!("admit candidate line member: {e:?}"))?
        .ok_or("candidate line member absent from closure")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let stopped = || external_stop.is_some_and(|stop| stop()) || std::time::Instant::now() >= deadline;
    let mut budget = SelectedNativeHeaderBudget::new(MAX_COMPILER_NATIVE_LINES_BYTES + 4 * 1024 * 1024, 128 * 1024 * 1024);
    let pin = store.pin_garbage_collection_bounded(&mut budget.memory, |_| header_checkpoint(&budget.work, &stopped))
        .map_err(|e| format!("pin candidate line member: {e:?}"))?;
    let reader = store.open_bounded_nonrelation_object(&pin, UntrustedObjectId::from_bytes(*member.object_id()),
        &[COMPILER_NATIVE_LINES_SCHEMA], None, length, &mut budget.memory, |_| header_checkpoint(&budget.work, &stopped))
        .map_err(|e| format!("open candidate line member: {e:?}"))?;
    if reader.payload_len() != length { return Err("candidate line member extent differs".into()); }
    let mut bytes = backend_store::try_allocate_exact_zeroed_bytes_budgeted(length, &mut budget.memory)
        .map_err(|e| format!("allocate candidate line member: {e:?}"))?;
    let admitted = reader.read_into(&mut bytes, &mut budget.memory,
        |_, _| header_checkpoint(&budget.work, &stopped),
        |view, _| CompilerNativeLinesMember::from_verified_view(view, &mut |_| header_checkpoint(&budget.work, &stopped).map_err(|_| CompilerEnvelopeError::Stopped))
            .map_err(|e| match e { CompilerEnvelopeError::Stopped => backend_store::StoreError::Bounds, _ => backend_store::StoreError::Corrupt }))
        .map_err(|e| format!("verify candidate line member: {e:?}"))?;
    if &admitted != member { return Err("candidate line member differs".into()); }
    Ok(())
}

fn metadata_member_ids(metadata: &CompilerPublicationMetadata) -> Vec<AuthorityHash> {
    metadata_member_ids_with_id(metadata, metadata.object_id())
}

fn metadata_member_ids_checked(metadata: &CompilerPublicationMetadata) -> Result<Vec<AuthorityHash>, CompilerEnvelopeError> {
    // This path propagates identity framing failures rather than falling back
    // to unbudgeted typed-object construction.
    let bytes = metadata.canonical_bytes();
    let id = backend_store::untrusted_object_id_for_encoded_value(metadata.schema(), &bytes)
        .map_err(|_| CompilerEnvelopeError::ByteLengthOverflow)?;
    let raw_id = *id.as_bytes();
    drop(bytes);
    Ok(metadata_member_ids_with_id(metadata, raw_id))
}

fn metadata_member_ids_with_id(metadata: &CompilerPublicationMetadata, metadata_id: AuthorityHash) -> Vec<AuthorityHash> {
    let plane_count = metadata
        .versioned_planes
        .as_ref()
        .map_or(0, VersionedPlaneMetadata::segment_count);
    let artifact_count = metadata.versioned_planes.as_ref().map_or(0, |planes| planes.artifacts().len());
    let optional_count = usize::from(metadata.native_program_sources.is_some())
        + usize::from(metadata.source_coverage.is_some())
        + usize::from(metadata.typescript_snapshot.is_some())
        + usize::from(metadata.csharp_evidence.is_some())
        + usize::from(metadata.native_headers.is_some())
        + usize::from(metadata.native_lines.is_some());
    let mut members = Vec::with_capacity(metadata.images.len() + plane_count + artifact_count + optional_count + 1);
    members.push(metadata_id);
    if let Some(member) = metadata.native_program_sources() {
        members.push(*member.object_id());
    }
    if let Some(member) = metadata.source_coverage() {
        members.push(*member.object_id());
    }
    if let Some(member) = metadata.typescript_snapshot_witness() {
        members.push(*member.object_id());
    }
    if let Some(member) = metadata.csharp_evidence() { members.push(*member.object_id()); }
    if let Some(member) = metadata.native_headers() { members.push(*member.object_id()); }
    if let Some(member) = metadata.native_lines() { members.push(*member.object_id()); }
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

/// Reopens an authenticated coverage object; old publications honestly return None.
pub fn reopen_source_coverage(
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<Option<backend_semantic::ir::SourceCoverageObservation>, String> {
    let Some(member) = metadata.source_coverage() else {
        return Ok(None);
    };
    let id = closure
        .admit_claim(UntrustedObjectId::from_bytes(*member.object_id()))
        .map_err(|e| format!("admit compiler coverage member: {e:?}"))?
        .ok_or("compiler coverage member absent from selected closure")?;
    let object = closure
        .get(id)
        .map_err(|e| format!("read compiler coverage member: {e:?}"))?
        .ok_or("compiler coverage member disappeared")?;
    if object.schema() != COMPILER_SOURCE_COVERAGE_SCHEMA
        || object.bytes().len() != member.byte_length() as usize
    {
        return Err("compiler coverage schema or extent mismatch".to_owned());
    }
    let coverage = backend_semantic::ir::SourceCoverageObservation::decode(object.bytes())
        .map_err(str::to_owned)?;
    if coverage.identity() != *member.identity()
        || coverage.completed() as usize != metadata.images().len()
    {
        return Err("compiler coverage identity or artifact count mismatch".to_owned());
    }
    let program = reopen_native_program_sources(closure, metadata)?
        .ok_or("compiler coverage has no authenticated native program")?;
    if coverage.program() != &program.membership_identity() {
        return Err("compiler coverage program identity mismatch".to_owned());
    }
    if let Some(catalog) = coverage.catalog() {
        if let Some((_, nodes)) = program.nodes() {
            let expected = nodes
                .iter()
                .map(|node| {
                    (
                        node.config_path.as_str(),
                        *node.config.as_ref(),
                        node.environment,
                    )
                })
                .collect::<Vec<_>>();
            let observed = catalog
                .rows()
                .iter()
                .filter_map(|row| match row {
                    backend_semantic::ir::SourceCoverageRow::Configuration {
                        path,
                        source,
                        environment,
                    } => Some((path.as_str(), *source, *environment)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if observed != expected {
                return Err(
                    "compiler coverage configuration differs from native program forest".to_owned(),
                );
            }
        } else if catalog.rows().iter().any(|row| {
            matches!(
                row,
                backend_semantic::ir::SourceCoverageRow::Configuration { .. }
            )
        }) {
            return Err(
                "single inferred program cannot authenticate a configured coverage row".to_owned(),
            );
        }
        let selected_sources = program
            .sources()
            .filter_map(|source| source.package_path.map(|path| (path, source.source)))
            .collect::<std::collections::BTreeMap<_, _>>();
        for row in catalog.rows() {
            if let backend_semantic::ir::SourceCoverageRow::Gap{path,cause:backend_semantic::ir::SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,..}=row
            && selected_sources.contains_key(path.as_str()) {
                return Err("configured exclusion is present in actual native program".to_owned());
            }
        }
        for row in catalog.rows() {
            if let backend_semantic::ir::SourceCoverageRow::Gap {
                path,
                source,
                byte_len,
                cause:
                    backend_semantic::ir::SourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted,
                ..
            } = row
            {
                if !selected_sources.get(path.as_str()).is_some_and(|selected| {
                    selected.identity.as_ref() == source && selected.byte_len == *byte_len
                }) {
                    return Err(
                        "TypeScript work-budget gap is outside its exact bound native source"
                            .to_owned(),
                    );
                }
            }
        }
    }
    Ok(Some(coverage))
}

/// Reads the exact C# evidence aggregate from the selected compiler closure.
/// The returned bytes are untrusted until the engine validates the full record
/// and correlates it with the selected unit, images and current source.
pub fn reopen_selected_csharp_evidence(store: &FileStore, selected: &SelectedGeneration) -> Result<Option<Vec<u8>>, String> {
    let reopened = reopen_selected_compiler_metadata(store, selected)?;
    let closure = store.open_closure_claim(ArtifactClosureClaim::from_bytes(*selected.closure_id()))
        .map_err(|error| format!("open selected compiler closure: {error:?}"))?;
    reopen_csharp_evidence_bounded(store, &closure, reopened.metadata())
}

fn reopen_csharp_evidence_bounded(
    store: &FileStore, closure: &backend_store::DurableManifest, metadata: &CompilerPublicationMetadata,
) -> Result<Option<Vec<u8>>, String> {
    let Some(member) = metadata.csharp_evidence() else { return Ok(None); };
    let length = usize::try_from(member.byte_length()).map_err(|_| "C# evidence extent overflow")?;
    if length == 0 || length > backend_semantic::ir::MAX_CSHARP_INVOCATION_EVIDENCE_BYTES {
        return Err("C# evidence extent invalid".to_owned());
    }
    let claim = UntrustedObjectId::from_bytes(*member.object_id());
    closure.admit_claim(claim).map_err(|error| format!("admit C# evidence: {error:?}"))?
        .ok_or("C# evidence absent from selected closure")?;
    let mut bytes = Vec::new();
    let verified = store.write_verified_object_payload(claim, u64::from(member.byte_length()), &mut bytes)
        .map_err(|error| format!("read selected C# evidence: {error:?}"))?;
    if verified.id().as_bytes() != member.object_id() || verified.schema() != COMPILER_CSHARP_EVIDENCE_SCHEMA
        || verified.payload_len() != u64::from(member.byte_length()) || bytes.len() != length
        || verified.key() != ObjectKey::<backend_semantic::ir::CSharpInvocationEvidenceSchema>::from_value(&bytes).as_bytes() {
        return Err("C# evidence schema, identity, or extent mismatch".to_owned());
    }
    Ok(Some(bytes))
}

/// Reopens the selected closure's complete TypeScript observation certificate.
/// The returned bytes remain untrusted until the engine decodes the versioned
/// record and replays every host and program validator.
pub fn reopen_typescript_snapshot_witness(
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<Option<Vec<u8>>, String> {
    reopen_typescript_snapshot_witness_shared(closure, metadata)
        .map(|bytes| bytes.map(|bytes| bytes.as_ref().to_vec()))
}

fn reopen_typescript_snapshot_witness_shared(
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<Option<std::sync::Arc<[u8]>>, String> {
    let Some(member) = metadata.typescript_snapshot_witness() else {
        return Ok(None);
    };
    let id = closure
        .admit_claim(UntrustedObjectId::from_bytes(*member.object_id()))
        .map_err(|error| format!("admit TypeScript snapshot member: {error:?}"))?
        .ok_or("TypeScript snapshot member absent from selected closure")?;
    let object = closure
        .get(id)
        .map_err(|error| format!("read TypeScript snapshot member: {error:?}"))?
        .ok_or("TypeScript snapshot member disappeared")?;
    if object.schema() != COMPILER_TYPESCRIPT_SNAPSHOT_WITNESS_SCHEMA
        || object.bytes().len() != member.byte_length() as usize
        || object.bytes().is_empty()
        || object.bytes().len() > backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES
        || object.key()
            != ObjectKey::<backend_semantic::ir::TypeScriptSnapshotWitnessSchema>::from_value(
                object.bytes(),
            )
            .as_bytes()
    {
        return Err("TypeScript snapshot schema, identity, or extent mismatch".to_owned());
    }
    Ok(Some(object.bytes_shared()))
}

/// Reads one selected ledger through the store's bounded verification stream.
/// The closure membership is checked before opening the CAS payload, and the
/// certificate-specific limit is enforced during the read, before allocation
/// can grow beyond the authenticated declared extent.
fn reopen_typescript_snapshot_witness_bounded(
    store: &FileStore,
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<Option<Vec<u8>>, String> {
    let Some(member) = metadata.typescript_snapshot_witness() else {
        return Ok(None);
    };
    let declared_len = usize::try_from(member.byte_length())
        .map_err(|_| "TypeScript snapshot extent overflow".to_owned())?;
    if declared_len == 0
        || declared_len > backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES
    {
        return Err("TypeScript snapshot schema, identity, or extent mismatch".to_owned());
    }
    let claim = UntrustedObjectId::from_bytes(*member.object_id());
    closure
        .admit_claim(claim)
        .map_err(|error| format!("admit TypeScript snapshot member: {error:?}"))?
        .ok_or("TypeScript snapshot member absent from selected closure")?;

    let mut bytes = Vec::new();
    let verified = store
        .write_verified_object_payload(claim, u64::from(member.byte_length()), &mut bytes)
        .map_err(|error| format!("read selected TypeScript snapshot member: {error:?}"))?;
    if verified.id().as_bytes() != member.object_id()
        || verified.schema() != COMPILER_TYPESCRIPT_SNAPSHOT_WITNESS_SCHEMA
        || verified.payload_len() != u64::from(member.byte_length())
        || verified.key()
            != ObjectKey::<backend_semantic::ir::TypeScriptSnapshotWitnessSchema>::from_value(
                &bytes,
            )
            .as_bytes()
        || bytes.len() != declared_len
    {
        return Err("TypeScript snapshot schema, identity, or extent mismatch".to_owned());
    }
    Ok(Some(bytes))
}

/// Checks an optional ledger's exact typed object and closure membership under
/// the certificate byte limit. This validates storage only; it grants no
/// selected-generation capability or current filesystem authority.
///
/// # Errors
/// Returns an error if a declared member is absent, corrupt, or differs in
/// schema, typed key, identity, or extent from the publication metadata.
pub fn verify_typescript_snapshot_witness_member(
    store: &FileStore,
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<(), String> {
    let _ = reopen_typescript_snapshot_witness_bounded(store, closure, metadata)?;
    Ok(())
}

/// Reopens only the selected closure's exact program member and validates its identity.
pub fn reopen_native_program_sources(
    closure: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<Option<backend_semantic::ir::NativeProgramSources>, String> {
    let Some(member) = metadata.native_program_sources() else {
        return Ok(None);
    };
    let id = closure
        .admit_claim(UntrustedObjectId::from_bytes(*member.object_id()))
        .map_err(|e| format!("admit native program source member: {e:?}"))?
        .ok_or_else(|| "native program source member is absent from selected closure".to_owned())?;
    let object = closure
        .get(id)
        .map_err(|e| format!("read native program source member: {e:?}"))?
        .ok_or_else(|| "native program source member disappeared".to_owned())?;
    if ![
        NATIVE_PROGRAM_SOURCE_MANIFEST_SCHEMA,
        NATIVE_PROGRAM_FOREST_SCHEMA,
    ]
    .contains(&object.schema())
        || object.bytes().len() != member.byte_length() as usize
    {
        return Err("native program source schema or extent mismatch".to_owned());
    }
    let sources = backend_semantic::ir::NativeProgramSources::decode(object.bytes())
        .map_err(|e| e.to_string())?;
    if sources.is_forest() != (object.schema() == NATIVE_PROGRAM_FOREST_SCHEMA)
        || sources.membership_identity() != *member.membership_identity()
    {
        return Err("native program membership identity mismatch".to_owned());
    }
    Ok(Some(sources))
}

fn verify_native_program_image(
    bytes: &[u8],
    program: Option<&backend_semantic::ir::NativeProgramSources>,
    mappings: Option<&std::collections::BTreeMap<&str, backend_semantic::ir::SourceIdentity>>,
) -> Result<(), String> {
    use backend_semantic::ir::{ImageProvenance, SemanticCoreReader, SemanticImageView};
    let Some(program) = program else {
        return Ok(());
    };
    let view = SemanticImageView::reopen(bytes).map_err(|e| e.to_string())?;
    let ImageProvenance::Captured {
        source,
        recipe,
        scope,
        ..
    } = view.image_facts().provenance
    else {
        return Err("program image has no captured provenance".to_owned());
    };
    let path = view
        .atom(scope.path)
        .and_then(|p| std::str::from_utf8(p).ok())
        .ok_or_else(|| "program image path is invalid".to_owned())?;
    if recipe.profile.language() != backend_semantic::vocabulary::Language::TypeScript
        || recipe.toolchain != program.toolchain()
        || mappings.and_then(|mappings| mappings.get(path)) != Some(&source)
        || !program.image_matches(
            path,
            backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(bytes).as_ref(),
        )
    {
        return Err("program member differs from compiler image source or recipe".to_owned());
    }
    Ok(())
}

fn verify_semantic_image_members(
    store: &FileStore,
    manifest: &backend_store::DurableManifest,
    metadata: &CompilerPublicationMetadata,
) -> Result<(), String> {
    let _coverage = reopen_source_coverage(manifest, metadata)?;
    let _ = reopen_typescript_snapshot_witness_bounded(store, manifest, metadata)?;
    let _ = reopen_csharp_evidence_bounded(store, manifest, metadata)?;
    let _ = reopen_native_headers_bounded(store, manifest, metadata, 96 * 1024 * 1024)?;
    let program = reopen_native_program_sources(manifest, metadata)?;
    let mappings = program.as_ref().map(|program| {
        program
            .sources()
            .filter_map(|row| row.package_path.map(|path| (path, row.source)))
            .collect::<std::collections::BTreeMap<_, _>>()
    });
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
        verify_native_program_image(object.bytes(), program.as_ref(), mappings.as_ref())?;
        if metadata.csharp_evidence().is_some() {
            use backend_semantic::ir::{ImageProvenance, SemanticCoreReader, SemanticImageView};
            let image = SemanticImageView::reopen(object.bytes()).map_err(|e| e.to_string())?;
            let ImageProvenance::Captured { recipe, .. } = image.image_facts().provenance else {
                return Err("C# evidence image has no captured provenance".to_owned());
            };
            if !matches!(recipe.profile, backend_semantic::vocabulary::LanguageProfile::CSharp(_))
                || recipe.tool != backend_semantic::vocabulary::NativeTool::CSharpCompiler {
                return Err("C# evidence image has the wrong producer".to_owned());
            }
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
    let _coverage = reopen_source_coverage(manifest, metadata)?;
    let program = reopen_native_program_sources(manifest, metadata)?;
    let mappings = program.as_ref().map(|program| {
        program
            .sources()
            .filter_map(|row| row.package_path.map(|path| (path, row.source)))
            .collect::<std::collections::BTreeMap<_, _>>()
    });
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
        verify_native_program_image(object.bytes(), program.as_ref(), mappings.as_ref())?;
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
    /// Metadata does not preserve a valid semantic manifest fact tuple.
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
    /// A full native program object has invalid schema, extent or membership.
    NativeProgramSources,
    /// The compiler coverage catalog is malformed or not exactly bound.
    SourceCoverage,
    /// A TypeScript snapshot certificate is malformed, oversized, or misbound.
    TypeScriptSnapshotWitness,
    /// A C# invocation evidence object is malformed, oversized, or misbound.
    CSharpInvocationEvidence,
    /// Native header aggregate is malformed or misbound.
    NativeHeaders,
    /// Caller cancellation or finite work stopped the checked traversal.
    Stopped,
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
) -> Result<(AuthorityHash, CompilationManifestFormat, u32, u32), CompilerEnvelopeError> {
    if bytes.len() < SEMANTIC_MANIFEST_HEADER_BYTES
        || bytes.len() > MAX_MANIFEST_BYTES
        || &bytes[..8] != SEMANTIC_MANIFEST_MAGIC
        || bytes[10..12] != [0, 0]
    {
        return Err(CompilerEnvelopeError::ManifestFacts);
    }
    let format = CompilationManifestFormat::from_version(u16::from_le_bytes([bytes[8], bytes[9]]))
        .filter(|format| format.is_semantic())
        .ok_or(CompilerEnvelopeError::ManifestFacts)?;
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
    Ok((identity, format, fragment_count, manifest_byte_length))
}

fn verify_manifest_image_inventory(
    manifest_bytes: &[u8],
    images: &[CompilerImageMember],
) -> Result<(), CompilerEnvelopeError> {
    let (_, _, fragment_count, _) = inspect_semantic_manifest(manifest_bytes)?;
    if usize::try_from(fragment_count).ok() != Some(images.len()) {
        return Err(CompilerEnvelopeError::ManifestFacts);
    }
    // Validate manifest facts without retaining an image-sized tuple vector.
    for ordinal in 0..images.len() {
        let entry_start = SEMANTIC_MANIFEST_HEADER_BYTES + ordinal * SEMANTIC_MANIFEST_ENTRY_BYTES;
        let image_start = entry_start + SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET;
        let identity: AuthorityHash = manifest_bytes[image_start..image_start + 32].try_into().map_err(|_| CompilerEnvelopeError::ManifestFacts)?;
        SemanticImageIdentity::try_from(identity).map_err(|_| CompilerEnvelopeError::ImageIdentity)?;
        let length_start = entry_start + SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET;
        let length = u32::from_le_bytes(manifest_bytes[length_start..length_start + 4].try_into().map_err(|_| CompilerEnvelopeError::ManifestFacts)?);
        if length == 0 { return Err(CompilerEnvelopeError::ImageLength); }
    }
    let bitmap_len = images.len().checked_add(7).ok_or(CompilerEnvelopeError::MemberCount)? / 8;
    let mut seen = backend_store::try_allocate_exact_zeroed_bytes(bitmap_len).map_err(|_| CompilerEnvelopeError::MemberCount)?;
    for image in images {
        let ordinal = usize::try_from(image.artifact_ordinal).map_err(|_| CompilerEnvelopeError::MemberOrder)?;
        if ordinal >= images.len() { return Err(CompilerEnvelopeError::MemberOrder); }
        let mask = 1u8 << (ordinal % 8);
        if seen[ordinal / 8] & mask != 0 { return Err(CompilerEnvelopeError::MemberOrder); }
        seen[ordinal / 8] |= mask;
        let entry_start = SEMANTIC_MANIFEST_HEADER_BYTES + ordinal * SEMANTIC_MANIFEST_ENTRY_BYTES;
        let image_start = entry_start + SEMANTIC_MANIFEST_IMAGE_IDENTITY_OFFSET;
        let length_start = entry_start + SEMANTIC_MANIFEST_IMAGE_LENGTH_OFFSET;
        let length = u32::from_le_bytes(manifest_bytes[length_start..length_start + 4].try_into().map_err(|_| CompilerEnvelopeError::ManifestFacts)?);
        if manifest_bytes[image_start..image_start + 32] != image.semantic_image_identity || length != image.byte_length {
            return Err(CompilerEnvelopeError::MetadataMismatch);
        }
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

struct CompilerProgramMetadataSchema;
impl Schema for CompilerProgramMetadataSchema {
    const DOMAIN: u8 = COMPILER_PROGRAM_METADATA_SCHEMA.domain();
    const TYPE: u16 = COMPILER_PROGRAM_METADATA_SCHEMA.ty();
    const VERSION: u8 = COMPILER_PROGRAM_METADATA_SCHEMA.version();
    type Value = CompilerPublicationMetadata;
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.canonical_bytes());
    }
}

struct CompilerCoverageMetadataSchema;
impl Schema for CompilerCoverageMetadataSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc002;
    const VERSION: u8 = 6;
    type Value = CompilerPublicationMetadata;
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.canonical_bytes());
    }
}

struct CompilerTypeScriptSnapshotMetadataSchema;
impl Schema for CompilerTypeScriptSnapshotMetadataSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc002;
    const VERSION: u8 = 7;
    type Value = CompilerPublicationMetadata;
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.canonical_bytes());
    }
}

struct CompilerNativeLinesMetadataSchema;
impl Schema for CompilerNativeLinesMetadataSchema {
    const DOMAIN: u8 = 0x7a; const TYPE: u16 = 0xc002; const VERSION: u8 = 10;
    type Value = CompilerPublicationMetadata;
    fn encode(value: &Self::Value, output: &mut Vec<u8>) { output.extend_from_slice(&value.canonical_bytes()); }
}

struct CompilerNativeHeadersMetadataSchema;
impl Schema for CompilerNativeHeadersMetadataSchema {
    const DOMAIN: u8 = 0x7a; const TYPE: u16 = 0xc002; const VERSION: u8 = 9;
    type Value = CompilerPublicationMetadata;
    fn encode(value: &Self::Value, output: &mut Vec<u8>) { output.extend_from_slice(&value.canonical_bytes()); }
}
// The bytes have already passed the aggregate's canonical decoder. Encoding
// them in one append avoids the geometric growth of per-entry Vec writes.
struct CompilerNativeHeadersBytesSchema;
impl Schema for CompilerNativeHeadersBytesSchema {
    const DOMAIN: u8 = 0x7a; const TYPE: u16 = 0xc00e; const VERSION: u8 = 1;
    type Value = [u8];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) { output.extend_from_slice(value); }
}
/// Checks the logical key against authenticated canonical bytes without
/// allocating a second encoding of the metadata value.
fn verified_payload_key(schema: SchemaIdentity, bytes: &[u8], expected: &AuthorityHash) -> Result<(), String> {
    let mut hash = backend_version::ObjectKeyHasher::new(schema, bytes.len()).map_err(|e| format!("compiler key: {e:?}"))?;
    hash.update(bytes).map_err(|e| format!("compiler key: {e:?}"))?;
    if &hash.finish().map_err(|e| format!("compiler key: {e:?}"))? != expected { return Err("compiler typed object key differs from canonical bytes".into()); }
    Ok(())
}

fn verified_payload_key_checkpointed(schema: SchemaIdentity, bytes: &[u8], expected: &AuthorityHash,
    budget: &SelectedNativeHeaderBudget, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
    let mut hash = backend_version::ObjectKeyHasher::new(schema, bytes.len()).map_err(|e| format!("compiler key: {e:?}"))?;
    for chunk in bytes.chunks(64 * 1024) {
        header_checkpoint(&budget.work, cancelled).map_err(|e| format!("compiler key stopped: {e:?}"))?;
        hash.update(chunk).map_err(|e| format!("compiler key: {e:?}"))?;
    }
    header_checkpoint(&budget.work, cancelled).map_err(|e| format!("compiler key stopped: {e:?}"))?;
    if &hash.finish().map_err(|e| format!("compiler key: {e:?}"))? != expected { return Err("compiler typed object key differs from canonical bytes".into()); }
    Ok(())
}

fn native_headers_key(bytes: &[u8]) -> Result<[u8; 32], String> {
    let mut hash = backend_version::ObjectKeyHasher::new(COMPILER_NATIVE_HEADERS_SCHEMA, bytes.len()).map_err(|e| format!("header key: {e:?}"))?;
    hash.update(bytes).map_err(|e| format!("header key: {e:?}"))?;
    hash.finish().map_err(|e| format!("header key: {e:?}"))
}

struct CompilerCSharpEvidenceMetadataSchema;
impl Schema for CompilerCSharpEvidenceMetadataSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc002;
    const VERSION: u8 = 8;
    type Value = CompilerPublicationMetadata;
    fn encode(value: &Self::Value, output: &mut Vec<u8>) { output.extend_from_slice(&value.canonical_bytes()); }
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

    fn borrowed_string(&mut self, maximum: usize) -> Result<&'bytes str, CompilerEnvelopeError> {
        let length = self.u32()? as usize;
        if length == 0 || length > maximum { return Err(CompilerEnvelopeError::Truncated); }
        std::str::from_utf8(self.take(length)?).map_err(|_| CompilerEnvelopeError::Truncated)
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

    #[test]
    fn selected_header_consumer_work_uses_same_ledger() {
        let budget = super::SelectedNativeHeaderBudget::new(0, 3);
        assert!(budget.checkpoint(2, &|| false).is_ok());
        assert_eq!(budget.remaining_work(), 1);
        assert!(budget.checkpoint(2, &|| false).is_err());
        assert_eq!(budget.remaining_work(), 1);
        assert!(budget.checkpoint(1, &|| true).is_err());
        assert_eq!(budget.remaining_work(), 1);
        assert!(budget.checkpoint(1, &|| false).is_ok());
        assert_eq!(budget.remaining_work(), 0);
    }
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
    fn compiler_coverage_catalog_reopens_after_restart_and_rejects_missing_or_foreign_member() {
        use backend_semantic::ir::{
            SourceCoverageGapCause, SourceCoverageManifest, SourceCoverageRow, SourceCoverageSchema,
        };
        let (image, _) = compiler_image();
        let legacy = metadata(image);
        let legacy_bytes = legacy.canonical_bytes();
        let legacy_id = legacy.object_id();
        assert!(
            CompilerPublicationMetadata::decode(&legacy_bytes)
                .unwrap()
                .source_coverage()
                .is_none()
        );
        use backend_semantic::ir::{
            NativeProgramForestSchema, NativeProgramNode, NativeProgramSource,
            NativeProgramSourceManifest, NativeProgramSources, SourceIdentity,
        };
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let program = NativeProgramSources::from_forest(
            toolchain,
            0,
            vec![NativeProgramNode {
                config_path: "tsconfig.app.json".into(),
                config: ContentId::from_canonical_bytes(b"config"),
                environment: [7; 32],
                references: vec![],
                program: Some(
                    NativeProgramSourceManifest::from_sources(
                        toolchain,
                        vec![NativeProgramSource {
                            program_path: "src/main.ts".into(),
                            package_path: Some("src/main.ts".into()),
                            source: SourceIdentity::from_bytes(b"export {};").unwrap(),
                        }],
                    )
                    .unwrap(),
                ),
            }],
        )
        .unwrap();
        let program_object = TypedObject::from_value(
            &ObjectKey::<NativeProgramForestSchema>::from_value(program.as_bytes()),
            program.as_bytes(),
        );
        let coverage = SourceCoverageManifest::new(
            [3; 32],
            [1, 0],
            program.membership_identity(),
            2,
            1,
            vec![
                SourceCoverageRow::Configuration {
                    path: "tsconfig.app.json".into(),
                    source: *ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                        b"config",
                    )
                    .as_ref(),
                    environment: [7; 32],
                },
                SourceCoverageRow::Gap {
                    path: "src/app.spec.ts".into(),
                    source: *ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                        b"spec",
                    )
                    .as_ref(),
                    byte_len: 4,
                    cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
                    diagnostic: None,
                },
            ],
        )
        .unwrap();
        let coverage =
            backend_semantic::ir::SourceCoverageObservation::from_catalog(coverage).unwrap();
        let bytes = coverage.encode();
        let object = TypedObject::from_value(
            &ObjectKey::<SourceCoverageSchema>::from_value(bytes.as_slice()),
            bytes.as_slice(),
        );
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).unwrap();
        let receipt = store
            .write_closure(
                &ClosureManifest::new(vec![program_object.clone(), object.clone()]).unwrap(),
            )
            .unwrap();
        let verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
            .unwrap();
        let member =
            CompilerSourceCoverageMember::from_verified_object(verified, &coverage).unwrap();
        assert_eq!(
            legacy.clone().with_source_coverage(member),
            Err(CompilerEnvelopeError::SourceCoverage),
            "coverage needs authenticated native program authority before canonicalization"
        );
        let program_verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(
                *program_object.id().as_bytes(),
            ))
            .unwrap();
        let program_member =
            CompilerProgramSourceMember::from_verified_program_sources(program_verified, &program)
                .unwrap();
        let current = legacy
            .with_native_program_sources(program_member)
            .unwrap()
            .with_source_coverage(member)
            .unwrap();
        assert_eq!(
            current.typed_object().schema(),
            COMPILER_COVERAGE_METADATA_SCHEMA
        );
        let encoded = current.canonical_bytes();
        assert_eq!(encoded.len(), legacy_bytes.len() + 137);
        assert_eq!(
            CompilerPublicationMetadata::decode(&encoded).unwrap(),
            current
        );
        assert!(metadata_member_ids(&current).contains(member.object_id()));
        drop(store);
        let store = FileStore::open(&directory, 1024 * 1024).unwrap();
        let closure = store.open_closure(receipt).unwrap();
        let reopened = reopen_source_coverage(
            &closure,
            &CompilerPublicationMetadata::decode(&encoded).unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            reopened, coverage,
            "cold CAS read retains actual config and gap cause"
        );
        // Publication-boundary capacity control: this is authenticated retained
        // evidence, not a claim that the native resolver compiled >16k inputs.
        for reason in [
            backend_semantic::ir::SourceCoverageCapacityReason::RowCount,
            backend_semantic::ir::SourceCoverageCapacityReason::EncodedBytes,
        ] {
            let capacity = backend_semantic::ir::SourceCoverageObservation::not_recorded_capacity(
                [3; 32],
                [1, 0],
                program.membership_identity(),
                20_001,
                1,
                reason,
            )
            .unwrap();
            let bytes = capacity.encode();
            let object = TypedObject::from_value(
                &ObjectKey::<SourceCoverageSchema>::from_value(bytes.as_slice()),
                bytes.as_slice(),
            );
            let receipt = store
                .write_closure(
                    &ClosureManifest::new(vec![program_object.clone(), object.clone()]).unwrap(),
                )
                .unwrap();
            let verified = store
                .verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
                .unwrap();
            let mut unavailable = current.clone();
            unavailable.source_coverage = Some(
                CompilerSourceCoverageMember::from_verified_object(verified, &capacity).unwrap(),
            );
            let encoded = unavailable.canonical_bytes();
            let cold_store = FileStore::open(&directory, 1024 * 1024).unwrap();
            let closure = cold_store.open_closure(receipt).unwrap();
            let cold = reopen_source_coverage(
                &closure,
                &CompilerPublicationMetadata::decode(&encoded).unwrap(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(cold, capacity);
            assert_eq!(cold.capacity_reason(), Some(reason));
            assert_eq!((cold.source_count(), cold.completed()), (20_001, 1));
            assert!(
                cold.catalog().is_none(),
                "cold reopen cannot invent a cause catalog"
            );
            let mut forged = unavailable;
            forged.source_coverage.as_mut().unwrap().identity[0] ^= 1;
            assert!(reopen_source_coverage(&closure, &forged).is_err());
        }
        let mut wrong = current.clone();
        wrong.source_coverage.as_mut().unwrap().identity[0] ^= 1;
        assert!(reopen_source_coverage(&closure, &wrong).is_err());
        let mut wrong = current.clone();
        wrong.source_coverage.as_mut().unwrap().byte_length += 1;
        assert!(reopen_source_coverage(&closure, &wrong).is_err());
        for change in 0..6 {
            let mut rows = coverage.catalog().unwrap().rows().to_vec();
            let mut program_id = program.membership_identity();
            match change {
                0 => {
                    let SourceCoverageRow::Configuration { path, .. } = &mut rows[0] else {
                        unreachable!()
                    };
                    *path = "tsconfig.wrong.json".into();
                }
                1 => {
                    let SourceCoverageRow::Configuration { source, .. } = &mut rows[0] else {
                        unreachable!()
                    };
                    *source =
                        *ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                            b"different config",
                        )
                        .as_ref();
                }
                2 => {
                    let SourceCoverageRow::Configuration { environment, .. } = &mut rows[0] else {
                        unreachable!()
                    };
                    *environment = [8; 32];
                }
                3 => program_id[0] ^= 1,
                4 => {
                    let SourceCoverageRow::Gap { path, .. } = &mut rows[1] else {
                        unreachable!()
                    };
                    *path = "src/main.ts".into();
                }
                _ => {
                    let SourceCoverageRow::Gap { cause, .. } = &mut rows[1] else {
                        unreachable!()
                    };
                    *cause = SourceCoverageGapCause::TypeScriptSourceWorkBudgetExhausted;
                }
            }
            let forged =
                SourceCoverageManifest::new([3; 32], [1, 0], program_id, 2, 1, rows).unwrap();
            let forged =
                backend_semantic::ir::SourceCoverageObservation::from_catalog(forged).unwrap();
            let bytes = forged.encode();
            let object = TypedObject::from_value(
                &ObjectKey::<SourceCoverageSchema>::from_value(bytes.as_slice()),
                bytes.as_slice(),
            );
            let id = store
                .write_closure(
                    &ClosureManifest::new(vec![program_object.clone(), object.clone()]).unwrap(),
                )
                .unwrap();
            let verified = store
                .verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
                .unwrap();
            let mut wrong = current.clone();
            wrong.source_coverage = Some(
                CompilerSourceCoverageMember::from_verified_object(verified, &forged).unwrap(),
            );
            let closure = store.open_closure(id).unwrap();
            assert!(
                reopen_source_coverage(&closure, &wrong).is_err(),
                "authenticated conflicting catalog {change}"
            );
        }
        let foreign = store
            .write_closure(&ClosureManifest::new(vec![self::member(b"unrelated")]).unwrap())
            .unwrap();
        let foreign = store.open_closure(foreign).unwrap();
        assert!(reopen_source_coverage(&foreign, &current).is_err());
        assert!(
            reopen_source_coverage(
                &foreign,
                &CompilerPublicationMetadata::decode(&legacy_bytes).unwrap()
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            CompilerPublicationMetadata::decode(&legacy_bytes)
                .unwrap()
                .object_id(),
            legacy_id,
            "legacy identities unchanged"
        );
        drop(closure);
        drop(foreign);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_program_metadata_preserves_legacy_identity_and_binds_exact_cas_object() {
        use backend_semantic::ir::{
            NativeProgramSource, NativeProgramSourceManifest, NativeProgramSourceManifestSchema,
            SourceIdentity,
        };
        let (image, _) = compiler_image();
        let old = metadata(image);
        let old_bytes = old.canonical_bytes();
        let old_id = old.object_id();
        let reopened = CompilerPublicationMetadata::decode(&old_bytes).expect("v4 metadata");
        assert_eq!(reopened.canonical_bytes(), old_bytes);
        assert_eq!(reopened.object_id(), old_id);
        assert!(reopened.native_program_sources().is_none());
        assert_eq!(
            reopened.typed_object().schema(),
            COMPILER_PUBLICATION_METADATA_SCHEMA
        );
        let sources = NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            vec![NativeProgramSource {
                program_path: "workspace/main.ts".into(),
                package_path: Some("main.ts".into()),
                source: SourceIdentity::from_bytes(b"export {};").expect("source"),
            }],
        )
        .expect("manifest");
        let object = TypedObject::from_value(
            &ObjectKey::<NativeProgramSourceManifestSchema>::from_value(sources.as_bytes()),
            sources.as_bytes(),
        );
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("CAS");
        store
            .write_object(&object)
            .expect("write full program once");
        let verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
            .expect("verified member");
        let member = CompilerProgramSourceMember::from_verified_object(verified, &sources)
            .expect("exact object");
        let current = old
            .with_native_program_sources(member)
            .expect("v5 membership");
        let bytes = current.canonical_bytes();
        assert_eq!(
            bytes.len(),
            old_bytes.len() + 68,
            "metadata retains only the small object reference"
        );
        assert_eq!(
            CompilerPublicationMetadata::decode(&bytes).expect("v5 roundtrip"),
            current
        );
        assert_eq!(
            current.typed_object().schema(),
            COMPILER_PROGRAM_METADATA_SCHEMA
        );
        assert!(metadata_member_ids(&current).contains(member.object_id()));
        let edited = NativeProgramSourceManifest::from_sources(
            sources.toolchain(),
            vec![NativeProgramSource {
                program_path: "workspace/main.ts".into(),
                package_path: Some("main.ts".into()),
                source: SourceIdentity::from_bytes(b"changed").expect("source"),
            }],
        )
        .expect("changed membership");
        assert_eq!(
            CompilerProgramSourceMember::from_verified_object(verified, &edited),
            Err(CompilerEnvelopeError::NativeProgramSources)
        );
        let mut future = bytes;
        future[METADATA_MAGIC.len()..METADATA_MAGIC.len() + 2].copy_from_slice(&12u16.to_le_bytes());
        assert_eq!(
            CompilerPublicationMetadata::decode(&future),
            Err(CompilerEnvelopeError::MetadataVersion)
        );
        drop(store);
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn csharp_metadata_v8_retains_exact_typed_evidence_and_legacy_bytes() {
        let (image, _) = compiler_image();
        let legacy = metadata(image);
        let old_bytes = legacy.canonical_bytes();
        let old_id = legacy.object_id();
        let bytes = b"CSE1 bounded claim-only fixture";
        let object = TypedObject::from_value(
            &ObjectKey::<backend_semantic::ir::CSharpInvocationEvidenceSchema>::from_value(bytes), bytes,
        );
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("CAS");
        store.write_object(&object).expect("write evidence");
        let verified = store.verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes())).expect("verified evidence");
        let evidence_member = CompilerCSharpEvidenceMember::from_verified_object(verified, bytes).expect("typed evidence");
        let current = legacy.clone().with_csharp_evidence(evidence_member).expect("v8 evidence");
        let reopened = CompilerPublicationMetadata::decode(&current.canonical_bytes()).expect("v8 roundtrip");
        assert_eq!(reopened, current);
        assert_eq!(reopened.typed_object().schema(), COMPILER_CSHARP_EVIDENCE_METADATA_SCHEMA);
        assert!(metadata_member_ids(&reopened).contains(evidence_member.object_id()));
        let closure_id = store.write_closure(&ClosureManifest::new(vec![object]).expect("closure")).expect("seal");
        let closure = store.open_closure(closure_id).expect("open");
        assert_eq!(reopen_csharp_evidence_bounded(&store, &closure, &reopened).unwrap().as_deref(), Some(bytes.as_slice()));
        let mut wrong = reopened.clone();
        wrong.csharp_evidence.as_mut().unwrap().byte_length += 1;
        assert!(reopen_csharp_evidence_bounded(&store, &closure, &wrong).is_err());
        let foreign_object = member(bytes);
        store.write_object(&foreign_object).expect("foreign object");
        let foreign_closure_id = store.write_closure(&ClosureManifest::new(vec![foreign_object.clone()]).expect("foreign closure")).expect("seal foreign");
        let foreign_closure = store.open_closure(foreign_closure_id).expect("open foreign");
        let mut wrong_schema = reopened.clone();
        wrong_schema.csharp_evidence.as_mut().unwrap().object_id = *foreign_object.id().as_bytes();
        assert!(reopen_csharp_evidence_bounded(&store, &foreign_closure, &wrong_schema).is_err());
        assert!(reopen_csharp_evidence_bounded(&store, &foreign_closure, &reopened).is_err());
        drop(foreign_closure);
        assert_eq!(CompilerPublicationMetadata::decode(&old_bytes).unwrap().object_id(), old_id);
        assert!(legacy.csharp_evidence().is_none());
        drop(closure); drop(store);
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn typescript_snapshot_metadata_v7_round_trips_and_reopens_the_typed_ledger() {
        let (image, _) = compiler_image();
        let program = CompilerProgramSourceMember {
            object_id: [0x11; 32],
            program: [0x22; 32],
            byte_length: 8,
        };
        let legacy = metadata(image)
            .with_native_program_sources(program)
            .expect("native program member");
        let witness_bytes = b"versioned complete TS ledger";
        let witness_object = TypedObject::from_value(
            &ObjectKey::<backend_semantic::ir::TypeScriptSnapshotWitnessSchema>::from_value(
                witness_bytes,
            ),
            witness_bytes,
        );
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("CAS");
        store.write_object(&witness_object).expect("write witness");
        let verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(
                *witness_object.id().as_bytes(),
            ))
            .expect("verified witness");
        let witness =
            CompilerTypeScriptSnapshotMember::from_verified_object(verified, witness_bytes)
                .expect("typed bounded witness");
        let current = legacy
            .with_typescript_snapshot_witness(witness)
            .expect("attach cold certificate");
        let bytes = current.canonical_bytes();
        let reopened = CompilerPublicationMetadata::decode(&bytes).expect("v7 metadata");
        assert_eq!(reopened, current);
        assert_eq!(
            reopened.typed_object().schema(),
            COMPILER_TYPESCRIPT_SNAPSHOT_METADATA_SCHEMA
        );
        assert!(metadata_member_ids(&reopened).contains(witness.object_id()));
        let closure_id = store
            .write_closure(&ClosureManifest::new(vec![witness_object]).expect("closure"))
            .expect("write closure");
        let closure = store.open_closure(closure_id).expect("reopen closure");
        assert_eq!(
            reopen_typescript_snapshot_witness(&closure, &reopened)
                .expect("validate typed member")
                .as_deref(),
            Some(witness_bytes.as_slice()),
        );
        let mut wrong_extent = reopened.clone();
        wrong_extent
            .typescript_snapshot
            .as_mut()
            .unwrap()
            .byte_length += 1;
        assert!(reopen_typescript_snapshot_witness(&closure, &wrong_extent).is_err());
        assert_eq!(
            verify_semantic_image_members(&store, &closure, &wrong_extent).unwrap_err(),
            "TypeScript snapshot schema, identity, or extent mismatch"
        );

        let mut wrong_schema = reopened.clone();
        let wrong_bytes = b"foreign schema object";
        let wrong_object = member(wrong_bytes);
        store
            .write_object(&wrong_object)
            .expect("write foreign schema");
        wrong_schema.typescript_snapshot.as_mut().unwrap().object_id =
            *wrong_object.id().as_bytes();
        let foreign_closure = store
            .write_closure(&ClosureManifest::new(vec![wrong_object]).expect("foreign closure"))
            .expect("write foreign closure");
        let foreign_closure = store.open_closure(foreign_closure).expect("open foreign");
        assert!(reopen_typescript_snapshot_witness(&foreign_closure, &wrong_schema).is_err());
        assert_eq!(
            verify_semantic_image_members(&store, &foreign_closure, &wrong_schema).unwrap_err(),
            "TypeScript snapshot schema, identity, or extent mismatch"
        );

        let missing_closure = store
            .write_closure(&ClosureManifest::new(vec![member(b"other")]).expect("missing closure"))
            .expect("write missing closure");
        let missing_closure = store.open_closure(missing_closure).expect("open missing");
        assert!(reopen_typescript_snapshot_witness(&missing_closure, &reopened).is_err());

        let with_coverage = metadata(compiler_image().0)
            .with_native_program_sources(program)
            .expect("program")
            .with_source_coverage(CompilerSourceCoverageMember {
                object_id: [0x33; 32],
                identity: [0x44; 32],
                byte_length: 1,
            })
            .expect("coverage")
            .with_typescript_snapshot_witness(witness)
            .expect("snapshot");
        assert_eq!(
            CompilerPublicationMetadata::decode(&with_coverage.canonical_bytes())
                .expect("v7 coverage metadata"),
            with_coverage,
        );
        drop(closure);
        drop(store);
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn metadata_versions_four_through_nine_preserve_canonical_roundtrip() {
        let base = metadata(compiler_image().0);
        let program = CompilerProgramSourceMember { object_id: [0x11; 32], program: [0x22; 32], byte_length: 8 };
        let coverage = CompilerSourceCoverageMember { object_id: [0x33; 32], identity: [0x44; 32], byte_length: 8 };
        let snapshot = CompilerTypeScriptSnapshotMember { object_id: [0x55; 32], byte_length: 8 };
        let csharp = CompilerCSharpEvidenceMember { object_id: [0x66; 32], byte_length: 8 };
        let headers = CompilerNativeHeadersMember { object_id: [0x77; 32], byte_length: 8 };
        let v5 = base.clone().with_native_program_sources(program).unwrap();
        let v6 = v5.clone().with_source_coverage(coverage).unwrap();
        let v7 = v6.clone().with_typescript_snapshot_witness(snapshot).unwrap();
        let v8 = base.clone().with_csharp_evidence(csharp).unwrap();
        let v9 = v7.clone().with_native_headers(headers).unwrap();
        for (expected_version, value) in [(4, base), (5, v5), (6, v6), (7, v7), (8, v8), (9, v9)] {
            let bytes = value.canonical_bytes();
            assert_eq!(metadata_member_ids_checked(&value).unwrap(), metadata_member_ids(&value));
            assert_eq!(u16::from_le_bytes(bytes[METADATA_MAGIC.len()..METADATA_MAGIC.len()+2].try_into().unwrap()), expected_version);
            let ids = metadata_member_ids_checked(&value).unwrap();
            let cells: Vec<u8> = ids.iter().flat_map(|id| id.iter().copied()).collect();
            let mut bitmap = vec![0; (MAX_MEMBER_IDS + 7) / 8];
            let parsed = borrowed_metadata::BorrowedCompilerMetadata::parse(
                &bytes, value.object_id(), &cells, &mut bitmap, &mut |_| Ok::<(), ()>(())).unwrap();
            assert_eq!(parsed.binding_identity, value.binding_identity);
            assert_eq!(parsed.semantic_catalog_root, value.semantic_manifest_root().copied());
            assert_eq!(parsed.native_headers, value.native_headers().copied());
            assert!(borrowed_metadata::BorrowedCompilerMetadata::parse(
                &bytes[..bytes.len()-1], value.object_id(), &cells, &mut bitmap, &mut |_| Ok::<(), ()>(())).is_err());
            let mut wrong_id = value.object_id(); wrong_id[0] ^= 1;
            assert!(borrowed_metadata::BorrowedCompilerMetadata::parse(
                &bytes, wrong_id, &cells, &mut bitmap, &mut |_| Ok::<(), ()>(())).is_err());
            assert!(matches!(borrowed_metadata::BorrowedCompilerMetadata::parse(
                &bytes, value.object_id(), &cells, &mut bitmap, &mut |_| Err::<(), ()>(())),
                Err(borrowed_metadata::BorrowedMetadataError::Stopped(()))));
            let bound = compiler_metadata_decode_bound(&bytes).unwrap();
            let mut reopened = None;
            let peak = allocation_counter::measure(|| { reopened = Some(CompilerPublicationMetadata::decode(&bytes).unwrap()); });
            assert_eq!(reopened.unwrap(), value);
            assert!(peak.bytes_max as usize <= bound, "metadata v{expected_version} allocation peak {peak:?}, bound {bound}");
            assert!(compiler_metadata_decode_bound(&bytes[..bytes.len()-1]).is_err());
            assert!(CompilerPublicationMetadata::decode(&bytes[..bytes.len()-1]).is_err());
        }
    }

    #[test]
    fn native_lines_metadata_v10_roundtrips_optional_header() {
        let base = metadata(compiler_image().0).with_native_program_sources(CompilerProgramSourceMember {
            object_id: [0x11; 32], program: [0x22; 32], byte_length: 8,
        }).unwrap();
        let header = CompilerNativeHeadersMember { object_id: [0x33; 32], byte_length: 8 };
        let lines = CompilerNativeLinesMember { object_id: [0x44; 32], byte_length: 8 };
        for value in [base.clone().with_native_lines(lines).unwrap(), base.with_native_headers(header).unwrap().with_native_lines(lines).unwrap()] {
            let wire = value.canonical_bytes();
            assert_eq!(u16::from_le_bytes(wire[METADATA_MAGIC.len()..METADATA_MAGIC.len()+2].try_into().unwrap()), 10);
            assert_eq!(value.typed_object().schema(), COMPILER_NATIVE_LINES_METADATA_SCHEMA);
            assert_eq!(CompilerPublicationMetadata::decode(&wire).unwrap(), value);
            assert!(metadata_member_ids_checked(&value).unwrap().contains(lines.object_id()));
            assert!(CompilerPublicationMetadata::decode(&wire[..wire.len()-1]).is_err());
        }
    }

    #[test]
    fn dense_v9_metadata_allocation_bound_covers_nested_planes_and_all_tails() {
        use backend_semantic::ir::{LanguageProfile, RustEdition, SemanticBuildIdentity, SemanticInputWitness, SemanticIrPlane, SemanticPlane, SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneSegment};
        use backend_semantic::vocabulary::Stage;
        use backend_version::{Coverage, ScopeRoot};
        let (image, image_object) = compiler_image();
        let generation = backend_semantic::ir::GenerationId::from_canonical_bytes(image_object.bytes());
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let payloads = (0..=255u8).map(|i| vec![i, 42]).collect::<Vec<_>>();
        let segments = payloads.iter().enumerate().map(|(i, payload)| SemanticPlaneSegment::from_payload(kind, [i as u8; 32], [i as u8; 32], 1, payload).unwrap()).collect::<Vec<_>>();
        let ids = segments.iter().zip(&payloads).map(|(segment, payload)| (segment.admit(kind, payload).unwrap(), payload.as_slice())).collect::<Vec<_>>();
        let plane = SemanticPlane::claimed(kind, segments, Coverage::Partial).unwrap();
        let manifest = SemanticPlaneManifest::new(generation,
            SemanticBuildIdentity::new([1; 32], [2; 32], LanguageProfile::Rust(RustEdition::Rust2024), Stage::LowerIr, [3; 32], [4; 32], [5; 32], [6; 32]),
            SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32])), vec![plane]).unwrap();
        let encoded = manifest.encode().unwrap();
        let planes = VersionedPlanePublication::from_payloads(&encoded, ids).unwrap();
        let metadata = metadata_with_versioned_planes(image, Some(planes.metadata().clone()))
            .with_native_program_sources(CompilerProgramSourceMember { object_id: [0x11; 32], program: [0x22; 32], byte_length: 8 }).unwrap()
            .with_source_coverage(CompilerSourceCoverageMember { object_id: [0x33; 32], identity: [0x44; 32], byte_length: 8 }).unwrap()
            .with_typescript_snapshot_witness(CompilerTypeScriptSnapshotMember { object_id: [0x55; 32], byte_length: 8 }).unwrap()
            .with_native_headers(CompilerNativeHeadersMember { object_id: [0x77; 32], byte_length: 8 }).unwrap();
        let bytes = metadata.canonical_bytes();
        let bound = compiler_metadata_decode_bound(&bytes).unwrap();
        let mut decoded = None;
        let peak = allocation_counter::measure(|| decoded = Some(CompilerPublicationMetadata::decode(&bytes).unwrap()));
        assert_eq!(decoded.unwrap(), metadata);
        assert!(peak.bytes_max as usize <= bound, "dense nested v9 peak {peak:?}, bound {bound}");
    }

    #[test]
    fn near_limit_synthetic_metadata_allocation_bound() {
        let (image, _) = compiler_image();
        let images = (0..MAX_MEMBER_IDS - 10).map(|i| {
            let mut row = image;
            row.object_id = [0; 32];
            row.object_id[24..].copy_from_slice(&(i as u64 + 1).to_be_bytes());
            row
        }).collect::<Vec<_>>();
        let value = metadata_with_images(images, None)
            .with_native_program_sources(CompilerProgramSourceMember { object_id: [0x11; 32], program: [0x22; 32], byte_length: 8 }).unwrap()
            .with_native_headers(CompilerNativeHeadersMember { object_id: [0x77; 32], byte_length: 8 }).unwrap();
        let bytes = value.canonical_bytes();
        assert!(bytes.len() > 48 * 1024 * 1024);
        let bound = compiler_metadata_decode_bound(&bytes).unwrap();
        let mut decoded = None;
        let peak = allocation_counter::measure(|| decoded = Some(CompilerPublicationMetadata::decode(&bytes).unwrap()));
        assert_eq!(decoded.unwrap(), value);
        assert!(peak.bytes_max as usize <= bound, "near-limit metadata peak {peak:?}, bound {bound}");
    }

    #[test]
    fn truncated_maximum_inventories_refuse_before_count_sized_allocation() {
        use allocation_counter::measure;
        let record = metadata(compiler_image().0);
        let mut bytes = record.canonical_bytes();
        let offset = METADATA_MAGIC.len() + 2 + 4 + record.manifest_bytes().len() + COMPILATION_BINDING_BYTES;
        bytes[offset..offset + 4].copy_from_slice(&(MAX_MEMBER_IDS as u32).to_le_bytes());
        bytes.truncate(offset + 4);
        let measured = measure(|| assert!(matches!(CompilerPublicationMetadata::decode(&bytes), Err(CompilerEnvelopeError::Truncated))));
        assert!(measured.bytes_max < 32 * 1024, "metadata preflight: {measured:?}");
        bytes.resize(offset + 4 + MAX_MEMBER_IDS * 72 - 1, 0);
        let measured = measure(|| assert!(matches!(CompilerPublicationMetadata::decode(&bytes), Err(CompilerEnvelopeError::Truncated))));
        assert!(measured.bytes_max < 32 * 1024, "metadata final row preflight: {measured:?}");
        bytes.resize(offset + 4 + MAX_MEMBER_IDS * 72, 0);
        bytes[METADATA_MAGIC.len()..METADATA_MAGIC.len() + 2].copy_from_slice(&NATIVE_HEADERS_METADATA_VERSION.to_le_bytes());
        bytes.extend_from_slice(&[0, 1]); // no versioned planes, program present
        bytes.extend_from_slice(&[0; 68]);
        bytes.extend_from_slice(&[0, 0]); // no coverage or snapshot
        bytes.extend_from_slice(&[0; 35]); // header pointer lacks its final byte
        let measured = measure(|| assert!(matches!(CompilerPublicationMetadata::decode(&bytes), Err(CompilerEnvelopeError::Truncated))));
        assert!(measured.bytes_max < 32 * 1024, "metadata optional tail preflight: {measured:?}");
        bytes.truncate(offset + 4 + MAX_MEMBER_IDS * 72);
        bytes[METADATA_MAGIC.len()..METADATA_MAGIC.len() + 2].copy_from_slice(&METADATA_VERSION.to_le_bytes());
        let mut nested = Vec::new();
        nested.extend_from_slice(b"TURVPM\0\0");
        nested.extend_from_slice(&3u16.to_le_bytes());
        nested.extend_from_slice(&[0; 32]);
        nested.extend_from_slice(&50_000u32.to_le_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&(nested.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&nested);
        let measured = measure(|| assert!(matches!(CompilerPublicationMetadata::decode(&bytes), Err(CompilerEnvelopeError::VersionedPlane(VersionedPlaneError::Truncated)))));
        assert!(measured.bytes_max < 32 * 1024, "nested metadata preflight: {measured:?}");
        let envelope = CompilerPublicationEnvelope::new(&attempt(), &record).unwrap();
        let mut bytes = envelope.canonical_bytes();
        let bound = compiler_envelope_decode_bound(&bytes).unwrap();
        let mut reopened = None;
        let peak = measure(|| { reopened = Some(CompilerPublicationEnvelope::decode(&bytes).unwrap()); });
        assert_eq!(reopened.unwrap(), envelope);
        assert!(peak.bytes_max as usize <= bound, "envelope allocation peak {peak:?}, bound {bound}");
        let offset = bytes.len() - envelope.member_ids().len() * 32 - 4;
        bytes[offset..offset + 4].copy_from_slice(&(MAX_MEMBER_IDS as u32).to_le_bytes());
        bytes.truncate(offset + 4);
        let measured = measure(|| assert!(matches!(CompilerPublicationEnvelope::decode(&bytes), Err(CompilerEnvelopeError::Truncated))));
        assert!(measured.bytes_max < 32 * 1024, "envelope preflight: {measured:?}");
        bytes.resize(offset + 4 + MAX_MEMBER_IDS * 32 - 1, 0);
        let measured = measure(|| assert!(matches!(CompilerPublicationEnvelope::decode(&bytes), Err(CompilerEnvelopeError::Truncated))));
        assert!(measured.bytes_max < 32 * 1024, "envelope final row preflight: {measured:?}");
    }

    #[test]
    fn selected_member_bitmap_preserves_deduplicated_set_and_stops() {
        let (image, _) = compiler_image();
        let shared = *image.object_id();
        let metadata = metadata(image)
            .with_native_program_sources(CompilerProgramSourceMember { object_id: shared, program: [1; 32], byte_length: 1 }).unwrap()
            .with_native_headers(CompilerNativeHeadersMember { object_id: shared, byte_length: 1 }).unwrap();
        let metadata_id = [0x91; 32];
        let ids = metadata_member_ids_with_id(&metadata, metadata_id);
        let wire = ids.iter().flat_map(|id| id.iter().copied()).collect::<Vec<_>>();
        let envelope = BorrowedSelectedEnvelope { metadata_id, logical_generation: metadata.binding_identity, semantic_root: metadata.semantic_manifest_root().copied(), member_wire: &wire };
        let mut budget = SelectedNativeHeaderBudget::new(1024, 10_000);
        assert!(verify_metadata_member_bitmap(&envelope, &metadata, metadata_id, &mut budget, &|| false).is_ok());
        assert_eq!(budget.remaining_memory(), 1024);
        assert!(verify_metadata_member_bitmap(&envelope, &metadata, metadata_id, &mut SelectedNativeHeaderBudget::new(0, 10_000), &|| false).is_err());
        assert!(verify_metadata_member_bitmap(&envelope, &metadata, metadata_id, &mut SelectedNativeHeaderBudget::new(1024, 10_000), &|| true).is_err());
        let mut altered = metadata.clone(); altered.native_headers.as_mut().unwrap().object_id = [0x92; 32];
        assert!(verify_metadata_member_bitmap(&envelope, &altered, metadata_id, &mut SelectedNativeHeaderBudget::new(1024, 10_000), &|| false).is_err());
        let mut extra = ids.clone(); extra.push([0xff; 32]); extra.sort_unstable(); extra.dedup();
        let extra_wire = extra.iter().flat_map(|id| id.iter().copied()).collect::<Vec<_>>();
        let extra_envelope = BorrowedSelectedEnvelope { member_wire: &extra_wire, ..envelope };
        assert!(verify_metadata_member_bitmap(&extra_envelope, &metadata, metadata_id, &mut SelectedNativeHeaderBudget::new(1024, 10_000), &|| false).is_err());
    }

    #[test]
    fn large_envelope_decode_obeys_shape_bound() {
        let record = metadata(compiler_image().0);
        let mut envelope = CompilerPublicationEnvelope::new(&attempt(), &record).unwrap();
        let mut ids = (0..8191u64).map(|value| {
            let mut id = [0u8; 32]; id[24..].copy_from_slice(&value.to_be_bytes()); id
        }).collect::<Vec<_>>();
        ids.push(*envelope.metadata_id());
        ids.sort_unstable(); ids.dedup();
        envelope.physical_pack_id = physical_pack_id(&ids);
        envelope.member_ids = ids.into_boxed_slice();
        let bytes = envelope.canonical_bytes();
        let bound = compiler_envelope_decode_bound(&bytes).unwrap();
        let mut decoded = None;
        let peak = allocation_counter::measure(|| decoded = Some(CompilerPublicationEnvelope::decode(&bytes).unwrap()));
        assert_eq!(decoded.unwrap(), envelope);
        assert!(peak.bytes_max as usize <= bound, "large envelope decode peak {peak:?}, bound {bound}");
    }

    #[test]
    fn native_headers_v9_binds_exact_closure_member_and_extent() {
        let (image, _) = compiler_image();
        let program = CompilerProgramSourceMember { object_id: [0x11; 32], program: [0x22; 32], byte_length: 8 };
        let snapshot = CompilerTypeScriptSnapshotMember { object_id: [0x33; 32], byte_length: 8 };
        let legacy = metadata(image).with_native_program_sources(program).unwrap().with_typescript_snapshot_witness(snapshot).unwrap();
        let legacy_bytes = legacy.canonical_bytes();
        use backend_semantic::native_ts_headers::{NativeTsHeaderAggregate, NativeTsHeaderEntry, NativeTsHeaderFile};
        use backend_semantic::ir::{DeclarationFamilyId, DeclarationIdentity, SourceIdentity, VariantFingerprint};
        let aggregate = NativeTsHeaderAggregate { files: vec![NativeTsHeaderFile {
            path: "src/main.ts".into(), source: SourceIdentity::from_bytes(b"function f() {}\n").unwrap(),
            image_identity: [7; 32], image_byte_len: 123, producer_recipe: [8; 32],
            entries: vec![NativeTsHeaderEntry { entity_id: 7,
                identity: DeclarationIdentity { family: DeclarationFamilyId::from_raw([1; 16]), variant: VariantFingerprint::from_raw([2; 16]) },
                compiler_extent: (0, 15), header_extent: (0, 12) }],
        }] };
        let bytes = aggregate.encode_checked().unwrap();
        assert_eq!(native_headers_key(&bytes).unwrap(), ObjectKey::<backend_semantic::native_ts_headers::NativeTsHeaderAggregateSchema>::from_value(&aggregate).to_bytes());
        let mut decode_budget = usize::MAX;
        NativeTsHeaderAggregate::decode_canonical_budgeted(&bytes, &mut decode_budget).unwrap();
        let decoded = usize::MAX - decode_budget;
        let writer = bytes.len() * 3 + 2 * std::mem::size_of::<usize>() + decoded * 2;
        let reader = bytes.len() * 2 + decoded;
        let mut initial_budget = writer;
        let object = compiler_native_headers_object(&bytes, &mut initial_budget).unwrap();
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).unwrap();
        store.write_object(&object).unwrap();
        let verified = store.verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes())).unwrap();
        let mut exact = writer;
        compiler_native_headers_object(&bytes, &mut exact).unwrap();
        let header_member = CompilerNativeHeadersMember::from_verified_object(verified, &bytes, &mut exact).unwrap();
        assert_eq!(exact, 0);
        let mut short = writer - 1;
        compiler_native_headers_object(&bytes, &mut short).unwrap();
        let verified_short = store.verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes())).unwrap();
        assert!(CompilerNativeHeadersMember::from_verified_object(verified_short, &bytes, &mut short).is_err());
        let current = legacy.clone().with_native_headers(header_member).unwrap();
        let without_snapshot = metadata(compiler_image().0).with_native_program_sources(program).unwrap().with_native_headers(header_member).unwrap();
        assert_eq!(CompilerPublicationMetadata::decode(&without_snapshot.canonical_bytes()).unwrap(), without_snapshot);
        assert_eq!(legacy.canonical_bytes(), legacy_bytes);
        assert_eq!(CompilerPublicationMetadata::decode(&current.canonical_bytes()).unwrap(), current);
        assert_eq!(current.typed_object().schema(), COMPILER_NATIVE_HEADERS_METADATA_SCHEMA);
        assert!(metadata_member_ids(&current).contains(header_member.object_id()));
        let id = store.write_closure(&ClosureManifest::new(vec![object]).unwrap()).unwrap();
        let closure = store.open_closure(id).unwrap();
        assert_eq!(reopen_native_headers_bounded(&store, &closure, &current, reader).unwrap().as_deref(), Some(bytes.as_slice()));
        assert!(reopen_native_headers_bounded(&store, &closure, &current, reader - 1).is_err());
        assert!(reopen_native_headers_bounded(&store, &closure, &legacy, 0).unwrap().is_none());
        let mut altered = current.clone();
        altered.native_headers.as_mut().unwrap().byte_length += 1;
        assert!(reopen_native_headers_bounded(&store, &closure, &altered, 1024).is_err());
        let id = store.write_closure(&ClosureManifest::new(vec![member(b"other")]).unwrap()).unwrap();
        let missing = store.open_closure(id).unwrap();
        assert!(reopen_native_headers_bounded(&store, &missing, &current, 1024).is_err());
        drop(missing); drop(closure); drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_header_object_and_selected_reopen_have_bounded_allocation_peaks() {
        use allocation_counter::measure;
        use backend_semantic::ir::{DeclarationFamilyId, DeclarationIdentity, NativeProgramSource, NativeProgramSourceManifest, NativeProgramSourceManifestSchema, SourceIdentity, VariantFingerprint};
        use backend_semantic::native_ts_headers::{NativeTsHeaderAggregate, NativeTsHeaderAggregateSchema, NativeTsHeaderEntry, NativeTsHeaderFile};
        let source_bytes = vec![b'x'; 40_000];
        let source = SourceIdentity::from_bytes(&source_bytes).unwrap();
        let aggregate = NativeTsHeaderAggregate { files: vec![NativeTsHeaderFile {
            path: "src/main.ts".into(), source, image_identity: [7; 32], image_byte_len: 123, producer_recipe: [8; 32],
            entries: (0..16_000).map(|i| NativeTsHeaderEntry { entity_id: i,
                identity: DeclarationIdentity { family: DeclarationFamilyId::from_raw([1; 16]), variant: VariantFingerprint::from_raw([2; 16]) },
                compiler_extent: (i, i + 2), header_extent: (i, i + 1), }).collect(),
        }] };
        let bytes = aggregate.encode_checked().unwrap();
        let mut decoded_budget = usize::MAX;
        NativeTsHeaderAggregate::decode_canonical_budgeted(&bytes, &mut decoded_budget).unwrap();
        let decoded = usize::MAX - decoded_budget;
        let constructor_credit = 3 * bytes.len() + 2 * std::mem::size_of::<usize>() + decoded;
        let mut object = None;
        let constructed = measure(|| {
            let mut credit = constructor_credit;
            object = Some(compiler_native_headers_object(&bytes, &mut credit).unwrap());
        });
        assert!(constructed.bytes_max as usize <= constructor_credit, "constructor peak: {constructed:?}");
        let object = object.unwrap();
        let legacy = TypedObject::from_value(&ObjectKey::<NativeTsHeaderAggregateSchema>::from_value(&aggregate), &aggregate);
        assert_eq!(object.id(), legacy.id());
        assert_eq!(object.version(), legacy.version());
        let directory = scratch_store();
        let store = FileStore::open(&directory, 8 * 1024 * 1024).unwrap();
        store.write_object(&object).unwrap();
        let verified = store.verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes())).unwrap();
        let header_member = CompilerNativeHeadersMember::from_verified_object(verified, &bytes, &mut (bytes.len() + decoded)).unwrap();
        let program = NativeProgramSourceManifest::from_sources(ContentId::from_canonical_bytes(b"compiler"), vec![NativeProgramSource {
            program_path: "src/main.ts".into(), package_path: Some("src/main.ts".into()), source,
        }]).unwrap();
        let program_object = TypedObject::from_value(&ObjectKey::<NativeProgramSourceManifestSchema>::from_value(program.as_bytes()), program.as_bytes());
        store.write_object(&program_object).unwrap();
        let program_member = CompilerProgramSourceMember::from_verified_object(store.verify_object_claim(UntrustedObjectId::from_bytes(*program_object.id().as_bytes())).unwrap(), &program).unwrap();
        let (image, image_object) = compiler_image();
        let metadata = metadata(image).with_native_program_sources(program_member).unwrap().with_native_headers(header_member).unwrap();
        let attempt = attempt();
        let envelope = CompilerPublicationEnvelope::new(&attempt, &metadata).unwrap();
        let mut objects = vec![image_object, program_object, object, metadata.typed_object(), envelope.typed_object()];
        objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        let closure_id = store.write_closure(&ClosureManifest::new(objects).unwrap()).unwrap();
        let candidate = envelope.candidate(attempt.clone(), *closure_id.as_bytes()).unwrap();
        let selected = selected_generation(&attempt, &candidate, 1);
        let envelope_wire = envelope.canonical_bytes();
        let borrowed = borrowed_selected_envelope(&envelope_wire, &selected, selected.candidate_id(), &|| Ok(())).unwrap();
        assert_eq!(borrowed.metadata_id, *envelope.metadata_id());
        assert!(borrowed.members().eq(envelope.member_ids().iter().copied()));
        assert!(matches!(borrowed_selected_envelope(&envelope_wire, &selected, selected.candidate_id(), &|| Err(CompilerEnvelopeError::Stopped)), Err(CompilerEnvelopeError::Stopped)));
        // Parser-level qualification controls: these bytes are deliberately not
        // reauthenticated as a selected CAS object, and are not owner proof.
        macro_rules! reject_edit {
            ($edit:expr) => {{
                let mut changed = envelope.clone();
                ($edit)(&mut changed);
                assert!(borrowed_selected_envelope(&changed.canonical_bytes(), &selected, selected.candidate_id(), &|| Ok(())).is_err());
            }};
        }
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.namespace.package = "other".into());
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.namespace.source = "other".into());
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.namespace.branch = "other".into());
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.namespace.environment = "other".into());
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.profile = "other".into());
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.attempt_id[0] ^= 1);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.attempt_epoch ^= 1);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.attempt_fence[0] ^= 1);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.input_digest[0] ^= 1);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.logical_generation[0] ^= 1);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.semantic_manifest_root = Some([0x91; 32]));
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.physical_pack_id[0] ^= 1);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| e.metadata_id = [0x93; 32]);
        reject_edit!(|e: &mut CompilerPublicationEnvelope| { let mut ids = e.member_ids.to_vec(); ids.push(*selected.candidate_id()); ids.sort_unstable(); e.member_ids = ids.into_boxed_slice(); });
        reject_edit!(|e: &mut CompilerPublicationEnvelope| { let mut ids = e.member_ids.to_vec(); ids.push(ids[0]); ids.sort_unstable(); e.member_ids = ids.into_boxed_slice(); });
        let mut bad_flag = envelope_wire.clone();
        let flag = bad_flag.len() - envelope.member_ids().len() * 32 - 4 - 32 - 1;
        bad_flag[flag] = 2;
        assert!(borrowed_selected_envelope(&bad_flag, &selected, selected.candidate_id(), &|| Ok(())).is_err());
        let mut changed = envelope_wire.clone();
        let end = changed.len(); changed[end - 1] ^= 1;
        assert!(borrowed_selected_envelope(&changed, &selected, selected.candidate_id(), &|| Ok(())).is_err());
        let reader_credit = 2 * bytes.len() + decoded;
        // Measure payload work only after the metadata and closure are already open.
        let closure = store.open_closure(closure_id).unwrap();
        let mut payload = None;
        let payload_peak = measure(|| {
            payload = reopen_native_headers_bounded(&store, &closure, &metadata, reader_credit).unwrap();
        });
        assert_eq!(payload.as_deref(), Some(bytes.as_slice()));
        assert!(payload_peak.bytes_max as usize <= reader_credit, "payload reopen peak: {payload_peak:?}");
        drop(payload);
        drop(closure);
        let mut witness = None;
        let selected_peak = measure(|| {
            witness = Some(reopen_selected_native_headers(&store, &selected, reader_credit).unwrap().unwrap());
        });
        assert_eq!(witness.unwrap().as_bytes(), bytes.as_slice());
        // This end-to-end observation covers only this five-object, one-image
        // fixture. The selected metadata/closure overhead is not bounded by
        // reader_credit and needs a separate production budget.
        const SMALL_FIXTURE_OVERHEAD: usize = 256 * 1024;
        assert!(selected_peak.bytes_max as usize <= reader_credit + SMALL_FIXTURE_OVERHEAD, "small selected fixture peak: {selected_peak:?}");
        drop(store);
        // This is a synthetic selected-generation fixture, not an owner receipt.
        let store = FileStore::open(&directory, 8 * 1024 * 1024).unwrap();
        let mut scratch = vec![0u8; 4 * 1024 * 1024];
        let mut scratch_budget = SelectedNativeHeaderBudget::new(64 * 1024 * 1024, 100_000);
        let borrowed_header = reopen_selected_native_headers_in_scratch(&store, &selected, &mut scratch_budget, &|| false, &mut scratch).unwrap().unwrap();
        assert_eq!(borrowed_header.object_id(), header_member.object_id());
        assert_eq!(borrowed_header.view().file("src/main.ts").unwrap().entry(15_999).unwrap().entity_id, 15_999);
        drop(borrowed_header);
        assert_eq!(scratch_budget.remaining_memory(), 60 * 1024 * 1024);
        let mut tiny = [0u8; 1];
        assert!(reopen_selected_native_headers_in_scratch(&store, &selected, &mut SelectedNativeHeaderBudget::new(64 * 1024 * 1024, 100_000), &|| false, &mut tiny).is_err());
        assert!(reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(0, 100_000), &|| false).is_err());
        assert!(reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(64 * 1024 * 1024, 100_000), &|| true).is_err());
        let mut budget = SelectedNativeHeaderBudget::new(64 * 1024 * 1024, 100_000);
        let mut bounded = None;
        let bounded_peak = measure(|| bounded = Some(reopen_selected_native_headers_budgeted(&store, &selected, &mut budget, &|| false).unwrap().unwrap()));
        assert_eq!(bounded.unwrap().as_bytes(), bytes.as_slice());
        assert!(bounded_peak.bytes_max as usize <= 64 * 1024 * 1024, "budgeted selected peak: {bounded_peak:?}");
        let used_work = 100_000 - budget.remaining_work();
        assert!(reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(64 * 1024 * 1024, used_work), &|| false).is_ok());
        assert!(reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(64 * 1024 * 1024, used_work - 1), &|| false).is_err());
        let mut low = 0usize;
        let mut high = 64 * 1024 * 1024;
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            if reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(mid, 100_000), &|| false).is_ok() { high = mid; } else { low = mid; }
        }
        assert!(reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(high, 100_000), &|| false).is_ok());
        assert!(reopen_selected_native_headers_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(high - 1, 100_000), &|| false).is_err());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_lines_selected_cold_reopen_checks_member_budget_and_source() {
        use backend_semantic::ir::{NativeProgramSource, NativeProgramSourceManifest, NativeProgramSourceManifestSchema, SourceIdentity};
        use backend_semantic::native_source_lines::{NativeSourceLineAggregate, NativeSourceLineAggregateSchema, NativeSourceLineFile, NativeSourceLineAggregateView};
        let source_bytes = b"a\r\nb\nc";
        let source = SourceIdentity::from_bytes(source_bytes).unwrap();
        let aggregate = NativeSourceLineAggregate { files: vec![NativeSourceLineFile { path: "src/main.ts".into(), source, line_starts: vec![0, 3, 5] }] };
        let object = TypedObject::from_value(&ObjectKey::<NativeSourceLineAggregateSchema>::from_value(&aggregate), &aggregate);
        let line_member = CompilerNativeLinesMember { object_id: *object.id().as_bytes(), byte_length: object.bytes().len() as u32 };
        let program = NativeProgramSourceManifest::from_sources(ContentId::from_canonical_bytes(b"compiler"), vec![NativeProgramSource {
            program_path: "src/main.ts".into(), package_path: Some("src/main.ts".into()), source,
        }]).unwrap();
        let program_object = TypedObject::from_value(&ObjectKey::<NativeProgramSourceManifestSchema>::from_value(program.as_bytes()), program.as_bytes());
        let directory = scratch_store();
        let store = FileStore::open(&directory, 8 * 1024 * 1024).unwrap();
        store.write_object(&program_object).unwrap();
        let program_member = CompilerProgramSourceMember::from_verified_object(store.verify_object_claim(UntrustedObjectId::from_bytes(*program_object.id().as_bytes())).unwrap(), &program).unwrap();
        let (image, image_object) = compiler_image();
        let metadata = metadata(image).with_native_program_sources(program_member).unwrap().with_native_lines(line_member).unwrap();
        let attempt = attempt();
        let envelope = CompilerPublicationEnvelope::new(&attempt, &metadata).unwrap();
        let mut objects = vec![image_object.clone(), program_object.clone(), object.clone(), metadata.typed_object(), envelope.typed_object()];
        objects.sort_by_key(|o| (o.schema(), *o.key(), *o.version()));
        let closure = store.write_closure(&ClosureManifest::new(objects).unwrap()).unwrap();
        let selected = selected_generation(&attempt, &envelope.candidate(attempt.clone(), *closure.as_bytes()).unwrap(), 1);
        let mut scratch_credit = 8 * 1024 * 1024;
        let pin = store.pin_garbage_collection_bounded(&mut scratch_credit, |_| Ok(())).unwrap();
        let reader = store.open_bounded_nonrelation_object(&pin, UntrustedObjectId::from_bytes(*object.id().as_bytes()), &[COMPILER_NATIVE_LINES_SCHEMA], None, object.bytes().len(), &mut scratch_credit, |_| Ok(())).unwrap();
        let mut wire = vec![0; object.bytes().len()];
        let admitted = reader.read_into(&mut wire, &mut scratch_credit, |_, _| Ok(()), |view, _| {
            CompilerNativeLinesMember::from_verified_view(view, &mut |_| Ok(())).map_err(|_| backend_store::StoreError::Corrupt)
        }).unwrap();
        assert_eq!(admitted, line_member);
        drop(pin);
        drop(store);
        let store = FileStore::open(&directory, 8 * 1024 * 1024).unwrap();
        let mut delivered = (0usize, 0usize, 0usize);
        let mut visitor_budget = SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000);
        visit_selected_publication_wire(&store, &selected, &mut visitor_budget, &|| false, 8 * 1024 * 1024, 1, |wire, budget| {
            match wire {
                SelectedPublicationWire::Program { bytes, .. } => { assert!(!bytes.is_empty()); delivered.0 += 1; }
                SelectedPublicationWire::Image { bytes, .. } => { assert!(!bytes.is_empty()); delivered.1 += 1; }
                SelectedPublicationWire::Lines { bytes, .. } => { assert!(!bytes.is_empty()); delivered.2 += 1; }
                SelectedPublicationWire::Headers { .. } => panic!("unexpected header member"),
            }
            budget.reserve_retained(1).map_err(|e| format!("{e:?}"))?;
            Ok(())
        }).unwrap();
        assert_eq!(delivered, (1, 1, 1));
        assert_eq!(visitor_budget.remaining_memory(), 8 * 1024 * 1024 - 3);
        let stopped = std::cell::Cell::new(false);
        let mut detached = Vec::new();
        let late = visit_selected_publication_wire(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000), &|| stopped.get(), 8 * 1024 * 1024, 1, |wire, _| {
            if let SelectedPublicationWire::Program { .. } = wire { detached.push(1); stopped.set(true); }
            Ok(())
        });
        assert!(late.is_err());
        assert_eq!(detached, [1]); // This partial plan is never published after failure.
        let visitor_work = 100_000 - visitor_budget.remaining_work();
        assert!(visit_selected_publication_wire(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, visitor_work - 1), &|| false, 8 * 1024 * 1024, 1, |_, _| Ok(())).is_err());
        assert!(visit_selected_publication_wire(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000), &|| true, 8 * 1024 * 1024, 1, |_, _| Ok(())).is_err());
        assert!(visit_selected_publication_wire(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000), &|| false, 0, 1, |_, _| Ok(())).is_err());
        let mut credit = SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000);
        let witness = reopen_selected_native_lines_budgeted(&store, &selected, &mut credit, &|| false).unwrap().unwrap();
        assert_eq!(witness.object_id(), line_member.object_id());
        let view = NativeSourceLineAggregateView::parse(witness.as_bytes(), &mut |_| Ok::<(), ()>(())).unwrap();
        let file = view.find_file("src/main.ts", &mut |_| Ok::<(), ()>(())).unwrap().unwrap();
        assert!(file.matches_source(source_bytes, &mut |_| Ok::<(), ()>(())).unwrap());
        assert!(!file.matches_source(b"z\r\nb\nc", &mut |_| Ok::<(), ()>(())).unwrap());
        let used = 100_000 - credit.remaining_work();
        assert!(reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, used - 1), &|| false).is_err());
        assert!(reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, used), &|| false).is_ok());
        assert!(reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(0, 100_000), &|| false).is_err());
        let mut low = 0usize;
        let mut high = 8 * 1024 * 1024;
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            if reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(mid, 100_000), &|| false).is_ok() { high = mid; } else { low = mid; }
        }
        assert!(reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(high, 100_000), &|| false).is_ok());
        assert!(reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(high - 1, 100_000), &|| false).is_err());
        assert!(reopen_selected_native_lines_budgeted(&store, &selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000), &|| true).is_err());
        let mut missing = vec![image_object, program_object.clone(), metadata.typed_object(), envelope.typed_object()];
        missing.sort_by_key(|o| (o.schema(), *o.key(), *o.version()));
        let policy = ArtifactBudget::new(16, 64, 8 * 1024 * 1024, 1024 * 1024, 64);
        let verifier = FileStoreCompilerPublicationVerifier::new(&store, policy, &envelope, &metadata);
        let valid_manifest = store.open_closure(closure).unwrap();
        verify_native_lines_for_admission(&store, &valid_manifest, &metadata, policy, None).unwrap();
        drop(valid_manifest);
        // These physically valid object IDs are not sufficient to admit an
        // incorrectly typed or malformed line member before selection.
        struct TestLineBytes;
        impl Schema for TestLineBytes {
            const DOMAIN: u8 = 0x7a; const TYPE: u16 = 0xc00f; const VERSION: u8 = 1;
            type Value = [u8];
            fn encode(value: &Self::Value, out: &mut Vec<u8>) { out.extend_from_slice(value); }
        }
        let bad_wire: &[u8] = b"NLM1\0\0\0\x01";
        let bad_object = TypedObject::from_value(&ObjectKey::<TestLineBytes>::from_value(bad_wire), bad_wire);
        for bad_member in [
            CompilerNativeLinesMember { object_id: *program_object.id().as_bytes(), byte_length: program_object.bytes().len() as u32 },
            CompilerNativeLinesMember { object_id: *bad_object.id().as_bytes(), byte_length: bad_object.bytes().len() as u32 },
        ] {
            let (bad_image, bad_image_object) = compiler_image();
            let bad_metadata = self::metadata(bad_image).with_native_program_sources(program_member).unwrap().with_native_lines(bad_member).unwrap();
            let bad_envelope = CompilerPublicationEnvelope::new(&attempt, &bad_metadata).unwrap();
            let mut payloads = vec![bad_image_object, program_object.clone(), bad_metadata.typed_object(), bad_envelope.typed_object()];
            if bad_member.object_id == *bad_object.id().as_bytes() { payloads.push(bad_object.clone()); }
            payloads.sort_by_key(|o| (o.schema(), *o.key(), *o.version()));
            let bad_closure = store.write_closure(&ClosureManifest::new(payloads).unwrap()).unwrap();
            let bad_candidate = bad_envelope.candidate(attempt.clone(), *bad_closure.as_bytes()).unwrap();
            let bad_verifier = FileStoreCompilerPublicationVerifier::new(&store, policy, &bad_envelope, &bad_metadata);
            let error = bad_verifier.verify_closure(&ClosureClaim::for_candidate(&bad_candidate)).unwrap_err();
            assert!(error.contains("candidate line member"), "must reach typed line validation: {error}");
        }
        let missing_closure = store.write_closure(&ClosureManifest::new(missing).unwrap()).unwrap();
        let missing_selected = selected_generation(&attempt, &envelope.candidate(attempt.clone(), *missing_closure.as_bytes()).unwrap(), 2);
        let missing_candidate = envelope.candidate(attempt.clone(), *missing_closure.as_bytes()).unwrap();
        assert!(verifier.verify_closure(&ClosureClaim::for_candidate(&missing_candidate)).is_err());
        assert!(reopen_selected_native_lines_budgeted(&store, &missing_selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000), &|| false).is_err());
        assert!(visit_selected_publication_wire(&store, &missing_selected, &mut SelectedNativeHeaderBudget::new(8 * 1024 * 1024, 100_000), &|| false, 8 * 1024 * 1024, 1, |_, _| Ok(())).is_err());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn selected_metadata_keeps_snapshot_payload_lazy_but_explicit_reopen_refuses_missing_bytes() {
        use backend_semantic::ir::{
            NativeProgramSource, NativeProgramSourceManifest, NativeProgramSourceManifestSchema,
            SourceIdentity,
        };

        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("open FileStore");
        let (image, image_object) = compiler_image();
        let sources = NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            vec![NativeProgramSource {
                program_path: "workspace/main.ts".into(),
                package_path: Some("main.ts".into()),
                source: SourceIdentity::from_bytes(b"export {}; ").expect("source"),
            }],
        )
        .expect("program source manifest");
        let program_object = TypedObject::from_value(
            &ObjectKey::<NativeProgramSourceManifestSchema>::from_value(sources.as_bytes()),
            sources.as_bytes(),
        );
        store.write_object(&program_object).expect("write program");
        let program_member = CompilerProgramSourceMember::from_verified_object(
            store
                .verify_object_claim(UntrustedObjectId::from_bytes(
                    *program_object.id().as_bytes(),
                ))
                .expect("verify program"),
            &sources,
        )
        .expect("typed program member");

        let certificate_bytes = b"authenticated cold replay certificate";
        let certificate_object = TypedObject::from_value(
            &ObjectKey::<backend_semantic::ir::TypeScriptSnapshotWitnessSchema>::from_value(
                certificate_bytes,
            ),
            certificate_bytes,
        );
        store
            .write_object(&certificate_object)
            .expect("write certificate");
        let certificate_member = CompilerTypeScriptSnapshotMember::from_verified_object(
            store
                .verify_object_claim(UntrustedObjectId::from_bytes(
                    *certificate_object.id().as_bytes(),
                ))
                .expect("verify certificate"),
            certificate_bytes,
        )
        .expect("typed certificate member");
        let metadata = metadata(image)
            .with_native_program_sources(program_member)
            .expect("program metadata")
            .with_typescript_snapshot_witness(certificate_member)
            .expect("snapshot metadata");
        let attempt = attempt();
        let envelope =
            CompilerPublicationEnvelope::new(&attempt, &metadata).expect("selected envelope");
        let metadata_object = metadata.typed_object();
        let envelope_object = envelope.typed_object();
        let mut closure_objects = vec![
            image_object,
            program_object,
            certificate_object.clone(),
            metadata_object,
            envelope_object,
        ];
        closure_objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        let closure = ClosureManifest::new(closure_objects).expect("complete selected closure");
        let closure_id = store.write_closure(&closure).expect("persist closure");
        let candidate = envelope
            .candidate(attempt.clone(), *closure_id.as_bytes())
            .expect("candidate");
        let selected = selected_generation(&attempt, &candidate, 1);

        let certificate_name = certificate_object
            .id()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let certificate_path = directory
            .join("objects")
            .join(format!("{certificate_name}.object"));
        assert_eq!(
            reopen_selected_typescript_snapshot_witness(&store, &selected)
                .expect("read selected certificate")
                .expect("certificate is present")
                .as_bytes(),
            certificate_bytes,
        );
        drop(store);

        let original_payload = std::fs::read(&certificate_path).expect("read certificate CAS");
        let mut certificate_permissions = std::fs::metadata(&certificate_path)
            .expect("certificate metadata")
            .permissions();
        certificate_permissions.set_readonly(false);
        std::fs::set_permissions(&certificate_path, certificate_permissions)
            .expect("make test CAS writable");
        std::fs::write(&certificate_path, b"corrupt CAS envelope")
            .expect("corrupt optional certificate payload");
        let corrupt_store = FileStore::open(&directory, 1024 * 1024).expect("reopen corrupt CAS");
        reopen_selected_compiler_metadata(&corrupt_store, &selected)
            .expect("metadata selection ignores optional payload corruption");
        assert!(reopen_selected_typescript_snapshot_witness(&corrupt_store, &selected).is_err());
        drop(corrupt_store);
        let mut certificate_permissions = std::fs::metadata(&certificate_path)
            .expect("corrupt certificate metadata")
            .permissions();
        certificate_permissions.set_readonly(false);
        std::fs::set_permissions(&certificate_path, certificate_permissions)
            .expect("make corrupt test CAS writable");
        std::fs::write(&certificate_path, original_payload).expect("restore certificate CAS");
        let mut certificate_permissions = std::fs::metadata(&certificate_path)
            .expect("restored certificate metadata")
            .permissions();
        certificate_permissions.set_readonly(true);
        std::fs::set_permissions(&certificate_path, certificate_permissions)
            .expect("reseal test CAS");
        let mut certificate_permissions = std::fs::metadata(&certificate_path)
            .expect("resealed certificate metadata")
            .permissions();
        certificate_permissions.set_readonly(false);
        std::fs::set_permissions(&certificate_path, certificate_permissions)
            .expect("make test CAS removable");
        std::fs::remove_file(&certificate_path)
            .expect("simulate missing optional certificate payload");

        let missing_store = FileStore::open(&directory, 1024 * 1024).expect("reopen missing CAS");
        let reopened = reopen_selected_compiler_metadata(&missing_store, &selected)
            .expect("metadata selection reads only the exact member inventory");
        assert_eq!(reopened.metadata(), &metadata);
        assert!(reopen_selected_typescript_snapshot_witness(&missing_store, &selected).is_err());
        drop(missing_store);
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn forest_membership_reopens_one_durable_object_with_exact_leaf_owners() {
        use backend_semantic::ir::{
            NativeProgramForestSchema, NativeProgramNode, NativeProgramSource,
            NativeProgramSourceManifest, NativeProgramSources, SourceIdentity,
        };
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let leaf = |path: &str| {
            NativeProgramSourceManifest::from_sources(
                toolchain,
                vec![NativeProgramSource {
                    program_path: path.into(),
                    package_path: Some(path.into()),
                    source: SourceIdentity::from_bytes(b"export {};").expect("source"),
                }],
            )
            .expect("leaf")
        };
        let app = leaf("src/app.ts");
        let config = leaf("vite.config.ts");
        let mut builder = backend_semantic::ir::IrBuilder::new();
        let source = SourceIdentity::from_bytes(b"export {};").expect("source");
        let recipe = backend_semantic::vocabulary::CompileRecipeFact::derive(
            backend_semantic::vocabulary::LanguageProfile::TypeScript(
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
            ),
            backend_semantic::vocabulary::Stage::LowerIr,
            backend_semantic::vocabulary::NativeTool::TypeScriptCompiler,
            source.identity,
            toolchain,
        );
        builder
            .set_image_provenance(
                source,
                recipe,
                backend_semantic::ir::PackageLineage::new("npm", "forest-fixture")
                    .expect("lineage"),
                "src/app.ts",
            )
            .expect("image owner");
        let ir = builder.finish().expect("image");
        let mut image_bytes =
            vec![0; backend_semantic::ir::full_semantic_image_len(&ir).expect("length")];
        backend_semantic::ir::encode_full_semantic_image(&ir, &mut image_bytes)
            .expect("encoded image");
        let image_identity = *SemanticImageIdentity::from_encoded_bytes(&image_bytes).as_ref();
        let (image, image_object) = compiler_image_from_bytes(&image_bytes);
        let sources = NativeProgramSources::from_forest(
            toolchain,
            1,
            vec![
                NativeProgramNode {
                    config_path: "tsconfig.app.json".into(),
                    config: ContentId::from_canonical_bytes(b"app"),
                    environment: [1; 32],
                    references: vec![],
                    program: Some(app.clone()),
                },
                NativeProgramNode {
                    config_path: "tsconfig.json".into(),
                    config: ContentId::from_canonical_bytes(b"root"),
                    environment: [2; 32],
                    references: vec![0, 2],
                    program: None,
                },
                NativeProgramNode {
                    config_path: "tsconfig.node.json".into(),
                    config: ContentId::from_canonical_bytes(b"node"),
                    environment: [3; 32],
                    references: vec![],
                    program: Some(config.clone()),
                },
            ],
        )
        .expect("forest")
        .with_image_bindings([("src/app.ts".to_owned(), image_identity)])
        .expect("partial image binding");
        let object = TypedObject::from_value(
            &ObjectKey::<NativeProgramForestSchema>::from_value(sources.as_bytes()),
            sources.as_bytes(),
        );
        let directory = scratch_store();
        let store = FileStore::open(&directory, 1024 * 1024).expect("store");
        let id = store
            .write_closure(
                &ClosureManifest::new(vec![object.clone(), image_object.clone()])
                    .expect("one membership object and exact image"),
            )
            .expect("durable closure");
        let verified = store
            .verify_object_claim(UntrustedObjectId::from_bytes(*object.id().as_bytes()))
            .expect("verified");
        let member = CompilerProgramSourceMember::from_verified_program_sources(verified, &sources)
            .expect("forest member");
        let metadata = metadata(image.clone())
            .with_native_program_sources(member)
            .expect("metadata");
        let metadata = CompilerPublicationMetadata::decode(&metadata.canonical_bytes())
            .expect("metadata cold decode");
        drop(store);
        let store = FileStore::open(&directory, 1024 * 1024).expect("cold store");
        let closure = store.open_closure(id).expect("selected closure");
        let reopened = reopen_native_program_sources(&closure, &metadata)
            .expect("reopen forest")
            .expect("present");
        assert_eq!(reopened, sources);
        verify_semantic_image_members(&store, &closure, &metadata)
            .expect("cold image binds exact program node");
        for bindings in [
            vec![],
            vec![(
                "src/app.ts".to_owned(),
                *SemanticImageIdentity::from_encoded_bytes(b"another program output").as_ref(),
            )],
        ] {
            let invalid = sources
                .clone()
                .with_image_bindings(bindings)
                .expect("canonical alternative binding");
            let invalid_object = TypedObject::from_value(
                &ObjectKey::<NativeProgramForestSchema>::from_value(invalid.as_bytes()),
                invalid.as_bytes(),
            );
            let invalid_id = store
                .write_closure(
                    &ClosureManifest::new(vec![invalid_object.clone(), image_object.clone()])
                        .unwrap(),
                )
                .unwrap();
            let verified = store
                .verify_object_claim(UntrustedObjectId::from_bytes(
                    *invalid_object.id().as_bytes(),
                ))
                .unwrap();
            let invalid_member =
                CompilerProgramSourceMember::from_verified_program_sources(verified, &invalid)
                    .unwrap();
            let invalid_metadata = super::tests::metadata(image.clone())
                .with_native_program_sources(invalid_member)
                .unwrap();
            let invalid_closure = store.open_closure(invalid_id).unwrap();
            assert!(
                verify_semantic_image_members(&store, &invalid_closure, &invalid_metadata).is_err(),
                "missing or mismatched image binding cannot reopen positive authority"
            );
        }
        assert!(reopened.single_program().is_none());
        let owners = reopened
            .sources()
            .map(|row| (row.package_path.unwrap().to_owned(), row.program))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(owners["src/app.ts"], app.program());
        assert_eq!(owners["vite.config.ts"], config.program());
        assert_ne!(owners["src/app.ts"], owners["vite.config.ts"]);
        let unrelated = ClosureManifest::new(vec![self::member(b"unrelated")]).expect("unrelated");
        let unrelated_id = store.write_closure(&unrelated).expect("write unrelated");
        let unrelated = store.open_closure(unrelated_id).expect("open unrelated");
        assert!(reopen_native_program_sources(&unrelated, &metadata).is_err());
        drop(unrelated);
        drop(closure);
        drop(store);
        std::fs::remove_dir_all(directory).expect("cleanup");
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
        manifest_bytes[8..10].copy_from_slice(
            &CompilationManifestFormat::SemanticV2
                .wire_version()
                .to_le_bytes(),
        );
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
    fn engine_contextual_manifest_round_trips_versioned_closure_and_ordinals() {
        use backend_engine::driver::{CompiledFragment, CompiledSemantic};
        use backend_engine::publication::manifest::SemanticImageRegion;
        use backend_engine::publication::publication::publish_semantic;
        use backend_engine::publication::semantic_immutable::ImmutableSemanticImageStore;
        use backend_engine::publication::{PublishControl, SemanticPublicationScratch};
        use backend_semantic::ir::{
            FragmentView, IrBuilder, PackageLineage, PreparedFragment, SourceIdentity,
        };
        use backend_semantic::vocabulary::{
            CompileRecipeFact, LanguageProfile, NativeTool, PythonVersion, Stage,
        };
        use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
        use backend_version::{SourceFactDomain, ToolchainDomain};
        use std::num::NonZeroUsize;

        let directory = scratch_store();
        std::fs::create_dir(&directory).expect("create Engine publication directory");
        let artifacts = directory.join("artifacts");
        let publisher = DurablePublisher::create(
            &PublicationPaths::in_directory(&directory.join("journal")),
            PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
                .expect("publication limits"),
        )
        .expect("create compiler publication owner");
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b""),
            byte_len: 0,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Python(PythonVersion::Python314),
            Stage::LowerIr,
            NativeTool::Python,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"contextual-toolchain"),
        );
        let mut compact = [0_u8; 256];
        let length = PreparedFragment::prepare(source, recipe, &[], &[], &[])
            .expect("prepare same empty compact module")
            .write_into(&mut compact)
            .expect("encode compact module")
            .len();
        let make = |path| {
            let fragment =
                FragmentView::validate(&compact[..length]).expect("admit compact module");
            let mut builder = IrBuilder::new();
            builder
                .set_image_provenance(
                    source,
                    recipe,
                    PackageLineage::new("pypi", "httpie").expect("package lineage"),
                    path,
                )
                .expect("capture distinct module path");
            CompiledSemantic {
                artifact: CompiledFragment {
                    source,
                    recipe,
                    fragment,
                },
                ir: builder.finish().expect("finish context-bound image"),
            }
        };
        let mut manifest = [0_u8; 1024];
        let mut facts = [None; 2];
        let mut ordinals = [0_usize; 2];
        let mut plan = [SemanticImageRegion::EMPTY; 2];
        let mut semantic_bytes = [0_u8; 16_384];
        let mut locality = [0_u8; 1024];
        let mut binding = [0_u8; COMPILATION_BINDING_BYTES];
        let compiled = [
            make("httpie/cli/__init__.py"),
            make("httpie/output/__init__.py"),
        ];
        let published = publish_semantic(
            &publisher,
            &artifacts,
            &compiled,
            PublishControl::Continue,
            SemanticPublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut facts,
                ordinals: &mut ordinals,
                semantic_image_plan: &mut plan,
                semantic_image_output: &mut semantic_bytes,
                locality_output: &mut locality,
                binding_output: &mut binding,
            },
        )
        .expect("Engine publishes both identical-content contextual modules");
        assert_eq!(
            published.manifest.format,
            CompilationManifestFormat::SemanticV3
        );
        assert_eq!(published.manifest.fragment_count, 2);
        assert_eq!(
            facts[0].expect("first fact").fragment,
            facts[1].expect("second fact").fragment
        );
        let manifest =
            &manifest[..usize::try_from(published.manifest.byte_length).expect("manifest length")];
        let image_store =
            ImmutableSemanticImageStore::new(&artifacts).expect("open Engine image store");
        let mut image_output = [0_u8; 16_384];
        let mut images = Vec::new();
        let mut image_objects = Vec::new();
        let mut plane_publications = Vec::new();
        for (ordinal, fact) in facts.iter().enumerate() {
            let image_fact = fact
                .expect("canonical fact")
                .semantic_image
                .expect("paired image");
            let image = image_store
                .open(image_fact, &mut image_output)
                .expect("reopen actual Engine image");
            let (member, object) = CompilerImageMember::from_bytes_for_ordinal(
                u32::try_from(ordinal).expect("ordinal"),
                image.as_ref(),
                *image_fact.identity.as_ref(),
            )
            .expect("admit actual Engine image");
            plane_publications.push(semantic_planes(
                backend_semantic::ir::GenerationId::from_canonical_bytes(image.as_ref()),
            ));
            images.push(member);
            image_objects.push(object);
        }
        let planes = VersionedPlaneMetadata::from_artifacts(
            plane_publications
                .iter()
                .enumerate()
                .map(|(ordinal, planes)| {
                    plane_artifact_at(planes, u32::try_from(ordinal).expect("ordinal"))
                })
                .collect(),
        )
        .expect("complete exact per-image catalog");
        let metadata = CompilerPublicationMetadata::new_with_versioned_planes(
            manifest,
            binding,
            images.clone(),
            Some(planes.clone()),
        )
        .expect("Turso admits actual contextual compiler manifest");
        assert_eq!(metadata.manifest_version(), 3);
        assert_eq!(
            CompilerPublicationMetadata::decode(&metadata.canonical_bytes())
                .expect("metadata roundtrip"),
            metadata
        );
        let attempt = attempt();
        let envelope =
            CompilerPublicationEnvelope::new(&attempt, &metadata).expect("contextual envelope");
        assert_eq!(
            CompilerPublicationEnvelope::decode(&envelope.canonical_bytes())
                .expect("envelope roundtrip"),
            envelope
        );
        let plane_objects = combine_plane_objects(&plane_publications.iter().collect::<Vec<_>>());
        let closure_directory = scratch_store();
        let (store, receipt) = publish_envelope_closure_with_images(
            &closure_directory,
            &envelope,
            &metadata,
            &image_objects,
            &plane_objects,
            false,
        );
        let candidate = envelope
            .candidate(attempt.clone(), *receipt.closure().as_bytes())
            .expect("contextual candidate");
        FileStoreCompilerPublicationVerifier::new(
            &store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &envelope,
            &metadata,
        )
        .verify_closure(&ClosureClaim::for_candidate(&candidate))
        .expect("exact contextual closure proof");
        let selected = selected_generation(&attempt, &candidate, 1);
        drop(store);
        let reopened_store = FileStore::open(&closure_directory, 1024 * 1024)
            .expect("kernel-cold closure store reopen");
        let reopened = reopen_selected_compiler_publication(
            &reopened_store,
            ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8),
            &selected,
        )
        .expect("cold contextual selected publication");
        assert_eq!(reopened.metadata().manifest_version(), 3);
        assert_eq!(reopened.images().len(), 2);
        for image in reopened.images() {
            let ordinal = usize::try_from(image.member().artifact_ordinal()).expect("ordinal");
            assert_eq!(image.bytes(), image_objects[ordinal].bytes());
            assert_eq!(image.member(), &images[ordinal]);
        }
        let mut wrong_ordinals = images;
        wrong_ordinals[0].artifact_ordinal = 1;
        wrong_ordinals[1].artifact_ordinal = 0;
        assert_eq!(
            CompilerPublicationMetadata::new_with_versioned_planes(
                manifest,
                binding,
                wrong_ordinals,
                Some(planes)
            ),
            Err(CompilerEnvelopeError::MetadataMismatch)
        );
        drop(reopened_store);
        publisher
            .shutdown()
            .expect("retire compiler publication owner");
        std::fs::remove_dir_all(directory).expect("remove compiler artifacts");
        std::fs::remove_dir_all(closure_directory).expect("remove contextual closure");
    }

    #[test]
    fn semantic_manifest_generations_preserve_v2_and_reject_unknown_or_malformed() {
        let (image, _) = compiler_image();
        let legacy = metadata(image);
        assert_eq!(legacy.manifest_version(), 2);
        assert_eq!(
            CompilerPublicationMetadata::decode(&legacy.canonical_bytes())
                .expect("legacy metadata roundtrip"),
            legacy
        );
        for version in [0_u16, 1, 4, u16::MAX] {
            let mut bytes = legacy.manifest_bytes().to_vec();
            bytes[8..10].copy_from_slice(&version.to_le_bytes());
            assert_eq!(
                inspect_semantic_manifest(&bytes),
                Err(CompilerEnvelopeError::ManifestFacts)
            );
        }
        for version in [2_u16, 3] {
            let mut bytes = legacy.manifest_bytes().to_vec();
            bytes[8..10].copy_from_slice(&version.to_le_bytes());
            assert_eq!(
                inspect_semantic_manifest(&bytes)
                    .expect("supported semantic generation")
                    .1
                    .wire_version(),
                version
            );
            let mut reserved = bytes.clone();
            reserved[10] = 1;
            assert_eq!(
                inspect_semantic_manifest(&reserved),
                Err(CompilerEnvelopeError::ManifestFacts)
            );
            let mut trailing = bytes.clone();
            trailing.push(0);
            assert_eq!(
                inspect_semantic_manifest(&trailing),
                Err(CompilerEnvelopeError::ManifestFacts)
            );
            bytes.pop();
            assert_eq!(
                inspect_semantic_manifest(&bytes),
                Err(CompilerEnvelopeError::ManifestFacts)
            );
        }
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
        let stopped = || true;
        let stopped_verifier = FileStoreCompilerPublicationVerifier::new(
            &store, ArtifactBudget::new(8, 64, 1024 * 1024, 1024 * 1024, 8), &envelope, &metadata,
        ).with_stop(&stopped);
        assert!(stopped_verifier.verify_closure(&claim).unwrap_err().contains("verification stopped"));
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
