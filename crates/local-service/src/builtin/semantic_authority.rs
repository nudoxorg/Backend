//! Local adapter between Turso's selected semantic closures and product views.
//!
//! Turso owns the selected head and source observation. The workspace relation
//! below is only a projection: startup rebuilds it from Turso before loading a
//! cached view, and image misses reopen the exact selected FileStore closure.

use super::s3_publication::{
    PublicationFence, RemoteClosureSelection, S3ClosurePublisher, SelectedClosurePublisher,
};
use super::versioned_planes::coordinate_identity;
use super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, generation_residence,
};
use crate::compiler_trust::{TRUSTED_COMPILER_POLICY_FILE_NAME, TrustedCompilerWorkerPolicy};
use backend_engine::builtin::{
    PartialSemanticCoverage, ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    SemanticPublicationClaim, SemanticPublicationCoverage,
};
use backend_engine::cluster_transport::EndpointId;
use backend_extension_turso::{
    AttemptInvalidatedByObservationProof, AuthorityHash, AuthorityNamespace,
    COMPILER_SEMANTIC_IMAGE_SCHEMA, CandidateAttempt, CandidateAttemptRecoveryClaim,
    CompilerImageMember, CompilerPublicationEnvelope, CompilerPublicationMetadata,
    ExistingGenerationSelection, ProjectionKind, ReopenedCompilerMetadata, SelectedFrontier,
    SelectedGeneration, SourceObservation, SourceObservationReceipt, SourceObservationValue,
    SupersededAttemptProof, TursoAuthority, VersionedPlaneArtifactMetadata, VersionedPlaneMember,
    VersionedPlaneMetadata, reopen_selected_compiler_metadata,
};
use backend_library::interface::{SemanticImageAuthority, SemanticImageSnapshot};
use backend_semantic::vocabulary::LanguageProfile;
use backend_semantic::vocabulary::Stage;
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ClosureCompositionBudget, ClosureId,
    ClosureMembershipChange, FileStore, GcLimits, GcReport, GcRoot, ObjectId,
    PinnedStoredClosureReceipt, StreamingClosureBudget, StreamingClosureBuilder, TypedObject,
    UntrustedObjectId,
};
use backend_version::{
    ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding, ObjectKey, ObjectVersion, Schema,
    SchemaIdentity,
};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const LOCAL_BRANCH: &str = "locald";
const LOCAL_ENVIRONMENT: &str = "locald-product-v1";
const CAS_DIRECTORY: &str = "semantic-objects";
const AUTHORITY_FILE: &str = backend_extension_turso::AUTHORITY_FILE_NAME;
const MAX_OBJECTS: usize = 100_002;
const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_OBJECT_BYTES: usize = 512 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 16 * 1024;
const MAX_PUT_CALLS: usize = 1_000_000;
const AUTHORITY_PAGE: usize = 512;

/// Durable identity retained before the owner sends a stored-result ACK.
///
/// The product key is supplied separately so the authority namespace is
/// reconstructed from canonical package/coordinate/profile types rather than
/// trusted from an opaque namespace digest in the pending journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingRemoteResultIdentity {
    namespace_id: [u8; 16],
    epoch: u64,
    attempt_id: [u8; 16],
    fence: AuthorityHash,
    input_digest: AuthorityHash,
    candidate_id: AuthorityHash,
    target_root: AuthorityHash,
    selected_closure_id: AuthorityHash,
    expected_generation: u64,
    worker_closure_id: AuthorityHash,
    worker_object_count: u32,
    worker_payload_bytes: u64,
    worker_bytes_verified: u64,
}

impl PendingRemoteResultIdentity {
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new(
        namespace_id: [u8; 16],
        epoch: u64,
        attempt_id: [u8; 16],
        fence: AuthorityHash,
        input_digest: AuthorityHash,
        candidate_id: AuthorityHash,
        target_root: AuthorityHash,
        selected_closure_id: AuthorityHash,
        expected_generation: u64,
        worker_closure_id: AuthorityHash,
        worker_object_count: u32,
        worker_payload_bytes: u64,
        worker_bytes_verified: u64,
    ) -> Self {
        Self {
            namespace_id,
            epoch,
            attempt_id,
            fence,
            input_digest,
            candidate_id,
            target_root,
            selected_closure_id,
            expected_generation,
            worker_closure_id,
            worker_object_count,
            worker_payload_bytes,
            worker_bytes_verified,
        }
    }

    #[must_use]
    pub(crate) const fn namespace_id(&self) -> [u8; 16] {
        self.namespace_id
    }

    #[must_use]
    pub(crate) const fn epoch(&self) -> u64 {
        self.epoch
    }

    #[must_use]
    pub(crate) const fn attempt_id(&self) -> &[u8; 16] {
        &self.attempt_id
    }

    #[must_use]
    pub(crate) const fn fence(&self) -> &AuthorityHash {
        &self.fence
    }

    #[must_use]
    pub(crate) const fn input_digest(&self) -> &AuthorityHash {
        &self.input_digest
    }

    #[must_use]
    pub(crate) const fn candidate_id(&self) -> &AuthorityHash {
        &self.candidate_id
    }

    #[must_use]
    pub(crate) const fn target_root(&self) -> &AuthorityHash {
        &self.target_root
    }

    #[must_use]
    pub(crate) const fn selected_closure_id(&self) -> &AuthorityHash {
        &self.selected_closure_id
    }

    #[must_use]
    pub(crate) const fn expected_generation(&self) -> u64 {
        self.expected_generation
    }

    #[must_use]
    pub(crate) const fn worker_closure_id(&self) -> &AuthorityHash {
        &self.worker_closure_id
    }

    #[must_use]
    pub(crate) const fn worker_object_count(&self) -> u32 {
        self.worker_object_count
    }

    #[must_use]
    pub(crate) const fn worker_payload_bytes(&self) -> u64 {
        self.worker_payload_bytes
    }

    #[must_use]
    pub(crate) const fn worker_bytes_verified(&self) -> u64 {
        self.worker_bytes_verified
    }
}

/// Opaque durable authority decision for recovering a pending remote-result
/// acknowledgement after locald restarts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingRemoteResultProof {
    /// Turso history proves that this exact candidate and closure were
    /// selected at the expected immutable generation.
    SelectedHistory(SelectedGeneration),
    /// Turso proves this exact attempt was superseded before selection.
    Superseded(SupersededAttemptProof),
}

/// Local implementation of the exact compiler semantic-image object schema.
/// The schema identity is shared with Turso, while these bytes are derived
/// directly from the borrowed staged payload without materializing a wrapper.
struct CompilerImagePayloadSchema;

impl Schema for CompilerImagePayloadSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc003;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

fn streaming_budget() -> StreamingClosureBudget {
    StreamingClosureBudget::new(
        MAX_OBJECTS,
        MAX_PAYLOAD_BYTES,
        MAX_OBJECT_BYTES,
        MAX_CHUNK_BYTES,
        MAX_PUT_CALLS,
        StreamingClosureBudget::metadata_input_bytes_for(MAX_OBJECTS).unwrap_or(usize::MAX),
    )
}

fn stream_typed_object(
    builder: &mut StreamingClosureBuilder,
    object: &TypedObject,
) -> Result<ObjectId, BuiltinModelError> {
    let claim = ArtifactObjectClaim::new(
        object.schema(),
        *object.key(),
        *object.version(),
        u64::try_from(object.bytes().len())
            .map_err(|_| BuiltinModelError("semantic object length exceeds u64".to_owned()))?,
    );
    let mut stream = builder
        .begin_object(claim)
        .map_err(|error| BuiltinModelError(format!("begin semantic output object: {error:?}")))?;
    for chunk in object.bytes().chunks(MAX_CHUNK_BYTES) {
        stream.write(chunk).map_err(|error| {
            BuiltinModelError(format!("write semantic output object: {error:?}"))
        })?;
    }
    stream
        .finish()
        .map_err(|error| BuiltinModelError(format!("finish semantic output object: {error:?}")))
}

fn closure_composition_budget(
    changes: usize,
) -> Result<ClosureCompositionBudget, BuiltinModelError> {
    let metadata_bytes = usize::try_from(MAX_METADATA_BYTES)
        .map_err(|_| BuiltinModelError("semantic metadata limit does not fit usize".to_owned()))?;
    Ok(ClosureCompositionBudget::new(
        MAX_OBJECTS,
        changes,
        MAX_PAYLOAD_BYTES.saturating_add(MAX_METADATA_BYTES),
        metadata_bytes,
    ))
}

fn pin_existing_closure(
    store: &FileStore,
    claim: ArtifactClosureClaim,
) -> Result<PinnedStoredClosureReceipt, BuiltinModelError> {
    store
        .compose_closure_index(Some(claim), &[], closure_composition_budget(0)?)
        .map_err(|error| BuiltinModelError(format!("pin compiler output closure: {error:?}")))
}

fn verify_then_publish_selected_closure<T>(
    verify: impl FnOnce() -> Result<T, BuiltinModelError>,
    publish: impl FnOnce(&T) -> Result<(), BuiltinModelError>,
) -> Result<T, BuiltinModelError> {
    let verified = verify()?;
    publish(&verified)?;
    Ok(verified)
}

fn pin_object_ids(
    store: &FileStore,
    object_ids: &[ObjectId],
) -> Result<PinnedStoredClosureReceipt, BuiltinModelError> {
    if object_ids.is_empty() || object_ids.len() > MAX_OBJECTS {
        return Err(BuiltinModelError(
            "semantic publication object inventory is empty or oversized".to_owned(),
        ));
    }
    let mut changes = object_ids
        .iter()
        .copied()
        .map(ClosureMembershipChange::add)
        .collect::<Vec<_>>();
    changes.sort_unstable_by_key(|change| change.object_id());
    if changes
        .windows(2)
        .any(|pair| pair[0].object_id() == pair[1].object_id())
    {
        return Err(BuiltinModelError(
            "semantic publication object inventory contains duplicates".to_owned(),
        ));
    }
    store
        .compose_closure_index(None, &changes, closure_composition_budget(changes.len())?)
        .map_err(|error| BuiltinModelError(format!("pin semantic output objects: {error:?}")))
}

fn stream_payload<S: Schema<Value = [u8]>>(
    builder: &mut backend_store::StreamingClosureBuilder,
    bytes: &[u8],
) -> Result<ObjectId, BuiltinModelError> {
    if bytes.is_empty() {
        return Err(BuiltinModelError(
            "semantic publication contains an empty object".to_owned(),
        ));
    }
    let schema = SchemaIdentity::new(S::DOMAIN, S::TYPE, S::VERSION);
    let key = ObjectKey::<S>::from_value(bytes);
    let version = ObjectVersion::<S>::from_value(bytes);
    let claim = ArtifactObjectClaim::new(
        schema,
        key.to_bytes(),
        version.to_bytes(),
        u64::try_from(bytes.len())
            .map_err(|_| BuiltinModelError("semantic object length exceeds u64".to_owned()))?,
    );
    let mut object = builder
        .begin_object(claim)
        .map_err(|error| BuiltinModelError(format!("begin semantic object stream: {error:?}")))?;
    for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
        object.write(chunk).map_err(|error| {
            BuiltinModelError(format!("write semantic object stream: {error:?}"))
        })?;
    }
    object
        .finish()
        .map_err(|error| BuiltinModelError(format!("finish semantic object stream: {error:?}")))
}

fn artifact_budget() -> ArtifactBudget {
    ArtifactBudget::new(
        MAX_OBJECTS,
        MAX_OBJECTS,
        MAX_PAYLOAD_BYTES,
        MAX_CHUNK_BYTES,
        MAX_PUT_CALLS,
    )
}

type HistoryKey = (ProductSemanticPublicationKey, [u8; 32]);

#[derive(Default)]
struct SelectedClosureSnapshot {
    by_binding: BTreeMap<HistoryKey, SelectedGeneration>,
}

struct SelectedClosureImageLoader {
    store: FileStore,
    selections: RwLock<SelectedClosureSnapshot>,
}

impl SelectedClosureImageLoader {
    fn remember(
        &self,
        key: ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
        selected: SelectedGeneration,
    ) -> Result<(), BuiltinModelError> {
        let binding = *claim.binding().identity.as_ref();
        let mut selections = self.selections.write().map_err(|_| {
            BuiltinModelError("semantic authority image snapshot is poisoned".to_owned())
        })?;
        selections
            .by_binding
            .entry((key, binding))
            .or_insert(selected);
        Ok(())
    }

    fn selected(
        &self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
    ) -> Result<SelectedGeneration, BuiltinModelError> {
        let binding = *claim.binding().identity.as_ref();
        self.selections
            .read()
            .map_err(|_| {
                BuiltinModelError("semantic authority image snapshot is poisoned".to_owned())
            })?
            .by_binding
            .get(&(key.clone(), binding))
            .cloned()
            .ok_or_else(|| {
                BuiltinModelError(
                    "semantic projection names a generation absent from Turso history".to_owned(),
                )
            })
    }
}

impl generation_residence::SelectedSemanticImageLoader for SelectedClosureImageLoader {
    fn load(
        &self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
        max_bytes: usize,
        max_images: usize,
    ) -> Result<Box<[SemanticImageSnapshot]>, BuiltinModelError> {
        let selected = self.selected(key, claim)?;
        let reopened =
            reopen_selected_compiler_metadata(&self.store, &selected).map_err(|error| {
                BuiltinModelError(format!("reopen selected semantic metadata: {error}"))
            })?;
        let manifest = self
            .store
            .open_closure_claim(ArtifactClosureClaim::from_bytes(*selected.closure_id()))
            .map_err(|error| {
                BuiltinModelError(format!("open selected semantic image index: {error:?}"))
            })?;
        let members = reopened.metadata().images();
        if members.len() > max_images {
            return Err(BuiltinModelError(format!(
                "selected semantic image count {} exceeds residence admission limit {max_images}",
                members.len()
            )));
        }
        let admitted_bytes = members.iter().try_fold(0_usize, |total, member| {
            total.checked_add(member.byte_length() as usize)
        });
        let Some(admitted_bytes) = admitted_bytes else {
            return Err(BuiltinModelError(
                "selected semantic image byte count overflowed residence accounting".to_owned(),
            ));
        };
        if admitted_bytes > max_bytes {
            return Err(BuiltinModelError(format!(
                "selected semantic image bytes {admitted_bytes} exceed residence admission limit {max_bytes}"
            )));
        }
        let mut images = Vec::new();
        images.try_reserve_exact(members.len()).map_err(|_| {
            BuiltinModelError("selected semantic image inventory is too large".to_owned())
        })?;
        for member in members {
            let claim = UntrustedObjectId::from_bytes(*member.object_id());
            let object_id = manifest
                .admit_claim(claim)
                .map_err(|error| {
                    BuiltinModelError(format!("admit selected semantic image claim: {error:?}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "selected semantic image is absent from its closure".to_owned(),
                    )
                })?;
            let object = manifest
                .get(object_id)
                .map_err(|error| {
                    BuiltinModelError(format!("read selected semantic image: {error:?}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "selected semantic image disappeared from its closure".to_owned(),
                    )
                })?;
            if object.schema() != COMPILER_SEMANTIC_IMAGE_SCHEMA
                || u32::try_from(object.bytes().len()).ok() != Some(member.byte_length())
            {
                return Err(BuiltinModelError(
                    "selected semantic image schema or length differs from Turso inventory"
                        .to_owned(),
                ));
            }
            let identity =
                ArtifactId::<IrSemanticImageEncoding, IrSemanticImageDomain>::from_encoded_bytes(
                    object.bytes(),
                );
            if identity.as_ref() != member.semantic_image_identity() {
                return Err(BuiltinModelError(
                    "selected semantic image identity differs from its Turso inventory".to_owned(),
                ));
            }
            let authority = SemanticImageAuthority {
                identity,
                byte_len: member.byte_length(),
            };
            images.push(
                SemanticImageSnapshot::try_from_reopened(authority, object.bytes()).map_err(
                    |error| {
                        BuiltinModelError(format!(
                            "admit selected semantic image from Turso closure: {error:?}"
                        ))
                    },
                )?,
            );
        }
        Ok(images.into_boxed_slice())
    }
}

struct HistoryFact {
    selected: SelectedGeneration,
}

/// Local-only proof that compiler output is bound to one Turso attempt and to
/// the exact CAS members that the selector will publish. The type is private,
/// non-Clone, and is created only by the staged compiler or checked-result
/// admission paths below.
struct AdmittedCompilation {
    attempt: CandidateAttempt,
    metadata: CompilerPublicationMetadata,
    /// Exact selected payload set, with every image and versioned-plane
    /// member reachable and no worker-only result members.
    payload_pin: PinnedStoredClosureReceipt,
    source_pin: Option<PinnedStoredClosureReceipt>,
    /// Remote worker result closure retained as evidence through selection.
    evidence_pin: Option<PinnedStoredClosureReceipt>,
    /// Exact Turso attempt fence, retaining a remote assignment only for remote work.
    publication_fence: PublicationFence,
    claim: SemanticPublicationClaim,
}

#[derive(Clone, Copy)]
struct CompilerBuildAdmission {
    target: [u8; 32],
    profile: LanguageProfile,
    stage: Stage,
    recipe: [u8; 32],
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl CompilerBuildAdmission {
    fn from_local(identity: backend_engine::application::LocalCompilerExecutionIdentity) -> Self {
        Self {
            target: *identity.target().as_ref(),
            profile: identity.profile(),
            stage: identity.stage(),
            recipe: *identity.recipe_identity().as_ref(),
            toolchain: identity.toolchain_identity(),
            environment: identity.environment_identity(),
            target_platform: identity.target_platform_identity(),
        }
    }

    fn from_host_local(
        identity: backend_engine::application::LocalCompilerPlaneExecutionIdentity,
    ) -> Self {
        Self {
            target: *identity.target().as_ref(),
            profile: identity.profile(),
            stage: identity.stage(),
            recipe: identity.recipe_identity().as_bytes(),
            toolchain: identity.toolchain_identity(),
            environment: identity.environment_identity(),
            target_platform: identity.target_platform_identity(),
        }
    }

    fn matches(self, build: &backend_semantic::ir::SemanticBuildIdentity) -> bool {
        self.target == *build.target()
            && self.profile == build.profile()
            && self.stage == build.stage()
            && self.recipe == *build.recipe()
            && self.toolchain == *build.toolchain()
            && self.environment == *build.environment()
            && self.target_platform == *build.target_platform()
    }
}

impl AdmittedCompilation {
    fn new(
        key: &ProductSemanticPublicationKey,
        attempt: CandidateAttempt,
        metadata: CompilerPublicationMetadata,
        payload_pin: PinnedStoredClosureReceipt,
        source_pin: Option<PinnedStoredClosureReceipt>,
        evidence_pin: Option<PinnedStoredClosureReceipt>,
        remote_scope: Option<backend_engine::cluster_transport::AssignmentScope>,
        claim: SemanticPublicationClaim,
        build_admission: CompilerBuildAdmission,
        store: &FileStore,
    ) -> Result<Self, BuiltinModelError> {
        let namespace =
            SemanticAuthority::namespace(key.package(), key.coordinate(), key.profile())?;
        if attempt.namespace() != &namespace
            || attempt.observation().observation().revision().as_ref()
                != Some(attempt.input_digest())
            || claim.manifest().identity.as_ref() != metadata.manifest_identity()
            || claim.binding().identity.as_ref() != metadata.binding_identity()
        {
            return Err(BuiltinModelError(
                "compiler output does not match its admitted attempt and semantic facts".to_owned(),
            ));
        }
        let publication_fence =
            PublicationFence::for_attempt(&attempt, remote_scope).map_err(|error| {
                BuiltinModelError(format!(
                    "build exact Turso publication fence for selected closure: {error}"
                ))
            })?;

        let planes = metadata.versioned_planes().ok_or_else(|| {
            BuiltinModelError(
                "selected compiler generation has no versioned semantic-plane catalog".to_owned(),
            )
        })?;
        if planes.artifacts().len() != metadata.images().len() {
            return Err(BuiltinModelError(
                "compiler image and semantic-plane catalog counts differ".to_owned(),
            ));
        }
        let payload_index = store
            .read_closure_index(payload_pin.receipt().closure())
            .map_err(|error| {
                BuiltinModelError(format!("read admitted compiler output closure: {error:?}"))
            })?;
        let mut image_ids = BTreeMap::new();
        let mut expected_members = BTreeSet::new();
        for image in metadata.images() {
            let ordinal = image.artifact_ordinal();
            let object_id = store
                .verify_object_claim(UntrustedObjectId::from_bytes(*image.object_id()))
                .map_err(|error| {
                    BuiltinModelError(format!("verify admitted compiler image: {error:?}"))
                })?
                .id();
            expected_members.insert(object_id);
            if image_ids.insert(ordinal, object_id).is_some()
                || !payload_index
                    .contains_object_id(object_id)
                    .map_err(|error| {
                        BuiltinModelError(format!("check compiler image membership: {error:?}"))
                    })?
            {
                return Err(BuiltinModelError(
                    "admitted compiler image is duplicated or absent from its output closure"
                        .to_owned(),
                ));
            }
        }
        for artifact in planes.artifacts() {
            let image_key = artifact.image_key();
            let ordinal = image_key.artifact_ordinal();
            let manifest_object_id = store
                .verify_object_claim(UntrustedObjectId::from_bytes(
                    *artifact.manifest_object_id(),
                ))
                .map_err(|error| {
                    BuiltinModelError(format!("verify admitted plane manifest: {error:?}"))
                })?
                .id();
            expected_members.insert(manifest_object_id);
            let manifest =
                backend_semantic::ir::SemanticPlaneManifest::decode(artifact.manifest_bytes())
                    .map_err(|error| {
                        BuiltinModelError(format!(
                            "reopen admitted semantic-plane manifest: {error}"
                        ))
                    })?;
            let image_id = image_ids.get(&ordinal).ok_or_else(|| {
                BuiltinModelError(
                    "semantic-plane artifact has no corresponding compiler image".to_owned(),
                )
            })?;
            if manifest.root() != image_key.manifest_root()
                || manifest.semantic_generation() != image_key.semantic_generation()
                || manifest.input().input_root() != attempt.input_digest()
                || manifest.build().profile() != key.profile()
                || !build_admission.matches(&manifest.build())
                || !payload_index
                    .contains_object_id(*image_id)
                    .map_err(|error| {
                        BuiltinModelError(format!("check compiler image membership: {error:?}"))
                    })?
                || !payload_index
                    .contains_object_id(manifest_object_id)
                    .map_err(|error| {
                        BuiltinModelError(format!("check plane manifest membership: {error:?}"))
                    })?
            {
                return Err(BuiltinModelError(
                    "semantic-plane metadata is not bound to this exact compiler attempt"
                        .to_owned(),
                ));
            }
            for member in artifact.members() {
                let member_object_id = store
                    .verify_object_claim(UntrustedObjectId::from_bytes(*member.object_id()))
                    .map_err(|error| {
                        BuiltinModelError(format!("verify admitted plane segment: {error:?}"))
                    })?
                    .id();
                expected_members.insert(member_object_id);
                if !payload_index
                    .contains_object_id(member_object_id)
                    .map_err(|error| {
                        BuiltinModelError(format!("check semantic segment membership: {error:?}"))
                    })?
                {
                    return Err(BuiltinModelError(
                        "semantic-plane segment is absent from its admitted output closure"
                            .to_owned(),
                    ));
                }
            }
        }
        if payload_pin.receipt().object_count()
            != u64::try_from(expected_members.len()).unwrap_or(u64::MAX)
        {
            return Err(BuiltinModelError(
                "selected semantic closure contains missing or unrelated payload members"
                    .to_owned(),
            ));
        }
        Ok(Self {
            attempt,
            metadata,
            payload_pin,
            source_pin,
            evidence_pin,
            publication_fence,
            claim,
        })
    }
}

/// Process-local handle to the selected semantic authority and its CAS.
///
/// The mutable Turso handle stays on the local owner thread. Read misses use a
/// lock-protected snapshot of exact selected-generation rows and a cloned
/// FileStore handle, allowing independent CAS reopens to proceed concurrently.
pub(crate) struct SemanticAuthority {
    authority: TursoAuthority,
    store: FileStore,
    workspace: PathBuf,
    s3_publisher: Option<Box<dyn SelectedClosurePublisher>>,
    compiler_trust_policy: std::path::PathBuf,
    image_loader: Arc<SelectedClosureImageLoader>,
    verified_segments: Arc<super::versioned_planes::VerifiedSegmentCache>,
    selected_image_readers: Arc<super::selected_full_image::VerifiedLocalImageReaderCache>,
    selected_image_plans: Arc<super::selected_full_image::SelectedFullImagePlanCache>,
    history: BTreeMap<HistoryKey, HistoryFact>,
    retained_generations: BTreeMap<HistoryKey, u64>,
    latest_observations: BTreeMap<ProductSemanticPublicationKey, SourceObservationReceipt>,
}

impl SemanticAuthority {
    pub(crate) fn open(workspace: &Path) -> Result<Self, BuiltinModelError> {
        let store = FileStore::open(workspace.join(CAS_DIRECTORY), 512 * 1024 * 1024)
            .map_err(|error| BuiltinModelError(format!("open semantic artifact CAS: {error:?}")))?;
        let s3_publisher = S3ClosurePublisher::from_env(workspace)
            .map_err(|error| BuiltinModelError(format!("configure owner S3 publication: {error}")))?
            .map(|publisher| Box::new(publisher) as Box<dyn SelectedClosurePublisher>);
        let authority =
            futures_executor::block_on(TursoAuthority::open(workspace.join(AUTHORITY_FILE)))
                .map_err(|error| {
                    BuiltinModelError(format!("open semantic selection authority: {error}"))
                })?;
        let image_loader = Arc::new(SelectedClosureImageLoader {
            store: store.clone(),
            selections: RwLock::new(SelectedClosureSnapshot::default()),
        });
        Ok(Self {
            authority,
            store,
            workspace: workspace.to_path_buf(),
            s3_publisher,
            compiler_trust_policy: workspace.join(TRUSTED_COMPILER_POLICY_FILE_NAME),
            image_loader,
            verified_segments: Arc::new(super::versioned_planes::VerifiedSegmentCache::default()),
            selected_image_readers: Arc::new(
                super::selected_full_image::VerifiedLocalImageReaderCache::default(),
            ),
            selected_image_plans: Arc::new(
                super::selected_full_image::SelectedFullImagePlanCache::default(),
            ),
            history: BTreeMap::new(),
            retained_generations: BTreeMap::new(),
            latest_observations: BTreeMap::new(),
        })
    }

    /// Clones the durable product CAS handle for the owner cluster transport.
    #[must_use]
    pub(crate) fn store(&self) -> FileStore {
        self.store.clone()
    }

    /// Creates the bounded semantic-plane range service over this authority's local CAS.
    #[must_use]
    pub(crate) fn versioned_plane_service(
        &self,
    ) -> super::versioned_planes::VersionedPlaneService<'_> {
        super::versioned_planes::VersionedPlaneService::with_cache_and_hydrator(
            self.store.clone(),
            Arc::clone(&self.verified_segments),
            self.s3_publisher.as_deref(),
        )
    }

    /// Resolves one product key from a freshly read Turso selected head and its exact closure.
    ///
    /// This deliberately does not consult the workspace relation or the image cache: those are
    /// projections of the authority and can lag a crash between Turso selection and projection
    /// repair.
    pub(crate) fn resolve_current_selected(
        &self,
        key: &ProductSemanticPublicationKey,
    ) -> Result<super::versioned_planes::SelectedVersionedPlanePublication, BuiltinModelError> {
        let (selected, reopened) = self.reopen_current_selected_metadata(key)?;
        super::versioned_planes::SelectedVersionedPlanePublication::from_reopened(
            key, &selected, &reopened,
        )
        .map_err(|error| {
            BuiltinModelError(format!("admit selected semantic plane metadata: {error}"))
        })
    }

    /// Reopens the exact current Turso selection and its compiler metadata.
    fn reopen_current_selected_metadata(
        &self,
        key: &ProductSemanticPublicationKey,
    ) -> Result<(SelectedGeneration, ReopenedCompilerMetadata), BuiltinModelError> {
        let frontier = self.current_selected_frontier(key)?;
        let selected = self.selected_generation_for_frontier(key, &frontier)?;
        let reopened =
            reopen_selected_compiler_metadata(&self.store, &selected).map_err(|error| {
                BuiltinModelError(format!("reopen selected semantic metadata: {error}"))
            })?;
        if !reopened.envelope().matches_selected(&selected) {
            return Err(BuiltinModelError(
                "selected semantic compiler envelope differs from Turso head".to_owned(),
            ));
        }
        Ok((selected, reopened))
    }

    /// Reads only the current Turso head. This cheap check is repeated before
    /// and after every full-image page; immutable compiler metadata is reopened
    /// only when the bounded selected-image plan cache misses.
    fn current_selected_frontier(
        &self,
        key: &ProductSemanticPublicationKey,
    ) -> Result<SelectedFrontier, BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        let frontier = futures_executor::block_on(self.authority.selected_frontier(&namespace))
            .map_err(|error| {
                BuiltinModelError(format!("read selected semantic frontier: {error}"))
            })?
            .ok_or_else(|| {
                BuiltinModelError("semantic authority has no selected generation".to_owned())
            })?;
        if frontier.namespace().package() != key.package().as_str()
            || frontier.namespace().source() != key.coordinate().as_str()
        {
            return Err(BuiltinModelError(
                "selected semantic frontier targets another product".to_owned(),
            ));
        }
        Ok(frontier)
    }

    fn selected_generation_for_frontier(
        &self,
        key: &ProductSemanticPublicationKey,
        frontier: &SelectedFrontier,
    ) -> Result<SelectedGeneration, BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        let selected = futures_executor::block_on(
            self.authority
                .selected_generation(&namespace, frontier.generation()),
        )
        .map_err(|error| BuiltinModelError(format!("read selected semantic generation: {error}")))?
        .ok_or_else(|| {
            BuiltinModelError("selected semantic generation is absent from history".to_owned())
        })?;
        if Self::selected_generation_stamp(key, &selected)?
            != Self::selected_frontier_stamp(key, frontier)?
        {
            return Err(BuiltinModelError(
                "selected semantic history differs from the current Turso frontier".to_owned(),
            ));
        }
        Ok(selected)
    }

    fn selected_frontier_stamp(
        key: &ProductSemanticPublicationKey,
        frontier: &SelectedFrontier,
    ) -> Result<backend_replication::SelectedGenerationStamp, BuiltinModelError> {
        let catalog_root = frontier.semantic_manifest_root().copied().ok_or_else(|| {
            BuiltinModelError("selected compiler generation has no image catalog".to_owned())
        })?;
        Self::selected_stamp_from_fields(
            key,
            frontier.namespace(),
            frontier.generation(),
            *frontier.target_root(),
            *frontier.closure_id(),
            catalog_root,
        )
    }

    fn selected_generation_stamp(
        key: &ProductSemanticPublicationKey,
        selected: &SelectedGeneration,
    ) -> Result<backend_replication::SelectedGenerationStamp, BuiltinModelError> {
        let catalog_root = selected.semantic_catalog_root().copied().ok_or_else(|| {
            BuiltinModelError("selected compiler generation has no image catalog".to_owned())
        })?;
        Self::selected_stamp_from_fields(
            key,
            selected.namespace(),
            selected.generation(),
            *selected.target_root(),
            *selected.closure_id(),
            catalog_root,
        )
    }

    fn selected_stamp_from_fields(
        key: &ProductSemanticPublicationKey,
        namespace: &AuthorityNamespace,
        generation: u64,
        target_root: [u8; 32],
        closure_id: [u8; 32],
        catalog_root: [u8; 32],
    ) -> Result<backend_replication::SelectedGenerationStamp, BuiltinModelError> {
        if namespace.package() != key.package().as_str()
            || namespace.source() != key.coordinate().as_str()
        {
            return Err(BuiltinModelError(
                "selected semantic frontier targets another product".to_owned(),
            ));
        }
        backend_replication::SelectedGenerationStamp::checked(
            namespace.namespace_id(),
            key.profile(),
            coordinate_identity(key.coordinate().as_str()),
            generation,
            target_root,
            closure_id,
            backend_semantic::ir::SemanticPlaneCatalogRoot::from_wire_claim(catalog_root),
        )
        .map_err(|error| {
            BuiltinModelError(format!("invalid selected semantic frontier stamp: {error}"))
        })
    }

    /// Admits exact metadata for one selected compiler image without copying
    /// the full NXFI payload. The catalog key and Turso image member are both
    /// checked before a range reader may use the physical CAS object.
    pub(crate) fn selected_full_image_plan(
        &self,
        key: &ProductSemanticPublicationKey,
        image: backend_semantic::ir::SemanticPlaneImageKey,
        expected_stamp: backend_replication::SelectedGenerationStamp,
    ) -> Result<
        super::selected_full_image::SelectedFullImagePlan,
        super::selected_full_image::SelectedFullImageError,
    > {
        let frontier = self.current_selected_frontier(key).map_err(|error| {
            super::selected_full_image::SelectedFullImageError::Authority(error.0)
        })?;
        let stamp = Self::selected_frontier_stamp(key, &frontier).map_err(|error| {
            super::selected_full_image::SelectedFullImageError::Authority(error.0)
        })?;
        if stamp != expected_stamp {
            return Err(super::selected_full_image::SelectedFullImageError::StaleSelection);
        }
        self.selected_image_plans
            .get_or_resolve(stamp, image, || {
                self.resolve_selected_full_image_plan(key, image, &frontier, stamp)
            })
            .map_err(|error| super::selected_full_image::SelectedFullImageError::Authority(error.0))
    }

    fn resolve_selected_full_image_plan(
        &self,
        key: &ProductSemanticPublicationKey,
        image: backend_semantic::ir::SemanticPlaneImageKey,
        frontier: &SelectedFrontier,
        stamp: backend_replication::SelectedGenerationStamp,
    ) -> Result<super::selected_full_image::SelectedFullImagePlan, BuiltinModelError> {
        let selected = self.selected_generation_for_frontier(key, frontier)?;
        let reopened =
            reopen_selected_compiler_metadata(&self.store, &selected).map_err(|error| {
                BuiltinModelError(format!("reopen selected semantic metadata: {error}"))
            })?;
        let publication =
            super::versioned_planes::SelectedVersionedPlanePublication::from_reopened(
                key, &selected, &reopened,
            )
            .map_err(|error| {
                BuiltinModelError(format!("admit selected semantic plane metadata: {error}"))
            })?;
        if publication.stamp() != stamp {
            return Err(BuiltinModelError(
                "selected semantic metadata differs from the current frontier".to_owned(),
            ));
        }
        let planes = reopened.metadata().versioned_planes().ok_or_else(|| {
            BuiltinModelError("selected compiler generation has no image catalog".to_owned())
        })?;
        let artifact = planes.artifact_for_image(image).ok_or_else(|| {
            BuiltinModelError("requested full image is absent from the selected catalog".to_owned())
        })?;
        let image_member = reopened
            .metadata()
            .images()
            .iter()
            .find(|member| member.artifact_ordinal() == image.artifact_ordinal())
            .ok_or_else(|| {
                BuiltinModelError("selected catalog image has no compiler image member".to_owned())
            })?;
        let identity = backend_semantic::ir::SemanticImageIdentity::try_from(
            *image_member.semantic_image_identity(),
        )
        .map_err(|error| {
            BuiltinModelError(format!(
                "selected compiler image identity is invalid: {error}"
            ))
        })?;
        let total_length = u64::from(image_member.byte_length());
        if total_length == 0 || total_length > backend_replication::MAX_SEMANTIC_IMAGE_BYTES {
            return Err(BuiltinModelError(
                "selected full semantic image exceeds the 128 MiB range-service bound".to_owned(),
            ));
        }
        let manifest =
            backend_semantic::ir::SemanticPlaneManifest::decode(artifact.manifest_bytes())
                .map_err(|error| {
                    BuiltinModelError(format!("decode selected semantic plane manifest: {error}"))
                })?;
        if manifest.build().profile() != key.profile()
            || manifest.semantic_generation() != image.semantic_generation()
            || manifest.root() != image.manifest_root()
        {
            return Err(BuiltinModelError(
                "selected full image differs from its semantic plane manifest".to_owned(),
            ));
        }
        Ok(super::selected_full_image::SelectedFullImagePlan {
            stamp,
            image,
            identity,
            total_length,
            object_id: *image_member.object_id(),
            closure_id: *selected.closure_id(),
            remote_selection: RemoteClosureSelection::from_selected(&selected),
            environment: *manifest.build().environment(),
            target_platform: *manifest.build().target_platform(),
        })
    }

    /// Reads a 16 KiB page from the exact closure member named by an admitted
    /// selected-image plan. Local object identity is verified before seeking;
    /// cold S3 objects use the selected publisher's verified temp-envelope LRU.
    pub(crate) fn read_selected_full_image_range(
        &self,
        plan: &super::selected_full_image::SelectedFullImagePlan,
        byte_range: backend_replication::ByteRange,
    ) -> Result<Vec<u8>, BuiltinModelError> {
        if byte_range.len == 0
            || byte_range.len > super::selected_full_image::MAX_SELECTED_IMAGE_RANGE_BYTES
            || byte_range
                .end()
                .ok()
                .is_none_or(|end| end > plan.total_length)
        {
            return Err(BuiltinModelError(
                "selected full semantic image range exceeds bounds".to_owned(),
            ));
        }
        let object_claim = UntrustedObjectId::from_bytes(plan.object_id);
        let closure = self
            .store
            .open_closure_claim(ArtifactClosureClaim::from_bytes(plan.closure_id))
            .map_err(|error| {
                BuiltinModelError(format!("open selected semantic image closure: {error:?}"))
            })?;
        let object_id = closure
            .admit_claim(object_claim)
            .map_err(|error| {
                BuiltinModelError(format!("admit selected semantic image member: {error:?}"))
            })?
            .ok_or_else(|| {
                BuiltinModelError("selected semantic image is absent from its closure".to_owned())
            })?;
        if object_id.as_bytes() != &plan.object_id {
            return Err(BuiltinModelError(
                "selected semantic image closure member identity differs".to_owned(),
            ));
        }

        let local = self
            .selected_image_readers
            .read_range(&self.store, plan, byte_range);
        super::selected_full_image::read_local_or_remote(local, || {
            let publisher = self.s3_publisher.as_deref().ok_or_else(|| {
                "selected semantic image is absent or corrupt in local CAS and S3 is unavailable"
                    .to_owned()
            })?;
            publisher
                .hydrate_object_range(
                    &self.store,
                    plan.remote_selection,
                    object_claim,
                    COMPILER_SEMANTIC_IMAGE_SCHEMA,
                    plan.total_length,
                    byte_range.start,
                    byte_range.len,
                )
                .map_err(|error| format!("hydrate selected semantic image page: {error:?}"))
        })
        .map_err(|error| match error {
            super::selected_full_image::SelectedImagePageError::Local(error) => BuiltinModelError(
                format!("read selected semantic image from local CAS: {error:?}"),
            ),
            super::selected_full_image::SelectedImagePageError::Remote(error) => {
                BuiltinModelError(error)
            }
        })
    }

    /// Reconstructs Turso evidence for one durable pending stored-result ACK.
    ///
    /// Selected history is checked before asking for a superseded-attempt
    /// proof. This matters when a result was selected and a later compile has
    /// since moved the head: its original stored ACK remains valid because the
    /// exact result is still an immutable selected-history event.
    pub(crate) fn prove_pending_remote_result(
        &self,
        key: &ProductSemanticPublicationKey,
        expected: &PendingRemoteResultIdentity,
    ) -> Result<PendingRemoteResultProof, BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        if namespace.namespace_id() != expected.namespace_id() {
            return Err(BuiltinModelError(
                "pending remote result names a different semantic authority namespace".to_owned(),
            ));
        }
        if expected.worker_object_count() == 0
            || expected.worker_payload_bytes() == 0
            || expected.worker_payload_bytes() > MAX_PAYLOAD_BYTES
        {
            return Err(BuiltinModelError(
                "pending remote result carries invalid worker closure totals".to_owned(),
            ));
        }
        let worker_closure = self
            .store
            .reopen_stored_closure(
                ArtifactClosureClaim::from_bytes(*expected.worker_closure_id()),
                artifact_budget(),
            )
            .map_err(|error| {
                BuiltinModelError(format!("verify pending worker result closure: {error:?}"))
            })?;
        if worker_closure.closure().as_bytes() != expected.worker_closure_id()
            || worker_closure.object_count() != u64::from(expected.worker_object_count())
            || worker_closure.bytes_verified() != expected.worker_bytes_verified()
        {
            return Err(BuiltinModelError(
                "pending worker result closure differs from its durable receipt".to_owned(),
            ));
        }

        let mut after_generation = None;
        loop {
            let page = futures_executor::block_on(self.authority.selected_generations(
                &namespace,
                after_generation,
                AUTHORITY_PAGE,
            ))
            .map_err(|error| {
                BuiltinModelError(format!("read selected history for pending ACK: {error}"))
            })?;
            if page.is_empty() {
                break;
            }
            for selected in page.iter() {
                let (attempt_id, epoch) = selected.attempt();
                if attempt_id != expected.attempt_id() || epoch != expected.epoch() {
                    continue;
                }
                let (selected_epoch, selected_fence) = selected.scheduler_fence();
                let exact = selected_epoch == expected.epoch()
                    && selected_fence == *expected.fence()
                    && selected.input_digest() == expected.input_digest()
                    && selected.candidate_id() == expected.candidate_id()
                    && selected.target_root() == expected.target_root()
                    && selected.closure_id() == expected.selected_closure_id();
                if !exact || selected.generation() != expected.expected_generation() {
                    return Err(BuiltinModelError(
                        "pending remote result conflicts with its immutable Turso selection history"
                            .to_owned(),
                    ));
                }
                reopen_selected_compiler_metadata(&self.store, selected).map_err(|error| {
                    BuiltinModelError(format!(
                        "verify pending selected compiler envelope and inventory: {error}"
                    ))
                })?;
                return Ok(PendingRemoteResultProof::SelectedHistory(selected.clone()));
            }
            after_generation = page.last().map(SelectedGeneration::generation);
            if page.len() < AUTHORITY_PAGE {
                break;
            }
        }

        let proof = futures_executor::block_on(self.authority.superseded_attempt_proof_for_fence(
            &namespace,
            expected.epoch(),
            *expected.fence(),
        ))
        .map_err(|error| {
            BuiltinModelError(format!("read Turso superseded-attempt proof: {error}"))
        })?
        .ok_or_else(|| {
            BuiltinModelError(
                "pending remote result is neither selected nor durably superseded".to_owned(),
            )
        })?;
        if proof.namespace() != &namespace
            || proof.attempt_id() != expected.attempt_id()
            || proof.epoch() != expected.epoch()
            || proof.fence() != expected.fence()
            || proof.input_digest() != expected.input_digest()
            || proof.current_epoch() <= expected.epoch()
        {
            return Err(BuiltinModelError(
                "Turso superseded-attempt proof differs from the pending result".to_owned(),
            ));
        }
        Ok(PendingRemoteResultProof::Superseded(proof))
    }

    pub(crate) fn install_image_loader(
        &self,
        residence: &mut generation_residence::SemanticGenerationResidence,
    ) {
        residence.install_selected_loader(self.image_loader.clone());
    }

    /// Collects this product CAS using a fresh Turso history-root snapshot.
    ///
    /// The store resolves roots only after acquiring its exclusive GC lease;
    /// this prevents a collector that waited on a publication pin from
    /// sweeping a closure selected while it waited. All retained selection
    /// history is rooted so explicit rollback remains available after GC.
    pub(crate) fn collect_garbage(&self, limits: GcLimits) -> Result<GcReport, BuiltinModelError> {
        // Remote segment residency stays opt-in until the process journey
        // proves selected metadata, image reads, and cold segment hydration
        // together after a real S3-backed sweep.
        self.collect_garbage_with_remote_segments(limits, false)
    }

    fn collect_garbage_with_remote_segments(
        &self,
        limits: GcLimits,
        allow_remote_segments: bool,
    ) -> Result<GcReport, BuiltinModelError> {
        let roots_store = self.store.clone();
        let roots_authority = &self.authority;
        let s3_publisher = self.s3_publisher.as_deref();
        self.store
            .collect_garbage_resolving_roots(
                |roots| {
                    // Preserve every exact worker/input closure still named
                    // by the durable owner journal for every GC mode. Read
                    // without reopening a writable journal so collection
                    // cannot prune or race a concurrent ACK transition. These
                    // are retention roots only; they grant no selection or
                    // acknowledgement authority.
                    let pending_closures =
                        super::pending_stored::PendingStoredAckJournal::read_gc_retention_closure_ids(
                            &self.workspace,
                        )
                        .map_err(|error| {
                            backend_store::StoreError::Io(format!(
                                "read pending compiler ACK roots for semantic GC: {error}"
                            ))
                        })?;
                    for closure_claim in pending_closures {
                        let claim = ArtifactClosureClaim::from_bytes(closure_claim);
                        let manifest = roots_store.open_closure_claim(claim)?;
                        let retained_closure = manifest.id();
                        if retained_closure.as_bytes() != &closure_claim {
                            return Err(backend_store::StoreError::Corrupt);
                        }
                        roots.add(GcRoot::Closure(retained_closure));
                    }
                    let mut after_namespace: Option<AuthorityNamespace> = None;
                    loop {
                        let frontiers = futures_executor::block_on(
                            roots_authority
                                .selected_frontiers(after_namespace.as_ref(), AUTHORITY_PAGE),
                        )
                        .map_err(|error| {
                            backend_store::StoreError::Io(format!(
                                "read Turso roots for semantic GC: {error}"
                            ))
                        })?;
                        if frontiers.is_empty() {
                            break;
                        }
                        for frontier in frontiers.iter() {
                            let namespace = frontier.namespace().clone();
                            let mut after_generation = None;
                            loop {
                                let generations = futures_executor::block_on(
                                    roots_authority.selected_generations(
                                        &namespace,
                                        after_generation,
                                        AUTHORITY_PAGE,
                                    ),
                                )
                                .map_err(|error| {
                                    backend_store::StoreError::Io(format!(
                                        "read Turso semantic history for GC: {error}"
                                    ))
                                })?;
                                if generations.is_empty() {
                                    break;
                                }
                                for selected in generations.iter() {
                                    let manifest = roots_store.open_closure_claim(
                                        ArtifactClosureClaim::from_bytes(*selected.closure_id()),
                                    )?;
                                    let closure = manifest.id();
                                    if closure.as_bytes() != selected.closure_id() {
                                        return Err(backend_store::StoreError::Corrupt);
                                    }
                                    // Only the live head has a current selected-plane range
                                    // fallback. Historical rows remain complete local roots so
                                    // explicit rollback and stale client reads keep working.
                                    let remote_segments = if allow_remote_segments
                                        && selected.generation() == frontier.generation()
                                    {
                                        s3_publisher.and_then(|publisher| {
                                            publisher
                                                .verified_remote_segments(&roots_store, selected)
                                                .ok()
                                                .flatten()
                                        })
                                    } else {
                                        None
                                    };
                                    let Some(remote_segments) = remote_segments.filter(|segments| {
                                        segments.closure() == closure
                                            && !segments.object_ids().is_empty()
                                    }) else {
                                        roots.add(GcRoot::Closure(closure));
                                        continue;
                                    };
                                    let Some(publisher) = s3_publisher else {
                                        roots.add(GcRoot::Closure(closure));
                                        continue;
                                    };
                                    let selected_identity =
                                        super::s3_publication::RemoteClosureSelection::from_selected(
                                            selected,
                                        );
                                    let expected_members = remote_segments.object_ids().to_vec();
                                    let claims = expected_members.iter().copied().map(
                                        UntrustedObjectId::from_bytes,
                                    );
                                    roots.add_remote_closure_member_claims(
                                        closure,
                                        claims,
                                        |checked_closure, checked_members| {
                                            if checked_closure != closure
                                                || checked_members.len() != expected_members.len()
                                                || checked_members
                                                    .iter()
                                                    .zip(&expected_members)
                                                    .any(|(checked, expected)| {
                                                        checked.as_bytes() != expected
                                                    })
                                            {
                                                return Err(backend_store::StoreError::Corrupt);
                                            }
                                            let current = futures_executor::block_on(
                                                roots_authority.selected_generation(
                                                    selected.namespace(),
                                                    selected.generation(),
                                                ),
                                            )
                                            .map_err(|error| {
                                                backend_store::StoreError::Io(format!(
                                                    "recheck Turso generation for remote GC: {error}"
                                                ))
                                            })?
                                            .ok_or(backend_store::StoreError::Corrupt)?;
                                            if super::s3_publication::RemoteClosureSelection::from_selected(
                                                &current,
                                            ) != selected_identity
                                            {
                                                return Err(backend_store::StoreError::Corrupt);
                                            }
                                            let has_receipt = publisher
                                                .has_durable_selected_closure(
                                                    &roots_store,
                                                    selected_identity,
                                                )
                                                .map_err(|error| {
                                                    backend_store::StoreError::Io(format!(
                                                        "reopen exact S3 receipt for semantic GC: {error}"
                                                    ))
                                                })?;
                                            let current_segments = publisher
                                                .verified_remote_segments(&roots_store, &current)
                                                .map_err(|error| {
                                                    backend_store::StoreError::Io(format!(
                                                        "verify S3 segment fallback for semantic GC: {error}"
                                                    ))
                                                })?
                                                .ok_or(backend_store::StoreError::Corrupt)?;
                                            if !has_receipt
                                                || current_segments.closure() != checked_closure
                                                || current_segments.object_ids()
                                                    != expected_members.as_slice()
                                            {
                                                return Err(backend_store::StoreError::Corrupt);
                                            }
                                            Ok(())
                                        },
                                    )?;
                                }
                                after_generation =
                                    generations.last().map(|value| value.generation());
                            }
                        }
                        after_namespace = frontiers.last().map(|value| value.namespace().clone());
                    }
                    Ok(())
                },
                limits,
            )
            .map_err(|error| BuiltinModelError(format!("collect semantic artifact CAS: {error:?}")))
    }

    pub(crate) fn namespace(
        package: &backend_engine::PackageReference,
        coordinate: &backend_library::interface::PackageUrl,
        profile: LanguageProfile,
    ) -> Result<AuthorityNamespace, BuiltinModelError> {
        AuthorityNamespace::semantic_profile(
            package.as_str(),
            coordinate.as_str(),
            LOCAL_BRANCH,
            LOCAL_ENVIRONMENT,
            profile_name(profile),
        )
        .map_err(|error| BuiltinModelError(format!("semantic authority namespace: {error}")))
    }

    pub(crate) fn observe(
        &mut self,
        key: &ProductSemanticPublicationKey,
        input_digest: [u8; 32],
        count: u64,
    ) -> Result<SourceObservationReceipt, BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        if let Some(latest) =
            futures_executor::block_on(self.authority.latest_source_observation(&namespace))
                .map_err(|error| {
                    BuiltinModelError(format!("read prior semantic observation: {error}"))
                })?
            && latest.observation().revision() == Some(input_digest)
            && latest.observation().value() == &SourceObservationValue::KnownCount(count)
        {
            self.latest_observations.insert(key.clone(), latest.clone());
            return Ok(latest);
        }
        let observation = SourceObservation::new(
            namespace,
            Some(input_digest),
            unix_millis(),
            SourceObservationValue::KnownCount(count),
        )
        .map_err(|error| BuiltinModelError(format!("semantic source observation: {error}")))?;
        let receipt =
            futures_executor::block_on(self.authority.record_source_observation(observation))
                .map_err(|error| {
                    BuiltinModelError(format!("persist semantic source observation: {error}"))
                })?;
        self.latest_observations
            .insert(key.clone(), receipt.clone());
        Ok(receipt)
    }

    /// Acquires the Turso fence that a local compiler or cluster assignment
    /// must carry until its checked candidate reaches publication admission.
    pub(crate) fn begin_candidate_attempt(
        &mut self,
        key: &ProductSemanticPublicationKey,
        observation: &SourceObservationReceipt,
    ) -> Result<CandidateAttempt, BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        let input_digest = observation
            .observation()
            .revision()
            .ok_or_else(|| BuiltinModelError("semantic input digest is absent".to_owned()))?;
        if observation.observation().namespace() != &namespace {
            return Err(BuiltinModelError(
                "semantic source observation belongs to a different namespace".to_owned(),
            ));
        }
        futures_executor::block_on(self.authority.begin_attempt(
            &namespace,
            input_digest,
            observation,
        ))
        .map_err(|error| BuiltinModelError(format!("acquire semantic compiler attempt: {error}")))
    }

    /// Reopens the exact still-current Turso attempt named by a pending owner
    /// journal row. The journal claim cannot mint authority; Turso rechecks
    /// its observation, head, epoch, fence and attempt state in one snapshot.
    pub(crate) fn recover_candidate_attempt(
        &self,
        claim: &CandidateAttemptRecoveryClaim,
    ) -> Result<CandidateAttempt, BuiltinModelError> {
        futures_executor::block_on(self.authority.recover_candidate_attempt(claim)).map_err(
            |error| BuiltinModelError(format!("recover semantic compiler attempt: {error}")),
        )
    }

    /// Proves that the exact persisted compiler attempt has been superseded.
    ///
    /// `None` means Turso has no matching superseded record; it does not
    /// distinguish a still-current attempt from an unknown row. Database
    /// failures remain errors so recovery cannot turn an unavailable authority
    /// into a terminal result disposition.
    pub(crate) fn prove_superseded_attempt(
        &self,
        namespace: &AuthorityNamespace,
        attempt_id: [u8; 16],
        epoch: u64,
        fence: AuthorityHash,
        input_digest: AuthorityHash,
    ) -> Result<Option<backend_extension_turso::SupersededAttemptProof>, BuiltinModelError> {
        let Some(proof) = futures_executor::block_on(
            self.authority
                .superseded_attempt_proof(namespace, attempt_id, epoch, fence),
        )
        .map_err(|error| {
            BuiltinModelError(format!(
                "prove superseded semantic compiler attempt: {error}"
            ))
        })?
        else {
            return Ok(None);
        };
        if proof.namespace() != namespace
            || proof.attempt_id() != &attempt_id
            || proof.epoch() != epoch
            || proof.fence() != &fence
            || proof.input_digest() != &input_digest
            || proof.current_epoch() <= epoch
        {
            return Err(BuiltinModelError(
                "Turso returned a superseded proof for a different compiler attempt".to_owned(),
            ));
        }
        Ok(Some(proof))
    }

    /// Proves that an exact state-0 compiler attempt was invalidated by a newer
    /// source observation before another attempt began. This authorizes only a
    /// terminal Admission rejection; it cannot select or revive the attempt.
    pub(crate) fn prove_attempt_invalidated_by_observation(
        &self,
        claim: &CandidateAttemptRecoveryClaim,
    ) -> Result<Option<AttemptInvalidatedByObservationProof>, BuiltinModelError> {
        let Some(proof) = futures_executor::block_on(
            self.authority
                .attempt_invalidated_by_observation_proof(claim),
        )
        .map_err(|error| {
            BuiltinModelError(format!(
                "prove compiler attempt invalidated by source observation: {error}"
            ))
        })?
        else {
            return Ok(None);
        };
        if proof.current_observation_sequence() <= proof.attempt_observation_sequence() {
            return Err(BuiltinModelError(
                "Turso returned an invalidated-observation proof for a different attempt"
                    .to_owned(),
            ));
        }
        Ok(Some(proof))
    }

    /// Rechecks the persisted source observation immediately before remote admission.
    pub(crate) fn source_observation_is_current(
        &self,
        key: &ProductSemanticPublicationKey,
        expected: &SourceObservationReceipt,
    ) -> Result<bool, BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        let current =
            futures_executor::block_on(self.authority.latest_source_observation(&namespace))
                .map_err(|error| {
                    BuiltinModelError(format!("read current semantic source observation: {error}"))
                })?;
        Ok(current.as_ref() == Some(expected))
    }

    pub(crate) fn publish_staged(
        &mut self,
        key: &ProductSemanticPublicationKey,
        attempt: CandidateAttempt,
        staged: &backend_engine::application::StagedSemanticPackage,
        admit_before_select: impl FnOnce(
            &backend_extension_turso::CandidateGeneration,
        ) -> Result<(), BuiltinModelError>,
    ) -> Result<(SemanticPublicationClaim, SelectedGeneration), BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        if attempt.namespace() != &namespace {
            return Err(BuiltinModelError(
                "staged semantic attempt belongs to another product namespace".to_owned(),
            ));
        }
        let staged_input = staged.input_witness();
        let build_admission = if let Some(execution_identity) = staged.execution_identity() {
            if execution_identity.profile() != key.profile()
                || execution_identity.stage() != Stage::LowerIr
            {
                return Err(BuiltinModelError(
                    "staged semantic output differs from its portable execution identity"
                        .to_owned(),
                ));
            }
            CompilerBuildAdmission::from_local(execution_identity)
        } else if let Some(plane_identity) = staged.plane_execution_identity() {
            if plane_identity.profile() != key.profile() || plane_identity.stage() != Stage::LowerIr
            {
                return Err(BuiltinModelError(
                    "staged semantic output differs from its host-local plane identity".to_owned(),
                ));
            }
            CompilerBuildAdmission::from_host_local(plane_identity)
        } else {
            return Err(BuiltinModelError(
                "staged semantic output has no admitted runtime identity".to_owned(),
            ));
        };
        if staged_input.input_root() != attempt.input_digest() {
            return Err(BuiltinModelError(
                "staged semantic output differs from its exact compiler attempt".to_owned(),
            ));
        }
        let planes = staged
            .versioned_planes()
            .map_err(|error| BuiltinModelError(format!("admit staged semantic planes: {error}")))?;
        if planes.artifacts().len() != staged.artifacts().len() || planes.artifacts().is_empty() {
            return Err(BuiltinModelError(
                "staged semantic plane catalog differs from compiler artifact count".to_owned(),
            ));
        }

        let mut builder = self
            .store
            .begin_streaming_closure(streaming_budget())
            .map_err(|error| {
                BuiltinModelError(format!("begin staged semantic closure: {error:?}"))
            })?;
        let mut images = Vec::with_capacity(staged.artifacts().len());
        let mut payload_ids = BTreeSet::new();
        for (ordinal, artifact) in staged.artifacts().iter().enumerate() {
            let ordinal_u32 = u32::try_from(ordinal).map_err(|_| {
                BuiltinModelError("semantic artifact ordinal exceeds u32".to_owned())
            })?;
            let object_ordinal = ordinal
                .checked_mul(2)
                .and_then(|value| value.checked_add(2))
                .ok_or_else(|| BuiltinModelError("semantic output ordinal overflow".to_owned()))?;
            let output = staged.output_object(object_ordinal).ok_or_else(|| {
                BuiltinModelError("staged semantic image object is absent".to_owned())
            })?;
            let identity = *artifact.semantic_image().identity.as_ref();
            let object_id =
                stream_payload::<CompilerImagePayloadSchema>(&mut builder, output.bytes())?;
            payload_ids.insert(object_id);
            let object = self
                .store
                .verify_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))
                .map_err(|error| {
                    BuiltinModelError(format!("verify staged semantic image object: {error:?}"))
                })?;
            let member = CompilerImageMember::from_verified_object(ordinal_u32, object, identity)
                .map_err(|error| {
                BuiltinModelError(format!("admit staged semantic image: {error}"))
            })?;
            images.push(member);
        }

        let mut plane_artifacts = Vec::with_capacity(planes.artifacts().len());
        for (ordinal, staged_plane) in planes.artifacts().iter().enumerate() {
            let ordinal_u32 = u32::try_from(ordinal).map_err(|_| {
                BuiltinModelError("semantic artifact ordinal exceeds u32".to_owned())
            })?;
            if usize::try_from(staged_plane.artifact_ordinal()).ok() != Some(ordinal) {
                return Err(BuiltinModelError(
                    "staged semantic plane artifact order is not canonical".to_owned(),
                ));
            }
            let manifest =
                backend_semantic::ir::SemanticPlaneManifest::decode(staged_plane.manifest_bytes())
                    .map_err(|error| {
                        BuiltinModelError(format!("decode staged semantic plane manifest: {error}"))
                    })?;
            let expected_generation = staged.semantic_vcs_generation(ordinal).ok_or_else(|| {
                BuiltinModelError("staged semantic generation is absent".to_owned())
            })?;
            let input = manifest.input();
            if manifest.semantic_generation() != expected_generation
                || input.input_root() != attempt.input_digest()
                || input.coverage().state() != backend_version::Coverage::Partial
                || manifest.build().profile() != key.profile()
                || manifest.build().stage() != Stage::LowerIr
            {
                return Err(BuiltinModelError(
                    "staged semantic plane is not bound to the exact current compiler input and scope"
                        .to_owned(),
                ));
            }
            let manifest_object_id = stream_payload::<
                backend_semantic::ir::VersionedPlaneManifestSchema,
            >(&mut builder, staged_plane.manifest_bytes())?;
            payload_ids.insert(manifest_object_id);
            let verified_manifest = self
                .store
                .verify_object_claim(UntrustedObjectId::from_bytes(
                    *manifest_object_id.as_bytes(),
                ))
                .map_err(|error| {
                    BuiltinModelError(format!("verify staged semantic plane manifest: {error:?}"))
                })?;
            let mut members = Vec::with_capacity(staged_plane.segment_count());
            for segment_ordinal in 0..staged_plane.segment_count() {
                let segment = staged_plane.segment(segment_ordinal).ok_or_else(|| {
                    BuiltinModelError("staged semantic segment is absent".to_owned())
                })?;
                let object_id = stream_payload::<backend_semantic::ir::VersionedPlaneSegmentSchema>(
                    &mut builder,
                    segment.payload(),
                )?;
                payload_ids.insert(object_id);
                let verified = self
                    .store
                    .verify_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))
                    .map_err(|error| {
                        BuiltinModelError(format!("verify staged semantic segment: {error:?}"))
                    })?;
                members.push(
                    VersionedPlaneMember::from_verified_object(segment.id(), verified).map_err(
                        |error| {
                            BuiltinModelError(format!("admit staged semantic segment: {error:?}"))
                        },
                    )?,
                );
            }
            plane_artifacts.push(
                VersionedPlaneArtifactMetadata::from_verified_parts(
                    ordinal_u32,
                    staged_plane.manifest_bytes(),
                    verified_manifest,
                    members,
                )
                .map_err(|error| {
                    BuiltinModelError(format!("admit staged semantic plane metadata: {error:?}"))
                })?,
            );
        }
        let payload_pin = builder.seal_pinned().map_err(|error| {
            BuiltinModelError(format!("seal staged semantic output closure: {error:?}"))
        })?;
        let expected_payload_object_count = u64::try_from(payload_ids.len()).map_err(|_| {
            BuiltinModelError("staged semantic object count exceeds u64".to_owned())
        })?;
        if payload_pin.receipt().object_count() != expected_payload_object_count {
            return Err(BuiltinModelError(
                "staged semantic payload closure has an incomplete member count".to_owned(),
            ));
        }
        let plane_metadata =
            VersionedPlaneMetadata::from_artifacts(plane_artifacts).map_err(|error| {
                BuiltinModelError(format!("admit staged semantic plane catalog: {error:?}"))
            })?;
        let binding_bytes: [u8; 108] = staged.binding_bytes().try_into().map_err(|_| {
            BuiltinModelError("staged semantic binding has the wrong width".to_owned())
        })?;
        let metadata = CompilerPublicationMetadata::new_with_versioned_planes(
            staged.manifest_bytes(),
            binding_bytes,
            images,
            Some(plane_metadata),
        )
        .map_err(|error| {
            BuiltinModelError(format!("admit semantic publication metadata: {error}"))
        })?;
        let claim =
            SemanticPublicationClaim::admit(staged.manifest_facts(), staged.binding_facts())
                .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let admitted = AdmittedCompilation::new(
            key,
            attempt,
            metadata,
            payload_pin,
            None,
            None,
            None,
            claim,
            build_admission,
            &self.store,
        )?;
        self.select_verified_closure(key, admitted, admit_before_select)
    }

    /// Admits a remote result after the coordinator has reopened and typed its
    /// complete worker-output closure. The retained input proof must name the
    /// exact current source observation and complete workspace root; the owner
    /// callback rechecks that same source fence before selection. The worker
    /// closure is evidence for this call only; Turso selects a new minimal
    /// closure containing the exact envelope, canonical manifest/binding
    /// metadata, and image members.
    pub(crate) fn publish_checked_remote(
        &mut self,
        key: &ProductSemanticPublicationKey,
        observation: &SourceObservationReceipt,
        attempt: CandidateAttempt,
        admitted: backend_engine::application::AdmittedRemoteCompilerCandidate,
        expected_artifacts: u32,
        expected_source_fence_digest: [u8; 32],
        admit_before_select: impl FnOnce(
            &backend_extension_turso::CandidateGeneration,
        ) -> Result<(), BuiltinModelError>,
    ) -> Result<(SemanticPublicationClaim, SelectedGeneration), BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        if attempt.namespace() != &namespace
            || observation.observation().namespace() != &namespace
            || attempt.observation() != observation
            || observation.observation().revision().as_ref() != Some(attempt.input_digest())
        {
            return Err(BuiltinModelError(
                "remote semantic candidate has a different source observation".to_owned(),
            ));
        }

        let candidate = admitted.candidate();
        if candidate.work().selected_base().is_some() {
            return Err(BuiltinModelError(
                "remote semantic publication only admits a fresh full-workspace compile".to_owned(),
            ));
        }
        let input_admission = admitted.input_admission();
        let input_evidence = input_admission.evidence();
        let remote_scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            input_evidence.assignment(),
            namespace.namespace_id(),
        )
        .map_err(|error| {
            BuiltinModelError(format!("derive owner S3 publication fence: {error}"))
        })?;
        let input_manifest = input_evidence.manifest();
        let execution_grant = input_admission.execution_grant();
        let (grant_package, grant_target, grant_recipe) = execution_grant.work_facts();
        let (grant_toolchain, grant_environment, grant_platform) =
            execution_grant.execution_facts();
        // The captured input root is the source-observation revision. The
        // scanner fence has its own exact digest and is rechecked by the callback.
        if input_evidence.namespace_id() != namespace.namespace_id()
            || input_evidence.source_observation_revision() != *attempt.input_digest()
            || input_manifest.input_root() != *attempt.input_digest()
            || input_evidence.source_fence_digest() != expected_source_fence_digest
            || candidate.work().input().full_workspace().is_none()
            || input_manifest.selected_base().is_some()
            || input_evidence.assignment().work() != candidate.work()
            || input_evidence.assignment().token() != candidate.token()
            || execution_grant.peer() != candidate.peer()
            || execution_grant.namespace_id() != namespace.namespace_id()
            || grant_package != input_manifest.package_lineage()
            || grant_target != *input_manifest.package_target().target().as_ref()
            || grant_recipe != input_manifest.recipe()
            || grant_toolchain != input_manifest.toolchain()
            || grant_environment != input_manifest.environment()
            || grant_platform != input_manifest.target_platform()
        {
            return Err(BuiltinModelError(
                "remote semantic candidate lacks the exact fresh workspace, source fence, or trusted-worker admission"
                    .to_owned(),
            ));
        }
        let peer = EndpointId::from_bytes(&candidate.peer().as_bytes()).map_err(|_| {
            BuiltinModelError("remote compiler candidate has an invalid peer identity".to_owned())
        })?;
        if !self.authorizes_remote_scope(
            peer,
            namespace.namespace_id(),
            input_manifest.recipe(),
            input_manifest.profile(),
            input_manifest.stage(),
            input_manifest.toolchain(),
            input_manifest.environment(),
            input_manifest.target_platform(),
        )? {
            return Err(BuiltinModelError(
                "remote compiler peer is not trusted for this exact execution scope".to_owned(),
            ));
        }
        let candidate_receipt = candidate.closure_receipt();
        let stored_receipt = admitted.receipt();
        let admitted_closure = admitted.closure();
        let output = admitted.output();
        if candidate.token().attempt().get() != attempt.scheduler_attempt_id()
            || candidate.token().fence().as_bytes() != attempt.fence_bytes()
            || candidate_receipt != stored_receipt
            || candidate_receipt.closure() != admitted_closure
            || output.closure_id() != admitted_closure
        {
            return Err(BuiltinModelError(
                "remote semantic candidate differs from its Turso attempt or admitted closure"
                    .to_owned(),
            ));
        }
        if output.artifacts().len() != usize::try_from(expected_artifacts).unwrap_or(usize::MAX)
            || output.manifest_facts().fragment_count != expected_artifacts
            || output.versioned_plane_artifacts().len() != output.artifacts().len()
        {
            return Err(BuiltinModelError(
                "remote compiler result does not contain every expected source exactly once"
                    .to_owned(),
            ));
        }

        // Pin the already admitted output closure and reuse its exact CAS
        // members. The Turso verifier will independently stream and recheck
        // every semantic image, c005 manifest, and c004 segment before select.
        let evidence_pin =
            pin_existing_closure(&self.store, ArtifactClosureClaim::from_id(admitted_closure))?;
        if evidence_pin.receipt().closure() != admitted_closure
            || evidence_pin.receipt().object_count() != stored_receipt.object_count()
            || evidence_pin.receipt().bytes_verified() != stored_receipt.bytes_verified()
        {
            return Err(BuiltinModelError(
                "pinned remote compiler closure differs from its admitted receipt".to_owned(),
            ));
        }
        let output_index = self
            .store
            .read_closure_index(admitted_closure)
            .map_err(|error| {
                BuiltinModelError(format!("read admitted remote result index: {error:?}"))
            })?;
        let mut images = Vec::with_capacity(output.artifacts().len());
        let mut selected_payload_ids = BTreeSet::new();
        for (ordinal, artifact) in output.artifacts().iter().enumerate() {
            if usize::try_from(artifact.artifact_ordinal()).ok() != Some(ordinal)
                || usize::try_from(output.versioned_plane_artifacts()[ordinal].artifact_ordinal())
                    .ok()
                    != Some(ordinal)
                || output.versioned_plane_artifacts()[ordinal].semantic_generation()
                    != artifact.semantic_generation()
            {
                return Err(BuiltinModelError(
                    "remote compiler artifact and versioned plane order disagree".to_owned(),
                ));
            }
            let object_id = artifact.semantic_image_object_id();
            selected_payload_ids.insert(object_id);
            if !output_index
                .contains_object_id(object_id)
                .map_err(|error| {
                    BuiltinModelError(format!("check remote image membership: {error:?}"))
                })?
            {
                return Err(BuiltinModelError(
                    "remote compiler image is absent from its admitted result closure".to_owned(),
                ));
            }
            let verified = self
                .store
                .verify_object_claim(UntrustedObjectId::from_bytes(*object_id.as_bytes()))
                .map_err(|error| {
                    BuiltinModelError(format!("verify remote semantic image object: {error:?}"))
                })?;
            images.push(
                CompilerImageMember::from_verified_object(
                    artifact.artifact_ordinal(),
                    verified,
                    *artifact.semantic_image_facts().identity.as_ref(),
                )
                .map_err(|error| {
                    BuiltinModelError(format!("admit remote semantic image: {error}"))
                })?,
            );
        }

        let mut plane_artifacts = Vec::with_capacity(output.versioned_plane_artifacts().len());
        let mut total_manifest_bytes = 0_u64;
        for plane_artifact in output.versioned_plane_artifacts() {
            let manifest_id = plane_artifact.manifest_object_id();
            selected_payload_ids.insert(manifest_id);
            if !output_index
                .contains_object_id(manifest_id)
                .map_err(|error| {
                    BuiltinModelError(format!("check remote plane manifest membership: {error:?}"))
                })?
            {
                return Err(BuiltinModelError(
                    "remote plane manifest is absent from its admitted result closure".to_owned(),
                ));
            }
            let manifest_bytes = plane_artifact.manifest_bytes();
            total_manifest_bytes = total_manifest_bytes
                .checked_add(u64::try_from(manifest_bytes.len()).map_err(|_| {
                    BuiltinModelError("remote plane manifest length exceeds u64".to_owned())
                })?)
                .ok_or_else(|| {
                    BuiltinModelError("remote plane metadata size overflow".to_owned())
                })?;
            if total_manifest_bytes > MAX_METADATA_BYTES {
                return Err(BuiltinModelError(
                    "remote plane manifests exceed the selected metadata budget".to_owned(),
                ));
            }
            let manifest = backend_semantic::ir::SemanticPlaneManifest::decode(manifest_bytes)
                .map_err(|error| {
                    BuiltinModelError(format!("decode admitted remote plane manifest: {error}"))
                })?;
            let image = output
                .artifacts()
                .get(usize::try_from(plane_artifact.artifact_ordinal()).unwrap_or(usize::MAX))
                .ok_or_else(|| {
                    BuiltinModelError("remote plane artifact ordinal is out of range".to_owned())
                })?;
            if manifest.root() != plane_artifact.manifest_root()
                || manifest.semantic_generation() != plane_artifact.semantic_generation()
                || manifest.semantic_generation() != image.semantic_generation()
                || manifest.input().input_root() != attempt.input_digest()
            {
                return Err(BuiltinModelError(
                    "remote plane manifest differs from its admitted image, input, or root"
                        .to_owned(),
                ));
            }
            let verified_manifest = self
                .store
                .verify_object_claim(UntrustedObjectId::from_bytes(*manifest_id.as_bytes()))
                .map_err(|error| {
                    BuiltinModelError(format!("verify remote plane manifest object: {error:?}"))
                })?;
            let mut members = Vec::new();
            for plane in plane_artifact.planes() {
                for segment in plane.segments() {
                    selected_payload_ids.insert(segment.object_id());
                    if !output_index
                        .contains_object_id(segment.object_id())
                        .map_err(|error| {
                            BuiltinModelError(format!(
                                "check remote semantic segment membership: {error:?}"
                            ))
                        })?
                    {
                        return Err(BuiltinModelError(
                            "remote semantic segment is absent from its admitted result closure"
                                .to_owned(),
                        ));
                    }
                    let verified = self
                        .store
                        .verify_object_claim(UntrustedObjectId::from_bytes(
                            *segment.object_id().as_bytes(),
                        ))
                        .map_err(|error| {
                            BuiltinModelError(format!("verify remote semantic segment: {error:?}"))
                        })?;
                    members.push(
                        VersionedPlaneMember::from_verified_object(segment.segment_id(), verified)
                            .map_err(|error| {
                                BuiltinModelError(format!(
                                    "admit remote semantic segment: {error:?}"
                                ))
                            })?,
                    );
                }
            }
            plane_artifacts.push(
                VersionedPlaneArtifactMetadata::from_verified_parts(
                    plane_artifact.artifact_ordinal(),
                    manifest_bytes,
                    verified_manifest,
                    members,
                )
                .map_err(|error| {
                    BuiltinModelError(format!("admit remote versioned planes: {error:?}"))
                })?,
            );
        }
        let binding_bytes: [u8; 108] = output.binding_bytes().try_into().map_err(|_| {
            BuiltinModelError("remote semantic binding has the wrong width".to_owned())
        })?;
        let planes = VersionedPlaneMetadata::from_artifacts(plane_artifacts).map_err(|error| {
            BuiltinModelError(format!("admit remote semantic plane catalog: {error:?}"))
        })?;
        let changes = selected_payload_ids
            .iter()
            .copied()
            .map(ClosureMembershipChange::add)
            .collect::<Vec<_>>();
        let payload_pin = self
            .store
            .compose_closure_index(None, &changes, closure_composition_budget(changes.len())?)
            .map_err(|error| {
                BuiltinModelError(format!("compose exact remote semantic payload: {error:?}"))
            })?;
        let metadata = CompilerPublicationMetadata::new_with_versioned_planes(
            output.manifest_bytes(),
            binding_bytes,
            images,
            Some(planes),
        )
        .map_err(|error| {
            BuiltinModelError(format!(
                "admit remote semantic publication metadata: {error}"
            ))
        })?;
        let claim =
            SemanticPublicationClaim::admit(output.manifest_facts(), output.binding_facts())
                .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let source_pin = pin_existing_closure(
            &self.store,
            ArtifactClosureClaim::from_id(input_admission.evidence().capture().closure()),
        )?;
        let build_admission = CompilerBuildAdmission {
            target: *input_manifest.package_target().target().as_ref(),
            profile: input_manifest.profile(),
            stage: input_manifest.stage(),
            recipe: input_manifest.recipe(),
            toolchain: input_manifest.toolchain(),
            environment: input_manifest.environment(),
            target_platform: input_manifest.target_platform(),
        };
        let admitted_compilation = AdmittedCompilation::new(
            key,
            attempt,
            metadata,
            payload_pin,
            Some(source_pin),
            Some(evidence_pin),
            Some(remote_scope),
            claim,
            build_admission,
            &self.store,
        )?;
        let policy_path = self.compiler_trust_policy.clone();
        let scope = (
            peer,
            namespace.namespace_id(),
            input_manifest.recipe(),
            input_manifest.profile(),
            input_manifest.stage(),
            input_manifest.toolchain(),
            input_manifest.environment(),
            input_manifest.target_platform(),
        );
        self.select_verified_closure(key, admitted_compilation, move |candidate| {
            if !Self::authorizes_remote_scope_at(&policy_path, scope)? {
                return Err(BuiltinModelError(
                    "remote compiler peer trust changed before semantic selection".to_owned(),
                ));
            }
            admit_before_select(candidate)
        })
    }

    fn authorizes_remote_scope(
        &self,
        peer: EndpointId,
        namespace_id: [u8; 16],
        recipe: [u8; 32],
        profile: LanguageProfile,
        stage: Stage,
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
    ) -> Result<bool, BuiltinModelError> {
        Self::authorizes_remote_scope_at(
            &self.compiler_trust_policy,
            (
                peer,
                namespace_id,
                recipe,
                profile,
                stage,
                toolchain,
                environment,
                target_platform,
            ),
        )
    }

    fn authorizes_remote_scope_at(
        path: &Path,
        scope: (
            EndpointId,
            [u8; 16],
            [u8; 32],
            LanguageProfile,
            Stage,
            [u8; 32],
            [u8; 32],
            [u8; 32],
        ),
    ) -> Result<bool, BuiltinModelError> {
        let policy = TrustedCompilerWorkerPolicy::load(path).map_err(|error| {
            BuiltinModelError(format!("load owner compiler trust policy: {error}"))
        })?;
        Ok(policy.authorizes(
            scope.0, scope.1, scope.2, scope.3, scope.4, scope.5, scope.6, scope.7,
        ))
    }

    /// The one publication kernel shared by local staged output and remote
    /// output after its source closure has been admitted into the local CAS.
    /// Callers supply typed semantic metadata and image objects; this method
    /// creates the Turso envelope, stores its exact closure, verifies every
    /// member, checks the caller's live-source fence, and advances Turso last.
    fn select_verified_closure(
        &mut self,
        key: &ProductSemanticPublicationKey,
        admitted: AdmittedCompilation,
        admit_before_select: impl FnOnce(
            &backend_extension_turso::CandidateGeneration,
        ) -> Result<(), BuiltinModelError>,
    ) -> Result<(SemanticPublicationClaim, SelectedGeneration), BuiltinModelError> {
        let AdmittedCompilation {
            attempt,
            metadata,
            payload_pin,
            source_pin,
            evidence_pin,
            publication_fence,
            claim,
        } = admitted;
        let namespace = attempt.namespace().clone();
        let envelope = CompilerPublicationEnvelope::new(&attempt, &metadata).map_err(|error| {
            BuiltinModelError(format!("build semantic publication envelope: {error}"))
        })?;
        let metadata_object = metadata.typed_object();
        let envelope_object = envelope.typed_object();
        let mut metadata_builder = self
            .store
            .begin_streaming_closure(streaming_budget())
            .map_err(|error| {
                BuiltinModelError(format!("begin semantic metadata closure: {error:?}"))
            })?;
        for object in [&metadata_object, &envelope_object] {
            let stored = stream_typed_object(&mut metadata_builder, object)?;
            if stored != object.id() {
                return Err(BuiltinModelError(
                    "streamed semantic metadata differs from its typed identity".to_owned(),
                ));
            }
        }
        let metadata_pin = metadata_builder.seal_pinned().map_err(|error| {
            BuiltinModelError(format!("seal pinned semantic metadata: {error:?}"))
        })?;
        let mut changes = vec![
            ClosureMembershipChange::add(metadata_object.id()),
            ClosureMembershipChange::add(envelope_object.id()),
        ];
        changes.sort_unstable_by_key(|change| change.object_id());
        let compose_budget = closure_composition_budget(changes.len())?;
        let selected_pin = self
            .store
            .compose_closure_index(
                Some(ArtifactClosureClaim::from_id(
                    payload_pin.receipt().closure(),
                )),
                &changes,
                compose_budget,
            )
            .map_err(|error| {
                BuiltinModelError(format!("compose semantic publication closure: {error:?}"))
            })?;
        let closure_id = *selected_pin.receipt().closure().as_bytes();
        let _metadata_closure_id = metadata_pin.receipt().closure();
        let _source_pin = source_pin;
        let _evidence_pin = evidence_pin;

        let admitted_attempt = attempt.clone();
        let candidate = envelope.candidate(attempt, closure_id).map_err(|error| {
            BuiltinModelError(format!("bind semantic publication candidate: {error}"))
        })?;
        let receipt = verify_then_publish_selected_closure(
            || {
                self.authority
                    .verify_compiler_publication(
                        &candidate,
                        &admitted_attempt,
                        &self.store,
                        artifact_budget(),
                        &envelope,
                        &metadata,
                    )
                    .map_err(|error| {
                        BuiltinModelError(format!("verify semantic publication closure: {error}"))
                    })
            },
            |_| {
                self.publish_selected_closure(
                    selected_pin.receipt().closure(),
                    closure_id,
                    *candidate.target_root(),
                    selected_pin.receipt().object_count(),
                    publication_fence,
                )
            },
        )?;
        admit_before_select(&candidate)?;
        let frontier =
            futures_executor::block_on(self.authority.compare_and_select(candidate, receipt))
                .map_err(|error| {
                    BuiltinModelError(format!("select semantic publication: {error}"))
                })?;
        let selected = futures_executor::block_on(
            self.authority
                .selected_generation(&namespace, frontier.generation()),
        )
        .map_err(|error| {
            BuiltinModelError(format!("reopen selected semantic generation: {error}"))
        })?
        .ok_or_else(|| BuiltinModelError("selected semantic generation disappeared".to_owned()))?;
        self.remember_selection(key.clone(), claim, selected.clone())?;
        self.history.insert(
            (key.clone(), *claim.binding().identity.as_ref()),
            HistoryFact {
                selected: selected.clone(),
            },
        );
        self.retained_generations.insert(
            (key.clone(), *claim.binding().identity.as_ref()),
            selected.generation(),
        );
        Ok((claim, selected))
    }

    fn publish_selected_closure(
        &self,
        closure: ClosureId,
        closure_id: [u8; 32],
        target_root: [u8; 32],
        expected_count: u64,
        publication_fence: PublicationFence,
    ) -> Result<(), BuiltinModelError> {
        let Some(publisher) = self.s3_publisher.as_ref() else {
            return Ok(());
        };
        let stored_receipt = publisher
            .publish_closure(
                &self.store,
                closure,
                target_root,
                expected_count,
                artifact_budget(),
                publication_fence,
            )
            .map_err(|error| {
                BuiltinModelError(format!("store exact selected closure in S3: {error}"))
            })?;
        stored_receipt
            .validate_for_selection(closure_id, target_root, publication_fence, expected_count)
            .map_err(|error| {
                BuiltinModelError(format!(
                    "verify exact selected closure S3 receipt before Turso selection: {error}"
                ))
            })
    }

    fn remember_selection(
        &self,
        key: ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
        selected: SelectedGeneration,
    ) -> Result<(), BuiltinModelError> {
        self.image_loader.remember(key, claim, selected)
    }

    pub(crate) fn freshness(
        &self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
    ) -> backend_engine::SemanticVersionFreshness {
        let history_key = (key.clone(), *claim.binding().identity.as_ref());
        let Some(history) = self.history.get(&history_key) else {
            return backend_engine::SemanticVersionFreshness::Unverified;
        };
        let Some(latest) = self.latest_observations.get(key) else {
            return backend_engine::SemanticVersionFreshness::Unverified;
        };
        let selected_input = *history.selected.input_digest();
        let Some(latest_input) = latest.observation().revision() else {
            return backend_engine::SemanticVersionFreshness::Unverified;
        };
        if history.selected.observation().sequence() == latest.sequence()
            && selected_input == latest_input
        {
            backend_engine::SemanticVersionFreshness::Current {
                input_digest: selected_input,
            }
        } else {
            backend_engine::SemanticVersionFreshness::Historical {
                selected_input,
                latest_input,
            }
        }
    }

    pub(crate) fn retained_generation(
        &self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
    ) -> Result<u64, BuiltinModelError> {
        self.retained_generations
            .get(&(key.clone(), *claim.binding().identity.as_ref()))
            .copied()
            .ok_or_else(|| {
                BuiltinModelError("semantic generation is absent from Turso history".to_owned())
            })
    }

    pub(crate) fn select_existing(
        &mut self,
        key: &ProductSemanticPublicationKey,
        claim: SemanticPublicationClaim,
    ) -> Result<(SelectedGeneration, ProductSemanticPublicationRecord), BuiltinModelError> {
        let namespace = Self::namespace(key.package(), key.coordinate(), key.profile())?;
        let retained_generation = self.retained_generation(key, claim)?;
        let retained = futures_executor::block_on(
            self.authority
                .selected_generation(&namespace, retained_generation),
        )
        .map_err(|error| BuiltinModelError(format!("read retained semantic generation: {error}")))?
        .ok_or_else(|| BuiltinModelError("semantic generation is no longer retained".to_owned()))?;
        let latest =
            futures_executor::block_on(self.authority.latest_source_observation(&namespace))
                .map_err(|error| {
                    BuiltinModelError(format!("read latest semantic source observation: {error}"))
                })?;
        if let Some(latest) = latest.as_ref() {
            self.latest_observations.insert(key.clone(), latest.clone());
        }
        let intent = latest.as_ref().map_or(
            ExistingGenerationSelection::AcknowledgeHistorical,
            |latest| {
                if retained.observation().sequence() == latest.sequence()
                    && latest.observation().revision().as_ref() == Some(retained.input_digest())
                {
                    ExistingGenerationSelection::RequireCurrentObservation
                } else {
                    ExistingGenerationSelection::AcknowledgeHistorical
                }
            },
        );
        let expected = futures_executor::block_on(self.authority.selected_frontier(&namespace))
            .map_err(|error| {
                BuiltinModelError(format!("read selected semantic frontier: {error}"))
            })?;
        let frontier =
            futures_executor::block_on(self.authority.select_existing_compiler_generation(
                &namespace,
                expected.as_ref(),
                retained_generation,
                intent,
                &self.store,
            ))
            .map_err(|error| {
                BuiltinModelError(format!("select retained semantic generation: {error}"))
            })?;
        let selected = futures_executor::block_on(
            self.authority
                .selected_generation(&namespace, frontier.generation()),
        )
        .map_err(|error| BuiltinModelError(format!("read selected semantic rollback: {error}")))?
        .ok_or_else(|| BuiltinModelError("selected semantic rollback disappeared".to_owned()))?;
        self.remember_selection(key.clone(), claim, selected.clone())?;
        self.history.insert(
            (key.clone(), *claim.binding().identity.as_ref()),
            HistoryFact {
                selected: selected.clone(),
            },
        );
        self.retained_generations.insert(
            (key.clone(), *claim.binding().identity.as_ref()),
            selected.generation(),
        );
        let record = ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim,
        };
        Ok((selected, record))
    }

    /// Rebuilds the workspace semantic relation from every locald-selected
    /// Turso head before a persisted view journal can be admitted.
    pub(crate) fn reconcile_workspace(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> Result<(), BuiltinModelError> {
        let mut desired =
            BTreeMap::<ProductSemanticPublicationKey, ProductSemanticPublicationRecord>::new();
        let mut after: Option<AuthorityNamespace> = None;
        loop {
            let page = futures_executor::block_on(
                self.authority
                    .selected_frontiers(after.as_ref(), AUTHORITY_PAGE),
            )
            .map_err(|error| {
                BuiltinModelError(format!("enumerate semantic authority heads: {error}"))
            })?;
            if page.is_empty() {
                break;
            }
            for frontier in page.iter() {
                after = Some(frontier.namespace().clone());
                if !is_local_semantic_namespace(frontier.namespace()) {
                    continue;
                }
                let key = key_from_namespace(frontier.namespace())?;
                let mut generation_after = None;
                loop {
                    let generations =
                        futures_executor::block_on(self.authority.selected_generations(
                            frontier.namespace(),
                            generation_after,
                            AUTHORITY_PAGE,
                        ))
                        .map_err(|error| {
                            BuiltinModelError(format!("enumerate semantic history: {error}"))
                        })?;
                    if generations.is_empty() {
                        break;
                    }
                    for selected in generations.iter() {
                        generation_after = Some(selected.generation());
                        let (claim, record) = reopen_record(&self.store, selected)?;
                        let history_key = key
                            .for_generation_bytes(*claim.binding().identity.as_ref())
                            .map_err(|error| BuiltinModelError(error.to_owned()))?;
                        if let Some(prior) = desired.insert(history_key.clone(), record.clone())
                            && prior != record
                        {
                            return Err(BuiltinModelError(
                                "Turso history rebound one immutable semantic generation"
                                    .to_owned(),
                            ));
                        }
                        self.remember_selection(key.clone(), claim, selected.clone())?;
                        let history_key = (key.clone(), *claim.binding().identity.as_ref());
                        self.history.insert(
                            history_key.clone(),
                            HistoryFact {
                                selected: selected.clone(),
                            },
                        );
                        self.retained_generations
                            .insert(history_key, selected.generation());
                    }
                    if generations.len() < AUTHORITY_PAGE {
                        break;
                    }
                }
                let selected = futures_executor::block_on(
                    self.authority
                        .selected_generation(frontier.namespace(), frontier.generation()),
                )
                .map_err(|error| {
                    BuiltinModelError(format!("read semantic authority head: {error}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError("semantic authority head is absent from history".to_owned())
                })?;
                let (claim, record) = reopen_record(&self.store, &selected)?;
                desired.insert(key.clone(), record);
                self.remember_selection(key.clone(), claim, selected)?;
                if let Some(observation) = futures_executor::block_on(
                    self.authority
                        .latest_source_observation(frontier.namespace()),
                )
                .map_err(|error| {
                    BuiltinModelError(format!("read latest semantic source observation: {error}"))
                })? {
                    self.latest_observations.insert(key, observation);
                }
            }
            if page.len() < AUTHORITY_PAGE {
                break;
            }
        }

        let relation = daemon
            .engine()
            .daemon()
            .owner()
            .snapshot()
            .relation::<BuiltinSemanticRelation>()
            .map_err(|error| BuiltinModelError(format!("open semantic projection: {error}")))?;
        // Check the independently persisted typed coverage against the exact
        // selected source-count and manifest evidence used to rebuild it. A
        // disagreement means neither projection can safely be admitted.
        for (key, desired_record) in &desired {
            let Some(ProductSemanticPublicationRecord::Published {
                coverage: existing_coverage,
                claim: existing_claim,
            }) = relation.lookup(key).map_err(|error| {
                BuiltinModelError(format!("read semantic publication coverage: {error}"))
            })?
            else {
                continue;
            };
            if let ProductSemanticPublicationRecord::Published { coverage, claim } = desired_record
                && existing_claim == *claim
            {
                ensure_recovered_coverage_matches(existing_coverage, *coverage)?;
            }
        }
        let mut changes = BTreeMap::<
            ProductSemanticPublicationKey,
            Option<ProductSemanticPublicationRecord>,
        >::new();
        let mut relation_after = None;
        loop {
            let page = relation
                .page(
                    relation_after.as_ref(),
                    backend_engine::MAX_SNAPSHOT_PAGE_ROWS,
                )
                .map_err(|error| BuiltinModelError(format!("page semantic projection: {error}")))?;
            for (key, record) in page.entries() {
                if let Some(desired_record) = desired.get(key) {
                    if desired_record != record {
                        changes.insert(key.clone(), Some(desired_record.clone()));
                    }
                } else if matches!(record, ProductSemanticPublicationRecord::Unavailable(_)) {
                    // A typed unavailable terminal records failed acquisition;
                    // it is not an authoritative generation head and remains
                    // a valid product status when no selected closure exists.
                } else {
                    changes.insert(key.clone(), None);
                }
            }
            let Some(next) = page.next().cloned() else {
                break;
            };
            relation_after = Some(next);
        }
        for (key, record) in desired {
            if relation
                .lookup(&key)
                .map_err(|error| BuiltinModelError(format!("read semantic projection: {error}")))?
                != Some(record.clone())
            {
                changes.insert(key, Some(record));
            }
        }
        let mut by_package =
            BTreeMap::<backend_engine::PackageKey, (String, Vec<BuiltinSemanticChange>)>::new();
        for (key, after) in changes {
            let package = key.package_key();
            let label = key.package().as_str().to_owned();
            by_package
                .entry(package)
                .or_insert_with(|| (label, Vec::new()))
                .1
                .push(BuiltinSemanticChange { key, after });
        }
        for (package, (label, semantic_changes)) in by_package {
            let intent = BuiltinIntent::index_with_semantics(
                package,
                label,
                Vec::<BuiltinSourceChange>::new(),
                semantic_changes,
            )?;
            super::commands::commit_builtin_intent(daemon, 0, &intent)?;
        }
        #[cfg(feature = "cluster-process-journey-hooks")]
        if std::env::var_os("BACKEND_JOURNEY_REMOTE_SEGMENT_GC")
            .is_some_and(|value| value.to_str() == Some("1"))
        {
            // Exercise the ordinary production root policy before the
            // stronger journey-only remote residency policy. Pending worker
            // and captured-input closures must survive either pass.
            self.collect_garbage(GcLimits::default())?;
            self.collect_garbage_with_remote_segments(GcLimits::default(), true)?;
        }
        Ok(())
    }

    pub(crate) fn mark_projections_current(&mut self) -> Result<(), BuiltinModelError> {
        let mut after: Option<AuthorityNamespace> = None;
        loop {
            let page = futures_executor::block_on(
                self.authority
                    .selected_frontiers(after.as_ref(), AUTHORITY_PAGE),
            )
            .map_err(|error| {
                BuiltinModelError(format!("enumerate semantic projection watermarks: {error}"))
            })?;
            if page.is_empty() {
                break;
            }
            for frontier in page.iter() {
                after = Some(frontier.namespace().clone());
                if !is_local_semantic_namespace(frontier.namespace()) {
                    continue;
                }
                for projector in [
                    ProjectionKind::Catalog,
                    ProjectionKind::Graph,
                    ProjectionKind::Lexical,
                ] {
                    futures_executor::block_on(self.authority.mark_projection_current(
                        frontier.namespace(),
                        projector,
                        frontier.generation(),
                        *frontier.target_root(),
                    ))
                    .map_err(|error| {
                        BuiltinModelError(format!("advance semantic projection watermark: {error}"))
                    })?;
                }
            }
            if page.len() < AUTHORITY_PAGE {
                break;
            }
        }
        Ok(())
    }
}

fn reopen_record(
    store: &FileStore,
    selected: &SelectedGeneration,
) -> Result<(SemanticPublicationClaim, ProductSemanticPublicationRecord), BuiltinModelError> {
    let reopened = reopen_selected_compiler_metadata(store, selected)
        .map_err(|error| BuiltinModelError(format!("reopen selected semantic history: {error}")))?;
    let metadata = reopened.metadata();
    let mut fragment_slots = vec![None; metadata.manifest_fragment_count() as usize];
    let manifest = backend_engine::publication::manifest::CompilationManifestView::validate(
        metadata.manifest_bytes(),
        &mut fragment_slots,
    )
    .map_err(|error| BuiltinModelError(format!("validate reopened semantic manifest: {error}")))?;
    if metadata.manifest_fragment_count() != manifest.fragment_count {
        return Err(BuiltinModelError(
            "reopened semantic manifest count differs from its durable metadata".to_owned(),
        ));
    }
    let binding = backend_engine::publication::binding::CompilationBindingView::validate(
        metadata.binding_bytes(),
    )
    .map_err(|error| BuiltinModelError(format!("validate reopened semantic binding: {error}")))?;
    let claim = SemanticPublicationClaim::admit(*manifest, *binding)
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
    let coverage = recovered_publication_coverage(
        selected.observation().observation().value(),
        manifest.fragment_count,
    )?;
    Ok((
        claim,
        ProductSemanticPublicationRecord::Published { coverage, claim },
    ))
}

/// Reconstructs project-scope coverage when an authority-selected generation
/// has no matching workspace relation row. Turso binds the selected generation
/// to a durable source-count observation, and its immutable compiler manifest
/// retains the number of published source artifacts. Local selection admits
/// gaps only after proving `artifacts + gaps == observed source count`; remote
/// selection requires exact equality. Unknown or inconsistent counts therefore
/// cannot safely be promoted to Complete.
fn recovered_publication_coverage(
    source_observation: &SourceObservationValue,
    completed: u32,
) -> Result<SemanticPublicationCoverage, BuiltinModelError> {
    let SourceObservationValue::KnownCount(total) = source_observation else {
        return Err(BuiltinModelError(
            "selected semantic generation has no durable source-scope count; refusing to recover publication coverage"
                .to_owned(),
        ));
    };
    let total = u32::try_from(*total).map_err(|_| {
        BuiltinModelError("selected semantic source-scope count exceeds u32".to_owned())
    })?;
    let completed = NonZeroU32::new(completed).ok_or_else(|| {
        BuiltinModelError("selected semantic generation has no published artifacts".to_owned())
    })?;
    let total = NonZeroU32::new(total).ok_or_else(|| {
        BuiltinModelError("selected semantic generation has an empty source scope".to_owned())
    })?;
    match completed.get().cmp(&total.get()) {
        std::cmp::Ordering::Equal => Ok(SemanticPublicationCoverage::Complete),
        std::cmp::Ordering::Less => Ok(SemanticPublicationCoverage::Partial(
            PartialSemanticCoverage::new(completed, total)
                .map_err(|error| BuiltinModelError(error.to_owned()))?,
        )),
        std::cmp::Ordering::Greater => Err(BuiltinModelError(
            "selected semantic manifest exceeds its durable source-scope count".to_owned(),
        )),
    }
}

fn ensure_recovered_coverage_matches(
    persisted: SemanticPublicationCoverage,
    recovered: SemanticPublicationCoverage,
) -> Result<(), BuiltinModelError> {
    if persisted == recovered {
        Ok(())
    } else {
        Err(BuiltinModelError(
            "persisted semantic coverage differs from the selected source count and manifest"
                .to_owned(),
        ))
    }
}

fn artifact_object_claim(object: &TypedObject) -> Result<ArtifactObjectClaim, BuiltinModelError> {
    let length = u64::try_from(object.bytes().len())
        .map_err(|_| BuiltinModelError("semantic object length overflow".to_owned()))?;
    Ok(
        ArtifactObjectClaim::new(object.schema(), *object.key(), *object.version(), length)
            .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes())),
    )
}

fn is_local_semantic_namespace(namespace: &AuthorityNamespace) -> bool {
    namespace.branch() == LOCAL_BRANCH
        && namespace.environment() == LOCAL_ENVIRONMENT
        && namespace.plane().profile().is_some()
}

fn key_from_namespace(
    namespace: &AuthorityNamespace,
) -> Result<ProductSemanticPublicationKey, BuiltinModelError> {
    let package = backend_engine::PackageReference::parse(namespace.package().to_owned()).map_err(
        |error| BuiltinModelError(format!("decode semantic authority package: {error:?}")),
    )?;
    let coordinate = backend_library::interface::PackageUrl::parse(namespace.source().to_owned())
        .map_err(|error| {
        BuiltinModelError(format!("decode semantic authority coordinate: {error:?}"))
    })?;
    let profile_name = namespace.plane().profile().ok_or_else(|| {
        BuiltinModelError("semantic authority namespace has no profile plane".to_owned())
    })?;
    let code = profile_code(profile_name)?;
    let profile = LanguageProfile::try_from(code).map_err(|error| {
        BuiltinModelError(format!("decode semantic authority profile: {error}"))
    })?;
    ProductSemanticPublicationKey::new(package, coordinate, profile)
        .map_err(|error| BuiltinModelError(error.to_owned()))
}

fn profile_name(profile: LanguageProfile) -> String {
    let code = <[u8; 2]>::from(profile);
    format!("{:02x}{:02x}/lower-ir", code[0], code[1])
}

fn profile_code(profile_name: &str) -> Result<[u8; 2], BuiltinModelError> {
    let hex = profile_name.strip_suffix("/lower-ir").ok_or_else(|| {
        BuiltinModelError("semantic authority profile stage is unsupported".to_owned())
    })?;
    if hex.len() != 4 {
        return Err(BuiltinModelError(
            "semantic authority profile code is malformed".to_owned(),
        ));
    }
    let mut code = [0_u8; 2];
    for (index, byte) in code.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&hex[start..start + 2], 16).map_err(|error| {
            BuiltinModelError(format!("semantic authority profile code: {error}"))
        })?;
    }
    Ok(code)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;
    use backend_semantic::vocabulary::{PackageUrl, RustEdition};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn recovered_coverage_uses_the_selected_source_scope_and_fails_closed() {
        let partial = PartialSemanticCoverage::new(
            NonZeroU32::new(3).expect("nonzero completed count"),
            NonZeroU32::new(5).expect("nonzero source count"),
        )
        .expect("strictly partial coverage");
        assert_eq!(
            recovered_publication_coverage(&SourceObservationValue::KnownCount(5), 3)
                .expect("derive exact partial source coverage"),
            SemanticPublicationCoverage::Partial(partial),
        );
        assert_eq!(
            recovered_publication_coverage(&SourceObservationValue::KnownCount(5), 5)
                .expect("derive exact complete source coverage"),
            SemanticPublicationCoverage::Complete,
        );
        assert!(recovered_publication_coverage(&SourceObservationValue::Unknown, 3).is_err());
        assert!(
            recovered_publication_coverage(
                &SourceObservationValue::Unavailable("source count unavailable".into()),
                3,
            )
            .is_err()
        );
        assert!(recovered_publication_coverage(&SourceObservationValue::KnownCount(3), 4).is_err());
        assert!(recovered_publication_coverage(&SourceObservationValue::KnownCount(5), 0).is_err());
    }

    #[test]
    fn persisted_coverage_cannot_override_selected_count_evidence() {
        let partial = SemanticPublicationCoverage::Partial(
            PartialSemanticCoverage::new(
                NonZeroU32::new(3).expect("nonzero completed count"),
                NonZeroU32::new(5).expect("nonzero source count"),
            )
            .expect("strictly partial coverage"),
        );
        assert!(
            ensure_recovered_coverage_matches(SemanticPublicationCoverage::Complete, partial,)
                .is_err()
        );
        assert!(ensure_recovered_coverage_matches(partial, partial).is_ok());
    }

    struct ScratchWorkspace(std::path::PathBuf);

    impl ScratchWorkspace {
        fn new() -> Self {
            let sequence = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-semantic-authority-cutover-{}-{sequence}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create scratch workspace");
            Self(path)
        }
    }

    impl Drop for ScratchWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct RejectingS3Publisher {
        seen: Arc<std::sync::Mutex<Option<PublicationFence>>>,
    }

    impl SelectedClosurePublisher for RejectingS3Publisher {
        fn publish_closure(
            &self,
            _store: &FileStore,
            _closure: ClosureId,
            _target_root: [u8; 32],
            _expected_count: u64,
            _budget: ArtifactBudget,
            publication_fence: PublicationFence,
        ) -> Result<
            super::super::s3_publication::ExactS3ClosureReceipt,
            super::super::s3_publication::PublicationError,
        > {
            *self.seen.lock().expect("lock recording publisher") = Some(publication_fence);
            Err(super::super::s3_publication::PublicationError::Remote)
        }

        fn hydrate_object(
            &self,
            _store: &FileStore,
            _selected: super::super::s3_publication::RemoteClosureSelection,
            _object_id: UntrustedObjectId,
            _expected_schema: backend_version::SchemaIdentity,
            _expected_payload_len: u64,
        ) -> Result<Vec<u8>, super::super::s3_publication::PublicationError> {
            Err(super::super::s3_publication::PublicationError::Remote)
        }

        fn has_durable_selected_closure(
            &self,
            _store: &FileStore,
            _selected: super::super::s3_publication::RemoteClosureSelection,
        ) -> Result<bool, super::super::s3_publication::PublicationError> {
            Ok(false)
        }

        fn verified_remote_segments(
            &self,
            _store: &FileStore,
            _selected: &SelectedGeneration,
        ) -> Result<
            Option<super::super::s3_publication::VerifiedRemoteSegmentSet>,
            super::super::s3_publication::PublicationError,
        > {
            Ok(None)
        }
    }

    #[test]
    fn local_candidate_selection_calls_configured_s3_before_advancing_head() {
        let workspace = ScratchWorkspace::new();
        let package =
            backend_engine::PackageReference::parse("pkg:cargo/local-s3-fence@1.0.0".to_owned())
                .expect("valid semantic package");
        let coordinate = PackageUrl::parse("pkg:cargo/local-s3-fence@1.0.0".to_owned())
            .expect("valid semantic coordinate");
        let profile = LanguageProfile::Rust(RustEdition::Rust2021);
        let key = ProductSemanticPublicationKey::new(package, coordinate, profile)
            .expect("valid semantic selection key");

        let mut authority = SemanticAuthority::open(&workspace.0).expect("open authority");
        let observation = authority
            .observe(&key, [0x31; 32], 1)
            .expect("record local source observation");
        let attempt = authority
            .begin_candidate_attempt(&key, &observation)
            .expect("begin exact local Turso attempt");
        let publication_fence =
            PublicationFence::for_attempt(&attempt, None).expect("local Turso publication fence");
        assert!(publication_fence.assignment_scope().is_none());

        let mut builder = authority
            .store
            .begin_streaming_closure(streaming_budget())
            .expect("begin test selected closure");
        stream_payload::<CompilerImagePayloadSchema>(&mut builder, b"local selected image")
            .expect("store test selected object");
        let pin = builder.seal_pinned().expect("seal test selected closure");
        let seen = Arc::new(std::sync::Mutex::new(None));
        authority.s3_publisher = Some(Box::new(RejectingS3Publisher {
            seen: Arc::clone(&seen),
        }));

        let rejected = verify_then_publish_selected_closure(
            || Err::<(), _>(BuiltinModelError("local candidate rejected".to_owned())),
            |_| {
                authority.publish_selected_closure(
                    pin.receipt().closure(),
                    *pin.receipt().closure().as_bytes(),
                    [0x52; 32],
                    pin.receipt().object_count(),
                    publication_fence,
                )
            },
        );
        assert!(rejected.is_err());
        assert_eq!(
            *seen.lock().expect("read rejected candidate publisher"),
            None,
            "an invalid local candidate must not cause a remote S3 upload"
        );

        let result = verify_then_publish_selected_closure(
            || Ok(()),
            |_| {
                authority.publish_selected_closure(
                    pin.receipt().closure(),
                    *pin.receipt().closure().as_bytes(),
                    [0x52; 32],
                    pin.receipt().object_count(),
                    publication_fence,
                )
            },
        );
        assert!(
            result.is_err(),
            "storage rejection must stop local selection"
        );
        assert_eq!(
            *seen.lock().expect("read recording publisher"),
            Some(publication_fence),
            "the configured S3 path receives the exact local Turso attempt fence"
        );
        let namespace = attempt.namespace().clone();
        assert!(
            futures_executor::block_on(authority.authority.selected_frontier(&namespace))
                .expect("read local selected frontier")
                .is_none(),
            "the selected head cannot advance when S3 has no exact receipt"
        );
    }

    #[test]
    fn observation_first_survives_cold_reopen_before_source_projection_commit() {
        let workspace = ScratchWorkspace::new();
        let package =
            backend_engine::PackageReference::parse("pkg:cargo/authority-cutover@1.0.0".to_owned())
                .expect("valid semantic package");
        let coordinate = PackageUrl::parse("pkg:cargo/authority-cutover@1.0.0".to_owned())
            .expect("valid semantic coordinate");
        let profile = LanguageProfile::Rust(RustEdition::Rust2021);
        let key = ProductSemanticPublicationKey::new(package, coordinate, profile)
            .expect("valid semantic selection key");
        let expected_digest = [0x6d; 32];

        // This models a process ending after the durable source observation and
        // before its workspace relation transaction can be committed.
        let mut authority = SemanticAuthority::open(&workspace.0).expect("open authority");
        let observed = authority
            .observe(&key, expected_digest, 1)
            .expect("persist observed source revision");
        drop(authority);

        let reopened = SemanticAuthority::open(&workspace.0).expect("cold reopen authority");
        let namespace = SemanticAuthority::namespace(key.package(), key.coordinate(), profile)
            .expect("valid semantic authority namespace");
        let latest =
            futures_executor::block_on(reopened.authority.latest_source_observation(&namespace))
                .expect("read reopened source observation")
                .expect("source observation survived process restart");
        assert_eq!(latest.sequence(), observed.sequence());
        assert_eq!(latest.observation().revision(), Some(expected_digest));
        assert_eq!(
            latest.observation().value(),
            &SourceObservationValue::KnownCount(1)
        );
        assert!(
            futures_executor::block_on(reopened.authority.selected_frontier(&namespace))
                .expect("read reopened selected frontier")
                .is_none(),
            "an observation alone must not create or advance a selected head"
        );
    }
}
