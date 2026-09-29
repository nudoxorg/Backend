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

use alloc::vec::Vec;

use crate::ir::versioned_records::aggregate::{
    SemanticTypedPlaneVerificationLimitsV2, TypedPlaneFamilyPayloadsV2, TypedPlaneSegmentPayloadV2,
    VerifiedTypedPlaneFamilyV2, VerifiedTypedPlaneInventoryV2, VerifiedTypedPlaneSegmentV2,
    verify_semantic_typed_plane_inventory_v2,
};
use crate::ir::{
    ImageProvenance, SemanticBuildIdentity, SemanticImageAuthority, SemanticImageFacts,
    SemanticInputClaimV2, SemanticIrPlane, SemanticTypedPlaneManifestV2,
};
use thiserror::Error;

const CONTENT_ROOT_DOMAIN: &str = "backend.semantic.ir.content.v2";
const GENERATION_ROOT_DOMAIN: &str = "backend.semantic.ir.generation.v2";
const ROOT_FORMAT_VERSION: u8 = 2;
const IR_FAMILY_COUNT: usize = 7;
/// Hard cap for temporary descriptor/payload adapter storage before the
/// aggregate verifier opens its bounded row-index workspace.
pub const MAX_TYPED_PLANE_VERIFICATION_ADAPTER_BYTES: usize = 16 * 1024 * 1024;

/// Fixed semantic typed-plane verifier workload tiers.
///
/// The standard tier is suitable for routine admission. `LargePackage` raises
/// the bounded row, reference, segment, and payload ceilings without allowing
/// callers to construct arbitrary verifier limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticTypedPlaneVerificationTierV2 {
    /// 8,192 segments, 64 MiB c004 bytes, 250k rows, and 1m references.
    Standard,
    /// 65,536 segments, 512 MiB c004 bytes, 2m rows, and 8m references.
    LargePackage,
}

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
    pub const fn from_verified_content(content: &VerifiedTypedPlaneContentV2) -> Self {
        content.content_root
    }
}

/// An untrusted V2 content-root claim decoded from a wire manifest.
///
/// The claim cannot be used as a semantic content identity until it has been
/// compared with [`VerifiedTypedPlaneContentV2::content_root`].
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
/// input/read-frontier claims, rather than from NXFI bytes. A fresh owner
/// admission is required before selecting a generation; volatile producer,
/// session, and evidence identities are kept outside this root.
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
    pub const fn from_verified_content(content: &VerifiedTypedPlaneContentV2) -> Self {
        content.generation_root
    }
}

/// An untrusted V2 generation-root claim decoded from a wire manifest.
///
/// The claim cannot be used as a semantic-generation identity until it has
/// been compared with [`VerifiedTypedPlaneContentV2::generation_root`].
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

    /// Canonical row-content root for this family, independent of segment cuts.
    #[must_use]
    pub const fn family_root(&self) -> SemanticGenerationFamilyRootV2 {
        self.family_root
    }

    /// Number of canonical typed rows in this family.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Copies the partition-independent canonical row commitment from an
    /// aggregate verifier token. Segment IDs and ranges are validated as
    /// transport metadata but never enter semantic content identity.
    fn from_verified_family(verified: &VerifiedTypedPlaneFamilyV2) -> Self {
        Self {
            family: verified.family(),
            family_root: SemanticGenerationFamilyRootV2(verified.semantic_row_root()),
            row_count: verified.row_count(),
        }
    }
}

/// Opaque proof that all mandatory typed semantic families were decoded,
/// cross-checked, and matched their exact c004 payload bytes on cold reopen.
/// It contains claims only, and never grants the engine owner authority to
/// select a generation. A fresh selection must separately bind its
/// deterministic input/read claim to owner-admitted read-closure evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedTypedPlaneContentV2 {
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_claim: SemanticInputClaimV2,
    family_commitments: [SemanticGenerationFamilyCommitmentV2; IR_FAMILY_COUNT],
    content_root: SemanticContentRootV2,
    generation_root: SemanticGenerationRootV2,
}

impl VerifiedTypedPlaneContentV2 {
    /// Mints semantic roots from a token issued by the semantic all-family
    /// verifier. This method accepts no wire claims or raw segment material.
    pub(crate) fn from_verified_inventory(
        inventory: VerifiedTypedPlaneInventoryV2,
    ) -> Result<Self, SemanticGenerationProofError> {
        let build = inventory.build();
        let image_facts = inventory.image_facts();
        let input_claim = SemanticInputClaimV2::from_witness(&inventory.input_witness());
        validate_image_facts(build, image_facts)?;
        validate_input_claim(input_claim)?;

        let summaries = inventory.families();
        let commitments = summaries
            .each_ref()
            .map(|summary| SemanticGenerationFamilyCommitmentV2::from_verified_family(summary));

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

        let content_root = compute_content_root(image_facts, &commitments);
        let generation_root = compute_generation_root(build, input_claim, content_root);
        Ok(Self {
            build,
            image_facts,
            input_claim,
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

    /// Non-authorizing input/read claim included in the generation root.
    #[must_use]
    pub const fn input_claim(&self) -> SemanticInputClaimV2 {
        self.input_claim
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

/// Independently verifies one cold V2 manifest and the exact ordered c004
/// payload closure it names. The result proves content and deterministic
/// generation claims only. An engine owner must separately bind the input/read
/// claim to a fresh read-closure admission before selecting the generation.
pub fn verify_typed_plane_content_v2(
    manifest: &SemanticTypedPlaneManifestV2,
    exact_ordered_payloads: &[&[u8]],
) -> Result<VerifiedTypedPlaneContentV2, SemanticGenerationProofError> {
    verify_typed_plane_content_v2_with_tier(
        manifest,
        exact_ordered_payloads,
        SemanticTypedPlaneVerificationTierV2::Standard,
    )
}

/// Independently verifies one cold V2 manifest using an explicit bounded
/// workload tier. Tier selection changes resource ceilings, never the strict
/// family grammar, census, row roots, or cross-family checks.
pub fn verify_typed_plane_content_v2_with_tier(
    manifest: &SemanticTypedPlaneManifestV2,
    exact_ordered_payloads: &[&[u8]],
    tier: SemanticTypedPlaneVerificationTierV2,
) -> Result<VerifiedTypedPlaneContentV2, SemanticGenerationProofError> {
    let expected_payload_count = manifest
        .resource_usage()
        .map_err(|_| SemanticGenerationProofError::ManifestResourcePolicy)?
        .segment_descriptors();
    if expected_payload_count != exact_ordered_payloads.len() {
        return Err(SemanticGenerationProofError::PayloadCountMismatch {
            expected: expected_payload_count,
            observed: exact_ordered_payloads.len(),
        });
    }
    enforce_verification_adapter_bound(expected_payload_count)?;

    let mut ordered_payload_index = 0_usize;
    let mut segment_payload_sets = Vec::new();
    segment_payload_sets
        .try_reserve_exact(IR_FAMILY_COUNT)
        .map_err(|_| SemanticGenerationProofError::Allocation)?;
    for family in manifest.families() {
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(family.segments().len())
            .map_err(|_| SemanticGenerationProofError::Allocation)?;
        for segment in family.segments() {
            let payload = exact_ordered_payloads
                .get(ordered_payload_index)
                .copied()
                .ok_or(SemanticGenerationProofError::PayloadCountMismatch {
                    expected: expected_payload_count,
                    observed: ordered_payload_index,
                })?;
            ordered_payload_index = ordered_payload_index
                .checked_add(1)
                .ok_or(SemanticGenerationProofError::ManifestResourcePolicy)?;
            segments.push(TypedPlaneSegmentPayloadV2::new(
                *segment.first_key(),
                *segment.last_key(),
                segment.row_count(),
                segment.byte_length(),
                segment.id_claim(),
                payload,
            ));
        }
        segment_payload_sets.push(segments);
    }
    let mut family_payloads = Vec::new();
    family_payloads
        .try_reserve_exact(IR_FAMILY_COUNT)
        .map_err(|_| SemanticGenerationProofError::Allocation)?;
    for (family, segments) in manifest.families().iter().zip(&segment_payload_sets) {
        family_payloads.push(TypedPlaneFamilyPayloadsV2::new(
            family.family(),
            family.row_count(),
            segments,
        ));
    }
    if ordered_payload_index != exact_ordered_payloads.len() {
        return Err(SemanticGenerationProofError::PayloadCountMismatch {
            expected: ordered_payload_index,
            observed: exact_ordered_payloads.len(),
        });
    }
    let family_payloads: [TypedPlaneFamilyPayloadsV2<'_>; IR_FAMILY_COUNT] = family_payloads
        .try_into()
        .map_err(|_| SemanticGenerationProofError::ManifestResourcePolicy)?;
    let limits = match tier {
        SemanticTypedPlaneVerificationTierV2::Standard => {
            SemanticTypedPlaneVerificationLimitsV2::standard()
        }
        SemanticTypedPlaneVerificationTierV2::LargePackage => {
            SemanticTypedPlaneVerificationLimitsV2::large_package()
        }
    };
    let inventory = verify_semantic_typed_plane_inventory_v2(
        manifest.build(),
        manifest.image_facts(),
        manifest.input_claim().as_claimed_witness(),
        &family_payloads,
        exact_ordered_payloads,
        limits,
    )
    .map_err(|_| SemanticGenerationProofError::TypedPlaneInventoryRejected)?;
    validate_inventory_matches_manifest(&inventory, manifest)?;
    let content = VerifiedTypedPlaneContentV2::from_verified_inventory(inventory)?;
    if !manifest
        .content_root_claim()
        .matches(content.content_root())
    {
        return Err(SemanticGenerationProofError::ContentRootClaimMismatch);
    }
    if !manifest
        .generation_root_claim()
        .matches(content.generation_root())
    {
        return Err(SemanticGenerationProofError::GenerationRootClaimMismatch);
    }
    Ok(content)
}

fn enforce_verification_adapter_bound(
    segment_count: usize,
) -> Result<(), SemanticGenerationProofError> {
    let segment_bytes = segment_count
        .checked_mul(core::mem::size_of::<TypedPlaneSegmentPayloadV2<'static>>())
        .ok_or(SemanticGenerationProofError::ManifestResourcePolicy)?;
    let fixed_bytes = IR_FAMILY_COUNT
        .checked_mul(
            core::mem::size_of::<TypedPlaneFamilyPayloadsV2<'static>>()
                + core::mem::size_of::<Vec<TypedPlaneSegmentPayloadV2<'static>>>(),
        )
        .and_then(|bytes| bytes.checked_add(4096))
        .ok_or(SemanticGenerationProofError::ManifestResourcePolicy)?;
    let estimated = segment_bytes
        .checked_add(fixed_bytes)
        .ok_or(SemanticGenerationProofError::ManifestResourcePolicy)?;
    if estimated > MAX_TYPED_PLANE_VERIFICATION_ADAPTER_BYTES {
        return Err(
            SemanticGenerationProofError::VerificationAdapterLimitExceeded {
                estimated,
                maximum: MAX_TYPED_PLANE_VERIFICATION_ADAPTER_BYTES,
            },
        );
    }
    Ok(())
}

fn validate_inventory_matches_manifest(
    inventory: &VerifiedTypedPlaneInventoryV2,
    manifest: &SemanticTypedPlaneManifestV2,
) -> Result<(), SemanticGenerationProofError> {
    if inventory.build() != manifest.build()
        || inventory.image_facts() != manifest.image_facts()
        || SemanticInputClaimV2::from_witness(&inventory.input_witness()) != manifest.input_claim()
    {
        return Err(SemanticGenerationProofError::InventoryClaimMismatch);
    }

    for (index, (verified, claimed)) in inventory
        .families()
        .iter()
        .zip(manifest.families())
        .enumerate()
    {
        let expected = required_family(index, manifest.build().profile());
        if verified.family() != expected
            || claimed.family() != expected
            || verified.row_count() != claimed.row_count()
            || verified.segments().len() != claimed.segments().len()
        {
            return Err(SemanticGenerationProofError::InventoryFamilyMismatch { index });
        }
        for (verified_segment, claimed_segment) in
            verified.segments().iter().zip(claimed.segments())
        {
            if verified_segment.first_key() != claimed_segment.first_key()
                || verified_segment.last_key() != claimed_segment.last_key()
                || verified_segment.row_count() != claimed_segment.row_count()
                || verified_segment.byte_length() != claimed_segment.byte_length()
                || verified_segment.admitted_id().as_bytes()
                    != claimed_segment.id_claim().as_bytes()
            {
                return Err(SemanticGenerationProofError::InventorySegmentMismatch { index });
            }
        }
    }
    Ok(())
}

/// Failure to verify a cold typed semantic content and generation claim.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SemanticGenerationProofError {
    /// Strict payload decoding or complete family census failed.
    #[error("strict V2 typed-plane payload and family verification rejected the closure")]
    TypedPlaneInventoryRejected,
    /// The bounded manifest could not produce a canonical family/payload view.
    #[error("V2 typed-plane manifest resource counters or family view are invalid")]
    ManifestResourcePolicy,
    /// Manifest segment descriptors and the supplied exact c004 payloads differ in count.
    #[error("V2 typed-plane manifest expects {expected} payloads, observed {observed}")]
    PayloadCountMismatch { expected: usize, observed: usize },
    /// Allocating bounded family/payload adapter storage failed.
    #[error("V2 typed-plane verifier adapter allocation failed")]
    Allocation,
    /// Temporary borrowed payload adapter storage exceeds its fixed cap.
    #[error(
        "V2 typed-plane verifier adapters need an estimated {estimated} bytes; maximum is {maximum}"
    )]
    VerificationAdapterLimitExceeded { estimated: usize, maximum: usize },
    /// Aggregate verified values did not preserve the manifest claims.
    #[error("verified V2 typed-plane inventory differs from its manifest claims")]
    InventoryClaimMismatch,
    /// A family descriptor differs from the aggregate-verified family.
    #[error("verified V2 typed-plane family differs from its manifest at slot {index}")]
    InventoryFamilyMismatch { index: usize },
    /// A segment descriptor differs from its aggregate-verified payload.
    #[error("verified V2 typed-plane segment differs from its manifest at family slot {index}")]
    InventorySegmentMismatch { index: usize },
    /// The untrusted content-root claim does not match the exact typed payloads.
    #[error("V2 typed-plane content-root claim does not match the verified payloads")]
    ContentRootClaimMismatch,
    /// The untrusted generation-root claim does not match the canonical claims.
    #[error("V2 typed-plane generation-root claim does not match its canonical claims")]
    GenerationRootClaimMismatch,
    /// A V2 generation claim does not assert complete input/read coverage.
    #[error("V2 typed-plane input/read claim is not Complete")]
    InputClaimNotComplete,
    /// A family was missing, duplicated, or out of the required canonical order.
    #[error(
        "semantic generation family inventory differs at slot {index}: expected {expected:?}, observed {observed:?}"
    )]
    FamilyInventory {
        index: usize,
        expected: SemanticIrPlane,
        observed: SemanticIrPlane,
    },
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

fn validate_input_claim(
    input_claim: SemanticInputClaimV2,
) -> Result<(), SemanticGenerationProofError> {
    if input_claim.coverage_state() != backend_version::Coverage::Complete {
        return Err(SemanticGenerationProofError::InputClaimNotComplete);
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
    input_claim: SemanticInputClaimV2,
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
    hasher.update(
        &input_claim
            .as_claimed_witness()
            .generation_root_commitment_v2(),
    );
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
        AdmittedProducerObservation, AuthorityScopeClaim, ContentId, Coverage,
        CoverageAdmissionError, CoverageWitness, ObjectVersion, ProducerObservationClaims,
        ProducerObservationVerifier, ScopeRoot, SemanticScopeDomain, SourceFactDomain,
        ToolchainDomain, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation,
    };

    use super::*;
    use crate::ir::{
        AtomId, LanguageProfile, RustEdition, SemanticInputWitness, SemanticScopeClaim,
        SemanticScopeFacts, SemanticTypedPlaneFamilyDescriptorV2, SourceIdentity,
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

    fn admitted_input(identity: u8) -> SemanticInputWitness {
        input_with_admission(identity, [7; 32], [8; 32], vec![9, 10])
    }

    fn input_claim(identity: u8) -> SemanticInputClaimV2 {
        SemanticInputClaimV2::from_witness(&admitted_input(identity))
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
        core::array::from_fn(|index| SemanticGenerationFamilyCommitmentV2 {
            family: kinds[index],
            family_root: SemanticGenerationFamilyRootV2(
                [u8::try_from(index + 1).expect("family index fits u8"); 32],
            ),
            row_count: 0,
        })
    }

    fn facts() -> SemanticImageFacts {
        SemanticImageFacts {
            authority: SemanticImageAuthority::Shared,
            provenance: ImageProvenance::Unavailable,
        }
    }

    fn empty_manifest(
        content_root: [u8; 32],
        generation_root: [u8; 32],
    ) -> SemanticTypedPlaneManifestV2 {
        let families = [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(build().profile()),
        ]
        .map(|family| {
            SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(family, 0, Vec::new())
                .expect("empty family descriptor is canonical")
        });
        SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build(),
            facts(),
            SemanticInputClaimV2::from_untrusted_claims(
                [7; 32],
                ScopeRoot::from_bytes([8; 32]),
                backend_version::Coverage::Complete,
            ),
            UntrustedSemanticContentRootV2::from_wire_claim(content_root),
            UntrustedSemanticGenerationRootV2::from_wire_claim(generation_root),
            families,
        )
        .expect("empty seven-family manifest is canonical")
    }

    fn verify_empty_inventory(
        manifest: &SemanticTypedPlaneManifestV2,
    ) -> VerifiedTypedPlaneInventoryV2 {
        let empty_segments: [Vec<TypedPlaneSegmentPayloadV2<'static>>; IR_FAMILY_COUNT] =
            core::array::from_fn(|_| Vec::new());
        let families = core::array::from_fn(|index| {
            let claimed = &manifest.families()[index];
            TypedPlaneFamilyPayloadsV2::new(
                claimed.family(),
                claimed.row_count(),
                &empty_segments[index],
            )
        });
        verify_semantic_typed_plane_inventory_v2(
            manifest.build(),
            manifest.image_facts(),
            manifest.input_claim().as_claimed_witness(),
            &families,
            &[],
            SemanticTypedPlaneVerificationLimitsV2::standard(),
        )
        .expect("cold aggregate verifier admits exact empty closure")
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
    fn cold_verifier_reopens_empty_closure_and_recomputes_both_roots() {
        let claims = empty_manifest([0; 32], [0; 32]);
        let inventory = verify_empty_inventory(&claims);
        let computed = VerifiedTypedPlaneContentV2::from_verified_inventory(inventory)
            .expect("root proof uses content-only inventory");

        let claims = empty_manifest(
            *computed.content_root().as_bytes(),
            *computed.generation_root().as_bytes(),
        );
        let wire = claims.canonical_bytes().expect("encode c007 manifest");
        let manifest = SemanticTypedPlaneManifestV2::decode(&wire)
            .expect("cold reopen decodes the exact c007 bytes");
        let reopened = verify_typed_plane_content_v2(&manifest, &[])
            .expect("cold reopen independently recomputes both roots");

        assert_eq!(reopened.content_root(), computed.content_root());
        assert_eq!(reopened.generation_root(), computed.generation_root());
        assert!(
            !reopened
                .input_claim()
                .as_claimed_witness()
                .coverage()
                .is_authorized_complete()
        );
    }

    #[test]
    fn cold_verifier_rejects_unreferenced_payloads() {
        let claims = empty_manifest([0; 32], [0; 32]);
        assert!(matches!(
            verify_typed_plane_content_v2(&claims, &[b"extra payload"]),
            Err(SemanticGenerationProofError::PayloadCountMismatch {
                expected: 0,
                observed: 1
            })
        ));
    }

    #[test]
    fn cold_verifier_rejects_a_nonmatching_content_root_claim() {
        let claims = empty_manifest([0; 32], [0; 32]);
        let inventory = verify_empty_inventory(&claims);
        let computed = VerifiedTypedPlaneContentV2::from_verified_inventory(inventory)
            .expect("root proof uses content-only inventory");
        let mut false_content = *computed.content_root().as_bytes();
        false_content[0] ^= 1;
        let manifest = empty_manifest(false_content, *computed.generation_root().as_bytes());

        assert!(matches!(
            verify_typed_plane_content_v2(&manifest, &[]),
            Err(SemanticGenerationProofError::ContentRootClaimMismatch)
        ));
    }

    #[test]
    fn cold_complete_input_claim_does_not_create_owner_authority() {
        let input = SemanticInputClaimV2::from_untrusted_claims(
            [1; 32],
            ScopeRoot::from_bytes([2; 32]),
            Coverage::Complete,
        );

        assert!(validate_input_claim(input).is_ok());
        assert!(
            !input
                .as_claimed_witness()
                .coverage()
                .is_authorized_complete()
        );
        assert!(matches!(
            validate_input_claim(SemanticInputClaimV2::from_untrusted_claims(
                [1; 32],
                ScopeRoot::from_bytes([2; 32]),
                Coverage::Partial,
            )),
            Err(SemanticGenerationProofError::InputClaimNotComplete)
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

        let generation_root = compute_generation_root(build(), input_claim(1), content_root);
        let another_input = compute_generation_root(build(), input_claim(2), content_root);
        assert_ne!(generation_root, another_input);

        let different_admission_evidence = compute_generation_root(
            build(),
            SemanticInputClaimV2::from_witness(&input_with_admission(
                1,
                [70; 32],
                [80; 32],
                vec![1, 2, 3, 4],
            )),
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
        let different_build = compute_generation_root(other_build, input_claim(1), content_root);
        assert_ne!(generation_root, different_build);

        let language_facts = SemanticImageFacts {
            authority: SemanticImageAuthority::Language(profile),
            provenance: ImageProvenance::Unavailable,
        };
        let different_facts = compute_content_root(language_facts, &families);
        assert_ne!(content_root, different_facts);
        assert_ne!(
            generation_root,
            compute_generation_root(build(), input_claim(1), different_facts)
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

        let first_generation = compute_generation_root(first_build, input_claim(1), first_content);
        let second_generation =
            compute_generation_root(second_build, input_claim(2), second_content);
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
        let generation_root = compute_generation_root(build(), input_claim(1), content_root);
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
