//! Typed semantic-generation authority roots.
//!
//! V2 semantic content identity commits to semantic authority/profile facts
//! and every complete normalized typed-row family. Its generation identity
//! adds the canonical build and deterministic input/read-frontier claim.
//! Neither root identifies an NXFI serialization. Image provenance and live
//! admission evidence are verified metadata outside the content root; volatile
//! producer, context, and evidence also stay outside both hashes so equivalent
//! derivations can share content across sessions.
//! Family completeness uses V2's normalized-reachable policy, so unreferenced
//! legacy NXFI pool rows are not part of this semantic authority.

use crate::ir::versioned_records::aggregate::{
    VerifiedTypedPlaneInventoryV2, VerifiedTypedPlaneSegmentV2,
};
use crate::ir::{
    ImageProvenance, SemanticBuildIdentity, SemanticImageAuthority, SemanticImageFacts,
    SemanticInputWitness, SemanticIrPlane,
};
use backend_version::Coverage;
use thiserror::Error;

const CONTENT_ROOT_DOMAIN: &str = "backend.semantic.ir.content.v2";
const GENERATION_ROOT_DOMAIN: &str = "backend.semantic.ir.generation.v2";
const FAMILY_DOMAIN: &str = "backend.semantic.ir.family.v2";
const ROOT_FORMAT_VERSION: u8 = 2;
const IR_FAMILY_COUNT: usize = 7;

/// Content identity of one complete normalized-reachable typed IR closure.
///
/// This root commits semantic authority/profile facts and all seven complete
/// typed family roots/counts. Source, recipe, and scope provenance remain
/// checked metadata outside the content root, so equal semantic output can
/// share content storage across derivations.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticContentRootV2([u8; 32]);

impl SemanticContentRootV2 {
    /// Returns the canonical fixed-width content-root bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns whether this computed root equals an untrusted wire claim.
    #[must_use]
    pub fn matches_wire_claim(self, claim: UntrustedSemanticContentRootV2) -> bool {
        self.0 == claim.0
    }

    /// Returns the content identity computed by the semantic all-family verifier.
    #[must_use]
    pub const fn from_verified_closure(closure: &VerifiedTypedPlaneClosureV2) -> Self {
        closure.content_root
    }
}

/// An untrusted V2 content-root claim decoded from a wire manifest.
///
/// The claim cannot be used as a semantic content identity until it has been
/// compared with [`VerifiedTypedPlaneClosureV2::content_root`].
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UntrustedSemanticContentRootV2([u8; 32]);

impl UntrustedSemanticContentRootV2 {
    /// Retains bytes from an untrusted wire claim without admitting them.
    #[must_use]
    pub const fn from_wire_claim(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the untrusted fixed-width claim.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Checks this wire claim against an independently verified typed root.
    #[must_use]
    pub fn matches(self, root: SemanticContentRootV2) -> bool {
        self.0 == root.0
    }
}

/// Derivation identity of one complete typed semantic generation.
///
/// Unlike V1 [`GenerationId`](crate::ir::GenerationId), this value is derived
/// from typed family content plus canonical build and deterministic
/// input/read-frontier claims, rather than from NXFI bytes. Admission evidence
/// still has to be live and complete when the root is minted, but volatile
/// producer, session, and evidence identities are kept outside this root.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticGenerationRootV2([u8; 32]);

impl SemanticGenerationRootV2 {
    /// Returns the canonical fixed-width root bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns whether this computed root equals an untrusted wire claim.
    #[must_use]
    pub fn matches_wire_claim(self, claim: UntrustedSemanticGenerationRootV2) -> bool {
        self.0 == claim.0
    }

    /// Returns the identity computed by the semantic all-family verifier.
    #[must_use]
    pub const fn from_verified_closure(closure: &VerifiedTypedPlaneClosureV2) -> Self {
        closure.generation_root
    }
}

/// An untrusted V2 generation-root claim decoded from a wire manifest.
///
/// The claim cannot be used as a semantic-generation identity until it has
/// been compared with [`VerifiedTypedPlaneClosureV2::generation_root`].
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UntrustedSemanticGenerationRootV2([u8; 32]);

impl UntrustedSemanticGenerationRootV2 {
    /// Retains bytes from an untrusted wire claim without admitting them.
    #[must_use]
    pub const fn from_wire_claim(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the untrusted fixed-width claim.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Checks this wire claim against an independently verified typed root.
    #[must_use]
    pub fn matches(self, root: SemanticGenerationRootV2) -> bool {
        self.0 == root.0
    }
}

/// Content root of one mandatory V2 IR family.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticGenerationFamilyRootV2([u8; 32]);

impl SemanticGenerationFamilyRootV2 {
    /// Returns the canonical fixed-width family-root bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Content root and exact row count for one mandatory V2 IR family.
///
/// The seven entries are kept in a fixed order. A zero-row family still has a
/// descriptor and a nonzero canonical empty-family root, so omission cannot
/// be interpreted as an empty family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticGenerationFamilyCommitmentV2 {
    family: SemanticIrPlane,
    family_root: SemanticGenerationFamilyRootV2,
    row_count: u64,
}

impl SemanticGenerationFamilyCommitmentV2 {
    /// The exact mandatory IR family represented by this descriptor.
    #[must_use]
    pub const fn family(&self) -> SemanticIrPlane {
        self.family
    }

    /// Content root over its admitted segment IDs, ranges, and exact counts.
    #[must_use]
    pub const fn family_root(&self) -> SemanticGenerationFamilyRootV2 {
        self.family_root
    }

    /// Number of canonical typed rows in this family.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Builds a family commitment from exact segment payloads already
    /// admitted and decoded by the aggregate semantic verifier. An empty
    /// slice is an explicit empty family and receives its own domain root.
    fn from_verified_family_segments(
        family: SemanticIrPlane,
        segments: &[VerifiedTypedPlaneSegmentV2],
        generation_witness: SemanticInputWitness,
    ) -> Result<Self, SemanticGenerationProofError> {
        for (index, segment) in segments.iter().enumerate() {
            validate_segment_witness(family, segment.input_witness(), generation_witness)?;
            if segment.first_key() > segment.last_key() {
                return Err(SemanticGenerationProofError::SegmentOrder { index });
            }
            if let Some(previous) = index
                .checked_sub(1)
                .and_then(|previous| segments.get(previous))
                && previous.last_key() >= segment.first_key()
            {
                return Err(SemanticGenerationProofError::SegmentOrder { index });
            }
        }

        let row_count = segments.iter().try_fold(0_u64, |total, segment| {
            total.checked_add(u64::from(segment.row_count()))
        });
        let row_count = row_count.ok_or(SemanticGenerationProofError::RowCountOverflow)?;
        let family_root = compute_family_root(family, segments)?;
        Ok(Self {
            family,
            family_root,
            row_count,
        })
    }
}

/// Opaque proof that all mandatory typed semantic families were decoded,
/// cross-checked, and bound to one admitted image/input statement.
///
/// The only mint method consumes the aggregate verifier's opaque inventory
/// token. Remote admission code cannot create one from c005 claims or c004
/// bytes whose grammar, census, cross-family closure, and read binding have
/// not all been checked. This semantic proof is not an owner selection
/// capability; selecting or publishing a generation still requires the
/// engine's separately admitted input/read-frontier authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedTypedPlaneClosureV2 {
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    family_commitments: [SemanticGenerationFamilyCommitmentV2; IR_FAMILY_COUNT],
    content_root: SemanticContentRootV2,
    generation_root: SemanticGenerationRootV2,
}

impl VerifiedTypedPlaneClosureV2 {
    /// Mints semantic roots from a token issued by the semantic all-family
    /// verifier. This method accepts no wire claims or raw segment material.
    pub(crate) fn from_verified_inventory(
        inventory: VerifiedTypedPlaneInventoryV2,
    ) -> Result<Self, SemanticGenerationProofError> {
        let build = inventory.build();
        let image_facts = inventory.image_facts();
        let input_witness = inventory.input_witness();
        validate_image_facts(build, image_facts)?;
        validate_input_witness(input_witness)?;

        let summaries = inventory.families();
        let commitments = [
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::Core,
                summaries[0].segments(),
                input_witness,
            )?,
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::Types,
                summaries[1].segments(),
                input_witness,
            )?,
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::Relations,
                summaries[2].segments(),
                input_witness,
            )?,
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::Occurrences,
                summaries[3].segments(),
                input_witness,
            )?,
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::Documentation,
                summaries[4].segments(),
                input_witness,
            )?,
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::SourceProvenance,
                summaries[5].segments(),
                input_witness,
            )?,
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(
                SemanticIrPlane::LanguageExtensions(build.profile()),
                summaries[6].segments(),
                input_witness,
            )?,
        ];

        for (index, (summary, commitment)) in summaries.iter().zip(&commitments).enumerate() {
            let expected = required_family(index, build.profile());
            if summary.family() != expected || commitment.family != expected {
                return Err(SemanticGenerationProofError::FamilyInventory {
                    index,
                    expected,
                    observed: summary.family(),
                });
            }
        }

        for commitment in &commitments {
            if commitment.family_root.as_bytes() == &[0; 32] {
                return Err(SemanticGenerationProofError::ZeroFamilyRoot {
                    family: commitment.family,
                });
            }
        }

        let content_root = compute_content_root(image_facts, &commitments);
        let generation_root = compute_generation_root(build, input_witness, content_root);
        Ok(Self {
            build,
            image_facts,
            input_witness,
            family_commitments: commitments,
            content_root,
            generation_root,
        })
    }

    /// The exact build identity included in the root preimage.
    #[must_use]
    pub const fn build(&self) -> &SemanticBuildIdentity {
        &self.build
    }

    /// Canonical image authority and provenance facts retained by this proof.
    /// Only authority/profile enters the content root; provenance is checked
    /// against build/input authority and remains metadata.
    #[must_use]
    pub const fn image_facts(&self) -> SemanticImageFacts {
        self.image_facts
    }

    /// Exact input/read witness statement included in the root.
    #[must_use]
    pub const fn input_witness(&self) -> SemanticInputWitness {
        self.input_witness
    }

    /// The fixed, complete family inventory committed by this closure.
    #[must_use]
    pub const fn family_commitments(
        &self,
    ) -> &[SemanticGenerationFamilyCommitmentV2; IR_FAMILY_COUNT] {
        &self.family_commitments
    }

    /// The independently computed typed semantic content identity.
    #[must_use]
    pub const fn content_root(&self) -> SemanticContentRootV2 {
        self.content_root
    }

    /// The independently computed typed semantic derivation identity.
    #[must_use]
    pub const fn generation_root(&self) -> SemanticGenerationRootV2 {
        self.generation_root
    }
}

/// Failure to prove a complete typed semantic-generation closure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SemanticGenerationProofError {
    /// The source/read witness has no live complete authority admission.
    #[error("semantic generation input/read witness is not admitted complete: {state:?}")]
    InputWitnessNotComplete { state: Coverage },
    /// A segment's per-range source/read witness is not live and complete.
    #[error("semantic {family:?} segment input/read witness is not admitted complete: {state:?}")]
    SegmentWitnessNotComplete {
        family: SemanticIrPlane,
        state: Coverage,
    },
    /// A live segment witness does not bind the exact generation read frontier.
    #[error(
        "semantic {family:?} segment read witness differs from the admitted generation frontier"
    )]
    SegmentWitnessMismatch { family: SemanticIrPlane },
    /// A family was missing, duplicated, or out of the required canonical order.
    #[error(
        "semantic generation family inventory differs at slot {index}: expected {expected:?}, observed {observed:?}"
    )]
    FamilyInventory {
        index: usize,
        expected: SemanticIrPlane,
        observed: SemanticIrPlane,
    },
    /// Segment ranges overlap or are not in strict canonical order.
    #[error("semantic family segment ranges overlap or are unordered at index {index}")]
    SegmentOrder { index: usize },
    /// A family descriptor has no canonical content root.
    #[error("semantic generation family {family:?} has a zero family root")]
    ZeroFamilyRoot { family: SemanticIrPlane },
    /// The sum of rows in one family exceeds the supported width.
    #[error("semantic generation family row count overflows u64")]
    RowCountOverflow,
    /// One family has more segments than its canonical root framing can hold.
    #[error("semantic generation family segment count overflows u32")]
    SegmentCountOverflow,
    /// Image-level facts contradict the canonical build identity.
    #[error("semantic image facts differ from build identity at {field}")]
    ImageFactsBuildMismatch { field: &'static str },
}

fn validate_image_facts(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
) -> Result<(), SemanticGenerationProofError> {
    if let SemanticImageAuthority::Language(profile) = image_facts.authority
        && profile != build.profile()
    {
        return Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
            field: "language authority profile",
        });
    }
    if let ImageProvenance::Captured { recipe, .. } = image_facts.provenance {
        if recipe.profile != build.profile() {
            return Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
                field: "recipe profile",
            });
        }
        if recipe.stage != build.stage() {
            return Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
                field: "recipe stage",
            });
        }
        if recipe.identity.as_ref() != build.recipe() {
            return Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
                field: "recipe identity",
            });
        }
        if recipe.toolchain.as_ref() != build.toolchain() {
            return Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
                field: "recipe toolchain",
            });
        }
    }
    if let ImageProvenance::Captured { source, recipe, .. } = image_facts.provenance {
        let expected = crate::vocabulary::CompileRecipeFact::derive(
            recipe.profile,
            recipe.stage,
            recipe.tool,
            source.identity,
            recipe.toolchain,
        );
        if recipe.identity != expected.identity {
            return Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
                field: "source-bound recipe identity",
            });
        }
    }
    Ok(())
}

fn validate_input_witness(
    input_witness: SemanticInputWitness,
) -> Result<(), SemanticGenerationProofError> {
    let coverage = input_witness.coverage();
    if !coverage.is_authorized_complete() {
        return Err(SemanticGenerationProofError::InputWitnessNotComplete {
            state: coverage.state(),
        });
    }
    Ok(())
}

fn validate_segment_witness(
    family: SemanticIrPlane,
    segment_witness: SemanticInputWitness,
    generation_witness: SemanticInputWitness,
) -> Result<(), SemanticGenerationProofError> {
    let coverage = segment_witness.coverage();
    if !coverage.is_authorized_complete() {
        return Err(SemanticGenerationProofError::SegmentWitnessNotComplete {
            family,
            state: coverage.state(),
        });
    }
    if !segment_witness.same_admitted_frontier(&generation_witness) {
        return Err(SemanticGenerationProofError::SegmentWitnessMismatch { family });
    }
    Ok(())
}

fn required_family(index: usize, profile: crate::vocabulary::LanguageProfile) -> SemanticIrPlane {
    match index {
        0 => SemanticIrPlane::Core,
        1 => SemanticIrPlane::Types,
        2 => SemanticIrPlane::Relations,
        3 => SemanticIrPlane::Occurrences,
        4 => SemanticIrPlane::Documentation,
        5 => SemanticIrPlane::SourceProvenance,
        _ => SemanticIrPlane::LanguageExtensions(profile),
    }
}

fn compute_family_root(
    family: SemanticIrPlane,
    segments: &[VerifiedTypedPlaneSegmentV2],
) -> Result<SemanticGenerationFamilyRootV2, SemanticGenerationProofError> {
    let segment_count = u32::try_from(segments.len())
        .map_err(|_| SemanticGenerationProofError::SegmentCountOverflow)?;
    let mut hasher = blake3::Hasher::new_derive_key(FAMILY_DOMAIN);
    hasher.update(&[ROOT_FORMAT_VERSION]);
    hash_family_kind(family, &mut hasher);
    hasher.update(&segment_count.to_be_bytes());
    for segment in segments {
        hasher.update(segment.first_key());
        hasher.update(segment.last_key());
        hasher.update(&segment.row_count().to_be_bytes());
        hasher.update(&segment.byte_length().to_be_bytes());
        hasher.update(segment.admitted_id().as_bytes());
    }
    Ok(SemanticGenerationFamilyRootV2(
        *hasher.finalize().as_bytes(),
    ))
}

fn compute_content_root(
    image_facts: SemanticImageFacts,
    families: &[SemanticGenerationFamilyCommitmentV2; IR_FAMILY_COUNT],
) -> SemanticContentRootV2 {
    let mut hasher = blake3::Hasher::new_derive_key(CONTENT_ROOT_DOMAIN);
    hasher.update(&[ROOT_FORMAT_VERSION]);
    hash_image_authority(image_facts.authority, &mut hasher);

    hasher.update(&(IR_FAMILY_COUNT as u16).to_be_bytes());
    for family in families {
        hash_family_kind(family.family, &mut hasher);
        hasher.update(family.family_root.as_bytes());
        hasher.update(&family.row_count.to_be_bytes());
    }

    SemanticContentRootV2(*hasher.finalize().as_bytes())
}

fn compute_generation_root(
    build: SemanticBuildIdentity,
    input_witness: SemanticInputWitness,
    content_root: SemanticContentRootV2,
) -> SemanticGenerationRootV2 {
    let mut hasher = blake3::Hasher::new_derive_key(GENERATION_ROOT_DOMAIN);
    hasher.update(&[ROOT_FORMAT_VERSION]);
    hasher.update(content_root.as_bytes());
    hasher.update(build.package());
    hasher.update(build.target());
    hasher.update(&<[u8; 2]>::from(build.profile()));
    hasher.update(&[u8::from(build.stage())]);
    hasher.update(build.recipe());
    hasher.update(build.toolchain());
    hasher.update(build.environment());
    hasher.update(build.target_platform());
    hasher.update(&input_witness.generation_root_commitment_v2());
    SemanticGenerationRootV2(*hasher.finalize().as_bytes())
}

fn hash_family_kind(family: SemanticIrPlane, hasher: &mut blake3::Hasher) {
    let (tag, profile) = match family {
        SemanticIrPlane::Core => (0, None),
        SemanticIrPlane::Types => (1, None),
        SemanticIrPlane::Relations => (2, None),
        SemanticIrPlane::Occurrences => (3, None),
        SemanticIrPlane::Documentation => (4, None),
        SemanticIrPlane::SourceProvenance => (5, None),
        SemanticIrPlane::LanguageExtensions(profile) => (6, Some(profile)),
    };
    hasher.update(&[tag]);
    if let Some(profile) = profile {
        hasher.update(&<[u8; 2]>::from(profile));
    }
}

fn hash_image_authority(authority: SemanticImageAuthority, hasher: &mut blake3::Hasher) {
    match authority {
        SemanticImageAuthority::Shared => hasher.update(&[0]),
        SemanticImageAuthority::Language(profile) => {
            hasher.update(&[1]);
            hasher.update(&<[u8; 2]>::from(profile));
        }
    };
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use backend_version::{
        AdmittedProducerObservation, AuthorityScopeClaim, ContentId, CoverageAdmissionError,
        CoverageWitness, ObjectVersion, ProducerObservationClaims, ProducerObservationVerifier,
        ScopeRoot, SemanticScopeDomain, SourceFactDomain, ToolchainDomain,
        UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    };

    use super::*;
    use crate::ir::{
        AtomId, LanguageProfile, RustEdition, SemanticScopeClaim, SemanticScopeFacts,
        SourceIdentity,
    };
    use crate::vocabulary::{CompileRecipeFact, NativeTool, Stage};

    struct TestAuthority;

    impl backend_version::Schema for TestAuthority {
        const DOMAIN: u8 = 0x53;
        const TYPE: u16 = 0xfffd;
        type Value = [u8; 32];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
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

    fn input(identity: u8) -> SemanticInputWitness {
        input_with_admission(identity, [7; 32], [8; 32], vec![9, 10])
    }

    fn input_with_admission(
        identity: u8,
        producer_identity: [u8; 32],
        context: [u8; 32],
        evidence: Vec<u8>,
    ) -> SemanticInputWitness {
        let input_root = [identity; 32];
        let scope_value = [identity.wrapping_add(1); 32];
        let version = ObjectVersion::<TestAuthority>::from_value(&scope_value);
        let scope = ScopeRoot::from_bytes(version.to_bytes());
        let claim = AuthorityScopeClaim::from_object_version(version);
        let producer: AdmittedProducerObservation = admit_producer_observation(
            UntrustedProducerObservation::new(
                producer_identity,
                claim.scope_root(),
                context,
                evidence,
            ),
            &TestVerifier,
        )
        .expect("test producer observation is admitted");
        let witness = CoverageWitness::Complete(
            admit_complete_scope(claim, producer).expect("scope matches"),
        );
        SemanticInputWitness::admitted(input_root, scope, witness)
            .expect("exact input scope is admitted")
    }

    fn build() -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
        )
    }

    fn empty_families(
        profile: LanguageProfile,
    ) -> [SemanticGenerationFamilyCommitmentV2; IR_FAMILY_COUNT] {
        let kinds = [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(profile),
        ];
        kinds.map(|kind| {
            SemanticGenerationFamilyCommitmentV2::from_verified_family_segments(kind, &[], input(1))
                .expect("empty family root is canonical")
        })
    }

    fn facts() -> SemanticImageFacts {
        SemanticImageFacts {
            authority: SemanticImageAuthority::Shared,
            provenance: ImageProvenance::Unavailable,
        }
    }

    fn captured_facts(
        source_bytes: &[u8],
        scope_bytes: &[u8],
    ) -> (SemanticBuildIdentity, SemanticImageFacts) {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: u32::try_from(source_bytes.len()).expect("test source fits u32"),
        };
        let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(b"rust-toolchain");
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            toolchain,
        );
        let image_facts = SemanticImageFacts {
            authority: SemanticImageAuthority::Shared,
            provenance: ImageProvenance::Captured {
                source,
                recipe,
                claim: SemanticScopeClaim {
                    identity: ContentId::<SemanticScopeDomain>::from_canonical_bytes(scope_bytes),
                },
                scope: SemanticScopeFacts {
                    ecosystem: AtomId::new(0),
                    package: AtomId::new(1),
                    path: AtomId::new(2),
                    coordinate: None,
                },
            },
        };
        let build = SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            profile,
            Stage::LowerIr,
            *recipe.identity.as_ref(),
            *toolchain.as_ref(),
            [5; 32],
            [6; 32],
        );
        (build, image_facts)
    }

    #[test]
    fn all_seven_empty_families_have_explicit_content_roots() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let families = empty_families(profile);
        assert!(families.iter().all(|family| family.row_count() == 0));
        assert!(
            families
                .iter()
                .all(|family| family.family_root().as_bytes() != &[0; 32])
        );
    }

    #[test]
    fn claimed_complete_input_witness_is_not_owner_admitted() {
        let input = SemanticInputWitness::claimed_state(
            [1; 32],
            ScopeRoot::from_bytes([2; 32]),
            Coverage::Complete,
        );

        assert!(matches!(
            validate_input_witness(input),
            Err(SemanticGenerationProofError::InputWitnessNotComplete {
                state: Coverage::Complete
            })
        ));
    }

    #[test]
    fn segment_witness_must_bind_the_same_live_generation_frontier() {
        let generation = input(1);
        let same_frontier_different_evidence =
            input_with_admission(1, [70; 32], [80; 32], vec![1, 2, 3]);
        assert!(
            validate_segment_witness(
                SemanticIrPlane::Core,
                same_frontier_different_evidence,
                generation,
            )
            .is_ok()
        );

        assert!(matches!(
            validate_segment_witness(SemanticIrPlane::Core, input(2), generation),
            Err(SemanticGenerationProofError::SegmentWitnessMismatch {
                family: SemanticIrPlane::Core
            })
        ));
    }

    #[test]
    fn content_and_generation_roots_separate_content_from_derivation_claims() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let families = empty_families(profile);
        let content_root = compute_content_root(facts(), &families);
        let same_content_other_input = compute_content_root(facts(), &families);
        assert_eq!(content_root, same_content_other_input);

        let mut changed_families = families;
        changed_families[0] = SemanticGenerationFamilyCommitmentV2 {
            family: SemanticIrPlane::Core,
            family_root: SemanticGenerationFamilyRootV2([42; 32]),
            row_count: 1,
        };
        assert_ne!(
            content_root,
            compute_content_root(facts(), &changed_families)
        );

        let generation_root = compute_generation_root(build(), input(1), content_root);
        let another_input = compute_generation_root(build(), input(2), content_root);
        assert_ne!(generation_root, another_input);

        let different_admission_evidence = compute_generation_root(
            build(),
            input_with_admission(1, [70; 32], [80; 32], vec![1, 2, 3, 4]),
            content_root,
        );
        assert_eq!(generation_root, different_admission_evidence);

        let original_build = build();
        let other_build = SemanticBuildIdentity::new(
            *original_build.package(),
            *original_build.target(),
            original_build.profile(),
            original_build.stage(),
            *original_build.recipe(),
            *original_build.toolchain(),
            [7; 32],
            *original_build.target_platform(),
        );
        let different_build = compute_generation_root(other_build, input(1), content_root);
        assert_ne!(generation_root, different_build);

        let language_facts = SemanticImageFacts {
            authority: SemanticImageAuthority::Language(profile),
            provenance: ImageProvenance::Unavailable,
        };
        let different_facts = compute_content_root(language_facts, &families);
        assert_ne!(content_root, different_facts);
        assert_ne!(
            generation_root,
            compute_generation_root(build(), input(1), different_facts)
        );
    }

    #[test]
    fn image_source_recipe_and_scope_provenance_do_not_change_content_root() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let families = empty_families(profile);
        let (first_build, first_facts) = captured_facts(b"source one", b"scope one");
        let (second_build, second_facts) = captured_facts(b"source two", b"scope two");
        validate_image_facts(first_build, first_facts)
            .expect("first provenance agrees with its build");
        validate_image_facts(second_build, second_facts)
            .expect("second provenance agrees with its build");

        let first_content = compute_content_root(first_facts, &families);
        let second_content = compute_content_root(second_facts, &families);
        assert_eq!(first_content, second_content);

        let first_generation = compute_generation_root(first_build, input(1), first_content);
        let second_generation = compute_generation_root(second_build, input(2), second_content);
        assert_ne!(first_generation, second_generation);

        let ImageProvenance::Captured {
            mut source,
            recipe,
            claim,
            scope,
        } = first_facts.provenance
        else {
            unreachable!("test facts are captured")
        };
        source.identity = ContentId::<SourceFactDomain>::from_canonical_bytes(b"forged source");
        let mismatched_source_recipe = SemanticImageFacts {
            authority: first_facts.authority,
            provenance: ImageProvenance::Captured {
                source,
                recipe,
                claim,
                scope,
            },
        };
        assert!(matches!(
            validate_image_facts(first_build, mismatched_source_recipe),
            Err(SemanticGenerationProofError::ImageFactsBuildMismatch {
                field: "source-bound recipe identity"
            })
        ));
    }

    #[test]
    fn wire_root_claim_only_matches_the_computed_typed_root() {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let content_root = compute_content_root(facts(), &empty_families(profile));
        let generation_root = compute_generation_root(build(), input(1), content_root);
        let content_claim =
            UntrustedSemanticContentRootV2::from_wire_claim(*content_root.as_bytes());
        let generation_claim =
            UntrustedSemanticGenerationRootV2::from_wire_claim(*generation_root.as_bytes());

        assert!(content_claim.matches(content_root));
        assert!(generation_claim.matches(generation_root));
        assert!(!UntrustedSemanticContentRootV2::from_wire_claim([9; 32]).matches(content_root));
        assert!(
            !UntrustedSemanticGenerationRootV2::from_wire_claim([9; 32]).matches(generation_root)
        );
    }
}
