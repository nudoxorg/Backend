//! Content-addressed semantic planes and storage-neutral range hydration.
//!
//! This module describes immutable plane segments; it does not introduce a
//! second semantic-image payload grammar. Segment payloads remain in their
//! existing canonical representation and are borrowed only while their IDs
//! are checked. Manifests contain bounded metadata, stable key ranges, and
//! opaque input/coverage witnesses.

use alloc::vec::Vec;
use core::{cmp::Ordering, fmt};

use backend_version::{
    AuthorityScopeClaim, Coverage, CoverageWitness, ObjectVersion, Schema, SchemaIdentity,
    ScopeRoot,
};
use thiserror::Error;

use crate::ir::{GenerationId, LanguageProfile};
use crate::vocabulary::Stage;

const MAGIC: [u8; 4] = *b"SPLM";
const WIRE_VERSION: u8 = 3;
const CATALOG_MAGIC: &[u8; 7] = b"VPCAT\0\x01";
const CATALOG_HEADER_BYTES: usize = 7 + 4;
const CATALOG_ENTRY_BYTES: usize = 4 + 32 + 32 + 4;
const MAX_CATALOG_IMAGES: usize = 50_000;
const MAX_CATALOG_BYTES: usize = CATALOG_HEADER_BYTES + MAX_CATALOG_IMAGES * CATALOG_ENTRY_BYTES;
const MAX_PLANES: usize = 64;
const MAX_SEGMENTS_PER_PLANE: usize = 500_000;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
/// A range request is deliberately small so local mmap, FileStore, and future
/// Iroh readers can all use the same cursor without a full-image scratch copy.
pub const MAX_SEMANTIC_SEGMENT_BYTES: usize = 1024 * 1024;
const SEGMENT_WIRE_BYTES: usize = 32 + 32 + 4 + 8 + 32 + 32 + 32 + COVERAGE_WIRE_BYTES;
const COVERAGE_WIRE_BYTES: usize = 1 + 32 + 1 + 96;

/// Identity of one semantic plane segment's exact canonical payload bytes.
///
/// IDs use BLAKE3 with the `backend.semantic.ir.segment.v1` domain and bind
/// the plane identity, stable key interval, row count, byte length, and exact
/// borrowed payload. The payload itself is never copied by this module.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticSegmentId([u8; 32]);

impl SemanticSegmentId {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Untrusted content-ID bytes read from a manifest. This claim cannot be
/// placed in a local-have set until a payload is admitted against it.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UntrustedSemanticSegmentId([u8; 32]);

impl UntrustedSemanticSegmentId {
    #[must_use]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Identity of a complete canonical plane descriptor and its ordered segment
/// IDs. A coverage witness for that exact plane is scoped to this version.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticPlaneRoot([u8; 32]);

impl SemanticPlaneRoot {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Schema used to bind an authority coverage witness to one exact plane root.
///
/// Authorities can create an `ObjectVersion<SemanticPlaneCoverageScope>`
/// from a [`SemanticPlaneRoot`] and admit coverage for that exact output.
/// The semantic module checks scope equality but does not mint the witness.
pub struct SemanticPlaneCoverageScope;

impl Schema for SemanticPlaneCoverageScope {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 31;
    type Value = SemanticPlaneRoot;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// FileStore schema for one canonical semantic-plane segment payload.
pub struct VersionedPlaneSegmentSchema;

impl Schema for VersionedPlaneSegmentSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc004;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// FileStore schema for one canonical semantic-plane manifest payload.
pub struct VersionedPlaneManifestSchema;

impl Schema for VersionedPlaneManifestSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc005;
    const VERSION: u8 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Exact typed schema identity for a versioned-plane segment object.
pub const VERSIONED_PLANE_SEGMENT_SCHEMA: SchemaIdentity = SchemaIdentity::new(
    VersionedPlaneSegmentSchema::DOMAIN,
    VersionedPlaneSegmentSchema::TYPE,
    VersionedPlaneSegmentSchema::VERSION,
);

/// Exact typed schema identity for a versioned-plane manifest object.
pub const VERSIONED_PLANE_MANIFEST_SCHEMA: SchemaIdentity = SchemaIdentity::new(
    VersionedPlaneManifestSchema::DOMAIN,
    VersionedPlaneManifestSchema::TYPE,
    VersionedPlaneManifestSchema::VERSION,
);

/// Schema used to bind an authority's canonical-closure admission to one
/// exact semantic-plane manifest. The authority must first admit the
/// manifest bytes in its durable closure; a root claim alone is insufficient.
pub struct SemanticManifestCoverageScope;

impl Schema for SemanticManifestCoverageScope {
    const DOMAIN: u8 = 0x53;
    const TYPE: u16 = 32;
    type Value = SemanticManifestRoot;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Semantic IR plane independent of any embedding model.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticIrPlane {
    /// Canonical declaration and entity facts.
    Core,
    /// Canonical type expressions and their referenced pools.
    Types,
    /// Stable graph relations.
    Relations,
    /// Occurrence-site facts.
    Occurrences,
    /// Documentation facts.
    Documentation,
    /// Source provenance and spans.
    SourceProvenance,
    /// Facts whose grammar is selected by a closed source-language profile.
    LanguageExtensions(LanguageProfile),
}

/// Normalization applied to one embedding plane's vectors.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EmbeddingNormalization {
    /// Preserve the model's emitted values.
    None,
    /// L2-normalize each vector.
    L2,
    /// Mean-center and then L2-normalize each vector.
    MeanCenteredL2,
    /// A closed custom normalization implementation identified by digest.
    Custom([u8; 32]),
}

/// Full identity of an embedding output recipe. Embeddings have their own
/// plane key, so changing a model cannot change or complete the IR plane.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingPlaneIdentity {
    model: [u8; 32],
    model_version: [u8; 32],
    tokenizer: [u8; 32],
    dimension: u32,
    normalization: EmbeddingNormalization,
    toolchain: [u8; 32],
    recipe: [u8; 32],
}

impl EmbeddingPlaneIdentity {
    /// Creates a closed embedding identity. Dimension zero is invalid.
    pub fn new(
        model: [u8; 32],
        model_version: [u8; 32],
        tokenizer: [u8; 32],
        dimension: u32,
        normalization: EmbeddingNormalization,
        toolchain: [u8; 32],
        recipe: [u8; 32],
    ) -> Result<Self, SemanticManifestError> {
        if dimension == 0 {
            return Err(SemanticManifestError::ZeroEmbeddingDimension);
        }
        Ok(Self {
            model,
            model_version,
            tokenizer,
            dimension,
            normalization,
            toolchain,
            recipe,
        })
    }

    #[must_use]
    pub const fn model(&self) -> &[u8; 32] {
        &self.model
    }
    #[must_use]
    pub const fn model_version(&self) -> &[u8; 32] {
        &self.model_version
    }
    #[must_use]
    pub const fn tokenizer(&self) -> &[u8; 32] {
        &self.tokenizer
    }
    #[must_use]
    pub const fn dimension(&self) -> u32 {
        self.dimension
    }
    #[must_use]
    pub const fn normalization(&self) -> EmbeddingNormalization {
        self.normalization
    }
    #[must_use]
    pub const fn toolchain(&self) -> &[u8; 32] {
        &self.toolchain
    }
    #[must_use]
    pub const fn recipe(&self) -> &[u8; 32] {
        &self.recipe
    }
}

/// Stable namespace of one independently transferable semantic plane.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticPlaneKind {
    /// One compiler-produced IR plane.
    Ir(SemanticIrPlane),
    /// Embedding vectors produced by exactly this independent recipe.
    Embeddings(EmbeddingPlaneIdentity),
}

/// Compilation authority that must remain equal before segment reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticBuildIdentity {
    package: [u8; 32],
    target: [u8; 32],
    profile: LanguageProfile,
    stage: Stage,
    recipe: [u8; 32],
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl SemanticBuildIdentity {
    #[must_use]
    pub const fn new(
        package: [u8; 32],
        target: [u8; 32],
        profile: LanguageProfile,
        stage: Stage,
        recipe: [u8; 32],
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
    ) -> Self {
        Self {
            package,
            target,
            profile,
            stage,
            recipe,
            toolchain,
            environment,
            target_platform,
        }
    }

    #[must_use]
    pub const fn package(&self) -> &[u8; 32] {
        &self.package
    }
    #[must_use]
    pub const fn target(&self) -> &[u8; 32] {
        &self.target
    }
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }
    #[must_use]
    pub const fn recipe(&self) -> &[u8; 32] {
        &self.recipe
    }
    #[must_use]
    pub const fn toolchain(&self) -> &[u8; 32] {
        &self.toolchain
    }
    #[must_use]
    pub const fn environment(&self) -> &[u8; 32] {
        &self.environment
    }
    #[must_use]
    pub const fn target_platform(&self) -> &[u8; 32] {
        &self.target_platform
    }
}

/// Coverage state retained with a witness. `authorized` is true only while a
/// live, authority-admitted capability is held; canonical reopen preserves
/// the claim bytes but deliberately cannot recreate that capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticCoverageState {
    state: Coverage,
    authorized: bool,
}

impl SemanticCoverageState {
    #[must_use]
    pub const fn state(self) -> Coverage {
        self.state
    }
    #[must_use]
    pub const fn is_authorized_complete(self) -> bool {
        self.authorized && matches!(self.state, Coverage::Complete)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CoverageRecord {
    state: Coverage,
    scope: ScopeRoot,
    producer: Option<[u8; 32]>,
    context: Option<[u8; 32]>,
    evidence: Option<[u8; 32]>,
    witness: Option<CoverageWitness>,
}

impl CoverageRecord {
    fn from_witness(witness: CoverageWitness) -> Self {
        match witness {
            CoverageWitness::Complete(value) => Self {
                state: Coverage::Complete,
                scope: value.scope_root(),
                producer: Some(value.producer_identity()),
                context: Some(value.context()),
                evidence: Some(value.evidence_digest()),
                witness: Some(witness),
            },
            CoverageWitness::UntrustedComplete(value) => Self {
                state: Coverage::Complete,
                scope: value.scope_root(),
                producer: None,
                context: None,
                evidence: None,
                witness: Some(witness),
            },
            CoverageWitness::Closed(value) => Self {
                state: Coverage::Closed,
                scope: value.scope_root(),
                producer: None,
                context: None,
                evidence: None,
                witness: Some(witness),
            },
            CoverageWitness::Partial(value) => Self {
                state: Coverage::Partial,
                scope: value.scope_root(),
                producer: None,
                context: None,
                evidence: None,
                witness: Some(witness),
            },
            CoverageWitness::Unavailable(value) => Self {
                state: Coverage::Unavailable,
                scope: value.scope_root(),
                producer: None,
                context: None,
                evidence: None,
                witness: Some(witness),
            },
            CoverageWitness::Unsupported(value) => Self {
                state: Coverage::Unsupported,
                scope: value.scope_root(),
                producer: None,
                context: None,
                evidence: None,
                witness: Some(witness),
            },
        }
    }

    fn claimed(state: Coverage, scope: ScopeRoot) -> Self {
        Self {
            state,
            scope,
            producer: None,
            context: None,
            evidence: None,
            witness: None,
        }
    }

    fn status(self) -> SemanticCoverageState {
        SemanticCoverageState {
            state: self.state,
            authorized: self
                .witness
                .is_some_and(CoverageWitness::is_authorized_complete),
        }
    }

    fn matches_claim(self, observed: Self) -> bool {
        self.state == observed.state
            && self.scope == observed.scope
            && self.producer == observed.producer
            && self.context == observed.context
            && self.evidence == observed.evidence
    }

    fn encode(self, out: &mut Vec<u8>) {
        out.push(coverage_code(self.state));
        out.extend_from_slice(self.scope.as_bytes());
        match (self.producer, self.context, self.evidence) {
            (Some(producer), Some(context), Some(evidence)) => {
                out.push(1);
                out.extend_from_slice(&producer);
                out.extend_from_slice(&context);
                out.extend_from_slice(&evidence);
            }
            _ => {
                out.push(0);
                out.extend_from_slice(&[0; 96]);
            }
        }
    }
}

/// Opaque input identity paired with the admitted positive/negative read
/// frontier. The semantic layer checks only scope equality; the authority
/// layer owns validation of the read-manifest preimage and its claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticInputWitness {
    input_root: [u8; 32],
    read_manifest: ScopeRoot,
    coverage: CoverageRecord,
}

impl SemanticInputWitness {
    /// Records an unverified wire claim. It never authorizes local reuse.
    #[must_use]
    pub fn claimed(input_root: [u8; 32], read_manifest: ScopeRoot) -> Self {
        Self {
            input_root,
            read_manifest,
            coverage: CoverageRecord::claimed(Coverage::Complete, read_manifest),
        }
    }

    /// Records a non-complete observation with its opaque scope.
    #[must_use]
    pub fn claimed_state(input_root: [u8; 32], read_manifest: ScopeRoot, state: Coverage) -> Self {
        Self {
            input_root,
            read_manifest,
            coverage: CoverageRecord::claimed(state, read_manifest),
        }
    }

    /// Admits an input witness only when the authority capability covers the
    /// exact read-manifest scope. This does not validate that manifest's
    /// preimage; that proof belongs to the caller's admission boundary.
    pub fn admitted(
        input_root: [u8; 32],
        read_manifest: ScopeRoot,
        witness: CoverageWitness,
    ) -> Result<Self, SemanticManifestError> {
        let observed = witness.scope_root();
        if observed != read_manifest {
            return Err(SemanticManifestError::InputScopeMismatch {
                expected: read_manifest,
                observed,
            });
        }
        if !witness.is_authorized_complete() {
            return Err(SemanticManifestError::InputCoverageNotAdmitted {
                state: witness.state(),
            });
        }
        Ok(Self {
            input_root,
            read_manifest,
            coverage: CoverageRecord::from_witness(witness),
        })
    }

    /// Whether both admitted witnesses cover the exact same positive and
    /// negative read frontier. This compares opaque identities and authority
    /// admission, not the read-manifest preimage.
    #[must_use]
    pub fn same_admitted_frontier(&self, other: &Self) -> bool {
        self.input_root == other.input_root
            && self.read_manifest == other.read_manifest
            && self.coverage.status().is_authorized_complete()
            && other.coverage.status().is_authorized_complete()
    }

    /// Commits deterministic input/read-frontier claims for typed semantic
    /// generation V2. Producer/context/evidence attestations are admission
    /// provenance and deliberately stay outside content identity. Callers
    /// must separately require a live complete witness.
    #[must_use]
    pub fn generation_root_commitment_v2(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.semantic.input-read-claims.v2\0");
        hasher.update(&self.input_root);
        hasher.update(self.read_manifest.as_bytes());
        hasher.update(&[coverage_code(self.coverage.state)]);
        hasher.update(self.coverage.scope.as_bytes());
        *hasher.finalize().as_bytes()
    }

    #[must_use]
    pub const fn input_root(&self) -> &[u8; 32] {
        &self.input_root
    }
    #[must_use]
    pub const fn read_manifest_root(&self) -> ScopeRoot {
        self.read_manifest
    }
    #[must_use]
    pub fn coverage(&self) -> SemanticCoverageState {
        self.coverage.status()
    }

    /// Returns the live coverage capability, if this exact witness was
    /// admitted in the current process. Canonical wire metadata never
    /// recreates this capability.
    #[must_use]
    pub const fn authority_witness(&self) -> Option<CoverageWitness> {
        self.coverage.witness
    }

    fn re_admit(&mut self, witness: CoverageWitness) -> Result<(), SemanticManifestError> {
        let observed = CoverageRecord::from_witness(witness);
        if observed.scope != self.read_manifest {
            return Err(SemanticManifestError::InputScopeMismatch {
                expected: self.read_manifest,
                observed: observed.scope,
            });
        }
        if !self.coverage.matches_claim(observed) {
            return Err(SemanticManifestError::CoverageClaimMismatch);
        }
        self.coverage = observed;
        Ok(())
    }
}

/// One stable-key-anchored range in a semantic plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPlaneSegment {
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    byte_length: u64,
    id_claim: UntrustedSemanticSegmentId,
    admitted_id: Option<SemanticSegmentId>,
    input: SemanticInputWitness,
}

impl SemanticPlaneSegment {
    /// Hashes exact canonical payload bytes without copying them.
    pub fn from_payload(
        plane: SemanticPlaneKind,
        first_key: [u8; 32],
        last_key: [u8; 32],
        row_count: u32,
        payload: &[u8],
    ) -> Result<Self, SemanticManifestError> {
        validate_segment_claim(first_key, last_key, row_count, payload.len())?;
        Self::from_payload_with_witness(
            plane,
            first_key,
            last_key,
            row_count,
            payload,
            SemanticInputWitness::claimed([0; 32], ScopeRoot::from_bytes([0; 32])),
        )
    }

    /// Hashes exact bytes and binds the stable range to its independently
    /// admitted positive/negative input subfrontier. The evidence authority
    /// must prove that this subfrontier is complete for this range.
    pub fn from_payload_with_witness(
        plane: SemanticPlaneKind,
        first_key: [u8; 32],
        last_key: [u8; 32],
        row_count: u32,
        payload: &[u8],
        input: SemanticInputWitness,
    ) -> Result<Self, SemanticManifestError> {
        validate_segment_claim(first_key, last_key, row_count, payload.len())?;
        let id = segment_identity(plane, first_key, last_key, row_count, payload);
        Ok(Self {
            first_key,
            last_key,
            row_count,
            byte_length: payload.len() as u64,
            id_claim: segment_id_claim(id),
            admitted_id: Some(id),
            input,
        })
    }

    /// Admits a fetched or locally mapped payload against this exact claim.
    /// The returned ID is the only segment identity accepted as locally held.
    pub fn admit(
        &self,
        plane: SemanticPlaneKind,
        payload: &[u8],
    ) -> Result<SemanticSegmentId, SemanticManifestError> {
        let mut verifier = self.streaming_admission(plane);
        verifier.update(payload)?;
        verifier.finish()
    }

    /// Starts incremental admission of a fetched or locally mapped payload.
    /// Feed the exact payload bytes in any bounded chunks, then call
    /// [`SemanticSegmentVerifier::finish`] to obtain its admitted identity.
    #[must_use]
    pub fn streaming_admission(&self, plane: SemanticPlaneKind) -> SemanticSegmentVerifier {
        SemanticSegmentVerifier::new(
            plane,
            self.first_key,
            self.last_key,
            self.row_count,
            self.byte_length,
            self.id_claim,
        )
    }

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
    #[must_use]
    pub const fn id_claim(&self) -> UntrustedSemanticSegmentId {
        self.id_claim
    }
    #[must_use]
    pub const fn admitted_id(&self) -> Option<SemanticSegmentId> {
        self.admitted_id
    }
    #[must_use]
    pub const fn input_witness(&self) -> SemanticInputWitness {
        self.input
    }

    fn re_admit_input(&mut self, witness: CoverageWitness) -> Result<(), SemanticManifestError> {
        self.input.re_admit(witness)
    }
}

/// Incremental verifier for one exact semantic segment claim.
///
/// It binds the plane metadata and declared payload length before accepting
/// bytes, and only returns a [`SemanticSegmentId`] after the exact byte count
/// and claimed identity have both been checked.
pub struct SemanticSegmentVerifier {
    hasher: blake3::Hasher,
    expected_length: u64,
    observed_length: u64,
    id_claim: UntrustedSemanticSegmentId,
}

impl SemanticSegmentVerifier {
    fn new(
        plane: SemanticPlaneKind,
        first_key: [u8; 32],
        last_key: [u8; 32],
        row_count: u32,
        byte_length: u64,
        id_claim: UntrustedSemanticSegmentId,
    ) -> Self {
        Self {
            hasher: segment_hasher(plane, first_key, last_key, row_count, byte_length),
            expected_length: byte_length,
            observed_length: 0,
            id_claim,
        }
    }

    /// Adds the next exact payload bytes. A chunk that would exceed the
    /// declared payload length is rejected without hashing any part of it.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), SemanticManifestError> {
        let chunk_length =
            u64::try_from(chunk.len()).map_err(|_| SemanticManifestError::CountOverflow)?;
        let observed_length = self
            .observed_length
            .checked_add(chunk_length)
            .ok_or(SemanticManifestError::CountOverflow)?;
        if observed_length > self.expected_length {
            return Err(SemanticManifestError::SegmentLength {
                expected: self.expected_length,
                observed: observed_length,
            });
        }
        self.hasher.update(chunk);
        self.observed_length = observed_length;
        Ok(())
    }

    /// Finishes admission and returns the trusted identity only when the
    /// exact declared byte count and untrusted claim both match.
    pub fn finish(self) -> Result<SemanticSegmentId, SemanticManifestError> {
        if self.observed_length != self.expected_length {
            return Err(SemanticManifestError::SegmentLength {
                expected: self.expected_length,
                observed: self.observed_length,
            });
        }
        let observed = SemanticSegmentId(*self.hasher.finalize().as_bytes());
        if observed.as_bytes() != self.id_claim.as_bytes() {
            return Err(SemanticManifestError::SegmentIdentity {
                expected: self.id_claim,
                observed,
            });
        }
        Ok(observed)
    }
}

/// One plane and the exact list of immutable segments it claims to contain.
#[derive(Clone, Debug)]
pub struct SemanticPlane {
    kind: SemanticPlaneKind,
    segments: Vec<SemanticPlaneSegment>,
    coverage: CoverageRecord,
    root: SemanticPlaneRoot,
}

impl SemanticPlane {
    /// Creates a plane with an authority witness scoped to its canonical
    /// descriptor root. The witness is retained as a live capability.
    pub fn admitted(
        kind: SemanticPlaneKind,
        segments: Vec<SemanticPlaneSegment>,
        witness: CoverageWitness,
    ) -> Result<Self, SemanticManifestError> {
        validate_segments(&segments)?;
        let root = plane_root(kind, &segments);
        let expected = plane_coverage_scope(root);
        let observed = witness.scope_root();
        if observed != expected {
            return Err(SemanticManifestError::PlaneScopeMismatch { expected, observed });
        }
        Ok(Self {
            kind,
            segments,
            coverage: CoverageRecord::from_witness(witness),
            root,
        })
    }

    /// Creates a claim-only plane. This is useful after canonical reopen and
    /// cannot authorize fact reuse, even if the wire says `Complete`.
    pub fn claimed(
        kind: SemanticPlaneKind,
        segments: Vec<SemanticPlaneSegment>,
        state: Coverage,
    ) -> Result<Self, SemanticManifestError> {
        validate_segments(&segments)?;
        let root = plane_root(kind, &segments);
        let scope = plane_coverage_scope(root);
        Ok(Self {
            kind,
            segments,
            coverage: CoverageRecord::claimed(state, scope),
            root,
        })
    }

    /// Returns the exact scope root authorities must admit for this plane.
    #[must_use]
    pub fn coverage_scope(root: SemanticPlaneRoot) -> ScopeRoot {
        plane_coverage_scope(root)
    }

    #[must_use]
    pub const fn kind(&self) -> SemanticPlaneKind {
        self.kind
    }
    #[must_use]
    pub fn segments(&self) -> &[SemanticPlaneSegment] {
        &self.segments
    }
    #[must_use]
    pub const fn root(&self) -> SemanticPlaneRoot {
        self.root
    }
    #[must_use]
    pub fn coverage(&self) -> SemanticCoverageState {
        self.coverage.status()
    }

    /// Returns the exact live plane-coverage capability, if still held.
    #[must_use]
    pub const fn authority_witness(&self) -> Option<CoverageWitness> {
        self.coverage.witness
    }
}

/// Content identity of one canonical semantic-plane manifest.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticManifestRoot([u8; 32]);

impl SemanticManifestRoot {
    /// Creates an equality-only claim from canonical wire bytes.
    ///
    /// This does not admit a manifest, prove the bytes hash to this root, or
    /// grant closure authority. Callers must compare it with a decoded
    /// canonical manifest whose root was recomputed, and then separately
    /// establish closure admission before authority-sensitive operations.
    #[must_use]
    pub const fn from_wire_claim(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Exact plane-manifest selection for one canonical compiler artifact.
///
/// The manifest root is a content identity, not an authority witness. A caller must still admit
/// the referenced manifest and its segment closure before serving or reusing payloads.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticPlaneImageKey {
    artifact_ordinal: u32,
    semantic_generation: GenerationId,
    manifest_root: SemanticManifestRoot,
}

impl SemanticPlaneImageKey {
    /// Binds one canonical artifact ordinal to the exact VCS generation and plane-manifest root.
    #[must_use]
    pub const fn new(
        artifact_ordinal: u32,
        semantic_generation: GenerationId,
        manifest_root: SemanticManifestRoot,
    ) -> Self {
        Self {
            artifact_ordinal,
            semantic_generation,
            manifest_root,
        }
    }

    /// Constructs a key from a decoded or constructed canonical manifest.
    #[must_use]
    pub const fn from_manifest(artifact_ordinal: u32, manifest: &SemanticPlaneManifest) -> Self {
        Self::new(
            artifact_ordinal,
            manifest.semantic_generation(),
            manifest.root(),
        )
    }

    #[must_use]
    pub const fn artifact_ordinal(self) -> u32 {
        self.artifact_ordinal
    }

    #[must_use]
    pub const fn semantic_generation(self) -> GenerationId {
        self.semantic_generation
    }

    #[must_use]
    pub const fn manifest_root(self) -> SemanticManifestRoot {
        self.manifest_root
    }
}

/// Content identity of an ordered set of per-artifact versioned-plane manifests.
///
/// This storage-neutral aggregate commits to the artifact ordinal, semantic VCS generation, and
/// canonical manifest root for each image. It does not establish that an external store contains
/// or has admitted any referenced manifest or segment payload.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticPlaneCatalogRoot([u8; 32]);

impl SemanticPlaneCatalogRoot {
    /// Creates an equality-only claim read from a catalog envelope.
    ///
    /// This does not admit catalog bytes, manifests, or payload closure. A canonical catalog
    /// decoded by [`SemanticPlaneCatalog::decode`] recomputes its own root from the exact bytes.
    #[must_use]
    pub const fn from_wire_claim(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One image record in a semantic-plane catalog.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticPlaneCatalogEntry {
    image: SemanticPlaneImageKey,
    manifest_length: u32,
}

impl SemanticPlaneCatalogEntry {
    /// Binds a canonical image key to the exact manifest byte length.
    pub fn new(
        image: SemanticPlaneImageKey,
        manifest_length: u32,
    ) -> Result<Self, SemanticManifestError> {
        if manifest_length == 0 {
            return Err(SemanticManifestError::CatalogManifestLength);
        }
        Ok(Self {
            image,
            manifest_length,
        })
    }

    #[must_use]
    pub const fn image(self) -> SemanticPlaneImageKey {
        self.image
    }

    #[must_use]
    pub const fn manifest_length(self) -> u32 {
        self.manifest_length
    }
}

/// Canonical bounded catalog of per-artifact semantic-plane manifests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticPlaneCatalog {
    entries: Box<[SemanticPlaneCatalogEntry]>,
    root: SemanticPlaneCatalogRoot,
}

impl SemanticPlaneCatalog {
    /// Constructs a catalog from strictly increasing artifact ordinals.
    pub fn new(entries: Vec<SemanticPlaneCatalogEntry>) -> Result<Self, SemanticManifestError> {
        validate_catalog_entries(&entries)?;
        let wire = encode_catalog_entries(&entries)?;
        Ok(Self {
            entries: entries.into_boxed_slice(),
            root: catalog_identity(&wire),
        })
    }

    /// Decodes an exact canonical catalog. Referenced manifests remain claims until admitted.
    pub fn decode(bytes: &[u8]) -> Result<Self, SemanticManifestError> {
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(SemanticManifestError::CatalogTooLarge {
                observed: bytes.len(),
                maximum: MAX_CATALOG_BYTES,
            });
        }
        if bytes.get(..CATALOG_MAGIC.len()) != Some(CATALOG_MAGIC.as_slice()) {
            return Err(SemanticManifestError::BadCatalogMagic);
        }
        let mut reader = Reader::new(bytes);
        reader.take(CATALOG_MAGIC.len())?;
        let count = usize::try_from(reader.u32()?)
            .map_err(|_| SemanticManifestError::CatalogImageCountOverflow)?;
        if count > MAX_CATALOG_IMAGES {
            return Err(SemanticManifestError::TooManyCatalogImages {
                observed: count,
                maximum: MAX_CATALOG_IMAGES,
            });
        }
        let expected = CATALOG_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(CATALOG_ENTRY_BYTES)
                    .ok_or(SemanticManifestError::CountOverflow)?,
            )
            .ok_or(SemanticManifestError::CountOverflow)?;
        if bytes.len() != expected {
            return Err(SemanticManifestError::CatalogLength {
                expected,
                observed: bytes.len(),
            });
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count)
            .map_err(|_| SemanticManifestError::AllocationLimit)?;
        for _ in 0..count {
            let ordinal = reader.u32()?;
            let generation = GenerationId::from_raw(reader.array32()?);
            let root = SemanticManifestRoot::from_wire_claim(reader.array32()?);
            let length = reader.u32()?;
            entries.push(SemanticPlaneCatalogEntry::new(
                SemanticPlaneImageKey::new(ordinal, generation, root),
                length,
            )?);
        }
        if !reader.is_empty() {
            return Err(SemanticManifestError::CatalogLength {
                expected,
                observed: bytes.len(),
            });
        }
        validate_catalog_entries(&entries)?;
        Ok(Self {
            entries: entries.into_boxed_slice(),
            root: catalog_identity(bytes),
        })
    }

    /// Writes exact canonical catalog bytes.
    pub fn encode(&self) -> Result<Box<[u8]>, SemanticManifestError> {
        encode_catalog_entries(&self.entries)
    }

    #[must_use]
    pub fn entries(&self) -> &[SemanticPlaneCatalogEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn root(&self) -> SemanticPlaneCatalogRoot {
        self.root
    }
}

/// Canonical versioned list of independently reusable IR and embedding
/// planes. Segment payloads are not copied into this metadata object.
#[derive(Clone, Debug)]
pub struct SemanticPlaneManifest {
    semantic_generation: GenerationId,
    build: SemanticBuildIdentity,
    input: SemanticInputWitness,
    planes: Vec<SemanticPlane>,
    root: SemanticManifestRoot,
    claims_admitted: bool,
}

impl SemanticPlaneManifest {
    pub fn new(
        semantic_generation: GenerationId,
        build: SemanticBuildIdentity,
        input: SemanticInputWitness,
        planes: Vec<SemanticPlane>,
    ) -> Result<Self, SemanticManifestError> {
        validate_planes(&planes)?;
        let mut value = Self {
            semantic_generation,
            build,
            input,
            planes,
            root: SemanticManifestRoot([0; 32]),
            claims_admitted: false,
        };
        value.root = manifest_identity(&value.encode()?);
        value.claims_admitted = value.has_live_complete_claims();
        Ok(value)
    }

    /// Reopens canonical bounded metadata. Coverage bytes remain claims only;
    /// callers must independently admit the exact witnesses before reuse.
    pub fn decode(bytes: &[u8]) -> Result<Self, SemanticManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(SemanticManifestError::ManifestTooLarge {
                observed: bytes.len(),
                maximum: MAX_MANIFEST_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.take(4)? != MAGIC.as_slice() {
            return Err(SemanticManifestError::BadMagic);
        }
        let version = reader.u8()?;
        if version != WIRE_VERSION {
            return Err(SemanticManifestError::UnsupportedVersion(version));
        }
        let build = decode_build(&mut reader)?;
        let semantic_generation = GenerationId::from_raw(reader.array32()?);
        let input_root = reader.array32()?;
        let read_manifest = ScopeRoot::from_bytes(reader.array32()?);
        let coverage = decode_coverage(&mut reader)?;
        if coverage.scope != read_manifest {
            return Err(SemanticManifestError::InputScopeMismatch {
                expected: read_manifest,
                observed: coverage.scope,
            });
        }
        let input = SemanticInputWitness {
            input_root,
            read_manifest,
            coverage,
        };
        let plane_count = usize::from(reader.u16()?);
        if plane_count > MAX_PLANES {
            return Err(SemanticManifestError::TooManyPlanes {
                observed: plane_count,
                maximum: MAX_PLANES,
            });
        }
        let mut planes = Vec::new();
        planes
            .try_reserve_exact(plane_count)
            .map_err(|_| SemanticManifestError::AllocationLimit)?;
        for _ in 0..plane_count {
            let kind = decode_plane_kind(&mut reader)?;
            let coverage = decode_coverage(&mut reader)?;
            let segment_count =
                usize::try_from(reader.u32()?).map_err(|_| SemanticManifestError::CountOverflow)?;
            if segment_count > MAX_SEGMENTS_PER_PLANE {
                return Err(SemanticManifestError::TooManySegments {
                    observed: segment_count,
                    maximum: MAX_SEGMENTS_PER_PLANE,
                });
            }
            let remaining = reader.remaining();
            let segment_bytes = segment_count
                .checked_mul(SEGMENT_WIRE_BYTES)
                .ok_or(SemanticManifestError::CountOverflow)?;
            if segment_bytes > remaining {
                return Err(SemanticManifestError::Truncated);
            }
            let mut segments = Vec::new();
            segments
                .try_reserve_exact(segment_count)
                .map_err(|_| SemanticManifestError::AllocationLimit)?;
            for _ in 0..segment_count {
                let segment = SemanticPlaneSegment {
                    first_key: reader.array32()?,
                    last_key: reader.array32()?,
                    row_count: reader.u32()?,
                    byte_length: reader.u64()?,
                    id_claim: UntrustedSemanticSegmentId(reader.array32()?),
                    admitted_id: None,
                    input: decode_input_witness(&mut reader)?,
                };
                validate_segment_metadata(&segment)?;
                segments.push(segment);
            }
            validate_segments(&segments)?;
            let root = plane_root(kind, &segments);
            if coverage.scope != plane_coverage_scope(root) {
                return Err(SemanticManifestError::PlaneScopeMismatch {
                    expected: plane_coverage_scope(root),
                    observed: coverage.scope,
                });
            }
            planes.push(SemanticPlane {
                kind,
                segments,
                coverage,
                root,
            });
        }
        if !reader.is_empty() {
            return Err(SemanticManifestError::TrailingBytes(reader.remaining()));
        }
        validate_planes(&planes)?;
        Ok(Self {
            semantic_generation,
            build,
            input,
            planes,
            root: manifest_identity(bytes),
            claims_admitted: false,
        })
    }

    /// Re-admits a cold canonical manifest only after its exact bytes have
    /// been admitted in a durable closure and its original input, plane, and
    /// per-range witness claims have been re-established. The closure proof
    /// authenticates manifest claims without loading each segment payload;
    /// `verified_segment_ids` contains only payloads already locally hashed.
    pub fn admit_from_closure(
        &self,
        expected_root: SemanticManifestRoot,
        closure_witness: CoverageWitness,
        input_witness: CoverageWitness,
        plane_witnesses: &[CoverageWitness],
        segment_input_witnesses: &[CoverageWitness],
        verified_segment_ids: &[SemanticSegmentId],
    ) -> Result<Self, SemanticManifestError> {
        if self.root != expected_root {
            return Err(SemanticManifestError::StaleManifest {
                expected: self.root,
                observed: expected_root,
            });
        }
        let expected_closure_scope = manifest_coverage_scope(self.root);
        let observed_closure_scope = closure_witness.scope_root();
        if observed_closure_scope != expected_closure_scope {
            return Err(SemanticManifestError::ManifestClosureScopeMismatch {
                expected: expected_closure_scope,
                observed: observed_closure_scope,
            });
        }
        if !closure_witness.is_authorized_complete() {
            return Err(SemanticManifestError::ManifestClosureNotAdmitted);
        }
        let expected_segment_witnesses = self
            .planes
            .iter()
            .map(|plane| plane.segments.len())
            .sum::<usize>();
        if plane_witnesses.len() != self.planes.len()
            || segment_input_witnesses.len() != expected_segment_witnesses
        {
            return Err(SemanticManifestError::AdmissionWitnessCount {
                expected_planes: self.planes.len(),
                observed_planes: plane_witnesses.len(),
                expected_segments: expected_segment_witnesses,
                observed_segments: segment_input_witnesses.len(),
            });
        }
        validate_id_set(verified_segment_ids)?;

        let mut admitted = self.clone();
        admitted.input.re_admit(input_witness)?;
        let mut segment_witness_index = 0;
        for (plane_index, plane) in admitted.planes.iter_mut().enumerate() {
            let witness = plane_witnesses[plane_index];
            let observed = CoverageRecord::from_witness(witness);
            if observed.scope != plane_coverage_scope(plane.root) {
                return Err(SemanticManifestError::PlaneScopeMismatch {
                    expected: plane_coverage_scope(plane.root),
                    observed: observed.scope,
                });
            }
            if !plane.coverage.matches_claim(observed) {
                return Err(SemanticManifestError::CoverageClaimMismatch);
            }
            plane.coverage = observed;
            for segment in &mut plane.segments {
                segment.re_admit_input(segment_input_witnesses[segment_witness_index])?;
                segment_witness_index += 1;
                segment.admitted_id = verified_segment_ids
                    .binary_search_by(|id| id.as_bytes().cmp(segment.id_claim.as_bytes()))
                    .ok()
                    .map(|index| verified_segment_ids[index]);
            }
        }
        admitted.claims_admitted = true;
        Ok(admitted)
    }

    /// Whether the manifest's canonical claims and exact input/plane/range
    /// witnesses are admitted. Payload claims still require checked local IDs.
    #[must_use]
    pub const fn claims_admitted(&self) -> bool {
        self.claims_admitted
    }

    fn has_live_complete_claims(&self) -> bool {
        self.input.coverage().is_authorized_complete()
            && self.planes.iter().all(|plane| {
                plane.coverage().is_authorized_complete()
                    && plane.segments.iter().all(|segment| {
                        segment.admitted_id.is_some()
                            && segment.input.coverage().is_authorized_complete()
                    })
            })
    }

    /// Encodes the canonical `SPLM` metadata form. The output is bounded by
    /// `MAX_MANIFEST_BYTES`; payload bytes are never included.
    pub fn encode(&self) -> Result<Vec<u8>, SemanticManifestError> {
        let estimate = manifest_encoded_len(self)?;
        if estimate > MAX_MANIFEST_BYTES {
            return Err(SemanticManifestError::ManifestTooLarge {
                observed: estimate,
                maximum: MAX_MANIFEST_BYTES,
            });
        }
        let mut out = Vec::new();
        out.try_reserve_exact(estimate)
            .map_err(|_| SemanticManifestError::AllocationLimit)?;
        out.extend_from_slice(&MAGIC);
        out.push(WIRE_VERSION);
        encode_build(self.build, &mut out);
        out.extend_from_slice(self.semantic_generation.as_bytes());
        out.extend_from_slice(self.input.input_root());
        out.extend_from_slice(self.input.read_manifest_root().as_bytes());
        self.input.coverage.encode(&mut out);
        out.extend_from_slice(&(self.planes.len() as u16).to_be_bytes());
        for plane in &self.planes {
            encode_plane_kind(plane.kind, &mut out);
            plane.coverage.encode(&mut out);
            out.extend_from_slice(&(plane.segments.len() as u32).to_be_bytes());
            for segment in &plane.segments {
                encode_segment(*segment, &mut out);
            }
        }
        if out.len() != estimate {
            return Err(SemanticManifestError::InternalLengthMismatch {
                expected: estimate,
                observed: out.len(),
            });
        }
        Ok(out)
    }

    #[must_use]
    pub const fn root(&self) -> SemanticManifestRoot {
        self.root
    }
    /// Exact VCS generation of the canonical semantic IR represented by this plane set.
    #[must_use]
    pub const fn semantic_generation(&self) -> GenerationId {
        self.semantic_generation
    }
    #[must_use]
    pub const fn build(&self) -> SemanticBuildIdentity {
        self.build
    }
    #[must_use]
    pub const fn input(&self) -> SemanticInputWitness {
        self.input
    }
    #[must_use]
    pub fn planes(&self) -> &[SemanticPlane] {
        &self.planes
    }
    #[must_use]
    pub fn plane(&self, kind: SemanticPlaneKind) -> Option<&SemanticPlane> {
        self.planes
            .binary_search_by_key(&kind, SemanticPlane::kind)
            .ok()
            .and_then(|index| self.planes.get(index))
    }

    /// Returns the authority scope for admission of the exact canonical
    /// manifest bytes in a durable closure.
    #[must_use]
    pub fn closure_coverage_scope(root: SemanticManifestRoot) -> ScopeRoot {
        manifest_coverage_scope(root)
    }

    /// Commits only IR planes; embedding model changes do not affect this root.
    pub fn ir_root(&self) -> Result<SemanticManifestRoot, SemanticManifestError> {
        let ir_planes = self
            .planes
            .iter()
            .filter(|plane| matches!(plane.kind, SemanticPlaneKind::Ir(_)))
            .cloned()
            .collect::<Vec<_>>();
        let mut ir_only = Self {
            semantic_generation: self.semantic_generation,
            build: self.build,
            input: self.input,
            planes: ir_planes,
            root: SemanticManifestRoot([0; 32]),
            claims_admitted: self.claims_admitted,
        };
        let bytes = ir_only.encode()?;
        ir_only.root = manifest_identity(&bytes);
        Ok(ir_only.root)
    }

    /// Creates a storage-neutral cursor after checking the exact manifest root.
    pub fn hydration_cursor<'manifest, 'have>(
        &'manifest self,
        expected_root: SemanticManifestRoot,
        plane_filter: Option<SemanticPlaneKind>,
        have_ids: &'have [SemanticSegmentId],
    ) -> Result<SemanticHydrationCursor<'manifest, 'have>, SemanticManifestError> {
        SemanticHydrationCursor::new(self, expected_root, plane_filter, have_ids)
    }
}

/// One segment range request independent of mmap, FileStore, or Iroh layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticRangeRequest {
    /// Root that fixes the target manifest and all request metadata.
    pub manifest_root: SemanticManifestRoot,
    /// Plane identity, including the independent embedding recipe when relevant.
    pub plane: SemanticPlaneKind,
    /// Expected exact segment bytes identity.
    pub segment_id: UntrustedSemanticSegmentId,
    /// Inclusive lower stable key.
    pub first_key: [u8; 32],
    /// Inclusive upper stable key.
    pub last_key: [u8; 32],
    /// Exact expected payload length.
    pub byte_length: u64,
}

/// Snapshot of plane completeness and local segment presence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticHydrationCoverage {
    /// Completeness evidence for the selected plane or planes.
    pub plane: SemanticCoverageState,
    /// Number of selected segments in the manifest.
    pub total_segments: u32,
    /// Segments already present or acknowledged by this cursor.
    pub present_segments: u32,
    /// Segments still needing a verified local object.
    pub missing_segments: u32,
    /// Input read-frontier evidence.
    pub input: SemanticCoverageState,
    /// Every selected range has an admitted, exact input subfrontier.
    pub segment_inputs_authorized: bool,
}

impl SemanticHydrationCoverage {
    #[must_use]
    pub const fn is_hydrated(&self) -> bool {
        self.missing_segments == 0
    }

    /// Hydration is reusable as a complete fact set only when both read
    /// closure and output-plane completeness are authority-admitted.
    #[must_use]
    pub const fn is_reusable(&self) -> bool {
        self.is_hydrated()
            && self.input.is_authorized_complete()
            && self.plane.is_authorized_complete()
            && self.segment_inputs_authorized
    }
}

/// Resume position tied to one exact canonical manifest root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticHydrationCursorToken {
    manifest_root: SemanticManifestRoot,
    plane_filter: Option<SemanticPlaneKind>,
    ordinal: u32,
}

impl SemanticHydrationCursorToken {
    #[must_use]
    pub const fn manifest_root(&self) -> SemanticManifestRoot {
        self.manifest_root
    }
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    #[must_use]
    pub const fn plane_filter(&self) -> Option<SemanticPlaneKind> {
        self.plane_filter
    }

    /// Canonical compact resume token. It contains identities and ordinal only;
    /// physical offsets, file paths, and pack IDs remain storage-local.
    pub fn encode(self) -> Result<Vec<u8>, SemanticManifestError> {
        let mut out = Vec::new();
        out.try_reserve_exact(32 + 4 + 2 + 166)
            .map_err(|_| SemanticManifestError::AllocationLimit)?;
        out.extend_from_slice(self.manifest_root.as_bytes());
        out.extend_from_slice(&self.ordinal.to_be_bytes());
        match self.plane_filter {
            Some(plane) => {
                let mut encoded = Vec::new();
                encode_plane_kind(plane, &mut encoded);
                let length = u16::try_from(encoded.len())
                    .map_err(|_| SemanticManifestError::CountOverflow)?;
                out.extend_from_slice(&length.to_be_bytes());
                out.extend_from_slice(&encoded);
            }
            None => out.extend_from_slice(&0_u16.to_be_bytes()),
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SemanticManifestError> {
        let mut reader = Reader::new(bytes);
        let manifest_root = SemanticManifestRoot(reader.array32()?);
        let ordinal = reader.u32()?;
        let length = usize::from(reader.u16()?);
        let plane_filter = if length == 0 {
            None
        } else {
            let mut nested = Reader::new(reader.take(length)?);
            let plane = decode_plane_kind(&mut nested)?;
            if !nested.is_empty() {
                return Err(SemanticManifestError::TrailingBytes(nested.remaining()));
            }
            Some(plane)
        };
        if !reader.is_empty() {
            return Err(SemanticManifestError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            manifest_root,
            plane_filter,
            ordinal,
        })
    }
}

/// Bounded iterator that yields only missing logical segment ranges.
pub struct SemanticHydrationCursor<'manifest, 'have> {
    manifest: &'manifest SemanticPlaneManifest,
    plane_filter: Option<SemanticPlaneKind>,
    have_ids: &'have [SemanticSegmentId],
    plane_index: usize,
    segment_index: usize,
    ordinal: u32,
    total: u32,
    present: u32,
    pending: Option<UntrustedSemanticSegmentId>,
}

impl<'manifest, 'have> SemanticHydrationCursor<'manifest, 'have> {
    fn new(
        manifest: &'manifest SemanticPlaneManifest,
        expected_root: SemanticManifestRoot,
        plane_filter: Option<SemanticPlaneKind>,
        have_ids: &'have [SemanticSegmentId],
    ) -> Result<Self, SemanticManifestError> {
        if manifest.root != expected_root {
            return Err(SemanticManifestError::StaleManifest {
                expected: manifest.root,
                observed: expected_root,
            });
        }
        validate_id_set(have_ids)?;
        let mut cursor = Self {
            manifest,
            plane_filter,
            have_ids,
            plane_index: 0,
            segment_index: 0,
            ordinal: 0,
            total: count_selected(manifest, plane_filter)?,
            present: 0,
            pending: None,
        };
        cursor.normalize_position();
        cursor.count_present();
        Ok(cursor)
    }

    /// Restores a cursor only against the exact manifest root in its token.
    pub fn resume(
        manifest: &'manifest SemanticPlaneManifest,
        token: SemanticHydrationCursorToken,
        have_ids: &'have [SemanticSegmentId],
    ) -> Result<Self, SemanticManifestError> {
        if manifest.root != token.manifest_root {
            return Err(SemanticManifestError::StaleManifest {
                expected: manifest.root,
                observed: token.manifest_root,
            });
        }
        let mut cursor = Self::new(manifest, token.manifest_root, token.plane_filter, have_ids)?;
        if token.ordinal > cursor.total {
            return Err(SemanticManifestError::CursorOutOfRange {
                ordinal: token.ordinal,
                total: cursor.total,
            });
        }
        for _ in 0..token.ordinal {
            let Some((_, segment)) = cursor.current_segment() else {
                return Err(SemanticManifestError::CursorOutOfRange {
                    ordinal: token.ordinal,
                    total: cursor.total,
                });
            };
            if cursor
                .have_ids
                .binary_search_by(|id| id.as_bytes().cmp(segment.id_claim.as_bytes()))
                .is_err()
            {
                return Err(SemanticManifestError::CursorCheckpointMissingSegment {
                    segment: segment.id_claim,
                });
            }
            cursor.advance_position();
            cursor.ordinal = cursor.ordinal.saturating_add(1);
        }
        Ok(cursor)
    }

    /// Returns the next absent segment. Repeated calls return the same request
    /// until its exact ID is acknowledged after payload verification.
    pub fn next_request(&mut self) -> Option<SemanticRangeRequest> {
        if self.pending.is_some() {
            return self.pending_request();
        }
        loop {
            let (plane_kind, segment) = {
                let (plane, segment) = self.current_segment()?;
                (plane.kind, *segment)
            };
            if self
                .have_ids
                .binary_search_by(|id| id.as_bytes().cmp(segment.id_claim.as_bytes()))
                .is_ok()
            {
                self.advance_position();
                self.ordinal = self.ordinal.saturating_add(1);
                continue;
            }
            self.pending = Some(segment.id_claim);
            return Some(self.request_for(plane_kind, segment));
        }
    }

    /// Acknowledges only the currently requested ID. Call after the received
    /// range has passed [`SemanticPlaneSegment::admit`] and the exact payload
    /// has been durably admitted to the caller's backing store. Resume checks
    /// that every acknowledged prefix remains present in its durable-have set.
    pub fn acknowledge(
        &mut self,
        segment_id: SemanticSegmentId,
    ) -> Result<(), SemanticManifestError> {
        if self
            .pending
            .is_none_or(|claim| claim.as_bytes() != segment_id.as_bytes())
        {
            return Err(SemanticManifestError::UnexpectedAcknowledgement {
                expected: self.pending,
                observed: UntrustedSemanticSegmentId(*segment_id.as_bytes()),
            });
        }
        self.present = self.present.saturating_add(1);
        self.pending = None;
        self.advance_position();
        self.ordinal = self.ordinal.saturating_add(1);
        Ok(())
    }

    #[must_use]
    pub fn coverage(&self) -> SemanticHydrationCoverage {
        let plane = selected_plane_coverage(self.manifest, self.plane_filter);
        let segment_inputs_authorized = self
            .manifest
            .planes
            .iter()
            .filter(|plane| self.plane_filter.is_none_or(|filter| filter == plane.kind))
            .flat_map(|plane| plane.segments.iter())
            .all(|segment| segment.input.coverage().is_authorized_complete());
        SemanticHydrationCoverage {
            plane,
            total_segments: self.total,
            present_segments: self.present.min(self.total),
            missing_segments: self.total.saturating_sub(self.present.min(self.total)),
            input: self.manifest.input.coverage(),
            segment_inputs_authorized,
        }
    }

    #[must_use]
    pub const fn checkpoint(&self) -> SemanticHydrationCursorToken {
        SemanticHydrationCursorToken {
            manifest_root: self.manifest.root,
            plane_filter: self.plane_filter,
            ordinal: self.ordinal,
        }
    }

    fn pending_request(&self) -> Option<SemanticRangeRequest> {
        let (plane, segment) = self.current_segment()?;
        self.pending
            .is_some_and(|claim| claim == segment.id_claim)
            .then(|| self.request_for(plane.kind, *segment))
    }

    fn request_for(
        &self,
        kind: SemanticPlaneKind,
        segment: SemanticPlaneSegment,
    ) -> SemanticRangeRequest {
        SemanticRangeRequest {
            manifest_root: self.manifest.root,
            plane: kind,
            segment_id: segment.id_claim,
            first_key: segment.first_key,
            last_key: segment.last_key,
            byte_length: segment.byte_length,
        }
    }

    fn current_segment(&self) -> Option<(&SemanticPlane, &SemanticPlaneSegment)> {
        let plane = self.manifest.planes.get(self.plane_index)?;
        if self.plane_filter.is_some_and(|filter| filter != plane.kind) {
            return None;
        }
        plane
            .segments
            .get(self.segment_index)
            .map(|segment| (plane, segment))
    }

    fn advance_position(&mut self) {
        if self.plane_index < self.manifest.planes.len() {
            self.segment_index = self.segment_index.saturating_add(1);
        }
        self.normalize_position();
    }

    fn normalize_position(&mut self) {
        while let Some(plane) = self.manifest.planes.get(self.plane_index) {
            if self.plane_filter.is_some_and(|filter| filter != plane.kind)
                || self.segment_index >= plane.segments.len()
            {
                self.plane_index += 1;
                self.segment_index = 0;
            } else {
                return;
            }
        }
    }

    fn count_present(&mut self) {
        self.present = self
            .manifest
            .planes
            .iter()
            .filter(|plane| self.plane_filter.is_none_or(|filter| filter == plane.kind))
            .flat_map(|plane| plane.segments.iter())
            .filter(|segment| {
                self.have_ids
                    .binary_search_by(|id| id.as_bytes().cmp(segment.id_claim.as_bytes()))
                    .is_ok()
            })
            .count()
            .min(u32::MAX as usize) as u32;
    }
}

/// One checked delta operation between two exact plane manifests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDeltaAction {
    /// Reuse the exact existing immutable segment bytes.
    Reuse {
        plane: SemanticPlaneKind,
        /// Checked ID from the base payload. The target manifest's admitted
        /// closure claim is equal, so these exact bytes satisfy it without a
        /// target range read.
        segment_id: SemanticSegmentId,
        segment: SemanticPlaneSegment,
    },
    /// Fetch the changed or missing target range.
    Fetch(SemanticRangeRequest),
    /// Drop a stable range that is absent from the target manifest.
    Remove {
        plane: SemanticPlaneKind,
        segment: SemanticPlaneSegment,
    },
}

/// Checked streaming merge of two ordered plane descriptor sets.
pub struct SemanticDeltaCursor<'base, 'target> {
    base: &'base SemanticPlaneManifest,
    target: &'target SemanticPlaneManifest,
    base_position: (usize, usize),
    target_position: (usize, usize),
    may_reuse: bool,
}

impl<'base, 'target> SemanticDeltaCursor<'base, 'target> {
    /// Rejects a stale base before any action can be observed.
    pub fn new(
        base: &'base SemanticPlaneManifest,
        target: &'target SemanticPlaneManifest,
        expected_base_root: SemanticManifestRoot,
    ) -> Result<Self, SemanticManifestError> {
        if base.root != expected_base_root {
            return Err(SemanticManifestError::StaleManifest {
                expected: base.root,
                observed: expected_base_root,
            });
        }
        let may_reuse = base.build == target.build
            && base.claims_admitted
            && target.claims_admitted
            && base.input.coverage().is_authorized_complete()
            && target.input.coverage().is_authorized_complete();
        Ok(Self {
            base,
            target,
            base_position: (0, 0),
            target_position: (0, 0),
            may_reuse,
        })
    }

    pub fn next_action(&mut self) -> Option<SemanticDeltaAction> {
        loop {
            let base = positioned_segment(self.base, self.base_position);
            let target = positioned_segment(self.target, self.target_position);
            match (base, target) {
                (None, None) => return None,
                (Some((base_plane, base_segment)), None) => {
                    advance_manifest_position(self.base, &mut self.base_position);
                    return Some(SemanticDeltaAction::Remove {
                        plane: base_plane.kind,
                        segment: *base_segment,
                    });
                }
                (None, Some((target_plane, target_segment))) => {
                    advance_manifest_position(self.target, &mut self.target_position);
                    return Some(SemanticDeltaAction::Fetch(range_request(
                        self.target.root,
                        target_plane.kind,
                        *target_segment,
                    )));
                }
                (Some((base_plane, base_segment)), Some((target_plane, target_segment))) => {
                    match segment_order(
                        base_plane.kind,
                        base_segment,
                        target_plane.kind,
                        target_segment,
                    ) {
                        Ordering::Less => {
                            advance_manifest_position(self.base, &mut self.base_position);
                            return Some(SemanticDeltaAction::Remove {
                                plane: base_plane.kind,
                                segment: *base_segment,
                            });
                        }
                        Ordering::Greater => {
                            advance_manifest_position(self.target, &mut self.target_position);
                            return Some(SemanticDeltaAction::Fetch(range_request(
                                self.target.root,
                                target_plane.kind,
                                *target_segment,
                            )));
                        }
                        Ordering::Equal => {
                            advance_manifest_position(self.base, &mut self.base_position);
                            advance_manifest_position(self.target, &mut self.target_position);
                            let plane_complete =
                                base_plane.coverage.status().is_authorized_complete()
                                    && target_plane.coverage.status().is_authorized_complete();
                            if self.may_reuse
                                && plane_complete
                                && same_segment_claim(base_segment, target_segment)
                                && base_segment.admitted_id.is_some()
                                && base_segment.input.coverage().is_authorized_complete()
                                && target_segment.input.coverage().is_authorized_complete()
                            {
                                let Some(segment_id) = base_segment.admitted_id else {
                                    return Some(SemanticDeltaAction::Fetch(range_request(
                                        self.target.root,
                                        target_plane.kind,
                                        *target_segment,
                                    )));
                                };
                                return Some(SemanticDeltaAction::Reuse {
                                    plane: target_plane.kind,
                                    segment_id,
                                    segment: *target_segment,
                                });
                            }
                            return Some(SemanticDeltaAction::Fetch(range_request(
                                self.target.root,
                                target_plane.kind,
                                *target_segment,
                            )));
                        }
                    }
                }
            }
        }
    }
}

fn range_request(
    root: SemanticManifestRoot,
    plane: SemanticPlaneKind,
    segment: SemanticPlaneSegment,
) -> SemanticRangeRequest {
    SemanticRangeRequest {
        manifest_root: root,
        plane,
        segment_id: segment.id_claim,
        first_key: segment.first_key,
        last_key: segment.last_key,
        byte_length: segment.byte_length,
    }
}

fn segment_order(
    left_plane: SemanticPlaneKind,
    left: &SemanticPlaneSegment,
    right_plane: SemanticPlaneKind,
    right: &SemanticPlaneSegment,
) -> Ordering {
    left_plane
        .cmp(&right_plane)
        .then_with(|| left.first_key.cmp(&right.first_key))
        .then_with(|| left.last_key.cmp(&right.last_key))
}

/// Segment IDs bind the exact range and bytes. Each side still needs its own
/// complete input witness, but the package-wide frontiers may differ when an
/// unrelated input changed.
fn same_segment_claim(left: &SemanticPlaneSegment, right: &SemanticPlaneSegment) -> bool {
    left.first_key == right.first_key
        && left.last_key == right.last_key
        && left.row_count == right.row_count
        && left.byte_length == right.byte_length
        && left.id_claim == right.id_claim
}

fn positioned_segment(
    manifest: &SemanticPlaneManifest,
    mut position: (usize, usize),
) -> Option<(&SemanticPlane, &SemanticPlaneSegment)> {
    loop {
        let plane = manifest.planes.get(position.0)?;
        if let Some(segment) = plane.segments.get(position.1) {
            return Some((plane, segment));
        }
        position.0 += 1;
        position.1 = 0;
    }
}

fn advance_manifest_position(manifest: &SemanticPlaneManifest, position: &mut (usize, usize)) {
    while let Some(plane) = manifest.planes.get(position.0) {
        if position.1 + 1 < plane.segments.len() {
            position.1 += 1;
            return;
        }
        position.0 += 1;
        position.1 = 0;
    }
}

fn selected_plane_coverage(
    manifest: &SemanticPlaneManifest,
    filter: Option<SemanticPlaneKind>,
) -> SemanticCoverageState {
    let mut selected = manifest
        .planes
        .iter()
        .filter(|plane| filter.is_none_or(|kind| kind == plane.kind));
    let Some(first) = selected.next() else {
        return SemanticCoverageState {
            state: Coverage::Unavailable,
            authorized: false,
        };
    };
    let state = first.coverage.status();
    if selected.any(|plane| plane.coverage.status() != state) {
        return SemanticCoverageState {
            state: Coverage::Partial,
            authorized: false,
        };
    }
    state
}

fn count_selected(
    manifest: &SemanticPlaneManifest,
    filter: Option<SemanticPlaneKind>,
) -> Result<u32, SemanticManifestError> {
    let count = manifest
        .planes
        .iter()
        .filter(|plane| filter.is_none_or(|kind| kind == plane.kind))
        .try_fold(0_usize, |total, plane| {
            total.checked_add(plane.segments.len())
        })
        .ok_or(SemanticManifestError::CountOverflow)?;
    u32::try_from(count).map_err(|_| SemanticManifestError::CountOverflow)
}

fn validate_planes(planes: &[SemanticPlane]) -> Result<(), SemanticManifestError> {
    if planes.len() > MAX_PLANES {
        return Err(SemanticManifestError::TooManyPlanes {
            observed: planes.len(),
            maximum: MAX_PLANES,
        });
    }
    for pair in planes.windows(2) {
        if pair[0].kind >= pair[1].kind {
            return Err(SemanticManifestError::PlaneOrder);
        }
    }
    for plane in planes {
        validate_segments(&plane.segments)?;
        if plane.root != plane_root(plane.kind, &plane.segments) {
            return Err(SemanticManifestError::PlaneRootMismatch);
        }
        if plane.coverage.scope != plane_coverage_scope(plane.root) {
            return Err(SemanticManifestError::PlaneScopeMismatch {
                expected: plane_coverage_scope(plane.root),
                observed: plane.coverage.scope,
            });
        }
    }
    Ok(())
}

fn validate_segments(segments: &[SemanticPlaneSegment]) -> Result<(), SemanticManifestError> {
    if segments.len() > MAX_SEGMENTS_PER_PLANE {
        return Err(SemanticManifestError::TooManySegments {
            observed: segments.len(),
            maximum: MAX_SEGMENTS_PER_PLANE,
        });
    }
    for segment in segments {
        validate_segment_metadata(segment)?;
    }
    for pair in segments.windows(2) {
        if pair[0].last_key >= pair[1].first_key {
            return Err(SemanticManifestError::SegmentOrder);
        }
    }
    Ok(())
}

fn validate_segment_metadata(segment: &SemanticPlaneSegment) -> Result<(), SemanticManifestError> {
    if segment.first_key > segment.last_key {
        return Err(SemanticManifestError::KeyRange);
    }
    if segment.row_count == 0 {
        return Err(SemanticManifestError::ZeroSegmentRows);
    }
    if segment.byte_length == 0 || segment.byte_length > MAX_SEMANTIC_SEGMENT_BYTES as u64 {
        return Err(SemanticManifestError::SegmentBytes {
            observed: segment.byte_length,
            maximum: MAX_SEMANTIC_SEGMENT_BYTES as u64,
        });
    }
    Ok(())
}

fn validate_segment_claim(
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    byte_length: usize,
) -> Result<(), SemanticManifestError> {
    if first_key > last_key {
        return Err(SemanticManifestError::KeyRange);
    }
    if row_count == 0 {
        return Err(SemanticManifestError::ZeroSegmentRows);
    }
    if byte_length == 0 || byte_length > MAX_SEMANTIC_SEGMENT_BYTES {
        return Err(SemanticManifestError::SegmentBytes {
            observed: byte_length as u64,
            maximum: MAX_SEMANTIC_SEGMENT_BYTES as u64,
        });
    }
    Ok(())
}

fn segment_identity(
    plane: SemanticPlaneKind,
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    payload: &[u8],
) -> SemanticSegmentId {
    let mut hasher = segment_hasher(plane, first_key, last_key, row_count, payload.len() as u64);
    hasher.update(payload);
    SemanticSegmentId(*hasher.finalize().as_bytes())
}

fn segment_hasher(
    plane: SemanticPlaneKind,
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    byte_length: u64,
) -> blake3::Hasher {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.ir.segment.v1\0");
    update_plane_kind_hash(&mut hasher, plane);
    hasher.update(&first_key);
    hasher.update(&last_key);
    hasher.update(&row_count.to_be_bytes());
    hasher.update(&byte_length.to_be_bytes());
    hasher
}

fn segment_id_claim(id: SemanticSegmentId) -> UntrustedSemanticSegmentId {
    UntrustedSemanticSegmentId(*id.as_bytes())
}

fn plane_root(kind: SemanticPlaneKind, segments: &[SemanticPlaneSegment]) -> SemanticPlaneRoot {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.ir.plane.v1\0");
    update_plane_kind_hash(&mut hasher, kind);
    hasher.update(&(segments.len() as u32).to_be_bytes());
    for segment in segments {
        hasher.update(&segment.first_key);
        hasher.update(&segment.last_key);
        hasher.update(&segment.row_count.to_be_bytes());
        hasher.update(&segment.byte_length.to_be_bytes());
        hasher.update(segment.id_claim.as_bytes());
        hasher.update(&segment.input.input_root);
        hasher.update(segment.input.read_manifest.as_bytes());
        update_coverage_hash(&mut hasher, segment.input.coverage);
    }
    SemanticPlaneRoot(*hasher.finalize().as_bytes())
}

fn plane_coverage_scope(root: SemanticPlaneRoot) -> ScopeRoot {
    let version = ObjectVersion::<SemanticPlaneCoverageScope>::from_value(&root);
    ScopeRoot::from_bytes(version.to_bytes())
}

fn manifest_coverage_scope(root: SemanticManifestRoot) -> ScopeRoot {
    let version = ObjectVersion::<SemanticManifestCoverageScope>::from_value(&root);
    ScopeRoot::from_bytes(version.to_bytes())
}

fn manifest_identity(bytes: &[u8]) -> SemanticManifestRoot {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.ir.manifest.v1\0");
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    SemanticManifestRoot(*hasher.finalize().as_bytes())
}

fn catalog_identity(bytes: &[u8]) -> SemanticPlaneCatalogRoot {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.plane.catalog.v1");
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    SemanticPlaneCatalogRoot(*hasher.finalize().as_bytes())
}

fn validate_catalog_entries(
    entries: &[SemanticPlaneCatalogEntry],
) -> Result<(), SemanticManifestError> {
    if entries.len() > MAX_CATALOG_IMAGES {
        return Err(SemanticManifestError::TooManyCatalogImages {
            observed: entries.len(),
            maximum: MAX_CATALOG_IMAGES,
        });
    }
    for pair in entries.windows(2) {
        if pair[0].image.artifact_ordinal >= pair[1].image.artifact_ordinal {
            return Err(SemanticManifestError::CatalogImageOrder);
        }
    }
    if entries.iter().any(|entry| entry.manifest_length == 0) {
        return Err(SemanticManifestError::CatalogManifestLength);
    }
    Ok(())
}

fn encode_catalog_entries(
    entries: &[SemanticPlaneCatalogEntry],
) -> Result<Box<[u8]>, SemanticManifestError> {
    validate_catalog_entries(entries)?;
    let count = u32::try_from(entries.len())
        .map_err(|_| SemanticManifestError::CatalogImageCountOverflow)?;
    let length = CATALOG_HEADER_BYTES
        .checked_add(
            entries
                .len()
                .checked_mul(CATALOG_ENTRY_BYTES)
                .ok_or(SemanticManifestError::CountOverflow)?,
        )
        .ok_or(SemanticManifestError::CountOverflow)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| SemanticManifestError::AllocationLimit)?;
    bytes.extend_from_slice(CATALOG_MAGIC);
    bytes.extend_from_slice(&count.to_be_bytes());
    for entry in entries {
        bytes.extend_from_slice(&entry.image.artifact_ordinal.to_be_bytes());
        bytes.extend_from_slice(entry.image.semantic_generation.as_bytes());
        bytes.extend_from_slice(entry.image.manifest_root.as_bytes());
        bytes.extend_from_slice(&entry.manifest_length.to_be_bytes());
    }
    if bytes.len() != length {
        return Err(SemanticManifestError::CatalogLength {
            expected: length,
            observed: bytes.len(),
        });
    }
    Ok(bytes.into_boxed_slice())
}

fn manifest_encoded_len(manifest: &SemanticPlaneManifest) -> Result<usize, SemanticManifestError> {
    let mut total = 4_usize + 1 + build_wire_len() + 32 + 32 + 32 + COVERAGE_WIRE_BYTES + 2;
    for plane in &manifest.planes {
        let kind_len = plane_kind_wire_len(plane.kind);
        let segments_len = plane
            .segments
            .len()
            .checked_mul(SEGMENT_WIRE_BYTES)
            .ok_or(SemanticManifestError::CountOverflow)?;
        total = total
            .checked_add(kind_len)
            .and_then(|value| value.checked_add(COVERAGE_WIRE_BYTES))
            .and_then(|value| value.checked_add(4))
            .and_then(|value| value.checked_add(segments_len))
            .ok_or(SemanticManifestError::CountOverflow)?;
    }
    Ok(total)
}

const fn build_wire_len() -> usize {
    32 + 32 + 2 + 1 + 32 + 32 + 32 + 32
}

fn encode_build(value: SemanticBuildIdentity, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.package);
    out.extend_from_slice(&value.target);
    out.extend_from_slice(&<[u8; 2]>::from(value.profile));
    out.push(u8::from(value.stage));
    out.extend_from_slice(&value.recipe);
    out.extend_from_slice(&value.toolchain);
    out.extend_from_slice(&value.environment);
    out.extend_from_slice(&value.target_platform);
}

fn decode_build(reader: &mut Reader<'_>) -> Result<SemanticBuildIdentity, SemanticManifestError> {
    let package = reader.array32()?;
    let target = reader.array32()?;
    let profile_bytes: [u8; 2] = reader
        .take(2)?
        .try_into()
        .map_err(|_| SemanticManifestError::Truncated)?;
    let profile = LanguageProfile::try_from(profile_bytes)
        .map_err(|_| SemanticManifestError::UnknownProfile(profile_bytes))?;
    let stage_byte = reader.u8()?;
    let stage =
        Stage::try_from(stage_byte).map_err(|_| SemanticManifestError::UnknownStage(stage_byte))?;
    Ok(SemanticBuildIdentity {
        package,
        target,
        profile,
        stage,
        recipe: reader.array32()?,
        toolchain: reader.array32()?,
        environment: reader.array32()?,
        target_platform: reader.array32()?,
    })
}

fn encode_plane_kind(value: SemanticPlaneKind, out: &mut Vec<u8>) {
    match value {
        SemanticPlaneKind::Ir(ir) => {
            out.push(1);
            match ir {
                SemanticIrPlane::Core => out.push(1),
                SemanticIrPlane::Types => out.push(2),
                SemanticIrPlane::Relations => out.push(3),
                SemanticIrPlane::Occurrences => out.push(4),
                SemanticIrPlane::Documentation => out.push(5),
                SemanticIrPlane::SourceProvenance => out.push(6),
                SemanticIrPlane::LanguageExtensions(profile) => {
                    out.push(7);
                    out.extend_from_slice(&<[u8; 2]>::from(profile));
                }
            }
        }
        SemanticPlaneKind::Embeddings(value) => {
            out.push(2);
            out.extend_from_slice(&value.model);
            out.extend_from_slice(&value.model_version);
            out.extend_from_slice(&value.tokenizer);
            out.extend_from_slice(&value.dimension.to_be_bytes());
            encode_normalization(value.normalization, out);
            out.extend_from_slice(&value.toolchain);
            out.extend_from_slice(&value.recipe);
        }
    }
}

fn decode_plane_kind(reader: &mut Reader<'_>) -> Result<SemanticPlaneKind, SemanticManifestError> {
    match reader.u8()? {
        1 => {
            let plane = match reader.u8()? {
                1 => SemanticIrPlane::Core,
                2 => SemanticIrPlane::Types,
                3 => SemanticIrPlane::Relations,
                4 => SemanticIrPlane::Occurrences,
                5 => SemanticIrPlane::Documentation,
                6 => SemanticIrPlane::SourceProvenance,
                7 => {
                    let profile_bytes: [u8; 2] = reader
                        .take(2)?
                        .try_into()
                        .map_err(|_| SemanticManifestError::Truncated)?;
                    SemanticIrPlane::LanguageExtensions(
                        LanguageProfile::try_from(profile_bytes)
                            .map_err(|_| SemanticManifestError::UnknownProfile(profile_bytes))?,
                    )
                }
                code => return Err(SemanticManifestError::UnknownIrPlane(code)),
            };
            Ok(SemanticPlaneKind::Ir(plane))
        }
        2 => {
            let model = reader.array32()?;
            let model_version = reader.array32()?;
            let tokenizer = reader.array32()?;
            let dimension = reader.u32()?;
            let normalization = decode_normalization(reader)?;
            let toolchain = reader.array32()?;
            let recipe = reader.array32()?;
            Ok(SemanticPlaneKind::Embeddings(EmbeddingPlaneIdentity::new(
                model,
                model_version,
                tokenizer,
                dimension,
                normalization,
                toolchain,
                recipe,
            )?))
        }
        code => Err(SemanticManifestError::UnknownPlane(code)),
    }
}

fn plane_kind_wire_len(value: SemanticPlaneKind) -> usize {
    match value {
        SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(_)) => 4,
        SemanticPlaneKind::Ir(_) => 2,
        SemanticPlaneKind::Embeddings(_) => 1 + 32 + 32 + 32 + 4 + 33 + 32 + 32,
    }
}

fn encode_normalization(value: EmbeddingNormalization, out: &mut Vec<u8>) {
    match value {
        EmbeddingNormalization::None => {
            out.push(0);
            out.extend_from_slice(&[0; 32]);
        }
        EmbeddingNormalization::L2 => {
            out.push(1);
            out.extend_from_slice(&[0; 32]);
        }
        EmbeddingNormalization::MeanCenteredL2 => {
            out.push(2);
            out.extend_from_slice(&[0; 32]);
        }
        EmbeddingNormalization::Custom(identity) => {
            out.push(3);
            out.extend_from_slice(&identity);
        }
    }
}

fn decode_normalization(
    reader: &mut Reader<'_>,
) -> Result<EmbeddingNormalization, SemanticManifestError> {
    let code = reader.u8()?;
    let identity = reader.array32()?;
    match code {
        0 if identity == [0; 32] => Ok(EmbeddingNormalization::None),
        1 if identity == [0; 32] => Ok(EmbeddingNormalization::L2),
        2 if identity == [0; 32] => Ok(EmbeddingNormalization::MeanCenteredL2),
        3 => Ok(EmbeddingNormalization::Custom(identity)),
        other => Err(SemanticManifestError::UnknownNormalization(other)),
    }
}

fn encode_segment(value: SemanticPlaneSegment, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.first_key);
    out.extend_from_slice(&value.last_key);
    out.extend_from_slice(&value.row_count.to_be_bytes());
    out.extend_from_slice(&value.byte_length.to_be_bytes());
    out.extend_from_slice(value.id_claim.as_bytes());
    out.extend_from_slice(value.input.input_root());
    out.extend_from_slice(value.input.read_manifest_root().as_bytes());
    value.input.coverage.encode(out);
}

fn decode_input_witness(
    reader: &mut Reader<'_>,
) -> Result<SemanticInputWitness, SemanticManifestError> {
    let input_root = reader.array32()?;
    let read_manifest = ScopeRoot::from_bytes(reader.array32()?);
    let coverage = decode_coverage(reader)?;
    if coverage.scope != read_manifest {
        return Err(SemanticManifestError::InputScopeMismatch {
            expected: read_manifest,
            observed: coverage.scope,
        });
    }
    Ok(SemanticInputWitness {
        input_root,
        read_manifest,
        coverage,
    })
}

fn update_plane_kind_hash(hasher: &mut blake3::Hasher, plane: SemanticPlaneKind) {
    hasher.update(&(plane_kind_wire_len(plane) as u16).to_be_bytes());
    match plane {
        SemanticPlaneKind::Ir(ir) => {
            hasher.update(&[1]);
            match ir {
                SemanticIrPlane::Core => {
                    hasher.update(&[1]);
                }
                SemanticIrPlane::Types => {
                    hasher.update(&[2]);
                }
                SemanticIrPlane::Relations => {
                    hasher.update(&[3]);
                }
                SemanticIrPlane::Occurrences => {
                    hasher.update(&[4]);
                }
                SemanticIrPlane::Documentation => {
                    hasher.update(&[5]);
                }
                SemanticIrPlane::SourceProvenance => {
                    hasher.update(&[6]);
                }
                SemanticIrPlane::LanguageExtensions(profile) => {
                    hasher.update(&[7]);
                    hasher.update(&<[u8; 2]>::from(profile));
                }
            };
        }
        SemanticPlaneKind::Embeddings(value) => {
            hasher.update(&[2]);
            hasher.update(&value.model);
            hasher.update(&value.model_version);
            hasher.update(&value.tokenizer);
            hasher.update(&value.dimension.to_be_bytes());
            match value.normalization {
                EmbeddingNormalization::None => {
                    hasher.update(&[0]);
                    hasher.update(&[0; 32]);
                }
                EmbeddingNormalization::L2 => {
                    hasher.update(&[1]);
                    hasher.update(&[0; 32]);
                }
                EmbeddingNormalization::MeanCenteredL2 => {
                    hasher.update(&[2]);
                    hasher.update(&[0; 32]);
                }
                EmbeddingNormalization::Custom(identity) => {
                    hasher.update(&[3]);
                    hasher.update(&identity);
                }
            }
            hasher.update(&value.toolchain);
            hasher.update(&value.recipe);
        }
    }
}

fn update_coverage_hash(hasher: &mut blake3::Hasher, coverage: CoverageRecord) {
    hasher.update(&[coverage_code(coverage.state)]);
    hasher.update(coverage.scope.as_bytes());
    match (coverage.producer, coverage.context, coverage.evidence) {
        (Some(producer), Some(context), Some(evidence)) => {
            hasher.update(&[1]);
            hasher.update(&producer);
            hasher.update(&context);
            hasher.update(&evidence);
        }
        _ => {
            hasher.update(&[0]);
        }
    }
}

fn decode_coverage(reader: &mut Reader<'_>) -> Result<CoverageRecord, SemanticManifestError> {
    let code = reader.u8()?;
    let state = decode_coverage_code(code)?;
    let scope = ScopeRoot::from_bytes(reader.array32()?);
    let identity = reader.u8()?;
    let (producer, context, evidence) = match identity {
        0 => {
            let padding = reader.take(96)?;
            if padding.iter().any(|byte| *byte != 0) {
                return Err(SemanticManifestError::NonCanonicalPadding);
            }
            (None, None, None)
        }
        1 if state == Coverage::Complete => (
            Some(reader.array32()?),
            Some(reader.array32()?),
            Some(reader.array32()?),
        ),
        1 => return Err(SemanticManifestError::CoverageIdentityForNonComplete),
        value => return Err(SemanticManifestError::UnknownCoverageIdentity(value)),
    };
    Ok(CoverageRecord {
        state,
        scope,
        producer,
        context,
        evidence,
        witness: None,
    })
}

fn coverage_code(value: Coverage) -> u8 {
    match value {
        Coverage::Complete => 1,
        Coverage::Partial => 2,
        Coverage::Unavailable => 3,
        Coverage::Unsupported => 4,
        Coverage::Closed => 5,
    }
}

fn decode_coverage_code(value: u8) -> Result<Coverage, SemanticManifestError> {
    match value {
        1 => Ok(Coverage::Complete),
        2 => Ok(Coverage::Partial),
        3 => Ok(Coverage::Unavailable),
        4 => Ok(Coverage::Unsupported),
        5 => Ok(Coverage::Closed),
        code => Err(SemanticManifestError::UnknownCoverage(code)),
    }
}

fn validate_id_set(ids: &[SemanticSegmentId]) -> Result<(), SemanticManifestError> {
    for pair in ids.windows(2) {
        if pair[0] >= pair[1] {
            return Err(SemanticManifestError::LocalIdOrder);
        }
    }
    Ok(())
}

struct Reader<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Reader<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], SemanticManifestError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SemanticManifestError::CountOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(SemanticManifestError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, SemanticManifestError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(SemanticManifestError::Truncated)
    }
    fn u16(&mut self) -> Result<u16, SemanticManifestError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SemanticManifestError::Truncated)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, SemanticManifestError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| SemanticManifestError::Truncated)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, SemanticManifestError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| SemanticManifestError::Truncated)?,
        ))
    }
    fn array32(&mut self) -> Result<[u8; 32], SemanticManifestError> {
        self.take(32)?
            .try_into()
            .map_err(|_| SemanticManifestError::Truncated)
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
}

/// Precise rejection from plane construction, wire reopen, or cursor checks.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SemanticManifestError {
    #[error("embedding dimension must be positive")]
    ZeroEmbeddingDimension,
    #[error("semantic-plane catalog image ordinals must be strictly increasing")]
    CatalogImageOrder,
    #[error("semantic-plane catalog image count exceeds the canonical u32 extent")]
    CatalogImageCountOverflow,
    #[error("semantic-plane catalog manifest length must be nonzero")]
    CatalogManifestLength,
    #[error("semantic-plane catalog contains {observed} images, maximum is {maximum}")]
    TooManyCatalogImages { observed: usize, maximum: usize },
    #[error("semantic-plane catalog bytes are {observed}, maximum is {maximum}")]
    CatalogTooLarge { observed: usize, maximum: usize },
    #[error("semantic-plane catalog byte length is {observed}, expected {expected}")]
    CatalogLength { expected: usize, observed: usize },
    #[error("semantic-plane catalog magic is invalid")]
    BadCatalogMagic,
    #[error("segment stable-key interval is reversed")]
    KeyRange,
    #[error("segment must contain at least one row")]
    ZeroSegmentRows,
    #[error("segment has {observed} bytes, maximum is {maximum}")]
    SegmentBytes { observed: u64, maximum: u64 },
    #[error("segment payload length is {observed}, expected {expected}")]
    SegmentLength { expected: u64, observed: u64 },
    #[error("segment content ID differs: expected {expected:?}, observed {observed:?}")]
    SegmentIdentity {
        expected: UntrustedSemanticSegmentId,
        observed: SemanticSegmentId,
    },
    #[error("segments are not in strict non-overlapping stable-key order")]
    SegmentOrder,
    #[error("semantic planes are not in strict canonical order")]
    PlaneOrder,
    #[error("plane root does not match its segment descriptor")]
    PlaneRootMismatch,
    #[error("plane coverage scope differs: expected {expected:?}, observed {observed:?}")]
    PlaneScopeMismatch {
        expected: ScopeRoot,
        observed: ScopeRoot,
    },
    #[error("manifest closure scope differs: expected {expected:?}, observed {observed:?}")]
    ManifestClosureScopeMismatch {
        expected: ScopeRoot,
        observed: ScopeRoot,
    },
    #[error("canonical manifest closure is not authority-admitted complete")]
    ManifestClosureNotAdmitted,
    #[error("re-admitted coverage evidence differs from its canonical manifest claim")]
    CoverageClaimMismatch,
    #[error(
        "manifest admission supplied {observed_planes}/{expected_planes} plane and {observed_segments}/{expected_segments} segment witnesses"
    )]
    AdmissionWitnessCount {
        expected_planes: usize,
        observed_planes: usize,
        expected_segments: usize,
        observed_segments: usize,
    },
    #[error("input read-manifest scope differs: expected {expected:?}, observed {observed:?}")]
    InputScopeMismatch {
        expected: ScopeRoot,
        observed: ScopeRoot,
    },
    #[error("input coverage is {state:?}, not authority-admitted complete coverage")]
    InputCoverageNotAdmitted { state: Coverage },
    #[error("manifest root is stale: expected {expected:?}, observed {observed:?}")]
    StaleManifest {
        expected: SemanticManifestRoot,
        observed: SemanticManifestRoot,
    },
    #[error("manifest has {observed} planes, maximum is {maximum}")]
    TooManyPlanes { observed: usize, maximum: usize },
    #[error("plane has {observed} segments, maximum is {maximum}")]
    TooManySegments { observed: usize, maximum: usize },
    #[error("manifest is {observed} bytes, maximum is {maximum}")]
    ManifestTooLarge { observed: usize, maximum: usize },
    #[error("canonical manifest magic is invalid")]
    BadMagic,
    #[error("manifest version {0} is unsupported")]
    UnsupportedVersion(u8),
    #[error("unknown semantic plane code {0}")]
    UnknownPlane(u8),
    #[error("unknown IR plane code {0}")]
    UnknownIrPlane(u8),
    #[error("unknown normalization code {0}")]
    UnknownNormalization(u8),
    #[error("unknown coverage code {0}")]
    UnknownCoverage(u8),
    #[error("unknown coverage identity code {0}")]
    UnknownCoverageIdentity(u8),
    #[error("coverage evidence is present for a non-complete state")]
    CoverageIdentityForNonComplete,
    #[error("manifest contains unknown language profile {0:?}")]
    UnknownProfile([u8; 2]),
    #[error("manifest contains unknown compilation stage {0}")]
    UnknownStage(u8),
    #[error("manifest contains nonzero reserved bytes")]
    NonCanonicalPadding,
    #[error("manifest is truncated")]
    Truncated,
    #[error("manifest has {0} trailing bytes")]
    TrailingBytes(usize),
    #[error("manifest length computation overflowed")]
    CountOverflow,
    #[error("bounded metadata allocation failed")]
    AllocationLimit,
    #[error("encoded manifest length differs: expected {expected}, observed {observed}")]
    InternalLengthMismatch { expected: usize, observed: usize },
    #[error("local segment IDs are not in strict sorted order")]
    LocalIdOrder,
    #[error("cursor ordinal {ordinal} exceeds {total} selected segments")]
    CursorOutOfRange { ordinal: u32, total: u32 },
    #[error("cursor checkpoint skips segment {segment:?}, which is absent locally")]
    CursorCheckpointMissingSegment { segment: UntrustedSemanticSegmentId },
    #[error("cursor acknowledgement was {observed:?}, expected {expected:?}")]
    UnexpectedAcknowledgement {
        expected: Option<UntrustedSemanticSegmentId>,
        observed: UntrustedSemanticSegmentId,
    },
}

impl fmt::Display for SemanticCoverageState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} (authorized={})",
            self.state, self.authorized
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use backend_version::{
        AdmittedProducerObservation, AuthorityScopeClaim, CoverageAdmissionError,
        ProducerObservationClaims, ProducerObservationVerifier, UntrustedProducerObservation,
        admit_complete_scope, admit_producer_observation,
    };

    use super::*;
    use crate::ir::RustEdition;
    use crate::vocabulary::Stage;

    struct TestAuthority;
    impl Schema for TestAuthority {
        const DOMAIN: u8 = 0x53;
        const TYPE: u16 = 0xfffe;
        type Value = [u8; 32];
        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    struct TestVerifier;
    impl ProducerObservationVerifier for TestVerifier {
        type Error = CoverageAdmissionError;
        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }

    fn witness_for_version<T: Schema>(version: ObjectVersion<T>) -> CoverageWitness {
        let claim = AuthorityScopeClaim::from_object_version(version);
        let observed = claim.scope_root();
        let producer: AdmittedProducerObservation = admit_producer_observation(
            UntrustedProducerObservation::new([7; 32], observed, [8; 32], vec![9, 10]),
            &TestVerifier,
        )
        .expect("test producer observation is admitted");
        CoverageWitness::Complete(admit_complete_scope(claim, producer).expect("scope matches"))
    }

    fn input(identity: u8) -> SemanticInputWitness {
        let input_root = [identity; 32];
        let scope_value = [identity.wrapping_add(1); 32];
        let version = ObjectVersion::<TestAuthority>::from_value(&scope_value);
        let scope = ScopeRoot::from_bytes(version.to_bytes());
        SemanticInputWitness::admitted(input_root, scope, witness_for_version(version))
            .expect("exact input scope is admitted")
    }

    fn build(platform: u8) -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [platform; 32],
        )
    }

    fn segment(plane: SemanticPlaneKind, key: u8, payload: &[u8]) -> SemanticPlaneSegment {
        SemanticPlaneSegment::from_payload(plane, [key; 32], [key; 32], 1, payload)
            .expect("small canonical segment")
    }

    fn segment_with_input(
        plane: SemanticPlaneKind,
        key: u8,
        payload: &[u8],
        input: SemanticInputWitness,
    ) -> SemanticPlaneSegment {
        SemanticPlaneSegment::from_payload_with_witness(
            plane, [key; 32], [key; 32], 1, payload, input,
        )
        .expect("small canonical segment and exact input witness")
    }

    fn plane(kind: SemanticPlaneKind, segments: Vec<SemanticPlaneSegment>) -> SemanticPlane {
        let root = plane_root(kind, &segments);
        let version = ObjectVersion::<SemanticPlaneCoverageScope>::from_value(&root);
        SemanticPlane::admitted(kind, segments, witness_for_version(version))
            .expect("plane coverage is admitted for its exact root")
    }

    fn manifest(
        build: SemanticBuildIdentity,
        input: SemanticInputWitness,
        planes: Vec<SemanticPlane>,
    ) -> SemanticPlaneManifest {
        SemanticPlaneManifest::new(
            GenerationId::from_canonical_bytes(b"fixture semantic VCS generation"),
            build,
            input,
            planes,
        )
        .expect("canonical plane order")
    }

    #[test]
    fn coverage_wire_is_fixed_width_for_complete_and_partial_claims() {
        const EXPECTED_COVERAGE_BYTES: usize = 130;

        let complete = input(201).coverage;
        let mut complete_bytes = Vec::new();
        complete.encode(&mut complete_bytes);
        assert_eq!(complete_bytes.len(), EXPECTED_COVERAGE_BYTES);
        assert_eq!(complete_bytes[0], coverage_code(Coverage::Complete));
        assert_eq!(complete_bytes[33], 1);
        let mut complete_reader = Reader::new(&complete_bytes);
        let complete_reopened =
            decode_coverage(&mut complete_reader).expect("complete coverage bytes decode");
        assert!(complete_reader.is_empty());
        assert!(complete.matches_claim(complete_reopened));

        let partial = CoverageRecord::claimed(Coverage::Partial, ScopeRoot::from_bytes([0xA5; 32]));
        let mut partial_bytes = Vec::new();
        partial.encode(&mut partial_bytes);
        assert_eq!(partial_bytes.len(), EXPECTED_COVERAGE_BYTES);
        assert_eq!(partial_bytes[0], coverage_code(Coverage::Partial));
        assert_eq!(&partial_bytes[1..33], &[0xA5; 32]);
        assert_eq!(partial_bytes[33], 0);
        assert!(partial_bytes[34..].iter().all(|byte| *byte == 0));
        let mut partial_reader = Reader::new(&partial_bytes);
        let partial_reopened =
            decode_coverage(&mut partial_reader).expect("partial coverage bytes decode");
        assert!(partial_reader.is_empty());
        assert!(partial.matches_claim(partial_reopened));
    }

    #[test]
    fn v2_input_commitment_binds_deterministic_claims_but_not_admission_state() {
        let admitted = input(210);
        let same = input(210);
        let changed_root = input(211);
        let claimed =
            SemanticInputWitness::claimed(*admitted.input_root(), admitted.read_manifest_root());

        assert_eq!(
            admitted.generation_root_commitment_v2(),
            same.generation_root_commitment_v2()
        );
        assert_ne!(
            admitted.generation_root_commitment_v2(),
            changed_root.generation_root_commitment_v2()
        );
        assert_eq!(
            admitted.generation_root_commitment_v2(),
            claimed.generation_root_commitment_v2()
        );
        assert!(admitted.coverage().is_authorized_complete());
        assert!(!claimed.coverage().is_authorized_complete());
    }

    #[test]
    fn semantic_generation_is_independent_of_and_committed_by_manifest_root() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let input = input(202);
        let plane = plane(ir, vec![segment(ir, 1, b"canonical image rows")]);
        let generation_a = GenerationId::from_canonical_bytes(b"semantic image A");
        let generation_b = GenerationId::from_canonical_bytes(b"semantic image B");
        let first = SemanticPlaneManifest::new(generation_a, build(1), input, vec![plane.clone()])
            .expect("first canonical manifest");
        let second = SemanticPlaneManifest::new(generation_b, build(1), input, vec![plane])
            .expect("second canonical manifest");

        assert_eq!(first.build().target(), second.build().target());
        assert_ne!(first.semantic_generation(), second.semantic_generation());
        assert_ne!(first.root(), second.root());
        let reopened =
            SemanticPlaneManifest::decode(&first.encode().expect("canonical versioned manifest"))
                .expect("manifest reopens without substituting a selection root");
        assert_eq!(reopened.semantic_generation(), generation_a);
        assert_ne!(
            reopened.semantic_generation().as_bytes(),
            reopened.root().as_bytes()
        );
    }

    #[test]
    fn one_edit_fetches_only_the_changed_stable_range() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let unchanged_reads = input(101);
        let unchanged_target_reads = input(104);
        let old_reads = input(102);
        let edited_reads = input(103);
        let base = manifest(
            build(1),
            input(10),
            vec![plane(
                ir,
                vec![
                    segment_with_input(ir, 1, b"unchanged", unchanged_reads),
                    segment_with_input(ir, 3, b"old", old_reads),
                ],
            )],
        );
        let target = manifest(
            build(1),
            input(11),
            vec![plane(
                ir,
                vec![
                    segment_with_input(ir, 1, b"unchanged", unchanged_target_reads),
                    segment_with_input(ir, 3, b"edited", edited_reads),
                ],
            )],
        );
        let mut delta =
            SemanticDeltaCursor::new(&base, &target, base.root()).expect("base root is exact");
        let Some(SemanticDeltaAction::Reuse {
            segment_id,
            segment,
            ..
        }) = delta.next_action()
        else {
            panic!("identical stable-key bytes should reuse across complete input roots");
        };
        assert_eq!(
            segment_id,
            base.planes()[0].segments()[0].admitted_id().unwrap()
        );
        assert_eq!(
            segment.id_claim(),
            base.planes()[0].segments()[0].id_claim()
        );
        assert!(
            !base.planes()[0].segments()[0]
                .input_witness()
                .same_admitted_frontier(&target.planes()[0].segments()[0].input_witness())
        );
        assert!(matches!(
            delta.next_action(),
            Some(SemanticDeltaAction::Fetch(_))
        ));
        assert_eq!(delta.next_action(), None);
    }

    #[test]
    fn partial_segment_input_witness_never_authorizes_reuse() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let base = manifest(
            build(1),
            input(31),
            vec![plane(
                ir,
                vec![segment_with_input(ir, 1, b"same", input(32))],
            )],
        );
        let partial_segment_input =
            SemanticInputWitness::claimed([33; 32], ScopeRoot::from_bytes([34; 32]));
        let target = manifest(
            build(1),
            input(35),
            vec![plane(
                ir,
                vec![segment_with_input(ir, 1, b"same", partial_segment_input)],
            )],
        );
        let mut delta = SemanticDeltaCursor::new(&base, &target, base.root())
            .expect("both package input closures are admitted");
        assert!(matches!(
            delta.next_action(),
            Some(SemanticDeltaAction::Fetch(_))
        ));
        assert_eq!(delta.next_action(), None);
    }

    #[test]
    fn cold_reopened_target_reuses_admitted_claims_without_reading_target_ranges() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let unchanged_reads = input(111);
        let old_reads = input(112);
        let edited_reads = input(113);
        let base = manifest(
            build(1),
            input(110),
            vec![plane(
                ir,
                vec![
                    segment_with_input(ir, 1, b"unchanged", unchanged_reads),
                    segment_with_input(ir, 3, b"old", old_reads),
                ],
            )],
        );
        let target = manifest(
            build(1),
            input(120),
            vec![plane(
                ir,
                vec![
                    segment_with_input(ir, 1, b"unchanged", unchanged_reads),
                    segment_with_input(ir, 3, b"edited", edited_reads),
                ],
            )],
        );
        let bytes = target.encode().expect("bounded canonical manifest");
        let reopened = SemanticPlaneManifest::decode(&bytes).expect("cold reopen");
        assert!(!reopened.claims_admitted());

        let closure_version =
            ObjectVersion::<SemanticManifestCoverageScope>::from_value(&target.root());
        let input_witness = target
            .input()
            .authority_witness()
            .expect("source input witness is live");
        let plane = &target.planes()[0];
        let plane_witness = plane
            .authority_witness()
            .expect("source plane witness is live");
        let segment_witnesses = plane
            .segments()
            .iter()
            .map(|segment| {
                segment
                    .input_witness()
                    .authority_witness()
                    .expect("source range witness is live")
            })
            .collect::<Vec<_>>();
        let target = reopened
            .admit_from_closure(
                target.root(),
                witness_for_version(closure_version),
                input_witness,
                &[plane_witness],
                &segment_witnesses,
                &[],
            )
            .expect("exact canonical closure and witnesses are admitted");
        assert!(target.claims_admitted());
        assert!(
            target.planes()[0]
                .segments()
                .iter()
                .all(|segment| segment.admitted_id().is_none())
        );

        let mut delta =
            SemanticDeltaCursor::new(&base, &target, base.root()).expect("exact base root");
        let Some(SemanticDeltaAction::Reuse {
            segment_id,
            segment,
            ..
        }) = delta.next_action()
        else {
            panic!("unchanged exact range should reuse the checked base bytes");
        };
        assert_eq!(segment.first_key(), &[1; 32]);
        assert_eq!(
            segment_id,
            base.planes()[0].segments()[0].admitted_id().unwrap()
        );
        assert!(segment.admitted_id().is_none());
        assert!(matches!(
            delta.next_action(),
            Some(SemanticDeltaAction::Fetch(_))
        ));
        assert_eq!(delta.next_action(), None);
    }

    #[test]
    fn embedding_model_change_does_not_change_ir_root() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let embedding_one = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [20; 32],
                [21; 32],
                [24; 32],
                768,
                EmbeddingNormalization::L2,
                [22; 32],
                [23; 32],
            )
            .expect("positive dimension"),
        );
        let embedding_two = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [30; 32],
                [31; 32],
                [34; 32],
                1024,
                EmbeddingNormalization::MeanCenteredL2,
                [32; 32],
                [33; 32],
            )
            .expect("positive dimension"),
        );
        let base = manifest(
            build(1),
            input(40),
            vec![
                plane(ir, vec![segment(ir, 1, b"same-ir")]),
                plane(embedding_one, vec![segment(embedding_one, 1, b"vector-v1")]),
            ],
        );
        let changed = manifest(
            build(1),
            input(40),
            vec![
                plane(ir, vec![segment(ir, 1, b"same-ir")]),
                plane(embedding_two, vec![segment(embedding_two, 1, b"vector-v2")]),
            ],
        );
        assert_eq!(
            base.ir_root().expect("IR root"),
            changed.ir_root().expect("IR root")
        );
        assert_ne!(base.root(), changed.root());
    }

    #[test]
    fn tokenizer_identity_changes_only_the_embedding_plane_root() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let first_kind = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [40; 32],
                [41; 32],
                [42; 32],
                384,
                EmbeddingNormalization::L2,
                [43; 32],
                [44; 32],
            )
            .expect("positive dimension"),
        );
        let changed_tokenizer = SemanticPlaneKind::Embeddings(
            EmbeddingPlaneIdentity::new(
                [40; 32],
                [41; 32],
                [52; 32],
                384,
                EmbeddingNormalization::L2,
                [43; 32],
                [44; 32],
            )
            .expect("positive dimension"),
        );
        let first = manifest(
            build(1),
            input(60),
            vec![
                plane(ir, vec![segment(ir, 1, b"same-ir")]),
                plane(first_kind, vec![segment(first_kind, 1, b"same-vector")]),
            ],
        );
        let changed = manifest(
            build(1),
            input(60),
            vec![
                plane(ir, vec![segment(ir, 1, b"same-ir")]),
                plane(
                    changed_tokenizer,
                    vec![segment(changed_tokenizer, 1, b"same-vector")],
                ),
            ],
        );
        assert_eq!(first.ir_root(), changed.ir_root());
        assert_ne!(first.root(), changed.root());
        assert_eq!(first.planes()[1].segments()[0].byte_length(), 11);
        assert_eq!(changed.planes()[1].segments()[0].byte_length(), 11);
    }

    #[test]
    fn plane_catalog_wire_is_canonical_and_binds_generation_root_and_length() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let first_manifest = manifest(
            build(1),
            input(70),
            vec![plane(ir, vec![segment(ir, 1, b"catalog image")])],
        );
        let first_key = SemanticPlaneImageKey::from_manifest(0, &first_manifest);
        let entry = SemanticPlaneCatalogEntry::new(first_key, 120).expect("nonzero length");
        let catalog = SemanticPlaneCatalog::new(vec![entry]).expect("ordered catalog");
        let bytes = catalog.encode().expect("canonical encoding");
        assert_eq!(&bytes[..7], b"VPCAT\0\x01");
        assert_eq!(bytes.len(), CATALOG_HEADER_BYTES + CATALOG_ENTRY_BYTES);
        assert_eq!(catalog.root(), catalog_identity(&bytes));
        let reopened = SemanticPlaneCatalog::decode(&bytes).expect("canonical reopen");
        assert_eq!(reopened, catalog);
        assert_eq!(
            SemanticPlaneCatalogRoot::from_wire_claim(*catalog.root().as_bytes()),
            reopened.root()
        );
        assert_eq!(reopened.entries()[0].image(), first_key);
        assert_eq!(reopened.entries()[0].manifest_length(), 120);

        let changed_generation =
            SemanticPlaneImageKey::new(0, GenerationId::from_raw([99; 32]), first_manifest.root());
        let changed = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(changed_generation, 120).expect("nonzero length"),
        ])
        .expect("ordered catalog");
        assert_ne!(changed.root(), catalog.root());

        let changed_length = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(first_key, 121).expect("nonzero length"),
        ])
        .expect("ordered catalog");
        assert_ne!(changed_length.root(), catalog.root());
        let duplicate_ordinals = vec![
            SemanticPlaneCatalogEntry::new(first_key, 120).expect("nonzero length"),
            SemanticPlaneCatalogEntry::new(first_key, 120).expect("nonzero length"),
        ];
        assert_eq!(
            SemanticPlaneCatalog::new(duplicate_ordinals),
            Err(SemanticManifestError::CatalogImageOrder)
        );

        let second_manifest = manifest(
            build(2),
            input(71),
            vec![plane(ir, vec![segment(ir, 1, b"second catalog image")])],
        );
        let first_image = SemanticPlaneImageKey::from_manifest(0, &first_manifest);
        let second_image = SemanticPlaneImageKey::from_manifest(1, &second_manifest);
        let ordered_two_image_catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(first_image, 120).expect("nonzero length"),
            SemanticPlaneCatalogEntry::new(second_image, 121).expect("nonzero length"),
        ])
        .expect("strictly increasing canonical ordinals");
        let reordered_two_image_catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(
                SemanticPlaneImageKey::from_manifest(0, &second_manifest),
                121,
            )
            .expect("nonzero length"),
            SemanticPlaneCatalogEntry::new(
                SemanticPlaneImageKey::from_manifest(1, &first_manifest),
                120,
            )
            .expect("nonzero length"),
        ])
        .expect("strictly increasing canonical ordinals");
        assert_ne!(
            ordered_two_image_catalog.root(),
            reordered_two_image_catalog.root(),
            "changing which generation occupies each canonical artifact ordinal changes the catalog root"
        );
        assert_eq!(
            SemanticPlaneCatalog::new(vec![
                SemanticPlaneCatalogEntry::new(second_image, 121).expect("nonzero length"),
                SemanticPlaneCatalogEntry::new(first_image, 120).expect("nonzero length"),
            ]),
            Err(SemanticManifestError::CatalogImageOrder),
            "wire entries cannot be physically reordered without canonical ordinal order"
        );
    }

    #[test]
    fn missing_segment_is_a_bounded_storage_neutral_request() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Relations);
        let range_reads = input(51);
        let value = manifest(
            build(1),
            input(50),
            vec![plane(
                ir,
                vec![
                    segment_with_input(ir, 1, b"one", range_reads),
                    segment_with_input(ir, 3, b"three", range_reads),
                ],
            )],
        );
        let have = [segment(ir, 1, b"one")
            .admitted_id()
            .expect("producer observed payload bytes")];
        let mut cursor = value
            .hydration_cursor(value.root(), Some(ir), &have)
            .expect("exact manifest root");
        let request = cursor.next_request().expect("second range is missing");
        assert_eq!(request.plane, ir);
        assert_eq!(request.first_key, [3; 32]);
        assert_eq!(request.byte_length, 5);
        assert_eq!(cursor.next_request(), Some(request));
        let checked_id = value.planes()[0].segments()[1]
            .admit(ir, b"three")
            .expect("payload matches range request");
        cursor
            .acknowledge(checked_id)
            .expect("acknowledges only current range");
        assert!(cursor.coverage().is_reusable());
    }

    #[test]
    fn cursor_resume_rejects_checkpoint_whose_acknowledged_segment_is_not_durable() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Relations);
        let range_reads = input(151);
        let value = manifest(
            build(1),
            input(150),
            vec![plane(
                ir,
                vec![
                    segment_with_input(ir, 1, b"one", range_reads),
                    segment_with_input(ir, 3, b"three", range_reads),
                ],
            )],
        );
        let mut cursor = value
            .hydration_cursor(value.root(), Some(ir), &[])
            .expect("exact manifest root");
        let first_request = cursor.next_request().expect("first range requested");
        let checked_first = value.planes()[0].segments()[0]
            .admit(ir, b"one")
            .expect("first range bytes admitted");
        assert_eq!(
            first_request.segment_id.as_bytes(),
            checked_first.as_bytes()
        );
        cursor
            .acknowledge(checked_first)
            .expect("receipt advances cursor after caller verification");
        let second_request = cursor.next_request().expect("second range requested");
        let token = cursor.checkpoint();

        assert!(matches!(
            SemanticHydrationCursor::resume(&value, token, &[]),
            Err(SemanticManifestError::CursorCheckpointMissingSegment { .. })
        ));
        let durable_have = [checked_first];
        let mut resumed = SemanticHydrationCursor::resume(&value, token, &durable_have)
            .expect("durable prefix permits resume");
        assert_eq!(resumed.next_request(), Some(second_request));
    }

    #[test]
    fn cold_reopen_preserves_root_but_downgrades_witnesses_to_claims() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Documentation);
        let value = manifest(
            build(1),
            input(60),
            vec![plane(ir, vec![segment(ir, 1, b"docs")])],
        );
        let bytes = value.encode().expect("bounded canonical metadata");
        let reopened = SemanticPlaneManifest::decode(&bytes).expect("cold metadata reopen");
        assert_eq!(value.root(), reopened.root());
        assert!(!reopened.input().coverage().is_authorized_complete());
        assert!(!reopened.planes()[0].coverage().is_authorized_complete());
        let mut delta =
            SemanticDeltaCursor::new(&value, &reopened, value.root()).expect("exact base");
        assert!(matches!(
            delta.next_action(),
            Some(SemanticDeltaAction::Fetch(_))
        ));
    }

    #[test]
    fn stale_base_is_rejected_before_delta_iteration() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Types);
        let base = manifest(
            build(1),
            input(70),
            vec![plane(ir, vec![segment(ir, 1, b"type")])],
        );
        let target = manifest(
            build(1),
            input(71),
            vec![plane(ir, vec![segment(ir, 1, b"type")])],
        );
        let wrong = SemanticManifestRoot([99; 32]);
        assert!(matches!(
            SemanticDeltaCursor::new(&base, &target, wrong),
            Err(SemanticManifestError::StaleManifest { .. })
        ));
    }

    #[test]
    fn mixed_platform_inputs_never_reuse_segments() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let base = manifest(
            build(1),
            input(80),
            vec![plane(ir, vec![segment(ir, 1, b"same")])],
        );
        let target = manifest(
            build(2),
            input(81),
            vec![plane(ir, vec![segment(ir, 1, b"same")])],
        );
        let mut delta = SemanticDeltaCursor::new(&base, &target, base.root()).expect("exact base");
        assert!(matches!(
            delta.next_action(),
            Some(SemanticDeltaAction::Fetch(_))
        ));
    }

    #[test]
    fn payload_verification_rejects_tampering_and_cursor_tokens_are_root_bound() {
        let ir = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let value = manifest(
            build(1),
            input(90),
            vec![plane(ir, vec![segment(ir, 1, b"fact")])],
        );
        let claim_segment = value.planes()[0].segments()[0];
        assert!(matches!(
            claim_segment.admit(ir, b"fake"),
            Err(SemanticManifestError::SegmentIdentity { .. })
        ));
        let mut cursor = value
            .hydration_cursor(value.root(), Some(ir), &[])
            .expect("cursor");
        let _ = cursor.next_request();
        let token =
            SemanticHydrationCursorToken::decode(&cursor.checkpoint().encode().expect("token"))
                .expect("token reopen");
        assert_eq!(token.manifest_root(), value.root());
        assert!(SemanticHydrationCursor::resume(&value, token, &[]).is_ok());
        let other = manifest(
            build(1),
            input(91),
            vec![plane(ir, vec![segment(ir, 1, b"fact")])],
        );
        assert!(matches!(
            SemanticHydrationCursor::resume(&other, token, &[]),
            Err(SemanticManifestError::StaleManifest { .. })
        ));
    }

    #[test]
    fn streaming_segment_admission_checks_chunks_length_and_content_id() {
        let plane = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let payload = (0..70_123)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let segment = segment(plane, 42, &payload);

        let mut verifier = segment.streaming_admission(plane);
        let chunk_sizes = [1, 31, 4093, 17, 16_384, 997];
        let mut offset = 0;
        let mut chunk = 0;
        while offset < payload.len() {
            let end = (offset + chunk_sizes[chunk % chunk_sizes.len()]).min(payload.len());
            verifier
                .update(&payload[offset..end])
                .expect("bounded chunk is accepted");
            offset = end;
            chunk += 1;
        }
        assert_eq!(
            verifier.finish().expect("complete payload is admitted"),
            segment
                .admit(plane, &payload)
                .expect("borrowed admission is the same identity")
        );

        let mut truncated = segment.streaming_admission(plane);
        truncated
            .update(&payload[..payload.len() - 1])
            .expect("truncated prefix is accepted until finish");
        assert!(matches!(
            truncated.finish(),
            Err(SemanticManifestError::SegmentLength {
                expected,
                observed
            }) if expected == payload.len() as u64 && observed == payload.len() as u64 - 1
        ));

        let mut corrupted_payload = payload.clone();
        corrupted_payload[32_777] ^= 0x80;
        let mut corrupted = segment.streaming_admission(plane);
        for bytes in corrupted_payload.chunks(8191) {
            corrupted
                .update(bytes)
                .expect("same-length corrupted chunk is accepted for hashing");
        }
        assert!(matches!(
            corrupted.finish(),
            Err(SemanticManifestError::SegmentIdentity { .. })
        ));

        let mut overlong = segment.streaming_admission(plane);
        overlong
            .update(&payload)
            .expect("declared payload length is accepted");
        assert!(matches!(
            overlong.update(b"x"),
            Err(SemanticManifestError::SegmentLength { .. })
        ));
    }
}
