//! Shared canonical wire types for owner-fenced compiler result objects.

use crate::application::StagedVersionedPlaneArtifact;
use crate::compiler_input_manifest::CompilerInputManifestError;
use crate::publication::binding::{
    COMPILATION_BINDING_BYTES, CompilationBindingFacts, CompilationBindingView,
};
use crate::publication::manifest::{
    CompilationManifestFacts, CompilationManifestIdentity, CompilationManifestView,
};
use backend_cluster_transport::AssignmentScope;
use backend_semantic::ir::SemanticPlaneManifest;
use backend_store::hydration::VerifiedGenerationFacts;
use backend_store::{
    ArtifactObjectClaim, ClosureId, FileStore, ObjectId, TypedObject, UntrustedObjectId,
};
use backend_version::object::{ObjectKind, ObjectLength, ObjectRef};
use backend_version::schema::SchemaId;
use backend_version::{ObjectDomain, ObjectKey, ObjectVersion, Schema, SchemaIdentity};
use std::collections::BTreeSet;
use thiserror::Error;

const RESULT_MAGIC: &[u8; 8] = b"BKCRES04";
const RESULT_OUTPUT_KEY_DOMAIN: &[u8] = b"backend.cluster.compiler-output.v1\0";
pub(super) const RESULT_OUTPUT_SCHEMA_DOMAIN: u8 = 0xe8;
pub(super) const RESULT_OUTPUT_SCHEMA_TYPE: u16 = 1;
pub(super) const RESULT_OUTPUT_SCHEMA_VERSION: u8 = 1;
pub(super) const RESULT_ENVELOPE_SCHEMA_TYPE: u16 = 2;
pub(super) const RESULT_ENVELOPE_SCHEMA_VERSION: u8 = 4;
const RESULT_MEMBER_FIXED_BYTES: usize = 1 + 4 + 2 + 32 + 8 + 8 + 1 + 8 + 32;
pub(super) const MAX_RESULT_ENVELOPE_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_RESULT_MEMBERS: usize = 100_002;
pub(super) const RESULT_MANIFEST_ENTRY_KEY: u64 = 0;

/// Shared typed schema for one output member of a compiler result closure.
pub struct CompilerResultOutputSchema;

impl Schema for CompilerResultOutputSchema {
    const DOMAIN: u8 = RESULT_OUTPUT_SCHEMA_DOMAIN;
    const TYPE: u16 = RESULT_OUTPUT_SCHEMA_TYPE;
    const VERSION: u8 = RESULT_OUTPUT_SCHEMA_VERSION;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Shared typed schema for the complete owner-fenced compiler result envelope.
pub struct CompilerResultEnvelopeSchema;

impl Schema for CompilerResultEnvelopeSchema {
    const DOMAIN: u8 = RESULT_OUTPUT_SCHEMA_DOMAIN;
    const TYPE: u16 = RESULT_ENVELOPE_SCHEMA_TYPE;
    const VERSION: u8 = RESULT_ENVELOPE_SCHEMA_VERSION;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Closed role for one output object in canonical generation order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerResultMemberRole {
    /// The package's canonical semantic compilation manifest.
    Manifest,
    /// A compact IR fragment referenced by the manifest.
    Fragment,
    /// A complete portable semantic image paired with a fragment.
    SemanticImage,
    /// An independently typed compiler output plane, such as an embedding batch.
    /// It is closure-admitted but does not count as semantic IR generation coverage.
    AuxiliaryPlane,
    /// A canonical c005 per-artifact semantic-plane manifest.
    VersionedPlaneManifest,
    /// A canonical c004 segment referenced by one exact per-artifact plane manifest.
    VersionedPlaneSegment,
}

/// One generation member committed by the shared worker result envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerResultMemberV1 {
    pub(super) role: CompilerResultMemberRole,
    pub(super) reference: ObjectRef<ObjectDomain>,
    pub(super) key: u64,
    pub(super) parent: Option<u64>,
    pub(super) object_id: [u8; 32],
}

/// Borrowed-payload descriptor for streaming one typed result member into a CAS closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompilerResultOutputClaim {
    role: CompilerResultMemberRole,
    reference: ObjectRef<ObjectDomain>,
    key: u64,
    parent: Option<u64>,
    artifact_claim: ArtifactObjectClaim,
}

impl CompilerResultOutputClaim {
    /// Exact typed `ArtifactObjectClaim` to pass to `StreamingClosureBuilder::begin_object`.
    #[must_use]
    pub const fn artifact_claim(self) -> ArtifactObjectClaim {
        self.artifact_claim
    }

    #[must_use]
    pub const fn role(self) -> CompilerResultMemberRole {
        self.role
    }

    #[must_use]
    pub const fn reference(self) -> ObjectRef<ObjectDomain> {
        self.reference
    }

    #[must_use]
    pub const fn key(self) -> u64 {
        self.key
    }

    #[must_use]
    pub const fn parent(self) -> Option<u64> {
        self.parent
    }

    /// Binds the ID returned by the completed store stream to this exact output member.
    #[must_use]
    pub const fn member(self, object_id: ObjectId) -> CompilerResultMemberV1 {
        CompilerResultMemberV1 {
            role: self.role,
            reference: self.reference,
            key: self.key,
            parent: self.parent,
            object_id: *object_id.as_bytes(),
        }
    }
}

impl CompilerResultMemberV1 {
    /// Returns the canonical generation-member role.
    #[must_use]
    pub const fn role(self) -> CompilerResultMemberRole {
        self.role
    }

    /// Returns the exact typed semantic object reference.
    #[must_use]
    pub const fn reference(self) -> ObjectRef<ObjectDomain> {
        self.reference
    }

    /// Returns the stable generation entry key.
    #[must_use]
    pub const fn key(self) -> u64 {
        self.key
    }

    /// Returns the containing generation entry, or None for the manifest root.
    #[must_use]
    pub const fn parent(self) -> Option<u64> {
        self.parent
    }

    /// Returns the physical backend-store identity of the typed output object.
    #[must_use]
    pub const fn object_id(self) -> [u8; 32] {
        self.object_id
    }
}

/// Canonical, owner-fenced result metadata shared by worker and coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerResultEnvelopeV1 {
    pub(super) scope: AssignmentScope,
    pub(super) input_closure_id: [u8; 32],
    pub(super) input_manifest_object_id: [u8; 32],
    pub(super) generation: VerifiedGenerationFacts,
    pub(super) manifest_identity: CompilationManifestIdentity,
    pub(super) binding_facts: CompilationBindingFacts,
    pub(super) binding_bytes: Box<[u8]>,
    pub(super) members: Box<[CompilerResultMemberV1]>,
    auxiliary_planes: Box<[CompilerResultMemberV1]>,
    versioned_plane_members: Box<[CompilerResultMemberV1]>,
}

impl CompilerResultEnvelopeV1 {
    /// Reopens one shared typed result-envelope object.
    pub fn from_typed_object(object: &TypedObject) -> Result<Self, CompilerResultError> {
        let schema = object.schema();
        if schema.domain() != RESULT_OUTPUT_SCHEMA_DOMAIN
            || schema.ty() != RESULT_ENVELOPE_SCHEMA_TYPE
            || schema.version() != RESULT_ENVELOPE_SCHEMA_VERSION
        {
            return Err(CompilerResultError::EnvelopeSchema);
        }
        Self::decode(object.bytes())
    }

    /// Decodes and revalidates the exact canonical result-envelope wire bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, CompilerResultError> {
        if bytes.len() > MAX_RESULT_ENVELOPE_BYTES {
            return Err(CompilerResultError::EnvelopeLimit);
        }
        let mut reader = ResultReader::new(bytes);
        if reader.take(RESULT_MAGIC.len())? != RESULT_MAGIC {
            return Err(CompilerResultError::EnvelopeHeader);
        }
        let namespace_id = reader.array16()?;
        let work_id = reader.array16()?;
        let attempt = reader.u64_be()?;
        let fence = reader.array32()?;
        let scope = AssignmentScope::new(namespace_id, work_id, attempt, fence)
            .map_err(|_| CompilerResultError::EnvelopeScope)?;
        let input_closure_id = reader.array32()?;
        let input_manifest_object_id = reader.array32()?;
        let pinned_root = reader.array32()?;
        let dep_set = reader.array32()?;
        let manifest_identity = CompilationManifestIdentity::try_from(reader.array32()?.as_slice())
            .map_err(|_| CompilerResultError::ManifestIdentity)?;
        let binding_identity = reader.array32()?;
        let binding_length = reader.u32_be()?;
        if usize::try_from(binding_length).ok() != Some(COMPILATION_BINDING_BYTES) {
            return Err(CompilerResultError::BindingLength);
        }
        let binding_bytes = reader
            .take(COMPILATION_BINDING_BYTES)?
            .to_vec()
            .into_boxed_slice();
        let member_count =
            usize::try_from(reader.u32_be()?).map_err(|_| CompilerResultError::EnvelopeLimit)?;
        if member_count == 0 || member_count > MAX_RESULT_MEMBERS {
            return Err(CompilerResultError::MemberCount);
        }
        // Enforce canonical section order without allocating, then reserve only the members
        // each typed output vector will retain. Per-section validators below check exact order
        // and topology, avoiding a full envelope re-encode solely for a canonicality comparison.
        let member_counts = count_result_member_roles(&bytes[reader.offset..], member_count)?;
        let mut buffers = ResultMemberBuffers::reserve(member_counts)?;
        for _ in 0..member_count {
            let role = decode_result_member_role(reader.byte()?)?;
            let schema = SchemaId::try_from(reader.u32_be()?)
                .map_err(|_| CompilerResultError::MemberReference)?;
            let kind = u16::from_be_bytes(reader.array2()?);
            let content = reader.array32()?;
            let length = reader.u64_be()?;
            let key = reader.u64_be()?;
            let parent = match reader.byte()? {
                0 => {
                    if reader.u64_be()? != 0 {
                        return Err(CompilerResultError::MemberReference);
                    }
                    None
                }
                1 => Some(reader.u64_be()?),
                _ => return Err(CompilerResultError::MemberReference),
            };
            let object_id = reader.array32()?;
            let reference = ObjectRef {
                content: backend_version::ContentId::<ObjectDomain>::try_from(content.as_slice())
                    .map_err(|_| CompilerResultError::MemberReference)?,
                length: length.into(),
                schema,
                kind: kind.into(),
            };
            let member = CompilerResultMemberV1 {
                role,
                reference,
                key,
                parent,
                object_id,
            };
            match role {
                CompilerResultMemberRole::AuxiliaryPlane => buffers.auxiliary.push(member),
                CompilerResultMemberRole::VersionedPlaneManifest
                | CompilerResultMemberRole::VersionedPlaneSegment => {
                    buffers.versioned_planes.push(member);
                }
                _ => buffers.members.push(member),
            }
        }
        if !reader.is_empty() {
            return Err(CompilerResultError::EnvelopeTrailingBytes);
        }
        validate_member_topology(&buffers.members)?;
        validate_auxiliary_topology(&buffers.auxiliary)?;
        validate_versioned_plane_topology(&buffers.members, &buffers.versioned_planes)?;
        let mut all_object_ids = BTreeSet::new();
        if buffers
            .members
            .iter()
            .chain(buffers.auxiliary.iter())
            .chain(buffers.versioned_planes.iter())
            .any(|member| member.object_id == [0; 32] || !all_object_ids.insert(member.object_id))
        {
            return Err(CompilerResultError::MemberTopology);
        }
        let binding_view = CompilationBindingView::validate(&binding_bytes)
            .map_err(|_| CompilerResultError::BindingInvalid)?;
        if binding_view.identity.as_ref() != &binding_identity
            || binding_view.generation.pinned_root.as_ref() != &pinned_root
            || binding_view.generation.dep_set.as_ref() != &dep_set
        {
            return Err(CompilerResultError::BindingMismatch);
        }
        if input_closure_id == [0; 32] || input_manifest_object_id == [0; 32] {
            return Err(CompilerResultError::EnvelopeField);
        }
        let envelope = Self {
            scope,
            input_closure_id,
            input_manifest_object_id,
            generation: binding_view.generation,
            manifest_identity,
            binding_facts: *binding_view,
            binding_bytes,
            members: buffers.members.into_boxed_slice(),
            auxiliary_planes: buffers.auxiliary.into_boxed_slice(),
            versioned_plane_members: buffers.versioned_planes.into_boxed_slice(),
        };
        Ok(envelope)
    }

    /// Returns the exact fenced assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Returns the exact offered input closure.
    #[must_use]
    pub const fn input_closure_id(&self) -> [u8; 32] {
        self.input_closure_id
    }

    /// Returns the exact input-manifest object identity named by the Offer.
    #[must_use]
    pub const fn input_manifest_object_id(&self) -> [u8; 32] {
        self.input_manifest_object_id
    }

    /// Returns the complete generation facts claimed by this envelope.
    #[must_use]
    pub const fn generation_facts(&self) -> VerifiedGenerationFacts {
        self.generation
    }

    /// Returns the manifest identity carried by the envelope.
    #[must_use]
    pub const fn manifest_identity(&self) -> CompilationManifestIdentity {
        self.manifest_identity
    }

    /// Returns the binding facts reopened from canonical bytes.
    #[must_use]
    pub const fn binding_facts(&self) -> CompilationBindingFacts {
        self.binding_facts
    }

    /// Returns the canonical generation-to-manifest binding bytes.
    #[must_use]
    pub fn binding_bytes(&self) -> &[u8] {
        &self.binding_bytes
    }

    /// Returns the exact canonical generation-member inventory.
    #[must_use]
    pub fn members(&self) -> &[CompilerResultMemberV1] {
        &self.members
    }

    /// Returns typed independent output planes, which never count as IR generation entries.
    #[must_use]
    pub fn auxiliary_planes(&self) -> &[CompilerResultMemberV1] {
        &self.auxiliary_planes
    }

    /// Exact c005/c004 per-artifact versioned-plane member inventory.
    #[must_use]
    pub fn versioned_plane_members(&self) -> &[CompilerResultMemberV1] {
        &self.versioned_plane_members
    }
}

/// Envelope reopened from a checked result-closure index without loading output payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerResultClosureIndex {
    closure_id: ClosureId,
    envelope_object_id: ObjectId,
    object_count: u32,
    payload_bytes: u64,
    envelope: CompilerResultEnvelopeV1,
}

impl CompilerResultClosureIndex {
    /// Exact admitted logical result closure.
    #[must_use]
    pub const fn closure_id(&self) -> ClosureId {
        self.closure_id
    }

    /// Physical CAS identity of the small canonical result envelope.
    #[must_use]
    pub const fn envelope_object_id(&self) -> ObjectId {
        self.envelope_object_id
    }

    /// Exact number of objects in the closure.
    #[must_use]
    pub const fn object_count(&self) -> u32 {
        self.object_count
    }

    /// Exact total canonical payload bytes in the closure.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Decoded result envelope, checked against the exact closure member IDs.
    #[must_use]
    pub const fn envelope(&self) -> &CompilerResultEnvelopeV1 {
        &self.envelope
    }
}

/// Reopens one durable result using the metadata-only closure index.
///
/// Every member object's complete physical and typed identity is verified with a bounded store
/// verifier. Only the size-bounded result envelope payload is materialized; compiler output
/// payloads remain in CAS. The envelope inventory must exactly equal the closure IDs.
#[allow(clippy::too_many_arguments)]
pub fn reopen_compiler_result_envelope(
    store: &FileStore,
    closure_id: ClosureId,
    expected_object_count: u32,
    expected_payload_bytes: u64,
    expected_scope: AssignmentScope,
    expected_input_closure_id: [u8; 32],
    expected_input_manifest_object_id: [u8; 32],
    expected_target_root: [u8; 32],
    maximum_payload_bytes: u64,
) -> Result<CompilerResultClosureIndex, CompilerResultError> {
    if expected_object_count == 0
        || expected_object_count as usize > MAX_RESULT_MEMBERS
        || expected_payload_bytes == 0
        || expected_payload_bytes > maximum_payload_bytes
    {
        return Err(CompilerResultError::ClosureAdmission);
    }
    let index = store.read_closure_index(closure_id)?;
    if index
        .entry_count()
        .is_some_and(|count| count != expected_object_count as usize)
    {
        return Err(CompilerResultError::ClosureAdmission);
    }

    let mut ids = BTreeSet::new();
    let mut total_payload_bytes = 0_u64;
    let mut envelope_id = None;
    let mut after = None;
    loop {
        let page = index.page_ids(after, 256)?;
        if page.object_ids().is_empty() {
            break;
        }
        for id in page.object_ids() {
            if !ids.insert(*id.as_bytes()) || ids.len() > expected_object_count as usize {
                return Err(CompilerResultError::ClosureAdmission);
            }
            let verified =
                store.verify_object_claim(UntrustedObjectId::from_bytes(*id.as_bytes()))?;
            if verified.id() != *id {
                return Err(CompilerResultError::ClosureAdmission);
            }
            total_payload_bytes = total_payload_bytes
                .checked_add(verified.payload_len())
                .filter(|total| *total <= maximum_payload_bytes)
                .ok_or(CompilerResultError::EnvelopeLimit)?;
            let schema = verified.schema();
            if schema.domain() == RESULT_OUTPUT_SCHEMA_DOMAIN
                && schema.ty() == RESULT_ENVELOPE_SCHEMA_TYPE
                && schema.version() == RESULT_ENVELOPE_SCHEMA_VERSION
            {
                if verified.payload_len() > MAX_RESULT_ENVELOPE_BYTES as u64
                    || envelope_id.replace(*id).is_some()
                {
                    return Err(CompilerResultError::ClosureAdmission);
                }
            }
        }
        after = page.next();
        if after.is_none() {
            break;
        }
    }
    if ids.len() != expected_object_count as usize || total_payload_bytes != expected_payload_bytes
    {
        return Err(CompilerResultError::ClosureAdmission);
    }
    let envelope_object_id = envelope_id.ok_or(CompilerResultError::ClosureAdmission)?;
    let envelope_object = store.read_object(envelope_object_id)?;
    let envelope = CompilerResultEnvelopeV1::from_typed_object(&envelope_object)?;
    if envelope.scope() != expected_scope
        || envelope.input_closure_id() != expected_input_closure_id
        || envelope.input_manifest_object_id() != expected_input_manifest_object_id
        || envelope.generation_facts().pinned_root.as_ref() != &expected_target_root
    {
        return Err(CompilerResultError::AssignmentMismatch);
    }

    let mut expected_ids = BTreeSet::new();
    expected_ids.insert(*envelope_object_id.as_bytes());
    for member in envelope
        .members()
        .iter()
        .chain(envelope.auxiliary_planes())
        .chain(envelope.versioned_plane_members())
    {
        if !expected_ids.insert(member.object_id()) {
            return Err(CompilerResultError::ClosureAdmission);
        }
    }
    if expected_ids != ids {
        return Err(CompilerResultError::ClosureAdmission);
    }
    Ok(CompilerResultClosureIndex {
        closure_id,
        envelope_object_id,
        object_count: expected_object_count,
        payload_bytes: total_payload_bytes,
        envelope,
    })
}

/// Builds the one shared typed object wrapper for a verified semantic generation member.
pub fn compiler_result_output_object(
    claim: crate::publication::StagedSemanticObjectClaim,
    bytes: &[u8],
) -> Result<TypedObject, CompilerResultError> {
    result_output_object(
        claim.object(),
        *claim.key(),
        claim.parent().map(|parent| *parent),
        bytes,
    )
}

/// Describes one staged generation object for bounded streaming into local CAS.
///
/// The payload is validated by borrow and is not copied into a temporary typed object. The
/// returned claim carries the same schema/key/version identity used by the durable sink.
pub fn compiler_result_output_claim(
    claim: crate::publication::StagedSemanticObjectClaim,
    bytes: &[u8],
) -> Result<CompilerResultOutputClaim, CompilerResultError> {
    let role = generation_member_role_for_claim(claim.object(), *claim.key())?;
    result_output_claim(
        claim.object(),
        *claim.key(),
        claim.parent().map(|parent| *parent),
        bytes,
        role,
    )
}

/// Describes one independent typed output plane for bounded streaming into local CAS.
pub fn compiler_result_auxiliary_output_claim(
    reference: ObjectRef<ObjectDomain>,
    plane_ordinal: u64,
    bytes: &[u8],
) -> Result<CompilerResultOutputClaim, CompilerResultError> {
    result_output_claim(
        reference,
        plane_ordinal,
        None,
        bytes,
        CompilerResultMemberRole::AuxiliaryPlane,
    )
}

/// Converts a small already typed result-envelope object to its durable streaming claim.
#[must_use]
pub fn compiler_result_typed_object_claim(
    object: &TypedObject,
) -> Result<ArtifactObjectClaim, CompilerResultError> {
    Ok(ArtifactObjectClaim::new(
        object.schema(),
        *object.key(),
        *object.version(),
        u64::try_from(object.bytes().len())?,
    ))
}

/// Builds one typed wrapper for an independent output plane at a stable plane ordinal.
pub fn compiler_result_auxiliary_output_object(
    reference: ObjectRef<ObjectDomain>,
    plane_ordinal: u64,
    bytes: &[u8],
) -> Result<TypedObject, CompilerResultError> {
    result_output_object(reference, plane_ordinal, None, bytes)
}

/// Describes the c005 manifest and c004 segments for one exact staged artifact.
///
/// The returned claims are ordered as `[manifest, segment 0, ...]`. The manifest member's
/// entry key is the artifact ordinal and its parent is the matching semantic-image entry key.
/// Every segment's entry key is its ordinal within the artifact and its parent is the manifest
/// entry key. The function decodes the canonical manifest and verifies each borrowed segment's
/// kind, length, and content-derived semantic segment ID before returning any claim.
pub fn compiler_result_versioned_plane_output_claims(
    staged: &crate::application::StagedSemanticPackage,
    artifact: &StagedVersionedPlaneArtifact<'_>,
) -> Result<Box<[CompilerResultOutputClaim]>, CompilerResultError> {
    let artifact_ordinal = artifact.artifact_ordinal();
    let image = staged
        .semantic_output_object(artifact_ordinal)
        .ok_or(CompilerResultError::MemberTopology)?;
    let image_claim = compiler_result_output_claim(image.claim(), image.bytes())?;
    if image_claim.role != CompilerResultMemberRole::SemanticImage {
        return Err(CompilerResultError::MemberTopology);
    }
    let artifact_key = u64::try_from(artifact_ordinal)?;
    let image_key = image_claim.key;
    let manifest_bytes = artifact.manifest_bytes();
    let manifest = SemanticPlaneManifest::decode(manifest_bytes)
        .map_err(|_| CompilerResultError::VersionedPlaneManifest)?;
    if manifest
        .encode()
        .map_err(|_| CompilerResultError::VersionedPlaneManifest)?
        != manifest_bytes
    {
        return Err(CompilerResultError::VersionedPlaneManifest);
    }

    let manifest_reference = versioned_plane_reference(0xc005, manifest_bytes)?;
    let manifest_claim = result_output_claim(
        manifest_reference,
        artifact_key,
        Some(image_key),
        manifest_bytes,
        CompilerResultMemberRole::VersionedPlaneManifest,
    )?;

    let expected_segments = manifest
        .planes()
        .iter()
        .try_fold(0_usize, |count, plane| {
            count.checked_add(plane.segments().len())
        })
        .ok_or(CompilerResultError::MemberCount)?;
    if expected_segments != artifact.segment_count()
        || expected_segments == 0
        || expected_segments
            .checked_add(1)
            .is_none_or(|count| count > MAX_RESULT_MEMBERS)
    {
        return Err(CompilerResultError::VersionedPlaneTopology);
    }
    let mut claims = Vec::new();
    claims
        .try_reserve_exact(expected_segments + 1)
        .map_err(|_| CompilerResultError::Allocation)?;
    claims.push(manifest_claim);
    let mut segment_ordinal = 0_usize;
    for plane in manifest.planes() {
        for descriptor in plane.segments() {
            let segment = artifact
                .segment(segment_ordinal)
                .ok_or(CompilerResultError::VersionedPlaneTopology)?;
            if segment.kind() != plane.kind()
                || u64::try_from(segment.payload().len())? != descriptor.byte_length()
                || descriptor.admit(plane.kind(), segment.payload()).ok() != Some(segment.id())
                || descriptor.id_claim().as_bytes() != segment.id().as_bytes()
                || segment.payload().len() > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES
            {
                return Err(CompilerResultError::VersionedPlaneTopology);
            }
            let reference = versioned_plane_reference(0xc004, segment.payload())?;
            claims.push(result_output_claim(
                reference,
                u64::try_from(segment_ordinal)?,
                Some(artifact_key),
                segment.payload(),
                CompilerResultMemberRole::VersionedPlaneSegment,
            )?);
            segment_ordinal += 1;
        }
    }
    Ok(claims.into_boxed_slice())
}

fn validate_staged_versioned_plane_members(
    staged: &crate::application::StagedSemanticPackage,
    members: &[CompilerResultMemberV1],
) -> Result<(), CompilerResultError> {
    let staged_planes = staged
        .versioned_planes()
        .map_err(|_| CompilerResultError::VersionedPlaneManifest)?;
    let mut cursor = 0_usize;
    for artifact in staged_planes.artifacts() {
        for claim in compiler_result_versioned_plane_output_claims(staged, artifact)?.iter() {
            let member = members
                .get(cursor)
                .ok_or(CompilerResultError::VersionedPlaneTopology)?;
            if member.role != claim.role
                || member.reference != claim.reference
                || member.key != claim.key
                || member.parent != claim.parent
                || member.object_id == [0; 32]
            {
                return Err(CompilerResultError::VersionedPlaneTopology);
            }
            cursor += 1;
        }
    }
    if cursor != members.len() {
        return Err(CompilerResultError::VersionedPlaneTopology);
    }
    Ok(())
}

fn versioned_plane_reference(
    kind: u16,
    bytes: &[u8],
) -> Result<ObjectRef<ObjectDomain>, CompilerResultError> {
    Ok(ObjectRef {
        content: backend_version::ContentId::<ObjectDomain>::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::try_from(bytes.len())?),
        schema: SchemaId::Object,
        kind: ObjectKind::from(kind),
    })
}

pub(super) fn result_output_object(
    reference: ObjectRef<ObjectDomain>,
    key: u64,
    parent: Option<u64>,
    bytes: &[u8],
) -> Result<TypedObject, CompilerResultError> {
    let claim = result_output_claim(
        reference,
        key,
        parent,
        bytes,
        CompilerResultMemberRole::AuxiliaryPlane,
    )?;
    let typed_key = ObjectKey::<CompilerResultOutputSchema>::from_value(
        output_key_preimage(reference, key, parent)?.as_slice(),
    );
    let _ = claim;
    Ok(TypedObject::from_value(&typed_key, bytes))
}

fn result_output_claim(
    reference: ObjectRef<ObjectDomain>,
    key: u64,
    parent: Option<u64>,
    bytes: &[u8],
    role: CompilerResultMemberRole,
) -> Result<CompilerResultOutputClaim, CompilerResultError> {
    if u64::try_from(bytes.len()).ok() != Some(*reference.length)
        || backend_version::ContentId::<ObjectDomain>::from_canonical_bytes(bytes)
            != reference.content
    {
        return Err(CompilerResultError::OutputReference);
    }
    let key_bytes = output_key_preimage(reference, key, parent)?;
    let typed_key = ObjectKey::<CompilerResultOutputSchema>::from_value(key_bytes.as_slice());
    let schema = SchemaIdentity::new(
        RESULT_OUTPUT_SCHEMA_DOMAIN,
        RESULT_OUTPUT_SCHEMA_TYPE,
        RESULT_OUTPUT_SCHEMA_VERSION,
    );
    let artifact_claim = ArtifactObjectClaim::new(
        schema,
        typed_key.to_bytes(),
        ObjectVersion::<CompilerResultOutputSchema>::from_value(bytes).to_bytes(),
        u64::try_from(bytes.len())?,
    );
    Ok(CompilerResultOutputClaim {
        role,
        reference,
        key,
        parent,
        artifact_claim,
    })
}

fn output_key_preimage(
    reference: ObjectRef<ObjectDomain>,
    key: u64,
    parent: Option<u64>,
) -> Result<Vec<u8>, CompilerResultError> {
    let mut key_bytes = Vec::new();
    key_bytes
        .try_reserve_exact(RESULT_OUTPUT_KEY_DOMAIN.len() + 4 + 2 + 32 + 8 + 8 + 9)
        .map_err(|_| CompilerResultError::Allocation)?;
    key_bytes.extend_from_slice(RESULT_OUTPUT_KEY_DOMAIN);
    key_bytes.extend_from_slice(&u32::from(reference.schema).to_be_bytes());
    key_bytes.extend_from_slice(&(*reference.kind).to_be_bytes());
    key_bytes.extend_from_slice(reference.content.as_ref());
    key_bytes.extend_from_slice(&(*reference.length).to_be_bytes());
    key_bytes.extend_from_slice(&key.to_be_bytes());
    match parent {
        Some(parent) => {
            key_bytes.push(1);
            key_bytes.extend_from_slice(&parent.to_be_bytes());
        }
        None => {
            key_bytes.push(0);
            key_bytes.extend_from_slice(&0_u64.to_be_bytes());
        }
    }
    Ok(key_bytes)
}

pub(super) fn compiler_result_member_key(
    member: CompilerResultMemberV1,
) -> Result<[u8; 32], CompilerResultError> {
    let preimage = output_key_preimage(member.reference, member.key, member.parent)?;
    Ok(ObjectKey::<CompilerResultOutputSchema>::from_value(preimage.as_slice()).to_bytes())
}

/// Builds the bounded result-envelope object after streamed semantic members have been admitted.
///
/// `members` must be in staged semantic-generation order. Their physical IDs come from
/// `ObjectStream::finish`; this function verifies every typed reference, key, and parent against
/// staged borrowed bytes and emits an envelope with no physical-pack requirement.
pub fn compiler_result_envelope_object_from_members(
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    staged: &crate::application::StagedSemanticPackage,
    members: &[CompilerResultMemberV1],
) -> Result<TypedObject, CompilerResultError> {
    compiler_result_envelope_object_from_members_with_planes(
        scope,
        input_closure_id,
        input_manifest_object_id,
        staged,
        members,
        &[],
    )
}

/// Streaming counterpart of `compiler_result_envelope_object_with_planes`.
pub fn compiler_result_envelope_object_from_members_with_planes(
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    staged: &crate::application::StagedSemanticPackage,
    members: &[CompilerResultMemberV1],
    auxiliary_planes: &[CompilerResultMemberV1],
) -> Result<TypedObject, CompilerResultError> {
    compiler_result_envelope_object_from_members_with_output_planes(
        scope,
        input_closure_id,
        input_manifest_object_id,
        staged,
        members,
        auxiliary_planes,
        &[],
    )
}

/// Builds the canonical result envelope after semantic generation and versioned-plane objects
/// have been streamed into the same exact logical result closure.
pub fn compiler_result_envelope_object_from_members_with_versioned_planes(
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    staged: &crate::application::StagedSemanticPackage,
    members: &[CompilerResultMemberV1],
    versioned_plane_members: &[CompilerResultMemberV1],
) -> Result<TypedObject, CompilerResultError> {
    compiler_result_envelope_object_from_members_with_output_planes(
        scope,
        input_closure_id,
        input_manifest_object_id,
        staged,
        members,
        &[],
        versioned_plane_members,
    )
}

fn compiler_result_envelope_object_from_members_with_output_planes(
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    staged: &crate::application::StagedSemanticPackage,
    members: &[CompilerResultMemberV1],
    auxiliary_planes: &[CompilerResultMemberV1],
    versioned_plane_members: &[CompilerResultMemberV1],
) -> Result<TypedObject, CompilerResultError> {
    if input_closure_id == [0; 32]
        || input_manifest_object_id == [0; 32]
        || members.len() != staged.output_object_count()
        || members.is_empty()
        || members
            .len()
            .checked_add(auxiliary_planes.len())
            .and_then(|count| count.checked_add(versioned_plane_members.len()))
            .is_none_or(|count| count > MAX_RESULT_MEMBERS)
    {
        return Err(CompilerResultError::MemberCount);
    }
    for (ordinal, member) in members.iter().enumerate() {
        let output = staged
            .output_object(ordinal)
            .ok_or(CompilerResultError::MemberCount)?;
        let claim = compiler_result_output_claim(output.claim(), output.bytes())?;
        if member.role != claim.role
            || member.reference != claim.reference
            || member.key != claim.key
            || member.parent != claim.parent
            || member.object_id == [0; 32]
        {
            return Err(CompilerResultError::OutputObjectMismatch);
        }
    }
    validate_member_topology(members)?;
    validate_auxiliary_topology(auxiliary_planes)?;
    validate_versioned_plane_topology(members, versioned_plane_members)?;
    validate_staged_versioned_plane_members(staged, versioned_plane_members)?;
    let mut member_ids = BTreeSet::new();
    if members
        .iter()
        .chain(auxiliary_planes)
        .chain(versioned_plane_members)
        .any(|member| member.object_id == [0; 32] || !member_ids.insert(member.object_id))
    {
        return Err(CompilerResultError::MemberTopology);
    }
    let binding = CompilationBindingView::validate(staged.binding_bytes())
        .map_err(|_| CompilerResultError::BindingInvalid)?;
    if binding.generation != staged.generation_facts()
        || binding.identity != staged.binding_facts().identity
        || binding.manifest != staged.manifest_facts().identity
    {
        return Err(CompilerResultError::BindingMismatch);
    }
    let envelope = CompilerResultEnvelopeV1 {
        scope,
        input_closure_id,
        input_manifest_object_id,
        generation: staged.generation_facts(),
        manifest_identity: staged.manifest_facts().identity,
        binding_facts: *binding,
        binding_bytes: staged.binding_bytes().into(),
        members: members.to_vec().into_boxed_slice(),
        auxiliary_planes: auxiliary_planes.to_vec().into_boxed_slice(),
        versioned_plane_members: versioned_plane_members.to_vec().into_boxed_slice(),
    };
    let bytes = encode_result_envelope(&envelope)?;
    let key = ObjectKey::<CompilerResultEnvelopeSchema>::from_value(bytes.as_slice());
    Ok(TypedObject::from_value(&key, bytes.as_slice()))
}

/// A bounded protocol, storage, or semantic result rejection.
#[derive(Debug, Error)]
pub enum CompilerResultError {
    /// The typed envelope schema is not the shared ABI.
    #[error("compiler result envelope schema is unsupported")]
    EnvelopeSchema,
    /// The envelope header or magic is invalid.
    #[error("compiler result envelope header is invalid")]
    EnvelopeHeader,
    /// A scope field is invalid.
    #[error("compiler result envelope assignment scope is invalid")]
    EnvelopeScope,
    /// A required identity is zero.
    #[error("compiler result envelope contains an empty identity")]
    EnvelopeField,
    /// An envelope field or member count exceeds a fixed bound.
    #[error("compiler result envelope exceeds its fixed bound")]
    EnvelopeLimit,
    /// The member count is inconsistent or outside the bounded semantic schema.
    #[error("compiler result member count is invalid")]
    MemberCount,
    /// The result member role is unknown.
    #[error("compiler result member role is invalid")]
    MemberRole,
    /// A member's schema, identity, length, key, or parent is malformed.
    #[error("compiler result member reference is invalid")]
    MemberReference,
    /// The manifest identity is not a typed compilation manifest identity.
    #[error("compiler result manifest identity is invalid")]
    ManifestIdentity,
    /// Binding bytes do not have the fixed canonical width.
    #[error("compiler result binding length is invalid")]
    BindingLength,
    /// Binding grammar failed.
    #[error("compiler result binding is invalid")]
    BindingInvalid,
    /// Binding facts disagree with the envelope generation or manifest facts.
    #[error("compiler result binding facts do not match the envelope")]
    BindingMismatch,
    /// The envelope has trailing bytes.
    #[error("compiler result envelope has trailing bytes")]
    EnvelopeTrailingBytes,
    /// A generation output reference does not match its canonical bytes.
    #[error("compiler result output bytes do not match their semantic reference")]
    OutputReference,
    /// A generated output object differs from its exact staged claim.
    #[error("compiler result output object differs from its staged claim")]
    OutputObjectMismatch,
    /// Generation member ordering or topology differs from the semantic root ABI.
    #[error("compiler result member topology is invalid")]
    MemberTopology,
    /// Allocation failed while building a bounded result value.
    #[error("compiler result allocation failed")]
    Allocation,
    /// CAS closure failed typed admission or exact inventory comparison.
    #[error("compiler result closure admission failed")]
    ClosureAdmission,
    /// Result scope, input closure, manifest, or receipt metadata differs from the assignment.
    #[error("compiler result scope or receipt does not match the assignment")]
    AssignmentMismatch,
    /// The output manifest is not a complete canonical semantic manifest.
    #[error("compiler result semantic manifest is invalid")]
    ManifestInvalid,
    /// A c005 per-artifact plane manifest failed canonical admission.
    #[error("compiler result versioned-plane manifest is invalid")]
    VersionedPlaneManifest,
    /// A c005 manifest and its exact c004 segment inventory disagree.
    #[error("compiler result versioned-plane topology is invalid")]
    VersionedPlaneTopology,
    /// The per-image plane manifest does not bind the exact invocation and captured input.
    #[error("compiler result versioned-plane input binding does not match the assignment")]
    VersionedPlaneInputMismatch,
    /// A generic auxiliary member cannot bypass the typed per-image plane ABI.
    #[error("compiler result contains an unsupported generic auxiliary output")]
    UnsupportedAuxiliaryOutput,
    /// A compact fragment does not satisfy manifest facts.
    #[error("compiler result fragment does not match manifest facts")]
    FragmentMismatch,
    /// A semantic image does not satisfy manifest provenance or identity.
    #[error("compiler result semantic image does not match manifest facts")]
    SemanticImageMismatch,
    /// Recomputed complete semantic generation differs from the worker claim.
    #[error("compiler result generation facts do not recompute")]
    GenerationMismatch,
    /// An integer conversion exceeded a fixed-width wire field.
    #[error("compiler result integer width overflow")]
    IntegerWidth(#[from] std::num::TryFromIntError),
    /// Durable CAS rejected a member object.
    #[error("compiler result object could not be read from local CAS: {0:?}")]
    Store(backend_store::StoreError),
    /// The canonical compiler input manifest failed structural validation.
    #[error(transparent)]
    InputManifest(#[from] CompilerInputManifestError),
    /// The input manifest does not bind to the exact admitted compile work.
    #[error("compiler input manifest does not match the verified work identity")]
    InputManifestMismatch,
    /// The source authority rejected the exact positive and negative read set.
    #[error("compiler input read set was rejected by its source authority")]
    ReadSetRejected,
    /// The source snapshot, owner fence, or trusted-peer evidence was not admitted.
    #[error("compiler input or execution admission evidence differs from the assignment")]
    InputAdmission,
    /// The received input closure does not exactly contain its manifest and file snapshot.
    #[error("compiler input closure differs from its complete immutable snapshot")]
    InputClosureMismatch,
}

impl From<backend_store::StoreError> for CompilerResultError {
    fn from(error: backend_store::StoreError) -> Self {
        Self::Store(error)
    }
}

struct ResultReader<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> ResultReader<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], CompilerResultError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CompilerResultError::EnvelopeLimit)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CompilerResultError::EnvelopeHeader)?;
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, CompilerResultError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(CompilerResultError::EnvelopeHeader)
    }

    fn array2(&mut self) -> Result<[u8; 2], CompilerResultError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(bytes)
    }

    fn array16(&mut self) -> Result<[u8; 16], CompilerResultError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(bytes)
    }

    fn array32(&mut self) -> Result<[u8; 32], CompilerResultError> {
        let mut bytes = [0; 32];
        bytes.copy_from_slice(self.take(32)?);
        Ok(bytes)
    }

    fn u32_be(&mut self) -> Result<u32, CompilerResultError> {
        Ok(u32::from_be_bytes(self.array4()?))
    }

    fn array4(&mut self) -> Result<[u8; 4], CompilerResultError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(bytes)
    }

    fn u64_be(&mut self) -> Result<u64, CompilerResultError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(bytes))
    }

    const fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ResultMemberRoleCounts {
    generation: usize,
    auxiliary: usize,
    versioned_planes: usize,
}

struct ResultMemberBuffers {
    members: Vec<CompilerResultMemberV1>,
    auxiliary: Vec<CompilerResultMemberV1>,
    versioned_planes: Vec<CompilerResultMemberV1>,
}

impl ResultMemberBuffers {
    fn reserve(counts: ResultMemberRoleCounts) -> Result<Self, CompilerResultError> {
        let mut buffers = Self {
            members: Vec::new(),
            auxiliary: Vec::new(),
            versioned_planes: Vec::new(),
        };
        buffers
            .members
            .try_reserve_exact(counts.generation)
            .map_err(|_| CompilerResultError::Allocation)?;
        buffers
            .auxiliary
            .try_reserve_exact(counts.auxiliary)
            .map_err(|_| CompilerResultError::Allocation)?;
        buffers
            .versioned_planes
            .try_reserve_exact(counts.versioned_planes)
            .map_err(|_| CompilerResultError::Allocation)?;
        Ok(buffers)
    }
}

fn decode_result_member_role(value: u8) -> Result<CompilerResultMemberRole, CompilerResultError> {
    match value {
        1 => Ok(CompilerResultMemberRole::Manifest),
        2 => Ok(CompilerResultMemberRole::Fragment),
        3 => Ok(CompilerResultMemberRole::SemanticImage),
        4 => Ok(CompilerResultMemberRole::AuxiliaryPlane),
        5 => Ok(CompilerResultMemberRole::VersionedPlaneManifest),
        6 => Ok(CompilerResultMemberRole::VersionedPlaneSegment),
        _ => Err(CompilerResultError::MemberRole),
    }
}

fn result_member_section(role: CompilerResultMemberRole) -> u8 {
    match role {
        CompilerResultMemberRole::Manifest
        | CompilerResultMemberRole::Fragment
        | CompilerResultMemberRole::SemanticImage => 0,
        CompilerResultMemberRole::AuxiliaryPlane => 1,
        CompilerResultMemberRole::VersionedPlaneManifest
        | CompilerResultMemberRole::VersionedPlaneSegment => 2,
    }
}

/// Counts member roles and validates canonical section order without allocating.
/// The subsequent decode reserves each member vector to its exact role count, so their total
/// requested capacity is bounded by `member_count`, instead of reserving that count three times.
fn count_result_member_roles(
    frames: &[u8],
    member_count: usize,
) -> Result<ResultMemberRoleCounts, CompilerResultError> {
    if member_count == 0 || member_count > MAX_RESULT_MEMBERS {
        return Err(CompilerResultError::MemberCount);
    }
    let mut reader = ResultReader::new(frames);
    let mut counts = ResultMemberRoleCounts::default();
    let mut previous_section = 0_u8;
    for ordinal in 0..member_count {
        let role = decode_result_member_role(reader.byte()?)?;
        let section = result_member_section(role);
        if ordinal != 0 && section < previous_section {
            return Err(CompilerResultError::MemberTopology);
        }
        previous_section = section;
        let count = match section {
            0 => &mut counts.generation,
            1 => &mut counts.auxiliary,
            2 => &mut counts.versioned_planes,
            _ => return Err(CompilerResultError::MemberRole),
        };
        *count = count
            .checked_add(1)
            .ok_or(CompilerResultError::MemberCount)?;
        reader.take(RESULT_MEMBER_FIXED_BYTES - 1)?;
    }
    if !reader.is_empty() {
        return Err(CompilerResultError::EnvelopeTrailingBytes);
    }
    Ok(counts)
}

fn encode_result_envelope(
    envelope: &CompilerResultEnvelopeV1,
) -> Result<Vec<u8>, CompilerResultError> {
    let member_count = envelope
        .members
        .len()
        .checked_add(envelope.auxiliary_planes.len())
        .and_then(|count| count.checked_add(envelope.versioned_plane_members.len()))
        .ok_or(CompilerResultError::EnvelopeLimit)?;
    if member_count == 0 || member_count > MAX_RESULT_MEMBERS {
        return Err(CompilerResultError::MemberCount);
    }
    let capacity = 8_usize
        .checked_add(16 + 16 + 8 + 32 + 32 + 32 + 32 + 32 + 32 + 32 + 4)
        .and_then(|size| size.checked_add(COMPILATION_BINDING_BYTES))
        .and_then(|size| size.checked_add(4))
        .and_then(|size| size.checked_add(member_count.checked_mul(RESULT_MEMBER_FIXED_BYTES)?))
        .ok_or(CompilerResultError::EnvelopeLimit)?;
    if capacity > MAX_RESULT_ENVELOPE_BYTES {
        return Err(CompilerResultError::EnvelopeLimit);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| CompilerResultError::Allocation)?;
    bytes.extend_from_slice(RESULT_MAGIC);
    bytes.extend_from_slice(&envelope.scope.namespace_id);
    bytes.extend_from_slice(&envelope.scope.work_id);
    bytes.extend_from_slice(&envelope.scope.attempt.to_be_bytes());
    bytes.extend_from_slice(&envelope.scope.fence);
    bytes.extend_from_slice(&envelope.input_closure_id);
    bytes.extend_from_slice(&envelope.input_manifest_object_id);
    bytes.extend_from_slice(envelope.generation.pinned_root.as_ref());
    bytes.extend_from_slice(envelope.generation.dep_set.as_ref());
    bytes.extend_from_slice(envelope.manifest_identity.as_ref());
    bytes.extend_from_slice(envelope.binding_facts.identity.as_ref());
    bytes.extend_from_slice(&u32::try_from(envelope.binding_bytes.len())?.to_be_bytes());
    bytes.extend_from_slice(&envelope.binding_bytes);
    bytes.extend_from_slice(&u32::try_from(member_count)?.to_be_bytes());
    for member in envelope
        .members
        .iter()
        .chain(envelope.auxiliary_planes.iter())
        .chain(envelope.versioned_plane_members.iter())
    {
        bytes.push(match member.role {
            CompilerResultMemberRole::Manifest => 1,
            CompilerResultMemberRole::Fragment => 2,
            CompilerResultMemberRole::SemanticImage => 3,
            CompilerResultMemberRole::AuxiliaryPlane => 4,
            CompilerResultMemberRole::VersionedPlaneManifest => 5,
            CompilerResultMemberRole::VersionedPlaneSegment => 6,
        });
        bytes.extend_from_slice(&u32::from(member.reference.schema).to_be_bytes());
        bytes.extend_from_slice(&(*member.reference.kind).to_be_bytes());
        bytes.extend_from_slice(member.reference.content.as_ref());
        bytes.extend_from_slice(&(*member.reference.length).to_be_bytes());
        bytes.extend_from_slice(&member.key.to_be_bytes());
        match member.parent {
            Some(parent) => {
                bytes.push(1);
                bytes.extend_from_slice(&parent.to_be_bytes());
            }
            None => {
                bytes.push(0);
                bytes.extend_from_slice(&0_u64.to_be_bytes());
            }
        }
        bytes.extend_from_slice(&member.object_id);
    }
    Ok(bytes)
}

#[cfg(test)]
pub(super) fn result_envelope_object_from_parts(
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    generation: VerifiedGenerationFacts,
    manifest_identity: CompilationManifestIdentity,
    binding_bytes: &[u8],
    members: &[CompilerResultMemberV1],
    versioned_plane_members: &[CompilerResultMemberV1],
) -> Result<TypedObject, CompilerResultError> {
    result_envelope_object_from_parts_with_auxiliary(
        scope,
        input_closure_id,
        input_manifest_object_id,
        generation,
        manifest_identity,
        binding_bytes,
        members,
        &[],
        versioned_plane_members,
    )
}

#[cfg(test)]
pub(super) fn result_envelope_object_from_parts_with_auxiliary(
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    generation: VerifiedGenerationFacts,
    manifest_identity: CompilationManifestIdentity,
    binding_bytes: &[u8],
    members: &[CompilerResultMemberV1],
    auxiliary_planes: &[CompilerResultMemberV1],
    versioned_plane_members: &[CompilerResultMemberV1],
) -> Result<TypedObject, CompilerResultError> {
    if input_closure_id == [0; 32]
        || input_manifest_object_id == [0; 32]
        || members.is_empty()
        || members
            .len()
            .checked_add(auxiliary_planes.len())
            .and_then(|count| count.checked_add(versioned_plane_members.len()))
            .is_none_or(|count| count > MAX_RESULT_MEMBERS)
    {
        return Err(CompilerResultError::MemberCount);
    }
    validate_member_topology(members)?;
    validate_auxiliary_topology(auxiliary_planes)?;
    validate_versioned_plane_topology(members, versioned_plane_members)?;
    let mut ids = BTreeSet::new();
    if members
        .iter()
        .chain(auxiliary_planes)
        .chain(versioned_plane_members)
        .any(|member| member.object_id == [0; 32] || !ids.insert(member.object_id))
    {
        return Err(CompilerResultError::MemberTopology);
    }
    let binding = CompilationBindingView::validate(binding_bytes)
        .map_err(|_| CompilerResultError::BindingInvalid)?;
    if binding.generation != generation || binding.manifest != manifest_identity {
        return Err(CompilerResultError::BindingMismatch);
    }
    let envelope = CompilerResultEnvelopeV1 {
        scope,
        input_closure_id,
        input_manifest_object_id,
        generation,
        manifest_identity,
        binding_facts: *binding,
        binding_bytes: binding_bytes.into(),
        members: members.to_vec().into_boxed_slice(),
        auxiliary_planes: auxiliary_planes.to_vec().into_boxed_slice(),
        versioned_plane_members: versioned_plane_members.to_vec().into_boxed_slice(),
    };
    let bytes = encode_result_envelope(&envelope)?;
    let key = ObjectKey::<CompilerResultEnvelopeSchema>::from_value(bytes.as_slice());
    Ok(TypedObject::from_value(&key, bytes.as_slice()))
}

fn generation_member_role(ordinal: usize) -> Result<CompilerResultMemberRole, CompilerResultError> {
    match ordinal {
        0 => Ok(CompilerResultMemberRole::Manifest),
        value if value % 2 == 1 => Ok(CompilerResultMemberRole::Fragment),
        _ => Ok(CompilerResultMemberRole::SemanticImage),
    }
}

fn generation_member_role_for_claim(
    reference: ObjectRef<ObjectDomain>,
    key: u64,
) -> Result<CompilerResultMemberRole, CompilerResultError> {
    match reference.schema {
        SchemaId::CompilationManifest if key == RESULT_MANIFEST_ENTRY_KEY => {
            Ok(CompilerResultMemberRole::Manifest)
        }
        SchemaId::IrFragment if key % 2 == 1 => Ok(CompilerResultMemberRole::Fragment),
        SchemaId::IrSemanticImage if key != 0 && key % 2 == 0 => {
            Ok(CompilerResultMemberRole::SemanticImage)
        }
        _ => Err(CompilerResultError::OutputReference),
    }
}

fn validate_member_topology(members: &[CompilerResultMemberV1]) -> Result<(), CompilerResultError> {
    if members.is_empty() || members.len() % 2 == 0 {
        return Err(CompilerResultError::MemberTopology);
    }
    for (ordinal, member) in members.iter().enumerate() {
        let expected_role = generation_member_role(ordinal)?;
        let (schema, kind) = match expected_role {
            CompilerResultMemberRole::Manifest => (SchemaId::CompilationManifest, 1_u16),
            CompilerResultMemberRole::Fragment => (SchemaId::IrFragment, 1_u16),
            CompilerResultMemberRole::SemanticImage => (SchemaId::IrSemanticImage, 2_u16),
            CompilerResultMemberRole::AuxiliaryPlane
            | CompilerResultMemberRole::VersionedPlaneManifest
            | CompilerResultMemberRole::VersionedPlaneSegment => {
                return Err(CompilerResultError::MemberTopology);
            }
        };
        let expected_key =
            match expected_role {
                CompilerResultMemberRole::Manifest => 0,
                CompilerResultMemberRole::Fragment => u64::try_from(ordinal / 2)?
                    .checked_mul(2)
                    .and_then(|key| key.checked_add(1))
                    .ok_or(CompilerResultError::MemberTopology)?,
                CompilerResultMemberRole::SemanticImage => u64::try_from(ordinal / 2)?
                    .checked_mul(2)
                    .ok_or(CompilerResultError::MemberTopology)?,
                CompilerResultMemberRole::AuxiliaryPlane
                | CompilerResultMemberRole::VersionedPlaneManifest
                | CompilerResultMemberRole::VersionedPlaneSegment => unreachable!(),
            };
        let expected_parent = (ordinal != 0).then_some(RESULT_MANIFEST_ENTRY_KEY);
        if member.role != expected_role
            || member.reference.schema != schema
            || *member.reference.kind != kind
            || member.key != expected_key
            || member.parent != expected_parent
            || *member.reference.length == 0
            || *member.reference.content.as_ref() == [0; 32]
        {
            return Err(CompilerResultError::MemberTopology);
        }
    }
    let mut ids = BTreeSet::new();
    if members.iter().any(|member| !ids.insert(member.object_id)) {
        return Err(CompilerResultError::MemberTopology);
    }
    Ok(())
}

fn validate_auxiliary_topology(
    members: &[CompilerResultMemberV1],
) -> Result<(), CompilerResultError> {
    let mut previous: Option<(u32, u16, [u8; 32], u64)> = None;
    let mut ids = BTreeSet::new();
    for (ordinal, member) in members.iter().enumerate() {
        let key = (
            u32::from(member.reference.schema),
            *member.reference.kind,
            *member.reference.content.as_ref(),
            *member.reference.length,
        );
        if member.role != CompilerResultMemberRole::AuxiliaryPlane
            || member.key != u64::try_from(ordinal)?
            || member.parent.is_some()
            || *member.reference.length == 0
            || previous.is_some_and(|prior| prior >= key)
            || !ids.insert(member.object_id)
        {
            return Err(CompilerResultError::MemberTopology);
        }
        previous = Some(key);
    }
    Ok(())
}

pub(super) fn validate_versioned_plane_topology(
    generation_members: &[CompilerResultMemberV1],
    plane_members: &[CompilerResultMemberV1],
) -> Result<(), CompilerResultError> {
    if generation_members.len() < 3 || generation_members.len() % 2 == 0 || plane_members.is_empty()
    {
        return Err(CompilerResultError::VersionedPlaneTopology);
    }
    let artifact_count = (generation_members.len() - 1) / 2;
    let mut seen_ids = BTreeSet::new();
    if generation_members
        .iter()
        .any(|member| !seen_ids.insert(member.object_id))
    {
        return Err(CompilerResultError::MemberTopology);
    }

    let mut cursor = 0_usize;
    for artifact_ordinal in 0..artifact_count {
        let manifest_member = plane_members
            .get(cursor)
            .ok_or(CompilerResultError::VersionedPlaneTopology)?;
        let image_position = artifact_ordinal
            .checked_mul(2)
            .and_then(|ordinal| ordinal.checked_add(2))
            .ok_or(CompilerResultError::VersionedPlaneTopology)?;
        let image = generation_members
            .get(image_position)
            .ok_or(CompilerResultError::VersionedPlaneTopology)?;
        let ordinal = u64::try_from(artifact_ordinal)?;
        if manifest_member.role != CompilerResultMemberRole::VersionedPlaneManifest
            || manifest_member.reference.schema != SchemaId::Object
            || *manifest_member.reference.kind != 0xc005
            || manifest_member.key != ordinal
            || manifest_member.parent != Some(image.key)
            || *manifest_member.reference.length == 0
            || *manifest_member.reference.content.as_ref() == [0; 32]
            || manifest_member.object_id == [0; 32]
            || !seen_ids.insert(manifest_member.object_id)
        {
            return Err(CompilerResultError::VersionedPlaneTopology);
        }
        cursor += 1;

        let mut segment_ordinal = 0_u64;
        while let Some(segment) = plane_members.get(cursor)
            && segment.role == CompilerResultMemberRole::VersionedPlaneSegment
        {
            if segment.reference.schema != SchemaId::Object
                || *segment.reference.kind != 0xc004
                || segment.key != segment_ordinal
                || segment.parent != Some(ordinal)
                || *segment.reference.length == 0
                || *segment.reference.content.as_ref() == [0; 32]
                || segment.object_id == [0; 32]
                || !seen_ids.insert(segment.object_id)
            {
                return Err(CompilerResultError::VersionedPlaneTopology);
            }
            segment_ordinal = segment_ordinal
                .checked_add(1)
                .ok_or(CompilerResultError::VersionedPlaneTopology)?;
            cursor += 1;
        }
        if segment_ordinal == 0 {
            return Err(CompilerResultError::VersionedPlaneTopology);
        }
    }
    if cursor != plane_members.len() {
        return Err(CompilerResultError::VersionedPlaneTopology);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CompilerResultError, MAX_RESULT_MEMBERS, RESULT_MEMBER_FIXED_BYTES, ResultMemberBuffers,
        count_result_member_roles,
    };

    fn append_member_frame(frames: &mut Vec<u8>, role: u8) {
        frames.push(role);
        frames.resize(frames.len() + RESULT_MEMBER_FIXED_BYTES - 1, 0);
    }

    #[test]
    fn member_role_prepass_enforces_canonical_sections() {
        let mut frames = Vec::new();
        for role in [1, 2, 3, 4, 5, 6] {
            append_member_frame(&mut frames, role);
        }
        let counts = count_result_member_roles(&frames, 6).expect("count canonical member roles");
        assert_eq!(counts.generation, 3);
        assert_eq!(counts.auxiliary, 1);
        assert_eq!(counts.versioned_planes, 2);

        let mut out_of_order = Vec::new();
        append_member_frame(&mut out_of_order, 4);
        append_member_frame(&mut out_of_order, 3);
        assert!(matches!(
            count_result_member_roles(&out_of_order, 2),
            Err(CompilerResultError::MemberTopology)
        ));
    }

    #[test]
    fn maximum_member_prepass_reserves_only_the_role_counted_total() {
        let mut frames = Vec::with_capacity(MAX_RESULT_MEMBERS * RESULT_MEMBER_FIXED_BYTES);
        for _ in 0..MAX_RESULT_MEMBERS {
            append_member_frame(&mut frames, 4);
        }
        let counts = count_result_member_roles(&frames, MAX_RESULT_MEMBERS)
            .expect("scan the maximum allowed member count");
        assert_eq!(counts.generation, 0);
        assert_eq!(counts.auxiliary, MAX_RESULT_MEMBERS);
        assert_eq!(counts.versioned_planes, 0);

        let buffers = ResultMemberBuffers::reserve(counts).expect("reserve exact member roles");
        let member_slots = buffers.members.capacity()
            + buffers.auxiliary.capacity()
            + buffers.versioned_planes.capacity();
        assert_eq!(member_slots, MAX_RESULT_MEMBERS);
        assert!(
            member_slots * std::mem::size_of::<super::CompilerResultMemberV1>() < 16 * 1024 * 1024,
            "member vector backing storage must stay linear in the wire count"
        );
    }
}
