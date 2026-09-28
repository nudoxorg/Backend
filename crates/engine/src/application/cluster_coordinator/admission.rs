//! Exact CAS and semantic admission for a remotely staged compiler result.

#[cfg(test)]
use super::result_wire::result_output_object;
use super::result_wire::{
    CompilerResultEnvelopeV1, CompilerResultError, CompilerResultMemberRole,
    CompilerResultMemberV1, MAX_RESULT_ENVELOPE_BYTES, MAX_RESULT_MEMBERS,
    RESULT_ENVELOPE_SCHEMA_TYPE, RESULT_ENVELOPE_SCHEMA_VERSION, RESULT_MANIFEST_ENTRY_KEY,
    RESULT_OUTPUT_SCHEMA_DOMAIN, RESULT_OUTPUT_SCHEMA_TYPE, RESULT_OUTPUT_SCHEMA_VERSION,
};
use super::runtime::VerifiedCompilerInputAdmission;
use crate::compiler_cluster_transport::CompilerInvocationRecipeV2;
use crate::compiler_cluster_transport::StoredRemoteCompilerResult;
use crate::compiler_input_capture_v2::{CapturedFullWorkspaceV2, CompilerInputCaptureV2Error};
use crate::compiler_input_manifest_v2::CompilerInputManifestV2;
use crate::compiler_input_tree_v2::{CompilerInputTreeRecordV2, CompilerWorkspaceFileRoleV2};
use crate::publication::binding::{CompilationBindingFacts, CompilationBindingView};
use crate::publication::manifest::{
    CompilationManifestFacts, CompilationManifestFormat, CompilationManifestView,
};
use backend_execution::{
    CompilerAssignment, CompilerAssignmentRoute, StoredCompilerCandidate, VerifiedCompilerInput,
};
use backend_semantic::ir::{
    FragmentRangeManifest, FragmentView, GenerationId, ImageProvenance, SemanticCoreReader,
    SemanticCoverageState, SemanticImageIdentity, SemanticImageView, SemanticIrPlane,
    SemanticManifestRoot, SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneRoot,
    SemanticSegmentId, SourceIdentity,
};
use backend_semantic::vocabulary::CompileRecipeFact;
use backend_store::hydration::{PlanScratch, Projection, VerifiedGenerationFacts, demand, plan};
use backend_store::root::{
    ClosureScratch, GenerationRootBuilder, GenerationView, PreparedLocality, RootEntry,
    SelectedCount,
};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ClosureId, FileStore, ObjectId, StoredClosureReceipt,
    TypedObject, UntrustedObjectId,
};
use backend_version::object::{ObjectKind, ObjectLength, ObjectRef};
use backend_version::schema::SchemaId;
use backend_version::{
    ArtifactId, ContentPayloadHasher, Coverage, IrFragmentDomain, IrFragmentEncoding, ObjectDomain,
    ObjectVersionHasher, SchemaIdentity, SourceFactDomain,
};
use std::collections::BTreeMap;
use std::io::{self, Write};
/// An independently typed output object admitted into a result closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedRemoteCompilerPlane {
    member: CompilerResultMemberV1,
}

impl CheckedRemoteCompilerPlane {
    /// Returns the exact typed plane member descriptor.
    #[must_use]
    pub const fn member(&self) -> CompilerResultMemberV1 {
        self.member
    }
}

/// One remotely received fragment/image pair admitted against a semantic manifest row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedRemoteCompilerArtifact {
    artifact_ordinal: u32,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    fragment_identity: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    fragment_object_id: ObjectId,
    semantic_image_facts: crate::publication::SemanticImageArtifactFacts,
    semantic_generation: GenerationId,
    semantic_image_object_id: ObjectId,
}

impl CheckedRemoteCompilerArtifact {
    /// Canonical semantic-manifest artifact ordinal.
    #[must_use]
    pub const fn artifact_ordinal(&self) -> u32 {
        self.artifact_ordinal
    }

    /// Source facts reopened from the exact compact fragment.
    #[must_use]
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Recipe facts reopened from the exact compact fragment.
    #[must_use]
    pub const fn recipe(&self) -> CompileRecipeFact {
        self.recipe
    }

    /// Typed fragment identity proven by the output manifest and fragment bytes.
    #[must_use]
    pub const fn fragment_identity(&self) -> ArtifactId<IrFragmentEncoding, IrFragmentDomain> {
        self.fragment_identity
    }

    /// Physical CAS object identity for the typed fragment wrapper.
    #[must_use]
    pub const fn fragment_object_id(&self) -> ObjectId {
        self.fragment_object_id
    }

    /// Semantic-image identity and exact length proven by the output manifest.
    #[must_use]
    pub const fn semantic_image_facts(&self) -> crate::publication::SemanticImageArtifactFacts {
        self.semantic_image_facts
    }

    /// Canonical semantic VCS generation of this image's exact bytes.
    #[must_use]
    pub const fn semantic_generation(&self) -> GenerationId {
        self.semantic_generation
    }

    /// Physical CAS object identity for the typed semantic-image wrapper.
    #[must_use]
    pub const fn semantic_image_object_id(&self) -> ObjectId {
        self.semantic_image_object_id
    }
}

/// One verified range segment in an artifact's typed versioned plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedRemoteCompilerPlaneSegment {
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    byte_length: u64,
    segment_id: SemanticSegmentId,
    object_id: ObjectId,
    member: CompilerResultMemberV1,
}

impl CheckedRemoteCompilerPlaneSegment {
    #[must_use]
    pub const fn first_key(&self) -> &[u8; 32] {
        &self.first_key
    }

    #[must_use]
    pub const fn last_key(&self) -> &[u8; 32] {
        &self.last_key
    }

    #[must_use]
    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Segment identity admitted against the exact bounded payload.
    #[must_use]
    pub const fn segment_id(&self) -> SemanticSegmentId {
        self.segment_id
    }

    /// CAS object identity of the checked c004 segment wrapper.
    #[must_use]
    pub const fn object_id(&self) -> ObjectId {
        self.object_id
    }

    /// Exact result-closure member associated with this segment.
    #[must_use]
    pub const fn member(&self) -> CompilerResultMemberV1 {
        self.member
    }
}

/// One independent IR or embedding plane in an artifact's c005 manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedRemoteCompilerPlaneDescriptor {
    kind: SemanticPlaneKind,
    root: SemanticPlaneRoot,
    coverage: SemanticCoverageState,
    segments: Box<[CheckedRemoteCompilerPlaneSegment]>,
}

impl CheckedRemoteCompilerPlaneDescriptor {
    #[must_use]
    pub const fn kind(&self) -> SemanticPlaneKind {
        self.kind
    }

    #[must_use]
    pub const fn root(&self) -> SemanticPlaneRoot {
        self.root
    }

    /// Preserves claimed coverage; decoding does not recreate authority.
    #[must_use]
    pub const fn coverage(&self) -> SemanticCoverageState {
        self.coverage
    }

    #[must_use]
    pub fn segments(&self) -> &[CheckedRemoteCompilerPlaneSegment] {
        &self.segments
    }
}

/// Exact versioned plane set attached to one semantic image and VCS generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedRemoteCompilerPlaneArtifact {
    artifact_ordinal: u32,
    semantic_image_object_id: ObjectId,
    semantic_generation: GenerationId,
    manifest_root: SemanticManifestRoot,
    manifest_bytes: Box<[u8]>,
    manifest_object_id: ObjectId,
    manifest_member: CompilerResultMemberV1,
    planes: Box<[CheckedRemoteCompilerPlaneDescriptor]>,
}

impl CheckedRemoteCompilerPlaneArtifact {
    #[must_use]
    pub const fn artifact_ordinal(&self) -> u32 {
        self.artifact_ordinal
    }

    /// Exact semantic-image object this plane manifest describes.
    #[must_use]
    pub const fn semantic_image_object_id(&self) -> ObjectId {
        self.semantic_image_object_id
    }

    #[must_use]
    pub const fn semantic_generation(&self) -> GenerationId {
        self.semantic_generation
    }

    #[must_use]
    pub const fn manifest_root(&self) -> SemanticManifestRoot {
        self.manifest_root
    }

    /// Canonical c005 manifest bytes admitted against this image and its input witness.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// CAS identity of the exact c005 manifest wrapper.
    #[must_use]
    pub const fn manifest_object_id(&self) -> ObjectId {
        self.manifest_object_id
    }

    #[must_use]
    pub const fn manifest_member(&self) -> CompilerResultMemberV1 {
        self.manifest_member
    }

    #[must_use]
    pub fn planes(&self) -> &[CheckedRemoteCompilerPlaneDescriptor] {
        &self.planes
    }
}

/// Complete checked semantic output reconstructed from the exact admitted result closure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedRemoteCompilerOutput {
    closure_id: ClosureId,
    generation: VerifiedGenerationFacts,
    manifest_facts: CompilationManifestFacts,
    binding_facts: CompilationBindingFacts,
    manifest_bytes: Box<[u8]>,
    binding_bytes: Box<[u8]>,
    artifacts: Box<[CheckedRemoteCompilerArtifact]>,
    members: Box<[CompilerResultMemberV1]>,
    auxiliary_planes: Box<[CheckedRemoteCompilerPlane]>,
    versioned_plane_members: Box<[CompilerResultMemberV1]>,
    versioned_plane_artifacts: Box<[CheckedRemoteCompilerPlaneArtifact]>,
}

impl CheckedRemoteCompilerOutput {
    /// Exact durably admitted output closure.
    #[must_use]
    pub const fn closure_id(&self) -> ClosureId {
        self.closure_id
    }

    /// Complete root and dependency-set facts independently recomputed from the objects.
    #[must_use]
    pub const fn generation_facts(&self) -> VerifiedGenerationFacts {
        self.generation
    }

    /// Typed semantic-manifest facts reconstructed from canonical manifest bytes.
    #[must_use]
    pub const fn manifest_facts(&self) -> CompilationManifestFacts {
        self.manifest_facts
    }

    /// Typed generation-to-manifest binding facts reconstructed from canonical bytes.
    #[must_use]
    pub const fn binding_facts(&self) -> CompilationBindingFacts {
        self.binding_facts
    }

    /// Exact canonical semantic-manifest bytes.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// Exact canonical generation binding bytes.
    #[must_use]
    pub fn binding_bytes(&self) -> &[u8] {
        &self.binding_bytes
    }

    /// Fragment/image pairs in canonical semantic-manifest order.
    #[must_use]
    pub fn artifacts(&self) -> &[CheckedRemoteCompilerArtifact] {
        &self.artifacts
    }

    /// Exact semantic-generation closure inventory in canonical root order.
    #[must_use]
    pub fn members(&self) -> &[CompilerResultMemberV1] {
        &self.members
    }

    /// Independently typed planes, admitted without counting them as semantic IR members.
    #[must_use]
    pub fn auxiliary_planes(&self) -> &[CheckedRemoteCompilerPlane] {
        &self.auxiliary_planes
    }

    /// Exact c005/c004 member inventory, preserving canonical artifact grouping.
    #[must_use]
    pub fn versioned_plane_members(&self) -> &[CompilerResultMemberV1] {
        &self.versioned_plane_members
    }

    /// Verified per-image IR and independent plane metadata with exact CAS segment IDs.
    #[must_use]
    pub fn versioned_plane_artifacts(&self) -> &[CheckedRemoteCompilerPlaneArtifact] {
        &self.versioned_plane_artifacts
    }

    /// Streams one exact checked member payload from this result closure.
    ///
    /// The CAS reader rechecks closure membership, physical object identity,
    /// output schema, semantic reference, key and version while copying through
    /// its fixed-size buffer. The member must be part of this admitted output.
    pub fn write_member_payload<W: std::io::Write + ?Sized>(
        &self,
        store: &FileStore,
        object_id: ObjectId,
        maximum_payload_bytes: u64,
        output: &mut W,
    ) -> Result<backend_store::VerifiedObjectEnvelope, CompilerResultError> {
        let member = self
            .members
            .iter()
            .chain(self.auxiliary_planes.iter().map(|plane| &plane.member))
            .chain(self.versioned_plane_members.iter())
            .copied()
            .find(|member| member.object_id() == *object_id.as_bytes())
            .ok_or(CompilerResultError::ClosureAdmission)?;
        let index = store.read_closure_index(self.closure_id)?;
        if !index
            .contains_object_id(object_id)
            .map_err(CompilerResultError::Store)?
        {
            return Err(CompilerResultError::ClosureAdmission);
        }

        stream_checked_result_member(store, member, object_id, maximum_payload_bytes, output)
    }
}

struct CheckedResultPayloadWriter<'output, W: ?Sized> {
    output: &'output mut W,
    content: ContentPayloadHasher<ObjectDomain>,
    version: ObjectVersionHasher,
}

impl<'output, W: Write + ?Sized> CheckedResultPayloadWriter<'output, W> {
    fn new(
        output: &'output mut W,
        expected_length: u64,
        schema: SchemaIdentity,
    ) -> Result<Self, CompilerResultError> {
        let length = usize::try_from(expected_length)?;
        Ok(Self {
            output,
            content: ContentPayloadHasher::new(expected_length),
            version: ObjectVersionHasher::new(schema, length)
                .map_err(|_| CompilerResultError::OutputReference)?,
        })
    }

    fn finish(
        self,
    ) -> Result<(backend_version::ContentId<ObjectDomain>, [u8; 32]), CompilerResultError> {
        let content = self
            .content
            .finish()
            .map_err(|_| CompilerResultError::OutputReference)?;
        let version = self
            .version
            .finish()
            .map_err(|_| CompilerResultError::OutputReference)?;
        Ok((content, version))
    }
}

impl<W: Write + ?Sized> Write for CheckedResultPayloadWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        let chunk = &bytes[..written];
        self.content.push_chunk(chunk).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "result content length mismatch")
        })?;
        self.version.update(chunk).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "result version length mismatch")
        })?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

/// Opaque, storage-only remote compiler candidate plus its fully checked output evidence.
#[derive(Clone, Debug)]
pub struct AdmittedRemoteCompilerCandidate {
    candidate: StoredCompilerCandidate,
    receipt: StoredClosureReceipt,
    worker_result_receipt: crate::cluster_transport::ControlResultReceipt,
    physical_pack_id: Option<[u8; 32]>,
    output: CheckedRemoteCompilerOutput,
    input_admission: VerifiedCompilerInputAdmission,
}

impl AdmittedRemoteCompilerCandidate {
    /// Scheduler candidate; carries no head-selection authority.
    #[must_use]
    pub const fn candidate(&self) -> StoredCompilerCandidate {
        self.candidate
    }

    /// Exact durable CAS closure receipt retained by the scheduler.
    #[must_use]
    pub const fn receipt(&self) -> StoredClosureReceipt {
        self.receipt
    }

    /// Exact worker receipt admitted before this candidate was stored. A durable owner journal
    /// can retain it to repeat full admission after a crash between intent persistence and Turso
    /// compare-and-select.
    #[must_use]
    pub const fn worker_result_receipt(&self) -> crate::cluster_transport::ControlResultReceipt {
        self.worker_result_receipt
    }

    /// Optional physical pack receipt carried out of band by the worker.
    ///
    /// This value does not participate in the result envelope or logical closure identity.
    #[must_use]
    pub const fn physical_pack_id(&self) -> Option<[u8; 32]> {
        self.physical_pack_id
    }

    /// Exact checked result closure identity.
    #[must_use]
    pub const fn closure(&self) -> ClosureId {
        self.output.closure_id
    }

    /// Fully checked, still-unselected semantic output and independent planes.
    #[must_use]
    pub const fn output(&self) -> &CheckedRemoteCompilerOutput {
        &self.output
    }

    /// Owner source-observation and trusted-peer evidence bound to the input snapshot.
    #[must_use]
    pub const fn input_admission(&self) -> &VerifiedCompilerInputAdmission {
        &self.input_admission
    }
}

/// Reopens and admits one exact remote result only after its assignment, input snapshot,
/// durable closure, semantic generation and independent typed planes all match.
///
/// This function never selects a generation. Its return value is storage and semantic evidence
/// for the Turso authority's checked candidate path.
pub fn admit_remote_compiler_candidate(
    store: &FileStore,
    stored: StoredRemoteCompilerResult,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    capture: &CapturedFullWorkspaceV2,
    input_admission: VerifiedCompilerInputAdmission,
    budget: ArtifactBudget,
) -> Result<AdmittedRemoteCompilerCandidate, CompilerResultError> {
    if !matches!(assignment.route(), CompilerAssignmentRoute::Remote(_)) {
        return Err(CompilerResultError::AssignmentMismatch);
    }
    let candidate = stored.candidate();
    let wire_receipt = stored.receipt();
    let assignment_scope =
        crate::compiler_cluster_transport::compiler_assignment_scope(assignment, namespace_id)
            .map_err(|_| CompilerResultError::AssignmentMismatch)?;
    if candidate.work() != assignment.work()
        || candidate.token() != assignment.token()
        || candidate.peer()
            != match assignment.route() {
                CompilerAssignmentRoute::Remote(peer) => peer,
                CompilerAssignmentRoute::Local => {
                    return Err(CompilerResultError::AssignmentMismatch);
                }
            }
        || *candidate.closure_receipt().closure().as_bytes() != wire_receipt.closure_id
        || wire_receipt.scope != assignment_scope
    {
        return Err(CompilerResultError::AssignmentMismatch);
    }
    let work = assignment.work();
    let read_claim = work.input_identity();
    if !matches!(work.input(), VerifiedCompilerInput::FullWorkspaceFresh(_))
        || !input_admission.matches(assignment, namespace_id, capture)
    {
        return Err(CompilerResultError::InputAdmission);
    }
    let input_manifest = capture.manifest();
    let full_workspace_claim = work
        .input()
        .full_workspace()
        .ok_or(CompilerResultError::InputAdmission)?
        .claim();
    if input_manifest.identity_claim().ok() != Some(read_claim)
        || input_manifest.toolchain() == [0; 32]
        || input_manifest.environment() == [0; 32]
        || input_manifest.target_platform() == [0; 32]
        || input_manifest.max_output_bytes() != work.max_output_bytes()
        || work.selected_base().is_some()
        || full_workspace_claim.input_closure_id != *capture.closure().as_bytes()
        || full_workspace_claim.manifest_object_id != *capture.manifest_object_id().as_bytes()
        || input_manifest.source_fence_digest() != input_admission.evidence().source_fence_digest()
    {
        return Err(CompilerResultError::InputManifestMismatch);
    }
    input_manifest
        .encode()
        .map_err(|_| CompilerResultError::InputManifestMismatch)?;
    let source_identities = validate_input_closure_v2(store, capture, budget)?;
    let stored_receipt = candidate.closure_receipt();
    if *stored_receipt.closure().as_bytes() != wire_receipt.closure_id
        || stored_receipt.object_count() != u64::from(wire_receipt.object_count)
        || stored_receipt.payload_bytes() != wire_receipt.payload_bytes
    {
        return Err(CompilerResultError::AssignmentMismatch);
    }
    let reopened = store.reopen_stored_closure(
        ArtifactClosureClaim::from_bytes(wire_receipt.closure_id),
        budget,
    )?;
    if reopened.closure() != stored_receipt.closure()
        || reopened.object_count() != stored_receipt.object_count()
    {
        return Err(CompilerResultError::ClosureAdmission);
    }
    if wire_receipt
        .pack_id
        .is_some_and(|pack_id| pack_id == [0; 32])
    {
        return Err(CompilerResultError::AssignmentMismatch);
    }
    let checked_index = super::result_wire::reopen_compiler_result_envelope(
        store,
        reopened.closure(),
        wire_receipt.object_count,
        wire_receipt.payload_bytes,
        assignment_scope,
        *capture.closure().as_bytes(),
        *capture.manifest_object_id().as_bytes(),
        wire_receipt.target_root,
        budget.max_payload_bytes,
    )?;
    if checked_index.object_count() as usize > budget.max_closure_objects {
        return Err(CompilerResultError::ClosureAdmission);
    }
    let envelope = checked_index.envelope().clone();
    let closure = store.open_closure(checked_index.closure_id())?;

    let output = validate_semantic_result(
        store,
        &closure,
        &envelope,
        CompilerOutputRecipePolicy::InvocationV2(input_manifest.invocation_recipe()),
        VersionedPlaneInputBinding::from_manifest(input_manifest),
        source_identities,
        reopened.closure(),
        budget.max_payload_bytes,
    )?;
    Ok(AdmittedRemoteCompilerCandidate {
        candidate,
        receipt: stored_receipt,
        worker_result_receipt: wire_receipt,
        physical_pack_id: wire_receipt.pack_id,
        output,
        input_admission,
    })
}

/// Re-admits an exact durably stored worker result after owner restart.
///
/// The caller must hold the GC pin corresponding to `pinned_stored_receipt` through semantic
/// selection. This API reconstructs the private transport candidate only after the store derives
/// the payload total from verified CAS bytes, binds it to the exact worker receipt and assignment,
/// then runs the ordinary envelope, source, and versioned-plane admission path. It does not mint
/// a worker receipt or publication authority.
pub fn readmit_remote_compiler_candidate(
    store: &FileStore,
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    worker_receipt: crate::cluster_transport::ControlResultReceipt,
    pinned_stored_receipt: &backend_store::PinnedStoredClosureReceipt,
    capture: &CapturedFullWorkspaceV2,
    input_admission: VerifiedCompilerInputAdmission,
    budget: ArtifactBudget,
) -> Result<AdmittedRemoteCompilerCandidate, CompilerResultError> {
    let stored_receipt = pinned_stored_receipt
        .admit_recovered_payload(worker_receipt.payload_bytes)
        .map_err(CompilerResultError::Store)?;
    let stored = crate::compiler_cluster_transport::store_recovered_compiler_result(
        assignment,
        namespace_id,
        worker_receipt,
        stored_receipt,
    )
    .map_err(|_| CompilerResultError::AssignmentMismatch)?;
    admit_remote_compiler_candidate(
        store,
        stored,
        assignment,
        namespace_id,
        capture,
        input_admission,
        budget,
    )
}

fn validate_input_closure_v2(
    store: &FileStore,
    capture: &CapturedFullWorkspaceV2,
    budget: ArtifactBudget,
) -> Result<BTreeMap<([u8; 32], u32), usize>, CompilerResultError> {
    let verified = capture
        .verify_in_store(store)
        .map_err(|error| match error {
            CompilerInputCaptureV2Error::Store(error) => CompilerResultError::Store(error),
            _ => CompilerResultError::InputClosureMismatch,
        })?;
    if verified.closure() != capture.closure()
        || verified.manifest() != capture.manifest()
        || verified.manifest_object_id() != capture.manifest_object_id()
        || verified.member_ids().len() > budget.max_closure_objects
        || verified.member_ids().len() as u64 != capture.object_count()
    {
        return Err(CompilerResultError::InputClosureMismatch);
    }

    let mut source_files = BTreeMap::<[u8; 32], (u64, [u8; 32])>::new();
    let mut sources = BTreeMap::<([u8; 32], u32), usize>::new();
    for record in verified.workspace_records() {
        if let CompilerInputTreeRecordV2::File {
            path,
            role: CompilerWorkspaceFileRoleV2::Source,
            object_id,
            length,
            ..
        } = record
        {
            let source_identity = verified
                .source_identity(path)
                .ok_or(CompilerResultError::InputClosureMismatch)?;
            let source_identity_bytes = *source_identity.as_ref();
            if let Some(previous) =
                source_files.insert(*object_id, (*length, source_identity_bytes))
                && previous != (*length, source_identity_bytes)
            {
                return Err(CompilerResultError::InputClosureMismatch);
            }
            let source_key = (source_identity_bytes, u32::try_from(*length)?);
            let count = sources.entry(source_key).or_insert(0);
            *count = count
                .checked_add(1)
                .ok_or(CompilerResultError::InputClosureMismatch)?;
        }
    }
    if source_files.is_empty() {
        return Err(CompilerResultError::InputClosureMismatch);
    }

    let mut total_bytes = 0_u64;
    for object_id in verified.member_ids() {
        let object =
            store.verify_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))?;
        if object.id() != object_id {
            return Err(CompilerResultError::InputClosureMismatch);
        }
        total_bytes = total_bytes
            .checked_add(object.payload_len())
            .filter(|total| *total <= budget.max_payload_bytes)
            .ok_or(CompilerResultError::EnvelopeLimit)?;
    }
    Ok(sources)
}

#[derive(Clone, Copy)]
enum CompilerOutputRecipePolicy {
    InvocationV2(CompilerInvocationRecipeV2),
}

#[derive(Clone, Copy)]
struct VersionedPlaneInputBinding {
    package: [u8; 32],
    target: [u8; 32],
    profile: backend_semantic::vocabulary::LanguageProfile,
    stage: backend_semantic::vocabulary::Stage,
    recipe: [u8; 32],
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
    input_root: [u8; 32],
    read_manifest: [u8; 32],
}

impl VersionedPlaneInputBinding {
    fn from_manifest(manifest: &CompilerInputManifestV2) -> Self {
        let recipe = manifest.invocation_recipe();
        Self {
            package: *manifest.package_target().package().identity.as_ref(),
            target: *manifest.package_target().target().as_ref(),
            profile: manifest.profile(),
            stage: manifest.stage(),
            recipe: *recipe.identity().as_ref(),
            toolchain: *recipe.toolchain().as_ref(),
            environment: recipe.environment(),
            target_platform: recipe.target_platform(),
            input_root: manifest.input_root(),
            read_manifest: manifest.read_manifest(),
        }
    }
}

fn validate_semantic_result(
    store: &FileStore,
    closure: &backend_store::DurableManifest,
    envelope: &CompilerResultEnvelopeV1,
    recipe_policy: CompilerOutputRecipePolicy,
    plane_input: VersionedPlaneInputBinding,
    source_identities: BTreeMap<([u8; 32], u32), usize>,
    closure_id: ClosureId,
    maximum_member_bytes: u64,
) -> Result<CheckedRemoteCompilerOutput, CompilerResultError> {
    if !envelope.auxiliary_planes().is_empty() {
        return Err(CompilerResultError::UnsupportedAuxiliaryOutput);
    }
    if envelope.members.len() < 3 || envelope.members.len() % 2 == 0 {
        return Err(CompilerResultError::MemberCount);
    }
    let manifest_member = envelope
        .members
        .first()
        .ok_or(CompilerResultError::MemberCount)?;
    let manifest_object =
        checked_result_object(store, closure, *manifest_member, maximum_member_bytes)?;
    let manifest_bytes = manifest_object.bytes().to_vec().into_boxed_slice();
    let fragment_count = u32::from_le_bytes(
        manifest_bytes
            .get(12..16)
            .ok_or(CompilerResultError::ManifestInvalid)?
            .try_into()
            .map_err(|_| CompilerResultError::ManifestInvalid)?,
    );
    let fragment_count = usize::try_from(fragment_count)?;
    if fragment_count == 0
        || fragment_count > MAX_RESULT_MEMBERS / 2
        || manifest_bytes.len() > MAX_RESULT_ENVELOPE_BYTES
    {
        return Err(CompilerResultError::ManifestInvalid);
    }
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(fragment_count)
        .map_err(|_| CompilerResultError::Allocation)?;
    scratch.resize(fragment_count, None);
    let manifest = CompilationManifestView::validate(&manifest_bytes, &mut scratch)
        .map_err(|_| CompilerResultError::ManifestInvalid)?;
    if manifest.format != CompilationManifestFormat::SemanticV2
        || manifest.identity != envelope.manifest_identity
        || manifest.identity != envelope.binding_facts.manifest
        || manifest_member.reference
            != semantic_object_reference(SchemaId::CompilationManifest, 1, &manifest_bytes)?
    {
        return Err(CompilerResultError::ManifestInvalid);
    }
    let expected_member_count = fragment_count
        .checked_mul(2)
        .and_then(|count| count.checked_add(1))
        .ok_or(CompilerResultError::MemberCount)?;
    if envelope.members.len() != expected_member_count {
        return Err(CompilerResultError::MemberCount);
    }

    let mut artifacts = Vec::new();
    artifacts
        .try_reserve_exact(fragment_count)
        .map_err(|_| CompilerResultError::Allocation)?;
    let mut semantic_members = Vec::new();
    semantic_members
        .try_reserve_exact(expected_member_count)
        .map_err(|_| CompilerResultError::Allocation)?;

    let mut member_ordinal = 1_usize;
    let mut observed_source_identities = BTreeMap::new();
    for facts in manifest.fragments() {
        let fragment_member = *envelope
            .members
            .get(member_ordinal)
            .ok_or(CompilerResultError::MemberCount)?;
        let image_member = *envelope
            .members
            .get(member_ordinal + 1)
            .ok_or(CompilerResultError::MemberCount)?;
        let fragment_object =
            checked_result_object(store, closure, fragment_member, maximum_member_bytes)?;
        let fragment_bytes = fragment_object.bytes();
        let fragment = FragmentView::validate(fragment_bytes)
            .map_err(|_| CompilerResultError::FragmentMismatch)?;
        let ranges = FragmentRangeManifest::from_view(&fragment)
            .map_err(|_| CompilerResultError::FragmentMismatch)?;
        let source_key = (*facts.source.identity.as_ref(), facts.source.byte_len);
        admit_output_source_identity(
            &source_identities,
            &mut observed_source_identities,
            source_key,
        )?;
        if ranges.fragment != facts.fragment
            || ranges.fragment_length != facts.fragment_length
            || ranges.source != facts.source
            || ranges.recipe != facts.recipe
            || ranges.ranges != facts.ranges
            || !recipe_matches_policy(facts.recipe, facts.source, recipe_policy)
            || fragment_member.reference
                != semantic_object_reference(SchemaId::IrFragment, 1, fragment_bytes)?
        {
            return Err(CompilerResultError::FragmentMismatch);
        }
        let image_facts = facts
            .semantic_image
            .ok_or(CompilerResultError::ManifestInvalid)?;
        let image_object =
            checked_result_object(store, closure, image_member, maximum_member_bytes)?;
        let image_bytes = image_object.bytes();
        let image = SemanticImageView::reopen(image_bytes)
            .map_err(|_| CompilerResultError::SemanticImageMismatch)?;
        if SemanticImageIdentity::from_encoded_bytes(image_bytes) != image_facts.identity
            || u32::try_from(image_bytes.len())? != image_facts.byte_length
            || image_member.reference
                != semantic_object_reference(SchemaId::IrSemanticImage, 2, image_bytes)?
        {
            return Err(CompilerResultError::SemanticImageMismatch);
        }
        if let ImageProvenance::Captured { source, recipe, .. } = image.image_facts().provenance {
            if source != facts.source || recipe != facts.recipe {
                return Err(CompilerResultError::SemanticImageMismatch);
            }
        } else {
            return Err(CompilerResultError::SemanticImageMismatch);
        }
        artifacts.push(CheckedRemoteCompilerArtifact {
            artifact_ordinal: u32::try_from(artifacts.len())?,
            source: facts.source,
            recipe: facts.recipe,
            fragment_identity: facts.fragment,
            fragment_object_id: fragment_object.id(),
            semantic_image_facts: image_facts,
            semantic_generation: GenerationId::from_canonical_bytes(image_bytes),
            semantic_image_object_id: image_object.id(),
        });
        semantic_members.push(
            *envelope
                .members
                .get(member_ordinal)
                .ok_or(CompilerResultError::MemberCount)?,
        );
        semantic_members.push(
            *envelope
                .members
                .get(member_ordinal + 1)
                .ok_or(CompilerResultError::MemberCount)?,
        );
        member_ordinal += 2;
    }
    semantic_members.insert(0, *manifest_member);

    if observed_source_identities != source_identities {
        return Err(CompilerResultError::InputClosureMismatch);
    }

    let rebuilt = verify_semantic_generation_members(&semantic_members)?;
    if rebuilt != envelope.generation_facts()
        || envelope.binding_facts.generation != rebuilt
        || envelope.binding_facts.manifest != manifest.identity
    {
        return Err(CompilerResultError::GenerationMismatch);
    }

    let auxiliary = Vec::new();
    let versioned_plane_members = envelope.versioned_plane_members();
    let versioned_plane_artifacts = validate_versioned_plane_output(
        store,
        closure,
        envelope.members(),
        versioned_plane_members,
        &artifacts,
        plane_input,
        maximum_member_bytes,
    )?;
    Ok(CheckedRemoteCompilerOutput {
        closure_id,
        generation: rebuilt,
        manifest_facts: *manifest,
        binding_facts: envelope.binding_facts,
        manifest_bytes,
        binding_bytes: envelope.binding_bytes.clone(),
        artifacts: artifacts.into_boxed_slice(),
        members: semantic_members.into_boxed_slice(),
        auxiliary_planes: auxiliary.into_boxed_slice(),
        versioned_plane_members: versioned_plane_members.to_vec().into_boxed_slice(),
        versioned_plane_artifacts,
    })
}

fn admit_output_source_identity(
    expected: &BTreeMap<([u8; 32], u32), usize>,
    observed: &mut BTreeMap<([u8; 32], u32), usize>,
    source: ([u8; 32], u32),
) -> Result<(), CompilerResultError> {
    let expected_count = expected
        .get(&source)
        .copied()
        .ok_or(CompilerResultError::InputClosureMismatch)?;
    let observed_count = observed.entry(source).or_insert(0);
    *observed_count = observed_count
        .checked_add(1)
        .filter(|count| *count <= expected_count)
        .ok_or(CompilerResultError::InputClosureMismatch)?;
    if expected_count == 0 {
        return Err(CompilerResultError::InputClosureMismatch);
    }
    Ok(())
}

fn validate_versioned_plane_output(
    store: &FileStore,
    closure: &backend_store::DurableManifest,
    generation_members: &[CompilerResultMemberV1],
    plane_members: &[CompilerResultMemberV1],
    artifacts: &[CheckedRemoteCompilerArtifact],
    expected: VersionedPlaneInputBinding,
    maximum_member_bytes: u64,
) -> Result<Box<[CheckedRemoteCompilerPlaneArtifact]>, CompilerResultError> {
    if artifacts.is_empty() || (generation_members.len() - 1) / 2 != artifacts.len() {
        return Err(CompilerResultError::VersionedPlaneTopology);
    }
    super::result_wire::validate_versioned_plane_topology(generation_members, plane_members)?;

    let mut checked_artifacts = Vec::new();
    checked_artifacts
        .try_reserve_exact(artifacts.len())
        .map_err(|_| CompilerResultError::Allocation)?;
    let mut member_cursor = 0_usize;
    for (ordinal, artifact) in artifacts.iter().enumerate() {
        let manifest_member = *plane_members
            .get(member_cursor)
            .ok_or(CompilerResultError::VersionedPlaneTopology)?;
        let manifest_object =
            checked_result_object(store, closure, manifest_member, maximum_member_bytes)?;
        let manifest_bytes = manifest_object.bytes();
        if manifest_member.reference
            != semantic_object_reference(SchemaId::Object, 0xc005, manifest_bytes)?
        {
            return Err(CompilerResultError::VersionedPlaneManifest);
        }
        let manifest = SemanticPlaneManifest::decode(manifest_bytes)
            .map_err(|_| CompilerResultError::VersionedPlaneManifest)?;
        if manifest
            .encode()
            .map_err(|_| CompilerResultError::VersionedPlaneManifest)?
            != manifest_bytes
            || manifest.semantic_generation() != artifact.semantic_generation()
        {
            return Err(CompilerResultError::VersionedPlaneManifest);
        }
        let build = manifest.build();
        let input = manifest.input();
        if build.package() != &expected.package
            || build.target() != &expected.target
            || build.profile() != expected.profile
            || build.stage() != expected.stage
            || build.recipe() != &expected.recipe
            || build.toolchain() != &expected.toolchain
            || build.environment() != &expected.environment
            || build.target_platform() != &expected.target_platform
            || input.input_root() != &expected.input_root
            || input.read_manifest_root().as_bytes() != &expected.read_manifest
            || input.coverage().state() != Coverage::Partial
        {
            return Err(CompilerResultError::VersionedPlaneInputMismatch);
        }
        if manifest
            .plane(SemanticPlaneKind::Ir(SemanticIrPlane::Core))
            .is_none_or(|plane| plane.segments().is_empty())
        {
            return Err(CompilerResultError::VersionedPlaneTopology);
        }

        member_cursor += 1;
        let mut planes = Vec::new();
        planes
            .try_reserve_exact(manifest.planes().len())
            .map_err(|_| CompilerResultError::Allocation)?;
        let mut artifact_segment_ordinal = 0_u64;
        for plane in manifest.planes() {
            let mut segments = Vec::new();
            segments
                .try_reserve_exact(plane.segments().len())
                .map_err(|_| CompilerResultError::Allocation)?;
            for descriptor in plane.segments() {
                let member = *plane_members
                    .get(member_cursor)
                    .ok_or(CompilerResultError::VersionedPlaneTopology)?;
                if member.role() != CompilerResultMemberRole::VersionedPlaneSegment
                    || member.key() != artifact_segment_ordinal
                    || member.parent() != Some(u64::try_from(ordinal)?)
                {
                    return Err(CompilerResultError::VersionedPlaneTopology);
                }
                let object = checked_result_object(store, closure, member, maximum_member_bytes)?;
                let payload = object.bytes();
                if payload.len() > backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES
                    || member.reference()
                        != semantic_object_reference(SchemaId::Object, 0xc004, payload)?
                    || u64::try_from(payload.len())? != descriptor.byte_length()
                {
                    return Err(CompilerResultError::VersionedPlaneTopology);
                }
                let segment_id = descriptor
                    .admit(plane.kind(), payload)
                    .map_err(|_| CompilerResultError::VersionedPlaneTopology)?;
                if descriptor.id_claim().as_bytes() != segment_id.as_bytes() {
                    return Err(CompilerResultError::VersionedPlaneTopology);
                }
                segments.push(CheckedRemoteCompilerPlaneSegment {
                    first_key: *descriptor.first_key(),
                    last_key: *descriptor.last_key(),
                    row_count: descriptor.row_count(),
                    byte_length: descriptor.byte_length(),
                    segment_id,
                    object_id: object.id(),
                    member,
                });
                artifact_segment_ordinal = artifact_segment_ordinal
                    .checked_add(1)
                    .ok_or(CompilerResultError::VersionedPlaneTopology)?;
                member_cursor += 1;
            }
            planes.push(CheckedRemoteCompilerPlaneDescriptor {
                kind: plane.kind(),
                root: plane.root(),
                coverage: plane.coverage(),
                segments: segments.into_boxed_slice(),
            });
        }
        checked_artifacts.push(CheckedRemoteCompilerPlaneArtifact {
            artifact_ordinal: u32::try_from(ordinal)?,
            semantic_image_object_id: artifact.semantic_image_object_id(),
            semantic_generation: manifest.semantic_generation(),
            manifest_root: manifest.root(),
            manifest_bytes: manifest_bytes.to_vec().into_boxed_slice(),
            manifest_object_id: manifest_object.id(),
            manifest_member,
            planes: planes.into_boxed_slice(),
        });
    }
    if member_cursor != plane_members.len() {
        return Err(CompilerResultError::VersionedPlaneTopology);
    }
    Ok(checked_artifacts.into_boxed_slice())
}

fn recipe_matches_policy(
    recipe: CompileRecipeFact,
    source: SourceIdentity,
    policy: CompilerOutputRecipePolicy,
) -> bool {
    let expected = match policy {
        CompilerOutputRecipePolicy::InvocationV2(invocation) => {
            invocation.derive_source_recipe(source.identity)
        }
    };
    expected == recipe
}

fn semantic_object_reference(
    schema: SchemaId,
    kind: u16,
    bytes: &[u8],
) -> Result<ObjectRef<ObjectDomain>, CompilerResultError> {
    Ok(ObjectRef {
        content: backend_version::ContentId::<ObjectDomain>::from_canonical_bytes(bytes),
        length: ObjectLength::from(u64::try_from(bytes.len())?),
        schema,
        kind: ObjectKind::from(kind),
    })
}

/// Rebuilds semantic generation identity from its bounded canonical descriptors.
///
/// The caller has already admitted every member's typed wrapper and recomputed
/// each descriptor from one borrowed payload at a time. Reconstructing the root
/// and dependency set from those facts avoids concatenating the complete IR and
/// image payloads in memory.
fn verify_semantic_generation_members(
    members: &[CompilerResultMemberV1],
) -> Result<VerifiedGenerationFacts, CompilerResultError> {
    if members.len() < 3 || members.len() % 2 == 0 {
        return Err(CompilerResultError::MemberCount);
    }
    let mut builder = GenerationRootBuilder::with_capacity(members.len())
        .map_err(|_| CompilerResultError::Allocation)?;
    for (ordinal, member) in members.iter().copied().enumerate() {
        let valid = if ordinal == 0 {
            member.role() == CompilerResultMemberRole::Manifest
                && member.key() == RESULT_MANIFEST_ENTRY_KEY
                && member.parent().is_none()
        } else if ordinal % 2 == 1 {
            member.role() == CompilerResultMemberRole::Fragment
                && member.key()
                    == u64::try_from(ordinal).map_err(|_| CompilerResultError::MemberCount)?
                && member.parent() == Some(RESULT_MANIFEST_ENTRY_KEY)
        } else {
            member.role() == CompilerResultMemberRole::SemanticImage
                && member.key()
                    == u64::try_from(ordinal).map_err(|_| CompilerResultError::MemberCount)?
                && member.parent() == Some(RESULT_MANIFEST_ENTRY_KEY)
        };
        if !valid {
            return Err(CompilerResultError::MemberTopology);
        }
        builder
            .try_push(RootEntry {
                key: member.key().into(),
                parent: member.parent().map(Into::into),
                object: member.reference(),
            })
            .map_err(|_| CompilerResultError::GenerationMismatch)?;
    }
    let root = builder
        .finish()
        .map_err(|_| CompilerResultError::GenerationMismatch)?;
    let prepared = PreparedLocality::prepare(&root, &[])
        .map_err(|_| CompilerResultError::GenerationMismatch)?;
    let mut locality_bytes = Vec::new();
    locality_bytes
        .try_reserve_exact(usize::from(prepared.required_bytes))
        .map_err(|_| CompilerResultError::Allocation)?;
    locality_bytes.resize(usize::from(prepared.required_bytes), 0);
    let locality = prepared
        .write(&mut locality_bytes)
        .map_err(|_| CompilerResultError::GenerationMismatch)?;
    let view = GenerationView::new(&root, &locality)
        .map_err(|_| CompilerResultError::GenerationMismatch)?;
    let mut closure_scratch =
        ClosureScratch::new(root.len()).map_err(|_| CompilerResultError::Allocation)?;
    let mut plan_scratch = PlanScratch::new(SelectedCount::from(root.entry_count))
        .map_err(|_| CompilerResultError::Allocation)?;
    let plan = plan(
        demand(&view, Projection::CompleteGeneration),
        &mut closure_scratch,
        &mut plan_scratch,
        |_| true,
    )
    .map_err(|_| CompilerResultError::GenerationMismatch)?;
    if !plan.is_complete() {
        return Err(CompilerResultError::GenerationMismatch);
    }
    Ok(VerifiedGenerationFacts {
        pinned_root: plan.pinned_root,
        dep_set: plan.dep_set,
    })
}

fn checked_result_object(
    store: &FileStore,
    closure: &backend_store::DurableManifest,
    member: CompilerResultMemberV1,
    maximum_member_bytes: u64,
) -> Result<TypedObject, CompilerResultError> {
    let admitted = closure
        .admit_claim(UntrustedObjectId::from_bytes(member.object_id))?
        .ok_or(CompilerResultError::ClosureAdmission)?;
    stream_checked_result_member(
        store,
        member,
        admitted,
        maximum_member_bytes,
        &mut io::sink(),
    )?;
    let object = closure
        .get(admitted)?
        .ok_or(CompilerResultError::ClosureAdmission)?;
    if object.id().as_bytes() != &member.object_id {
        return Err(CompilerResultError::ClosureAdmission);
    }
    Ok(object)
}

fn stream_checked_result_member<W: Write + ?Sized>(
    store: &FileStore,
    member: CompilerResultMemberV1,
    object_id: ObjectId,
    maximum_payload_bytes: u64,
    output: &mut W,
) -> Result<backend_store::VerifiedObjectEnvelope, CompilerResultError> {
    let schema = SchemaIdentity::new(
        RESULT_OUTPUT_SCHEMA_DOMAIN,
        RESULT_OUTPUT_SCHEMA_TYPE,
        RESULT_OUTPUT_SCHEMA_VERSION,
    );
    let expected_key = super::result_wire::compiler_result_member_key(member)?;
    let mut writer = CheckedResultPayloadWriter::new(output, *member.reference.length, schema)?;
    let verified = store.write_verified_object_payload(
        UntrustedObjectId::from_bytes(*object_id.as_bytes()),
        maximum_payload_bytes,
        &mut writer,
    )?;
    let (content, version) = writer.finish()?;
    if verified.id() != object_id
        || verified.schema() != schema
        || verified.key() != &expected_key
        || verified.version() != &version
        || verified.payload_len() != *member.reference.length
        || content != member.reference.content
    {
        return Err(CompilerResultError::OutputObjectMismatch);
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{
        LocalCompiler, LocalCompilerConfig, LocalCompilerControl, LocalCompilerScratch,
        LocalCompilerTimeout, LocalToolchainSet, PackageSource, PackageSourceSet,
    };
    use crate::compiler_cluster_transport::CompilerInvocationRecipeV2;
    use crate::driver::{ResolvedToolchain, ToolchainSelection};
    use crate::publication::binding::{COMPILATION_BINDING_BYTES, CompilationBindingView};
    use crate::publication::manifest::{
        COMPILATION_MANIFEST_HEADER_BYTES, COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES,
        CanonicalSemanticCompilation, CompilationManifestView, SemanticImageRegion,
        StoredFragmentFacts,
    };
    use backend_cluster_transport::ControlResultReceipt;
    use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
    use backend_semantic::ir::{
        EmbeddingNormalization, EmbeddingPlaneIdentity, FragmentRangeManifest, FragmentView,
        SectionKind, SemanticBuildIdentity, SemanticImageView, SemanticInputWitness, SemanticPlane,
        SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneSegment,
    };
    use backend_semantic::vocabulary::{CStandard, LanguageProfile, NativeTool, PackageUrl, Stage};
    use backend_store::{ClosureManifest, FileStore, TypedObject};
    use std::{
        num::NonZeroUsize,
        path::PathBuf,
        process::Command,
        sync::atomic::AtomicBool,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn multi_source_result_uses_per_source_recipes_and_rejects_wrong_tool()
    -> Result<(), Box<dyn std::error::Error>> {
        let clang = find_clang().ok_or("clang is required for the result recipe regression")?;
        let version = Command::new(&clang).arg("--version").output()?;
        if !version.status.success() {
            return Err("clang version probe failed".into());
        }
        let root = TestDirectory::new()?;
        let package_root = root.child("package");
        let artifacts = root.child("artifacts");
        let journal = root.child("journal");
        let native_work = root.child("native-work");
        std::fs::create_dir_all(package_root.join("src"))?;
        std::fs::create_dir(&native_work)?;

        let source_text = "int recipe_fixture(void) { return 7; }\n";
        let second_source_text = "int recipe_fixture(void) { return 8; }\n";
        std::fs::write(package_root.join("src/recipe.c"), source_text)?;
        std::fs::write(package_root.join("src/second.c"), second_source_text)?;
        let toolchains = [ToolchainSelection::ResolvedNative(
            ResolvedToolchain::from_version(NativeTool::Clang, &clang, &version.stdout)?,
        )];
        let selected = LocalToolchainSet::validate(&toolchains)?;
        let cancelled = AtomicBool::new(false);
        let mut scratch = LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
        )?;
        let mut compiler = LocalCompiler::create(
            LocalCompilerConfig {
                toolchains: selected,
                artifact_directory: &artifacts,
                journal_directory: &journal,
                native_work_directory: &native_work,
                control: LocalCompilerControl {
                    timeout: LocalCompilerTimeout::new(std::time::Duration::from_secs(30))?,
                    cancelled: &cancelled,
                },
            },
            backend_store::journal::PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
            &mut scratch,
        )?;
        let package_url = PackageUrl::try_from("pkg:generic/recipe-fixture@1.0.0".to_owned())
            .map_err(|error| {
                std::io::Error::other(format!("parse recipe fixture package URL: {error:?}"))
            })?;
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(41),
                profile: LanguageProfile::C(CStandard::C23),
                stage: Stage::LowerIr,
            },
            package_url.clone(),
        )
        .map_err(|error| {
            std::io::Error::other(format!("create recipe fixture request: {error:?}"))
        })?;
        let sources = [
            PackageSource::new("src/recipe.c", source_text)?,
            PackageSource::new("src/second.c", second_source_text)?,
        ];
        let package = PackageSourceSet::new(&request, &package_root, &sources)?;
        let staged = compiler.compile_package_sources_staged(package, &mut |_| {})?;

        let mut output_objects = Vec::with_capacity(staged.output_object_count());
        let mut members = Vec::with_capacity(staged.output_object_count());
        for ordinal in 0..staged.output_object_count() {
            let output = staged
                .output_object(ordinal)
                .ok_or("staged output is missing")?;
            let output_claim =
                crate::application::compiler_result_output_claim(output.claim(), output.bytes())?;
            let object =
                crate::application::compiler_result_output_object(output.claim(), output.bytes())?;
            members.push(output_claim.member(object.id()));
            output_objects.push(object);
        }
        let scope =
            backend_cluster_transport::AssignmentScope::new([0x11; 16], [0x22; 16], 1, [0x33; 32])?;
        let first = *staged
            .artifacts()
            .first()
            .ok_or("staged artifact is absent")?;
        let recipe = first.recipe();
        let environment = [0x77; 32];
        let target_platform = [0x88; 32];
        let input_root = [0x99; 32];
        let read_manifest = [0xaa; 32];
        let invocation = CompilerInvocationRecipeV2::new(
            recipe.profile,
            recipe.stage,
            recipe.tool,
            recipe.toolchain,
            environment,
            target_platform,
            [0xbb; 32],
        )?;
        let build = SemanticBuildIdentity::new(
            *package_url.identity.as_ref(),
            *package_url.identity.as_ref(),
            recipe.profile,
            recipe.stage,
            *invocation.identity().as_ref(),
            *recipe.toolchain.as_ref(),
            environment,
            target_platform,
        );
        let semantic_input = SemanticInputWitness::claimed_state(
            input_root,
            backend_version::ScopeRoot::from_bytes(read_manifest),
            Coverage::Partial,
        );
        let plane_input = VersionedPlaneInputBinding {
            package: *build.package(),
            target: *build.target(),
            profile: build.profile(),
            stage: build.stage(),
            recipe: *build.recipe(),
            toolchain: *build.toolchain(),
            environment: *build.environment(),
            target_platform: *build.target_platform(),
            input_root,
            read_manifest,
        };
        let (versioned_plane_members, versioned_plane_objects) =
            test_versioned_plane_output(&staged, build, semantic_input, None)?;
        let envelope_object = super::super::result_wire::result_envelope_object_from_parts(
            scope,
            [0x44; 32],
            [0x55; 32],
            staged.generation_facts(),
            staged.manifest_facts().identity,
            staged.binding_bytes(),
            &members,
            &versioned_plane_members,
        )?;
        let generation_objects = output_objects;
        let closure_manifest = result_closure(
            &generation_objects,
            &versioned_plane_objects,
            envelope_object.clone(),
        )?;
        let closure_id = closure_manifest.id();
        let payload_bytes = closure_manifest
            .objects()
            .iter()
            .try_fold(0_u64, |total, object| {
                total.checked_add(u64::try_from(object.bytes().len()).ok()?)
            })
            .ok_or("result closure payload length overflow")?;
        let object_count = u32::try_from(closure_manifest.objects().len())?;
        let packless_receipt = ControlResultReceipt {
            scope,
            target_root: *staged.generation_facts().pinned_root.as_ref(),
            pack_id: None,
            closure_id: *closure_id.as_bytes(),
            object_count,
            payload_bytes,
            result_grant_pages: 1,
        };
        let packed_envelope = super::super::result_wire::result_envelope_object_from_parts(
            scope,
            [0x44; 32],
            [0x55; 32],
            staged.generation_facts(),
            staged.manifest_facts().identity,
            staged.binding_bytes(),
            &members,
            &versioned_plane_members,
        )?;
        let packed_layout_closure = result_closure(
            &generation_objects,
            &versioned_plane_objects,
            packed_envelope.clone(),
        )?;
        let packed_receipt = ControlResultReceipt {
            pack_id: Some([0xbb; 32]),
            closure_id: *packed_layout_closure.id().as_bytes(),
            ..packless_receipt
        };
        assert_ne!(packless_receipt.pack_id, packed_receipt.pack_id);
        assert_eq!(envelope_object.id(), packed_envelope.id());
        assert_eq!(closure_id, packed_layout_closure.id());

        let store = FileStore::open(root.child("result-store"), 512 * 1024 * 1024)
            .map_err(|error| std::io::Error::other(format!("open result store: {error:?}")))?;
        let closure_id = store
            .write_closure(&closure_manifest)
            .map_err(|error| std::io::Error::other(format!("store result closure: {error:?}")))?;
        let closure = store
            .open_closure(closure_id)
            .map_err(|error| std::io::Error::other(format!("open result closure: {error:?}")))?;
        let checked_index = crate::application::reopen_compiler_result_envelope(
            &store,
            closure_id,
            object_count,
            payload_bytes,
            scope,
            [0x44; 32],
            [0x55; 32],
            *staged.generation_facts().pinned_root.as_ref(),
            payload_bytes,
        )?;
        assert_eq!(checked_index.closure_id(), closure_id);
        assert_eq!(checked_index.object_count(), object_count);
        assert_eq!(checked_index.payload_bytes(), payload_bytes);
        assert_eq!(checked_index.envelope_object_id(), envelope_object.id());
        let envelope = CompilerResultEnvelopeV1::from_typed_object(&envelope_object)?;
        let mut source_identities = BTreeMap::new();
        for artifact in staged.artifacts() {
            let source = (
                *artifact.source().identity.as_ref(),
                artifact.source().byte_len,
            );
            *source_identities.entry(source).or_insert(0) += 1;
        }
        assert_eq!(source_identities.len(), 2);
        assert_ne!(
            staged.artifacts()[0].recipe().identity,
            staged.artifacts()[1].recipe().identity,
            "distinct source bytes need distinct per-source recipe identities"
        );
        let admitted_output = validate_semantic_result(
            &store,
            &closure,
            &envelope,
            CompilerOutputRecipePolicy::InvocationV2(invocation),
            plane_input,
            source_identities.clone(),
            closure_id,
            payload_bytes,
        )
        .map_err(|error| std::io::Error::other(format!("admit baseline result: {error:?}")))?;
        let checked_artifact = admitted_output
            .artifacts()
            .first()
            .ok_or("checked result omitted its first artifact")?;
        let mut streamed_image = Vec::new();
        admitted_output.write_member_payload(
            &store,
            checked_artifact.semantic_image_object_id(),
            payload_bytes,
            &mut streamed_image,
        )?;
        assert_eq!(
            u32::try_from(streamed_image.len())?,
            checked_artifact.semantic_image_facts().byte_length
        );
        assert_eq!(
            SemanticImageIdentity::from_encoded_bytes(&streamed_image),
            checked_artifact.semantic_image_facts().identity
        );
        assert_eq!(admitted_output.versioned_plane_artifacts().len(), 2);
        assert_eq!(
            admitted_output.versioned_plane_artifacts()[0].artifact_ordinal(),
            0
        );
        assert_eq!(
            admitted_output.versioned_plane_artifacts()[0].semantic_generation(),
            checked_artifact.semantic_generation()
        );
        assert_eq!(
            admitted_output.versioned_plane_artifacts()[0]
                .planes()
                .iter()
                .map(|plane| plane.segments().len())
                .sum::<usize>(),
            1
        );
        let wrong_platform_input = VersionedPlaneInputBinding {
            target_platform: [0xcc; 32],
            ..plane_input
        };
        assert!(matches!(
            validate_semantic_result(
                &store,
                &closure,
                &envelope,
                CompilerOutputRecipePolicy::InvocationV2(invocation),
                wrong_platform_input,
                source_identities.clone(),
                closure_id,
                payload_bytes,
            ),
            Err(CompilerResultError::VersionedPlaneInputMismatch)
        ));

        let generic_plane_bytes = b"untyped result plane";
        let generic_plane_reference =
            semantic_object_reference(SchemaId::Object, 0xc006, generic_plane_bytes)?;
        let generic_plane_claim = crate::application::compiler_result_auxiliary_output_claim(
            generic_plane_reference,
            0,
            generic_plane_bytes,
        )?;
        let generic_plane_object = crate::application::compiler_result_auxiliary_output_object(
            generic_plane_reference,
            0,
            generic_plane_bytes,
        )?;
        let generic_plane_member = generic_plane_claim.member(generic_plane_object.id());
        let generic_plane_envelope =
            super::super::result_wire::result_envelope_object_from_parts_with_auxiliary(
                scope,
                [0x44; 32],
                [0x55; 32],
                staged.generation_facts(),
                staged.manifest_facts().identity,
                staged.binding_bytes(),
                &members,
                &[generic_plane_member],
                &versioned_plane_members,
            )?;
        let generic_plane_closure_manifest = result_closure_with_auxiliary(
            &generation_objects,
            &[generic_plane_object],
            &versioned_plane_objects,
            generic_plane_envelope.clone(),
        )?;
        let generic_plane_closure_id = store
            .write_closure(&generic_plane_closure_manifest)
            .map_err(|error| {
                std::io::Error::other(format!("store generic-plane result: {error:?}"))
            })?;
        let generic_plane_closure =
            store
                .open_closure(generic_plane_closure_id)
                .map_err(|error| {
                    std::io::Error::other(format!("open generic-plane result: {error:?}"))
                })?;
        let generic_plane_wire =
            CompilerResultEnvelopeV1::from_typed_object(&generic_plane_envelope)?;
        assert!(matches!(
            validate_semantic_result(
                &store,
                &generic_plane_closure,
                &generic_plane_wire,
                CompilerOutputRecipePolicy::InvocationV2(invocation),
                plane_input,
                source_identities.clone(),
                generic_plane_closure_id,
                payload_bytes,
            ),
            Err(CompilerResultError::UnsupportedAuxiliaryOutput)
        ));

        // One independent embedding segment on artifact zero gives the ABI an asymmetric
        // c004 inventory (two segments vs. one) while keeping each c005 tied to its image.
        let (embedding_plane_members, embedding_plane_objects) =
            test_versioned_plane_output(&staged, build, semantic_input, Some(0))?;
        let embedding_envelope_object =
            super::super::result_wire::result_envelope_object_from_parts(
                scope,
                [0x44; 32],
                [0x55; 32],
                staged.generation_facts(),
                staged.manifest_facts().identity,
                staged.binding_bytes(),
                &members,
                &embedding_plane_members,
            )?;
        let embedding_closure_manifest = result_closure(
            &generation_objects,
            &embedding_plane_objects,
            embedding_envelope_object.clone(),
        )?;
        let embedding_closure_id =
            store
                .write_closure(&embedding_closure_manifest)
                .map_err(|error| {
                    std::io::Error::other(format!("store embedding result closure: {error:?}"))
                })?;
        let embedding_closure = store.open_closure(embedding_closure_id).map_err(|error| {
            std::io::Error::other(format!("open embedding result closure: {error:?}"))
        })?;
        let embedding_envelope =
            CompilerResultEnvelopeV1::from_typed_object(&embedding_envelope_object)?;
        let embedding_output = validate_semantic_result(
            &store,
            &embedding_closure,
            &embedding_envelope,
            CompilerOutputRecipePolicy::InvocationV2(invocation),
            plane_input,
            source_identities.clone(),
            embedding_closure_id,
            payload_bytes,
        )
        .map_err(|error| std::io::Error::other(format!("admit embedding result: {error:?}")))?;
        let first_plane_artifact = &embedding_output.versioned_plane_artifacts()[0];
        assert_eq!(first_plane_artifact.planes().len(), 2);
        assert_eq!(
            first_plane_artifact.manifest_bytes(),
            embedding_plane_objects[0].bytes()
        );
        assert_eq!(
            first_plane_artifact.manifest_object_id(),
            embedding_plane_objects[0].id()
        );
        assert_eq!(
            first_plane_artifact.semantic_image_object_id(),
            embedding_output.artifacts()[0].semantic_image_object_id()
        );
        assert!(matches!(
            first_plane_artifact.planes()[1].kind(),
            SemanticPlaneKind::Embeddings(_)
        ));
        assert_eq!(first_plane_artifact.planes()[1].segments().len(), 1);
        assert_eq!(
            first_plane_artifact.planes()[1].segments()[0].object_id(),
            embedding_plane_objects[2].id()
        );
        assert_eq!(
            embedding_output.versioned_plane_artifacts()[1]
                .planes()
                .len(),
            1
        );

        // A well-formed c004 wrapper copied from artifact one cannot satisfy artifact zero's
        // exact descriptor, even after its result key and parent are rebuilt for artifact zero.
        let (cross_segment_member, cross_segment_object) = make_result_member(
            CompilerResultMemberRole::VersionedPlaneSegment,
            SchemaId::Object,
            0xc004,
            0,
            Some(0),
            embedding_plane_objects[4].bytes(),
        )?;
        assert_eq!(
            embedding_plane_objects[1].bytes().len(),
            embedding_plane_objects[4].bytes().len(),
            "cross-artifact mutation should preserve the segment length"
        );
        assert_ne!(
            embedding_plane_objects[1].bytes(),
            embedding_plane_objects[4].bytes(),
            "cross-artifact mutation must substitute distinct segment content"
        );
        let mut cross_payload_members = embedding_plane_members.clone();
        cross_payload_members[1] = cross_segment_member;
        let cross_payload_envelope = super::super::result_wire::result_envelope_object_from_parts(
            scope,
            [0x44; 32],
            [0x55; 32],
            staged.generation_facts(),
            staged.manifest_facts().identity,
            staged.binding_bytes(),
            &members,
            &cross_payload_members,
        )?;
        let mut cross_payload_objects = embedding_plane_objects.clone();
        cross_payload_objects[1] = cross_segment_object;
        let cross_payload_manifest = result_closure(
            &generation_objects,
            &cross_payload_objects,
            cross_payload_envelope.clone(),
        )?;
        let cross_payload_closure_id =
            store
                .write_closure(&cross_payload_manifest)
                .map_err(|error| {
                    std::io::Error::other(format!("store cross-artifact plane result: {error:?}"))
                })?;
        let cross_payload_closure =
            store
                .open_closure(cross_payload_closure_id)
                .map_err(|error| {
                    std::io::Error::other(format!("open cross-artifact plane result: {error:?}"))
                })?;
        let cross_payload_wire =
            CompilerResultEnvelopeV1::from_typed_object(&cross_payload_envelope)?;
        assert!(matches!(
            validate_semantic_result(
                &store,
                &cross_payload_closure,
                &cross_payload_wire,
                CompilerOutputRecipePolicy::InvocationV2(invocation),
                plane_input,
                source_identities.clone(),
                cross_payload_closure_id,
                payload_bytes,
            ),
            Err(CompilerResultError::VersionedPlaneTopology)
        ));

        let mut cross_artifact_members = embedding_plane_members.clone();
        cross_artifact_members[0].parent = Some(cross_artifact_members[0].parent.unwrap() + 2);
        assert!(matches!(
            super::super::result_wire::result_envelope_object_from_parts(
                scope,
                [0x44; 32],
                [0x55; 32],
                staged.generation_facts(),
                staged.manifest_facts().identity,
                staged.binding_bytes(),
                &members,
                &cross_artifact_members,
            ),
            Err(CompilerResultError::VersionedPlaneTopology)
        ));
        let mut missing_plane_members = embedding_plane_members.clone();
        let removed_segment = missing_plane_members.remove(2);
        assert_eq!(
            removed_segment.role(),
            CompilerResultMemberRole::VersionedPlaneSegment
        );
        let missing_plane_envelope = super::super::result_wire::result_envelope_object_from_parts(
            scope,
            [0x44; 32],
            [0x55; 32],
            staged.generation_facts(),
            staged.manifest_facts().identity,
            staged.binding_bytes(),
            &members,
            &missing_plane_members,
        )?;
        let mut missing_plane_objects = embedding_plane_objects.clone();
        missing_plane_objects.remove(2);
        let missing_plane_closure_manifest = result_closure(
            &generation_objects,
            &missing_plane_objects,
            missing_plane_envelope.clone(),
        )?;
        let missing_plane_closure_id = store
            .write_closure(&missing_plane_closure_manifest)
            .map_err(|error| {
                std::io::Error::other(format!("store incomplete plane closure: {error:?}"))
            })?;
        let missing_plane_closure =
            store
                .open_closure(missing_plane_closure_id)
                .map_err(|error| {
                    std::io::Error::other(format!("open incomplete plane closure: {error:?}"))
                })?;
        let missing_plane_wire =
            CompilerResultEnvelopeV1::from_typed_object(&missing_plane_envelope)?;
        assert!(matches!(
            validate_semantic_result(
                &store,
                &missing_plane_closure,
                &missing_plane_wire,
                CompilerOutputRecipePolicy::InvocationV2(invocation),
                plane_input,
                source_identities.clone(),
                missing_plane_closure_id,
                payload_bytes,
            ),
            Err(CompilerResultError::VersionedPlaneTopology)
        ));

        let mut one_source_missing = source_identities.clone();
        one_source_missing.remove(source_identities.keys().next().ok_or("source absent")?);
        assert!(matches!(
            validate_semantic_result(
                &store,
                &closure,
                &envelope,
                CompilerOutputRecipePolicy::InvocationV2(invocation),
                plane_input,
                one_source_missing,
                closure_id,
                payload_bytes,
            ),
            Err(CompilerResultError::InputClosureMismatch)
        ));

        // The alternate tool's per-source recipe is internally self-consistent,
        // but the portable invocation remains bound to Clang.
        let wrong_tool = if recipe.tool == NativeTool::Clang {
            NativeTool::Rustc
        } else {
            NativeTool::Clang
        };
        let alternate_recipe = CompileRecipeFact::derive(
            recipe.profile,
            recipe.stage,
            wrong_tool,
            first.source().identity,
            recipe.toolchain,
        );
        assert_ne!(alternate_recipe.identity, recipe.identity);
        let (mutated_objects, mutated_envelope_object) = reencode_result_with_recipe_mutation(
            &staged,
            scope,
            [0x44; 32],
            [0x55; 32],
            0,
            wrong_tool,
            &versioned_plane_members,
        )?;
        let mutated_closure_manifest = result_closure(
            &mutated_objects,
            &versioned_plane_objects,
            mutated_envelope_object.clone(),
        )?;
        let mutated_closure_id =
            store
                .write_closure(&mutated_closure_manifest)
                .map_err(|error| {
                    std::io::Error::other(format!("store mutated result closure: {error:?}"))
                })?;
        let mutated_closure = store.open_closure(mutated_closure_id).map_err(|error| {
            std::io::Error::other(format!("open mutated result closure: {error:?}"))
        })?;
        let mutated_envelope =
            CompilerResultEnvelopeV1::from_typed_object(&mutated_envelope_object)?;
        assert!(matches!(
            validate_semantic_result(
                &store,
                &mutated_closure,
                &mutated_envelope,
                CompilerOutputRecipePolicy::InvocationV2(invocation),
                plane_input,
                source_identities,
                mutated_closure_id,
                payload_bytes,
            ),
            Err(CompilerResultError::FragmentMismatch)
        ));
        Ok(())
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Result<Self, std::io::Error> {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(std::io::Error::other)?
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-compiler-result-recipe-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn child(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn find_clang() -> Option<PathBuf> {
        [
            "/usr/bin/clang",
            "/usr/local/bin/clang",
            "/opt/homebrew/bin/clang",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .or_else(|| {
            Command::new("which")
                .arg("clang")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| {
                    let path = String::from_utf8(output.stdout).ok()?;
                    Some(PathBuf::from(path.trim()))
                })
        })
    }

    fn result_closure(
        generation_objects: &[TypedObject],
        versioned_plane_objects: &[TypedObject],
        envelope: TypedObject,
    ) -> Result<ClosureManifest, Box<dyn std::error::Error>> {
        result_closure_with_auxiliary(generation_objects, &[], versioned_plane_objects, envelope)
    }

    fn result_closure_with_auxiliary(
        generation_objects: &[TypedObject],
        auxiliary_objects: &[TypedObject],
        versioned_plane_objects: &[TypedObject],
        envelope: TypedObject,
    ) -> Result<ClosureManifest, Box<dyn std::error::Error>> {
        let mut objects = generation_objects.to_vec();
        objects.extend_from_slice(auxiliary_objects);
        objects.extend_from_slice(versioned_plane_objects);
        objects.push(envelope);
        objects.sort_by(|left, right| {
            (left.schema(), *left.key(), *left.version()).cmp(&(
                right.schema(),
                *right.key(),
                *right.version(),
            ))
        });
        ClosureManifest::new(objects)
            .map_err(|error| std::io::Error::other(format!("build result closure: {error:?}")))
            .map_err(Into::into)
    }

    fn reencode_result_with_recipe_mutation(
        staged: &crate::application::StagedSemanticPackage,
        scope: backend_cluster_transport::AssignmentScope,
        input_closure_id: [u8; 32],
        input_manifest_object_id: [u8; 32],
        artifact_ordinal: usize,
        alternate_tool: NativeTool,
        versioned_plane_members: &[CompilerResultMemberV1],
    ) -> Result<(Vec<TypedObject>, TypedObject), Box<dyn std::error::Error>> {
        let count = staged.artifacts().len();
        if artifact_ordinal >= count || staged.output_object_count() != count * 2 + 1 {
            return Err("staged result member inventory is inconsistent".into());
        }
        let source_manifest = staged
            .output_object(0)
            .ok_or("staged semantic manifest is absent")?;
        let mut source_facts = vec![None; count];
        let _source_manifest_view =
            CompilationManifestView::validate(source_manifest.bytes(), &mut source_facts)?;

        let mut fragments = Vec::with_capacity(count);
        let mut images = Vec::with_capacity(count);
        for ordinal in 0..count {
            fragments.push(
                staged
                    .output_object(ordinal * 2 + 1)
                    .ok_or("staged fragment is absent")?
                    .bytes()
                    .to_vec(),
            );
            images.push(
                staged
                    .output_object(ordinal * 2 + 2)
                    .ok_or("staged semantic image is absent")?
                    .bytes()
                    .to_vec(),
            );
        }

        let original = FragmentView::validate(&fragments[artifact_ordinal])?;
        let alternate_recipe = CompileRecipeFact::derive(
            original.recipe.profile,
            original.recipe.stage,
            alternate_tool,
            original.source.identity,
            original.recipe.toolchain,
        );
        let range_manifest = FragmentRangeManifest::from_view(&original)?;
        let recipe_range = range_manifest
            .ranges
            .iter()
            .find(|range| range.section == SectionKind::RecipeFact)
            .ok_or("fragment recipe range is absent")?;
        let offset = usize::try_from(recipe_range.offset)?;
        let length = usize::try_from(recipe_range.length)?;
        let recipe_lane = fragments[artifact_ordinal]
            .get_mut(offset..offset + length)
            .ok_or("fragment recipe range exceeds its bytes")?;
        write_recipe_fact(recipe_lane, alternate_recipe);

        // The semantic image's fixed provenance cell stores profile/stage/tool,
        // the source-specific recipe identity, and its exact toolchain.
        let image = &mut images[artifact_ordinal];
        image[60..92].copy_from_slice(alternate_recipe.identity.as_ref());
        image[92..94].copy_from_slice(&<[u8; 2]>::from(alternate_recipe.profile));
        image[94] = u8::from(alternate_recipe.stage);
        image[95] = u8::from(alternate_recipe.tool);
        image[96..128].copy_from_slice(alternate_recipe.toolchain.as_ref());
        let reopened_image = SemanticImageView::reopen(image)?;
        assert!(matches!(
            reopened_image.image_facts().provenance,
            ImageProvenance::Captured { recipe, .. } if recipe == alternate_recipe
        ));

        let mut compiled = Vec::with_capacity(count);
        for bytes in &fragments {
            let fragment = FragmentView::validate(bytes)?;
            compiled.push(crate::driver::CompiledFragment {
                source: fragment.source,
                recipe: fragment.recipe,
                fragment,
            });
        }
        let mut semantic_bytes = Vec::new();
        let mut regions = Vec::with_capacity(count);
        for image in &images {
            let offset = semantic_bytes.len();
            semantic_bytes.extend_from_slice(image);
            regions.push(SemanticImageRegion::from_measurement(
                offset,
                u32::try_from(image.len())?,
            ));
        }
        let mut order = vec![0; count];
        let canonical = CanonicalSemanticCompilation::prepare(
            &compiled,
            &regions,
            &semantic_bytes,
            &mut order,
        )?;
        let manifest_length = COMPILATION_MANIFEST_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES)
                    .ok_or("semantic manifest length overflow")?,
            )
            .ok_or("semantic manifest length overflow")?;
        let mut manifest_bytes = vec![0; manifest_length];
        let mut manifest_facts_scratch: Vec<Option<StoredFragmentFacts>> = vec![None; count];
        let manifest = canonical.write_into(&mut manifest_bytes, &mut manifest_facts_scratch)?;
        let manifest_identity = manifest.identity;
        let mut ordered_fragments = Vec::new();
        let mut ordered_images = Vec::new();
        for ordinal in canonical.canonical_ordinals().iter().copied() {
            ordered_fragments.extend_from_slice(&fragments[ordinal]);
            ordered_images.extend_from_slice(
                regions[ordinal]
                    .bytes(&semantic_bytes)
                    .ok_or("semantic image region does not fit its owner")?,
            );
        }
        let mut locality = [0_u8; crate::application::MAX_LOCALITY_OUTPUT_BYTES];
        let generation = crate::publication::verify_reopened_semantic_generation(
            &manifest,
            &ordered_fragments,
            &ordered_images,
            &mut locality,
        )
        .map_err(|error| {
            std::io::Error::other(format!("rebuild semantic generation: {error:?}"))
        })?;
        let mut binding_bytes = [0_u8; COMPILATION_BINDING_BYTES];
        let _binding =
            CompilationBindingView::write_into(generation, manifest_identity, &mut binding_bytes)?;

        let mut objects = Vec::with_capacity(count * 2 + 1);
        let mut members = Vec::with_capacity(count * 2 + 1);
        let manifest_member = make_result_member(
            CompilerResultMemberRole::Manifest,
            SchemaId::CompilationManifest,
            1,
            0,
            None,
            &manifest_bytes,
        )?;
        objects.push(manifest_member.1);
        members.push(manifest_member.0);
        for (ordinal, original_ordinal) in
            canonical.canonical_ordinals().iter().copied().enumerate()
        {
            let fragment_member = make_result_member(
                CompilerResultMemberRole::Fragment,
                SchemaId::IrFragment,
                1,
                u64::try_from(ordinal)? * 2 + 1,
                Some(0),
                &fragments[original_ordinal],
            )?;
            let image_member = make_result_member(
                CompilerResultMemberRole::SemanticImage,
                SchemaId::IrSemanticImage,
                2,
                u64::try_from(ordinal)? * 2 + 2,
                Some(0),
                &images[original_ordinal],
            )?;
            members.push(fragment_member.0);
            objects.push(fragment_member.1);
            members.push(image_member.0);
            objects.push(image_member.1);
        }
        let envelope = super::super::result_wire::result_envelope_object_from_parts(
            scope,
            input_closure_id,
            input_manifest_object_id,
            generation,
            manifest_identity,
            &binding_bytes,
            &members,
            versioned_plane_members,
        )?;
        Ok((objects, envelope))
    }

    fn test_versioned_plane_output(
        staged: &crate::application::StagedSemanticPackage,
        build: SemanticBuildIdentity,
        input: SemanticInputWitness,
        embedding_artifact: Option<usize>,
    ) -> Result<(Vec<CompilerResultMemberV1>, Vec<TypedObject>), Box<dyn std::error::Error>> {
        const EMBEDDING_PAYLOAD: &[u8] = b"test embedding vector segment";
        let mut members = Vec::new();
        let mut objects = Vec::new();
        for artifact_ordinal in 0..staged.artifacts().len() {
            let image = staged
                .semantic_output_object(artifact_ordinal)
                .ok_or("staged semantic image claim is absent")?;
            let image_bytes = image.bytes();
            let generation = GenerationId::from_canonical_bytes(image_bytes);
            let core_kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
            let core_segment = SemanticPlaneSegment::from_payload_with_witness(
                core_kind,
                [0; 32],
                [0; 32],
                1,
                image_bytes,
                input,
            )?;
            let mut planes = vec![SemanticPlane::claimed(
                core_kind,
                vec![core_segment],
                Coverage::Complete,
            )?];
            let mut segment_payloads = vec![image_bytes];
            if embedding_artifact == Some(artifact_ordinal) {
                let identity = EmbeddingPlaneIdentity::new(
                    [0x21; 32],
                    [0x22; 32],
                    [0x23; 32],
                    3,
                    EmbeddingNormalization::None,
                    *build.toolchain(),
                    *build.recipe(),
                )?;
                let kind = SemanticPlaneKind::Embeddings(identity);
                let segment = SemanticPlaneSegment::from_payload_with_witness(
                    kind,
                    [0x44; 32],
                    [0x44; 32],
                    1,
                    EMBEDDING_PAYLOAD,
                    input,
                )?;
                planes.push(SemanticPlane::claimed(
                    kind,
                    vec![segment],
                    Coverage::Complete,
                )?);
                segment_payloads.push(EMBEDDING_PAYLOAD);
            }
            let manifest = SemanticPlaneManifest::new(generation, build, input, planes)?;
            let manifest_bytes = manifest.encode()?;
            let image_claim =
                crate::application::compiler_result_output_claim(image.claim(), image.bytes())?;
            let artifact_key = u64::try_from(artifact_ordinal)?;
            let (manifest_member, manifest_object) = make_result_member(
                CompilerResultMemberRole::VersionedPlaneManifest,
                SchemaId::Object,
                0xc005,
                artifact_key,
                Some(image_claim.key()),
                &manifest_bytes,
            )?;
            members.push(manifest_member);
            objects.push(manifest_object);
            for (segment_ordinal, payload) in segment_payloads.iter().enumerate() {
                let (member, object) = make_result_member(
                    CompilerResultMemberRole::VersionedPlaneSegment,
                    SchemaId::Object,
                    0xc004,
                    u64::try_from(segment_ordinal)?,
                    Some(artifact_key),
                    payload,
                )?;
                members.push(member);
                objects.push(object);
            }
        }
        Ok((members, objects))
    }

    #[test]
    fn source_role_admission_rejects_duplicates_and_non_source_claims() {
        let expected = BTreeMap::from([(([0x11; 32], 3), 2), (([0x22; 32], 5), 1)]);
        let mut observed = BTreeMap::new();
        assert!(admit_output_source_identity(&expected, &mut observed, ([0x11; 32], 3)).is_ok());
        assert!(admit_output_source_identity(&expected, &mut observed, ([0x11; 32], 3)).is_ok());
        assert_eq!(observed.get(&([0x11; 32], 3)), Some(&2));
        assert!(matches!(
            admit_output_source_identity(&expected, &mut observed, ([0x11; 32], 3)),
            Err(CompilerResultError::InputClosureMismatch)
        ));
        let mut observed = BTreeMap::new();
        assert!(matches!(
            admit_output_source_identity(&expected, &mut observed, ([0x33; 32], 3)),
            Err(CompilerResultError::InputClosureMismatch)
        ));
        assert!(observed.is_empty());
    }

    fn write_recipe_fact(bytes: &mut [u8], recipe: CompileRecipeFact) {
        bytes[..2].copy_from_slice(&<[u8; 2]>::from(recipe.profile));
        bytes[2] = u8::from(recipe.stage);
        bytes[3] = u8::from(recipe.tool);
        bytes[4..36].copy_from_slice(recipe.identity.as_ref());
        bytes[36..68].copy_from_slice(recipe.toolchain.as_ref());
    }

    fn make_result_member(
        role: CompilerResultMemberRole,
        schema: SchemaId,
        kind: u16,
        key: u64,
        parent: Option<u64>,
        bytes: &[u8],
    ) -> Result<(CompilerResultMemberV1, TypedObject), Box<dyn std::error::Error>> {
        let reference = semantic_object_reference(schema, kind, bytes)?;
        let object = result_output_object(reference, key, parent, bytes)?;
        let member = CompilerResultMemberV1 {
            role,
            reference,
            key,
            parent,
            object_id: *object.id().as_bytes(),
        };
        Ok((member, object))
    }
}
