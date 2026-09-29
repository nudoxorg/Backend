//! Canonical claim manifest for the V2 typed semantic-plane closure.
//!
//! This is deliberately separate from [`SemanticPlaneManifest`](crate::ir::SemanticPlaneManifest),
//! whose identity includes the V1 NXFI `GenerationId` and c003 image topology.
//! V2 manifests name seven typed row families and their c004-compatible
//! segment payloads. Decoding preserves only untrusted input and root claims;
//! it never recreates a live coverage capability.

use alloc::{boxed::Box, vec::Vec};

use backend_version::{
    CompileRecipeDomain, ContentId, Coverage, Schema, SchemaIdentity, ScopeRoot,
    SemanticScopeDomain, SourceFactDomain, ToolchainDomain,
};
use thiserror::Error;

use crate::ir::{
    AtomId, ImageProvenance, MAX_SEMANTIC_SEGMENT_BYTES, SemanticBuildIdentity,
    SemanticImageAuthority, SemanticImageFacts, SemanticInputWitness, SemanticIrPlane,
    SemanticScopeClaim, SemanticScopeFacts, SourceIdentity, UntrustedSemanticContentRootV2,
    UntrustedSemanticGenerationRootV2, UntrustedSemanticSegmentId,
};
use crate::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};

const MAGIC: [u8; 4] = *b"STPV";
const WIRE_VERSION: u16 = 2;
const HEADER_BYTES: usize = MAGIC.len() + core::mem::size_of::<u16>();
const FAMILY_COUNT: usize = 7;
const BUILD_BYTES: usize = 32 * 6 + 2 + 1;
const INPUT_CLAIM_BYTES: usize = 32 + 32 + 1;
const SEGMENT_BYTES: usize = 32 + 32 + 4 + 8 + 32;
/// Bounds the canonical manifest itself. Payloads live in separate c004
/// objects, so this ceiling covers descriptors and build/input claims only.
pub const MAX_TYPED_PLANE_MANIFEST_V2_BYTES: usize = 8 * 1024 * 1024;
/// Absolute descriptor ceiling, aligned with the aggregate verifier's large
/// tier. Standard admission applies its lower work limits independently.
pub const MAX_TYPED_PLANE_SEGMENTS_V2: usize = 65_536;
/// Conservative peak allocation budget for owned c007 decoding, excluding
/// the caller-owned wire buffer and c004 payloads.
pub const MAX_TYPED_PLANE_MANIFEST_V2_RESIDENT_BYTES: usize = 24 * 1024 * 1024;

/// Decode/work/resident counters derived from one validated manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneManifestV2ResourceUsage {
    wire_bytes: usize,
    segment_descriptors: usize,
    descriptor_validation_steps: usize,
    estimated_peak_resident_bytes: usize,
}

impl SemanticTypedPlaneManifestV2ResourceUsage {
    /// Canonical c007 object size in bytes.
    #[must_use]
    pub const fn wire_bytes(self) -> usize {
        self.wire_bytes
    }

    /// Number of c004 segment descriptors decoded or encoded.
    #[must_use]
    pub const fn segment_descriptors(self) -> usize {
        self.segment_descriptors
    }

    /// Upper bound on descriptor validation steps for this manifest.
    #[must_use]
    pub const fn descriptor_validation_steps(self) -> usize {
        self.descriptor_validation_steps
    }

    /// Conservative owned-decoder peak allocation estimate. This excludes
    /// the caller-owned input bytes and separate c004 payloads.
    #[must_use]
    pub const fn estimated_peak_resident_bytes(self) -> usize {
        self.estimated_peak_resident_bytes
    }
}

/// Deterministic source/read claim persisted by a V2 manifest.
///
/// This deliberately has no coverage capability, producer identity, context,
/// or evidence fields. Converting a live witness to this type discards those
/// admission details; the engine owner must retain its separate admitted
/// wrapper to authorize new selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticInputClaimV2 {
    input_root: [u8; 32],
    read_manifest_root: ScopeRoot,
    coverage_state: Coverage,
}

impl SemanticInputClaimV2 {
    /// Retains the deterministic fields of a live or claim-only witness.
    #[must_use]
    pub fn from_witness(witness: &SemanticInputWitness) -> Self {
        Self {
            input_root: *witness.input_root(),
            read_manifest_root: witness.read_manifest_root(),
            coverage_state: witness.coverage().state(),
        }
    }

    /// Retains untrusted fields decoded from the V2 manifest wire.
    #[must_use]
    pub const fn from_untrusted_claims(
        input_root: [u8; 32],
        read_manifest_root: ScopeRoot,
        coverage_state: Coverage,
    ) -> Self {
        Self {
            input_root,
            read_manifest_root,
            coverage_state,
        }
    }

    /// Recreates an explicitly claim-only semantic witness for deterministic
    /// root hashing. This cannot recreate an owner admission capability.
    #[must_use]
    pub fn as_claimed_witness(self) -> SemanticInputWitness {
        SemanticInputWitness::claimed_state(
            self.input_root,
            self.read_manifest_root,
            self.coverage_state,
        )
    }

    /// Input-root claim included in the generation root.
    #[must_use]
    pub const fn input_root(&self) -> &[u8; 32] {
        &self.input_root
    }

    /// Read-frontier root claim included in the generation root.
    #[must_use]
    pub const fn read_manifest_root(&self) -> ScopeRoot {
        self.read_manifest_root
    }

    /// Claimed coverage state; this is not proof of authorization.
    #[must_use]
    pub const fn coverage_state(&self) -> Coverage {
        self.coverage_state
    }
}

/// FileStore schema for one canonical V2 typed-family manifest.
pub struct SemanticTypedPlaneManifestV2Schema;

impl Schema for SemanticTypedPlaneManifestV2Schema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc007;
    const VERSION: u8 = 2;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Exact schema identity for a canonical V2 typed-family manifest object.
pub const SEMANTIC_TYPED_PLANE_MANIFEST_V2_SCHEMA: SchemaIdentity = SchemaIdentity::new(
    SemanticTypedPlaneManifestV2Schema::DOMAIN,
    SemanticTypedPlaneManifestV2Schema::TYPE,
    SemanticTypedPlaneManifestV2Schema::VERSION,
);

/// One untrusted stable-key segment claim in a V2 family manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneSegmentClaimV2 {
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    byte_length: u64,
    id_claim: UntrustedSemanticSegmentId,
}

impl SemanticTypedPlaneSegmentClaimV2 {
    /// Retains a worker or storage claim without admitting its payload bytes.
    pub fn from_untrusted_claims(
        first_key: [u8; 32],
        last_key: [u8; 32],
        row_count: u32,
        byte_length: u64,
        id_claim: UntrustedSemanticSegmentId,
    ) -> Result<Self, SemanticTypedPlaneManifestV2Error> {
        let segment = Self {
            first_key,
            last_key,
            row_count,
            byte_length,
            id_claim,
        };
        validate_segment_claim(segment, 0)?;
        Ok(segment)
    }

    /// First stable key claimed by this segment.
    #[must_use]
    pub const fn first_key(&self) -> &[u8; 32] {
        &self.first_key
    }

    /// Last stable key claimed by this segment.
    #[must_use]
    pub const fn last_key(&self) -> &[u8; 32] {
        &self.last_key
    }

    /// Number of typed rows claimed by this segment.
    #[must_use]
    pub const fn row_count(&self) -> u32 {
        self.row_count
    }

    /// Exact c004 payload byte length claimed by this segment.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Untrusted content identity claim; exact payload admission is separate.
    #[must_use]
    pub const fn id_claim(&self) -> UntrustedSemanticSegmentId {
        self.id_claim
    }
}

/// One mandatory V2 semantic row family, including an explicit empty family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneFamilyDescriptorV2 {
    family: SemanticIrPlane,
    row_count: u64,
    segments: Box<[SemanticTypedPlaneSegmentClaimV2]>,
}

impl SemanticTypedPlaneFamilyDescriptorV2 {
    /// Creates an untrusted family descriptor and validates its claimed counts
    /// and key ranges. Empty families are represented by an explicit value
    /// with zero rows and no segments.
    pub fn from_untrusted_claims(
        family: SemanticIrPlane,
        row_count: u64,
        segments: Vec<SemanticTypedPlaneSegmentClaimV2>,
    ) -> Result<Self, SemanticTypedPlaneManifestV2Error> {
        validate_family_claims(family, row_count, &segments)?;
        Ok(Self {
            family,
            row_count,
            segments: segments.into_boxed_slice(),
        })
    }

    /// Exact IR row family named by this descriptor.
    #[must_use]
    pub const fn family(&self) -> SemanticIrPlane {
        self.family
    }

    /// Claimed total family row count, checked against its segment descriptors.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Ordered segment claims for this family.
    #[must_use]
    pub fn segments(&self) -> &[SemanticTypedPlaneSegmentClaimV2] {
        &self.segments
    }
}

/// Canonical but untrusted V2 typed-family manifest.
///
/// The seven family entries are mandatory and ordered as Core, Types,
/// Relations, Occurrences, Documentation, SourceProvenance, and the exact
/// build-profile LanguageExtensions family. Every field remains a claim until
/// the aggregate semantic verifier admits all exact c004 payloads. Input/read
/// metadata uses [`SemanticInputClaimV2`], which cannot carry a live authority
/// capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticTypedPlaneManifestV2 {
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_claim: SemanticInputClaimV2,
    content_root_claim: UntrustedSemanticContentRootV2,
    generation_root_claim: UntrustedSemanticGenerationRootV2,
    families: [SemanticTypedPlaneFamilyDescriptorV2; FAMILY_COUNT],
}

impl SemanticTypedPlaneManifestV2 {
    /// Builds a manifest from claims. The input/read claim type contains no
    /// authority capability to copy or confer.
    pub fn from_untrusted_claims(
        build: SemanticBuildIdentity,
        image_facts: SemanticImageFacts,
        input_claim: SemanticInputClaimV2,
        content_root_claim: UntrustedSemanticContentRootV2,
        generation_root_claim: UntrustedSemanticGenerationRootV2,
        families: [SemanticTypedPlaneFamilyDescriptorV2; FAMILY_COUNT],
    ) -> Result<Self, SemanticTypedPlaneManifestV2Error> {
        let manifest = Self {
            build,
            image_facts,
            input_claim,
            content_root_claim,
            generation_root_claim,
            families,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Decodes one exact canonical V2 manifest. The result contains untrusted
    /// claims only; segment payloads and owner read admission are verified
    /// separately.
    pub fn decode(bytes: &[u8]) -> Result<Self, SemanticTypedPlaneManifestV2Error> {
        if bytes.len() > MAX_TYPED_PLANE_MANIFEST_V2_BYTES {
            return Err(SemanticTypedPlaneManifestV2Error::ManifestTooLarge {
                actual: bytes.len(),
                maximum: MAX_TYPED_PLANE_MANIFEST_V2_BYTES,
            });
        }
        let mut reader = Reader::new(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(SemanticTypedPlaneManifestV2Error::Magic);
        }
        let version = reader.u16()?;
        if version != WIRE_VERSION {
            return Err(SemanticTypedPlaneManifestV2Error::Version(version));
        }
        let build = decode_build(&mut reader)?;
        let image_facts = decode_image_facts(&mut reader)?;
        let input_root = reader.array32()?;
        let read_manifest = ScopeRoot::from_bytes(reader.array32()?);
        let coverage = decode_coverage(reader.u8()?)?;
        let input_claim =
            SemanticInputClaimV2::from_untrusted_claims(input_root, read_manifest, coverage);
        let content_root_claim = UntrustedSemanticContentRootV2::from_wire_claim(reader.array32()?);
        let generation_root_claim =
            UntrustedSemanticGenerationRootV2::from_wire_claim(reader.array32()?);
        let observed_families = reader.u8()?;
        if usize::from(observed_families) != FAMILY_COUNT {
            return Err(SemanticTypedPlaneManifestV2Error::FamilyCount {
                observed: observed_families,
            });
        }
        let mut families = Vec::new();
        families
            .try_reserve_exact(FAMILY_COUNT)
            .map_err(|_| SemanticTypedPlaneManifestV2Error::Allocation)?;
        let mut total_segments = 0_usize;
        for _ in 0..FAMILY_COUNT {
            let family = decode_family_kind(&mut reader)?;
            let row_count = reader.u64()?;
            let segment_count = usize::try_from(reader.u32()?)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::CountOverflow)?;
            total_segments = total_segments
                .checked_add(segment_count)
                .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
            if total_segments > MAX_TYPED_PLANE_SEGMENTS_V2 {
                return Err(SemanticTypedPlaneManifestV2Error::TooManySegments {
                    actual: total_segments,
                    maximum: MAX_TYPED_PLANE_SEGMENTS_V2,
                });
            }
            enforce_resident_peak_bound(total_segments)?;
            let segment_bytes = segment_count
                .checked_mul(SEGMENT_BYTES)
                .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
            if segment_bytes > reader.remaining() {
                return Err(SemanticTypedPlaneManifestV2Error::Truncated);
            }
            let mut segments = Vec::new();
            segments
                .try_reserve_exact(segment_count)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::Allocation)?;
            for _ in 0..segment_count {
                let first_key = reader.array32()?;
                let last_key = reader.array32()?;
                let row_count = reader.u32()?;
                let byte_length = reader.u64()?;
                let id_claim = UntrustedSemanticSegmentId::from_raw(reader.array32()?);
                segments.push(SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                    first_key,
                    last_key,
                    row_count,
                    byte_length,
                    id_claim,
                )?);
            }
            families.push(SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                family, row_count, segments,
            )?);
        }
        reader.finish()?;
        let families = families.try_into().map_err(
            |observed: Vec<SemanticTypedPlaneFamilyDescriptorV2>| {
                SemanticTypedPlaneManifestV2Error::FamilyCount {
                    observed: u8::try_from(observed.len()).unwrap_or(u8::MAX),
                }
            },
        )?;
        let manifest = Self::from_untrusted_claims(
            build,
            image_facts,
            input_claim,
            content_root_claim,
            generation_root_claim,
            families,
        )?;
        if manifest.canonical_bytes()?.as_slice() != bytes {
            return Err(SemanticTypedPlaneManifestV2Error::NonCanonical);
        }
        Ok(manifest)
    }

    /// Encodes canonical V2 manifest bytes, preserving only untrusted claims.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SemanticTypedPlaneManifestV2Error> {
        let encoded_length = self.encoded_length()?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(encoded_length)
            .map_err(|_| SemanticTypedPlaneManifestV2Error::Allocation)?;
        output.extend_from_slice(&MAGIC);
        output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
        encode_build(self.build, &mut output);
        encode_image_facts(self.image_facts, &mut output);
        output.extend_from_slice(self.input_claim.input_root());
        output.extend_from_slice(self.input_claim.read_manifest_root().as_bytes());
        output.push(coverage_code(self.input_claim.coverage_state()));
        output.extend_from_slice(self.content_root_claim.as_bytes());
        output.extend_from_slice(self.generation_root_claim.as_bytes());
        output.push(u8::try_from(FAMILY_COUNT).unwrap_or(u8::MAX));
        for family in &self.families {
            encode_family_kind(family.family, &mut output);
            output.extend_from_slice(&family.row_count.to_be_bytes());
            output.extend_from_slice(
                &u32::try_from(family.segments.len())
                    .map_err(|_| SemanticTypedPlaneManifestV2Error::CountOverflow)?
                    .to_be_bytes(),
            );
            for segment in &family.segments {
                output.extend_from_slice(segment.first_key());
                output.extend_from_slice(segment.last_key());
                output.extend_from_slice(&segment.row_count.to_be_bytes());
                output.extend_from_slice(&segment.byte_length.to_be_bytes());
                output.extend_from_slice(segment.id_claim.as_bytes());
            }
        }
        if output.len() != encoded_length {
            return Err(SemanticTypedPlaneManifestV2Error::LengthMismatch {
                expected: encoded_length,
                observed: output.len(),
            });
        }
        Ok(output)
    }

    /// Canonical build facts claimed by this manifest.
    #[must_use]
    pub const fn build(&self) -> SemanticBuildIdentity {
        self.build
    }

    /// Image authority and provenance claims retained for admission checks.
    #[must_use]
    pub const fn image_facts(&self) -> SemanticImageFacts {
        self.image_facts
    }

    /// Untrusted deterministic input/read claim, with no live capability.
    #[must_use]
    pub const fn input_claim(&self) -> SemanticInputClaimV2 {
        self.input_claim
    }

    /// Claimed typed semantic content root, not an admitted identity.
    #[must_use]
    pub const fn content_root_claim(&self) -> UntrustedSemanticContentRootV2 {
        self.content_root_claim
    }

    /// Claimed typed semantic generation root, not an admitted identity.
    #[must_use]
    pub const fn generation_root_claim(&self) -> UntrustedSemanticGenerationRootV2 {
        self.generation_root_claim
    }

    /// The fixed seven-family inventory in canonical order.
    #[must_use]
    pub const fn families(&self) -> &[SemanticTypedPlaneFamilyDescriptorV2; FAMILY_COUNT] {
        &self.families
    }

    /// Returns bounded decode/work/resident counters for this manifest.
    pub fn resource_usage(
        &self,
    ) -> Result<SemanticTypedPlaneManifestV2ResourceUsage, SemanticTypedPlaneManifestV2Error> {
        let segment_descriptors = self.families.iter().try_fold(0_usize, |total, family| {
            total.checked_add(family.segments.len())
        });
        let segment_descriptors =
            segment_descriptors.ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
        Ok(SemanticTypedPlaneManifestV2ResourceUsage {
            wire_bytes: self.encoded_length()?,
            segment_descriptors,
            descriptor_validation_steps: estimate_validation_steps(segment_descriptors)?,
            estimated_peak_resident_bytes: estimated_peak_resident_bytes(segment_descriptors)?,
        })
    }

    fn validate(&self) -> Result<(), SemanticTypedPlaneManifestV2Error> {
        let profile = self.build.profile();
        let expected_families = required_families(profile);
        let mut total_segments = 0_usize;
        let mut segment_ids = Vec::new();
        for (index, (family, expected)) in self.families.iter().zip(expected_families).enumerate() {
            if family.family != expected {
                return Err(SemanticTypedPlaneManifestV2Error::FamilyOrder {
                    index,
                    expected,
                    observed: family.family,
                });
            }
            validate_family_claims(family.family, family.row_count, &family.segments)?;
            total_segments = total_segments
                .checked_add(family.segments.len())
                .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
            if total_segments > MAX_TYPED_PLANE_SEGMENTS_V2 {
                return Err(SemanticTypedPlaneManifestV2Error::TooManySegments {
                    actual: total_segments,
                    maximum: MAX_TYPED_PLANE_SEGMENTS_V2,
                });
            }
            enforce_resident_peak_bound(total_segments)?;
            segment_ids
                .try_reserve(family.segments.len())
                .map_err(|_| SemanticTypedPlaneManifestV2Error::Allocation)?;
            segment_ids.extend(
                family
                    .segments
                    .iter()
                    .map(|segment| *segment.id_claim.as_bytes()),
            );
        }
        segment_ids.sort_unstable();
        if segment_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(SemanticTypedPlaneManifestV2Error::DuplicateSegmentId);
        }
        if self.input_claim.coverage_state() != Coverage::Complete {
            return Err(SemanticTypedPlaneManifestV2Error::InputClaimNotComplete {
                observed: self.input_claim.coverage_state(),
            });
        }
        self.encoded_length()?;
        Ok(())
    }

    fn encoded_length(&self) -> Result<usize, SemanticTypedPlaneManifestV2Error> {
        let mut total = HEADER_BYTES
            .checked_add(BUILD_BYTES)
            .and_then(|length| length.checked_add(image_facts_wire_len(self.image_facts)))
            .and_then(|length| length.checked_add(INPUT_CLAIM_BYTES))
            .and_then(|length| length.checked_add(32 + 32 + 1))
            .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
        let mut total_segments = 0_usize;
        for family in &self.families {
            let family_kind_length = family_kind_wire_len(family.family);
            let segment_bytes = family
                .segments
                .len()
                .checked_mul(SEGMENT_BYTES)
                .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
            total_segments = total_segments
                .checked_add(family.segments.len())
                .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
            total = total
                .checked_add(family_kind_length + 8 + 4)
                .and_then(|length| length.checked_add(segment_bytes))
                .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
        }
        if total_segments > MAX_TYPED_PLANE_SEGMENTS_V2 {
            return Err(SemanticTypedPlaneManifestV2Error::TooManySegments {
                actual: total_segments,
                maximum: MAX_TYPED_PLANE_SEGMENTS_V2,
            });
        }
        enforce_resident_peak_bound(total_segments)?;
        if total > MAX_TYPED_PLANE_MANIFEST_V2_BYTES {
            return Err(SemanticTypedPlaneManifestV2Error::ManifestTooLarge {
                actual: total,
                maximum: MAX_TYPED_PLANE_MANIFEST_V2_BYTES,
            });
        }
        Ok(total)
    }
}

fn estimated_peak_resident_bytes(
    segment_count: usize,
) -> Result<usize, SemanticTypedPlaneManifestV2Error> {
    // Account for the segment vector while it is converted into owned family
    // storage, the retained descriptors, the duplicate-ID validation vector,
    // and fixed family/container overhead. This is intentionally conservative.
    let per_segment = core::mem::size_of::<SemanticTypedPlaneSegmentClaimV2>()
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(core::mem::size_of::<[u8; 32]>()))
        .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
    let descriptors = segment_count
        .checked_mul(per_segment)
        .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
    let fixed = FAMILY_COUNT
        .checked_mul(
            core::mem::size_of::<SemanticTypedPlaneFamilyDescriptorV2>()
                + core::mem::size_of::<Vec<SemanticTypedPlaneSegmentClaimV2>>(),
        )
        .and_then(|bytes| bytes.checked_add(4096))
        .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
    descriptors
        .checked_add(fixed)
        .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)
}

fn enforce_resident_peak_bound(
    segment_count: usize,
) -> Result<(), SemanticTypedPlaneManifestV2Error> {
    let estimated = estimated_peak_resident_bytes(segment_count)?;
    if estimated > MAX_TYPED_PLANE_MANIFEST_V2_RESIDENT_BYTES {
        return Err(SemanticTypedPlaneManifestV2Error::ResidentLimitExceeded {
            estimated,
            maximum: MAX_TYPED_PLANE_MANIFEST_V2_RESIDENT_BYTES,
        });
    }
    Ok(())
}

fn estimate_validation_steps(
    segment_count: usize,
) -> Result<usize, SemanticTypedPlaneManifestV2Error> {
    if segment_count == 0 {
        return Ok(0);
    }
    let mut logarithm = 0_usize;
    let mut bound = 1_usize;
    while bound < segment_count {
        bound = bound
            .checked_mul(2)
            .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
        logarithm = logarithm
            .checked_add(1)
            .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
    }
    // Two linear descriptor passes plus a conservative n*ceil(log2(n)) sort
    // comparison estimate for cross-family duplicate-ID rejection.
    segment_count
        .checked_mul(2_usize.saturating_add(logarithm))
        .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)
}

/// Canonical V2 manifest decode, structure, and resource-limit failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SemanticTypedPlaneManifestV2Error {
    /// The payload does not begin with the V2 typed-plane magic.
    #[error("invalid V2 typed-plane manifest magic")]
    Magic,
    /// The payload uses an unsupported wire version.
    #[error("unsupported V2 typed-plane manifest version {0}")]
    Version(u16),
    /// The payload ended before a required field was complete.
    #[error("truncated V2 typed-plane manifest")]
    Truncated,
    /// The payload has bytes after its final canonical field.
    #[error("trailing bytes in V2 typed-plane manifest")]
    TrailingBytes,
    /// The wire encoding was valid but not canonical.
    #[error("V2 typed-plane manifest is not canonically encoded")]
    NonCanonical,
    /// The manifest or an allocation exceeds its safe resource bound.
    #[error("V2 typed-plane manifest has {actual} bytes; maximum is {maximum}")]
    ManifestTooLarge { actual: usize, maximum: usize },
    /// The manifest contains more segment descriptors than allowed.
    #[error("V2 typed-plane manifest has {actual} segments; maximum is {maximum}")]
    TooManySegments { actual: usize, maximum: usize },
    /// An allocation failed while decoding or encoding a bounded manifest.
    #[error("V2 typed-plane manifest allocation failed")]
    Allocation,
    /// The decoder's conservative owned-resident estimate exceeds its policy.
    #[error(
        "V2 typed-plane manifest needs an estimated {estimated} resident bytes; maximum is {maximum}"
    )]
    ResidentLimitExceeded { estimated: usize, maximum: usize },
    /// A field length, count, or fixed-width conversion overflowed.
    #[error("V2 typed-plane manifest length or count overflow")]
    CountOverflow,
    /// One mandatory family slot is absent from the exact seven-family list.
    #[error("V2 typed-plane manifest has {observed} family entries; expected seven")]
    FamilyCount { observed: u8 },
    /// A family slot is missing, duplicated, or in a noncanonical order.
    #[error("V2 typed-plane family at slot {index} is {observed:?}; expected {expected:?}")]
    FamilyOrder {
        index: usize,
        expected: SemanticIrPlane,
        observed: SemanticIrPlane,
    },
    /// A family total differs from the sum of its segment row counts.
    #[error("V2 typed-plane family row count does not match its segment inventory")]
    FamilyRowCount,
    /// A segment has no rows, invalid key order, or invalid payload length.
    #[error("invalid V2 typed-plane segment descriptor at index {index}")]
    SegmentClaim { index: usize },
    /// Segment key ranges overlap or are not strictly increasing.
    #[error("V2 typed-plane segment ranges overlap or are out of order at index {index}")]
    SegmentOrder { index: usize },
    /// The manifest repeats a semantic segment identity.
    #[error("V2 typed-plane manifest repeats a segment identity")]
    DuplicateSegmentId,
    /// A V2 generation manifest must claim complete input/read coverage.
    #[error("V2 typed-plane input claim is {observed:?}, not Complete")]
    InputClaimNotComplete { observed: Coverage },
    /// The profile bytes do not name a registered compiler profile.
    #[error("unknown V2 typed-plane language profile {0:?}")]
    UnknownProfile([u8; 2]),
    /// The stage byte does not name a registered semantic stage.
    #[error("unknown V2 typed-plane compiler stage {0}")]
    UnknownStage(u8),
    /// The tool byte does not name a registered native compiler.
    #[error("unknown V2 typed-plane native tool {0}")]
    UnknownTool(u8),
    /// The image-authority tag is not recognized.
    #[error("unknown V2 typed-plane image-authority tag {0}")]
    ImageAuthority(u8),
    /// The image-provenance tag is not recognized.
    #[error("unknown V2 typed-plane image-provenance tag {0}")]
    ImageProvenance(u8),
    /// The semantic family tag is not recognized.
    #[error("unknown V2 typed-plane family tag {0}")]
    FamilyTag(u8),
    /// A language-extension family has an invalid profile.
    #[error("unknown V2 typed-plane extension profile {0:?}")]
    UnknownExtensionProfile([u8; 2]),
    /// The optional scope-coordinate tag is not recognized.
    #[error("unknown V2 typed-plane scope-coordinate tag {0}")]
    ScopeCoordinate(u8),
    /// The source identity contains an invalid typed content-ID domain.
    #[error("invalid V2 typed-plane source identity")]
    SourceIdentity,
    /// The recipe identity contains an invalid typed content-ID domain.
    #[error("invalid V2 typed-plane recipe identity")]
    RecipeIdentity,
    /// The recipe toolchain contains an invalid typed content-ID domain.
    #[error("invalid V2 typed-plane toolchain identity")]
    ToolchainIdentity,
    /// The scope claim contains an invalid typed content-ID domain.
    #[error("invalid V2 typed-plane scope identity")]
    ScopeIdentity,
    /// The output length differed from the precomputed canonical length.
    #[error("V2 typed-plane manifest encoded {observed} bytes; expected {expected}")]
    LengthMismatch { expected: usize, observed: usize },
}

fn required_families(profile: LanguageProfile) -> [SemanticIrPlane; FAMILY_COUNT] {
    [
        SemanticIrPlane::Core,
        SemanticIrPlane::Types,
        SemanticIrPlane::Relations,
        SemanticIrPlane::Occurrences,
        SemanticIrPlane::Documentation,
        SemanticIrPlane::SourceProvenance,
        SemanticIrPlane::LanguageExtensions(profile),
    ]
}

fn validate_family_claims(
    family: SemanticIrPlane,
    row_count: u64,
    segments: &[SemanticTypedPlaneSegmentClaimV2],
) -> Result<(), SemanticTypedPlaneManifestV2Error> {
    let mut observed_rows = 0_u64;
    for (index, segment) in segments.iter().copied().enumerate() {
        validate_segment_claim(segment, index)?;
        if index > 0 && segments[index - 1].last_key >= segment.first_key {
            return Err(SemanticTypedPlaneManifestV2Error::SegmentOrder { index });
        }
        observed_rows = observed_rows
            .checked_add(u64::from(segment.row_count))
            .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
    }
    if observed_rows != row_count {
        return Err(SemanticTypedPlaneManifestV2Error::FamilyRowCount);
    }
    if matches!(family, SemanticIrPlane::LanguageExtensions(_))
        && segments.is_empty()
        && row_count != 0
    {
        return Err(SemanticTypedPlaneManifestV2Error::FamilyRowCount);
    }
    Ok(())
}

fn validate_segment_claim(
    segment: SemanticTypedPlaneSegmentClaimV2,
    index: usize,
) -> Result<(), SemanticTypedPlaneManifestV2Error> {
    if segment.row_count == 0
        || segment.byte_length == 0
        || segment.byte_length > MAX_SEMANTIC_SEGMENT_BYTES as u64
        || segment.first_key > segment.last_key
        || segment.id_claim.as_bytes() == &[0; 32]
    {
        return Err(SemanticTypedPlaneManifestV2Error::SegmentClaim { index });
    }
    Ok(())
}

fn encode_build(build: SemanticBuildIdentity, output: &mut Vec<u8>) {
    output.extend_from_slice(build.package());
    output.extend_from_slice(build.target());
    output.extend_from_slice(&<[u8; 2]>::from(build.profile()));
    output.push(u8::from(build.stage()));
    output.extend_from_slice(build.recipe());
    output.extend_from_slice(build.toolchain());
    output.extend_from_slice(build.environment());
    output.extend_from_slice(build.target_platform());
}

fn decode_build(
    reader: &mut Reader<'_>,
) -> Result<SemanticBuildIdentity, SemanticTypedPlaneManifestV2Error> {
    let package = reader.array32()?;
    let target = reader.array32()?;
    let profile_bytes = [reader.u8()?, reader.u8()?];
    let profile = LanguageProfile::try_from(profile_bytes)
        .map_err(|_| SemanticTypedPlaneManifestV2Error::UnknownProfile(profile_bytes))?;
    let stage_byte = reader.u8()?;
    let stage = Stage::try_from(stage_byte)
        .map_err(|_| SemanticTypedPlaneManifestV2Error::UnknownStage(stage_byte))?;
    Ok(SemanticBuildIdentity::new(
        package,
        target,
        profile,
        stage,
        reader.array32()?,
        reader.array32()?,
        reader.array32()?,
        reader.array32()?,
    ))
}

fn encode_image_facts(facts: SemanticImageFacts, output: &mut Vec<u8>) {
    match facts.authority {
        SemanticImageAuthority::Shared => output.push(0),
        SemanticImageAuthority::Language(profile) => {
            output.push(1);
            output.extend_from_slice(&<[u8; 2]>::from(profile));
        }
    }
    match facts.provenance {
        ImageProvenance::Unavailable => output.push(0),
        ImageProvenance::Captured {
            source,
            recipe,
            claim,
            scope,
        } => {
            output.push(1);
            output.extend_from_slice(source.identity.as_ref());
            output.extend_from_slice(&source.byte_len.to_be_bytes());
            output.extend_from_slice(recipe.identity.as_ref());
            output.extend_from_slice(&<[u8; 2]>::from(recipe.profile));
            output.push(u8::from(recipe.stage));
            output.push(u8::from(recipe.tool));
            output.extend_from_slice(recipe.toolchain.as_ref());
            output.extend_from_slice(claim.identity.as_ref());
            output.extend_from_slice(&scope.ecosystem.raw.to_be_bytes());
            output.extend_from_slice(&scope.package.raw.to_be_bytes());
            output.extend_from_slice(&scope.path.raw.to_be_bytes());
            match scope.coordinate {
                None => output.push(0),
                Some(coordinate) => {
                    output.push(1);
                    output.extend_from_slice(&coordinate.raw.to_be_bytes());
                }
            }
        }
    }
}

fn decode_image_facts(
    reader: &mut Reader<'_>,
) -> Result<SemanticImageFacts, SemanticTypedPlaneManifestV2Error> {
    let authority_tag = reader.u8()?;
    let authority =
        match authority_tag {
            0 => SemanticImageAuthority::Shared,
            1 => {
                let profile_bytes = [reader.u8()?, reader.u8()?];
                SemanticImageAuthority::Language(LanguageProfile::try_from(profile_bytes).map_err(
                    |_| SemanticTypedPlaneManifestV2Error::UnknownProfile(profile_bytes),
                )?)
            }
            tag => return Err(SemanticTypedPlaneManifestV2Error::ImageAuthority(tag)),
        };
    let provenance_tag = reader.u8()?;
    let provenance = match provenance_tag {
        0 => ImageProvenance::Unavailable,
        1 => {
            let source_identity = ContentId::<SourceFactDomain>::try_from(reader.array32()?)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::SourceIdentity)?;
            let source = SourceIdentity {
                identity: source_identity,
                byte_len: reader.u32()?,
            };
            let recipe_identity = ContentId::<CompileRecipeDomain>::try_from(reader.array32()?)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::RecipeIdentity)?;
            let profile_bytes = [reader.u8()?, reader.u8()?];
            let profile = LanguageProfile::try_from(profile_bytes)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::UnknownProfile(profile_bytes))?;
            let stage_byte = reader.u8()?;
            let stage = Stage::try_from(stage_byte)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::UnknownStage(stage_byte))?;
            let tool_byte = reader.u8()?;
            let tool = NativeTool::try_from(tool_byte)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::UnknownTool(tool_byte))?;
            let toolchain = ContentId::<ToolchainDomain>::try_from(reader.array32()?)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::ToolchainIdentity)?;
            let recipe = CompileRecipeFact {
                identity: recipe_identity,
                profile,
                stage,
                tool,
                toolchain,
            };
            let scope_identity = ContentId::<SemanticScopeDomain>::try_from(reader.array32()?)
                .map_err(|_| SemanticTypedPlaneManifestV2Error::ScopeIdentity)?;
            let scope = SemanticScopeFacts {
                ecosystem: AtomId::new(reader.u32()?),
                package: AtomId::new(reader.u32()?),
                path: AtomId::new(reader.u32()?),
                coordinate: match reader.u8()? {
                    0 => None,
                    1 => Some(AtomId::new(reader.u32()?)),
                    tag => return Err(SemanticTypedPlaneManifestV2Error::ScopeCoordinate(tag)),
                },
            };
            ImageProvenance::Captured {
                source,
                recipe,
                claim: SemanticScopeClaim {
                    identity: scope_identity,
                },
                scope,
            }
        }
        tag => return Err(SemanticTypedPlaneManifestV2Error::ImageProvenance(tag)),
    };
    Ok(SemanticImageFacts {
        authority,
        provenance,
    })
}

fn image_facts_wire_len(facts: SemanticImageFacts) -> usize {
    let authority = match facts.authority {
        SemanticImageAuthority::Shared => 1,
        SemanticImageAuthority::Language(_) => 3,
    };
    let provenance = match facts.provenance {
        ImageProvenance::Unavailable => 1,
        ImageProvenance::Captured { scope, .. } => {
            1 + 32
                + 4
                + 32
                + 2
                + 1
                + 1
                + 32
                + 32
                + 12
                + 1
                + usize::from(scope.coordinate.is_some()) * 4
        }
    };
    authority + provenance
}

fn encode_family_kind(family: SemanticIrPlane, output: &mut Vec<u8>) {
    match family {
        SemanticIrPlane::Core => output.push(0),
        SemanticIrPlane::Types => output.push(1),
        SemanticIrPlane::Relations => output.push(2),
        SemanticIrPlane::Occurrences => output.push(3),
        SemanticIrPlane::Documentation => output.push(4),
        SemanticIrPlane::SourceProvenance => output.push(5),
        SemanticIrPlane::LanguageExtensions(profile) => {
            output.push(6);
            output.extend_from_slice(&<[u8; 2]>::from(profile));
        }
    }
}

fn decode_family_kind(
    reader: &mut Reader<'_>,
) -> Result<SemanticIrPlane, SemanticTypedPlaneManifestV2Error> {
    match reader.u8()? {
        0 => Ok(SemanticIrPlane::Core),
        1 => Ok(SemanticIrPlane::Types),
        2 => Ok(SemanticIrPlane::Relations),
        3 => Ok(SemanticIrPlane::Occurrences),
        4 => Ok(SemanticIrPlane::Documentation),
        5 => Ok(SemanticIrPlane::SourceProvenance),
        6 => {
            let profile_bytes = [reader.u8()?, reader.u8()?];
            let profile = LanguageProfile::try_from(profile_bytes).map_err(|_| {
                SemanticTypedPlaneManifestV2Error::UnknownExtensionProfile(profile_bytes)
            })?;
            Ok(SemanticIrPlane::LanguageExtensions(profile))
        }
        tag => Err(SemanticTypedPlaneManifestV2Error::FamilyTag(tag)),
    }
}

fn family_kind_wire_len(family: SemanticIrPlane) -> usize {
    if matches!(family, SemanticIrPlane::LanguageExtensions(_)) {
        3
    } else {
        1
    }
}

fn coverage_code(coverage: Coverage) -> u8 {
    match coverage {
        Coverage::Complete => 1,
        Coverage::Partial => 2,
        Coverage::Unavailable => 3,
        Coverage::Unsupported => 4,
        Coverage::Closed => 5,
    }
}

fn decode_coverage(value: u8) -> Result<Coverage, SemanticTypedPlaneManifestV2Error> {
    match value {
        1 => Ok(Coverage::Complete),
        2 => Ok(Coverage::Partial),
        3 => Ok(Coverage::Unavailable),
        4 => Ok(Coverage::Unsupported),
        5 => Ok(Coverage::Closed),
        _ => Err(SemanticTypedPlaneManifestV2Error::NonCanonical),
    }
}

struct Reader<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> Reader<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'bytes [u8], SemanticTypedPlaneManifestV2Error> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SemanticTypedPlaneManifestV2Error::CountOverflow)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(SemanticTypedPlaneManifestV2Error::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, SemanticTypedPlaneManifestV2Error> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(SemanticTypedPlaneManifestV2Error::Truncated)
    }

    fn u16(&mut self) -> Result<u16, SemanticTypedPlaneManifestV2Error> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| SemanticTypedPlaneManifestV2Error::Truncated)?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, SemanticTypedPlaneManifestV2Error> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| SemanticTypedPlaneManifestV2Error::Truncated)?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, SemanticTypedPlaneManifestV2Error> {
        let bytes: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| SemanticTypedPlaneManifestV2Error::Truncated)?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn array32(&mut self) -> Result<[u8; 32], SemanticTypedPlaneManifestV2Error> {
        self.take(32)?
            .try_into()
            .map_err(|_| SemanticTypedPlaneManifestV2Error::Truncated)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn finish(self) -> Result<(), SemanticTypedPlaneManifestV2Error> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(SemanticTypedPlaneManifestV2Error::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use backend_version::{ContentId, SemanticScopeDomain, SourceFactDomain, ToolchainDomain};

    use super::*;
    use crate::ir::VERSIONED_PLANE_MANIFEST_SCHEMA;
    use crate::ir::{AtomId, SemanticScopeClaim, SemanticScopeFacts, SourceIdentity};
    use crate::vocabulary::RustEdition;

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

    fn facts() -> SemanticImageFacts {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source"),
            byte_len: 6,
        };
        let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(b"toolchain");
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            toolchain,
        );
        SemanticImageFacts {
            authority: SemanticImageAuthority::Shared,
            provenance: ImageProvenance::Captured {
                source,
                recipe,
                claim: SemanticScopeClaim {
                    identity: ContentId::<SemanticScopeDomain>::from_canonical_bytes(b"scope"),
                },
                scope: SemanticScopeFacts {
                    ecosystem: AtomId::new(0),
                    package: AtomId::new(1),
                    path: AtomId::new(2),
                    coordinate: Some(AtomId::new(3)),
                },
            },
        }
    }

    fn empty_families() -> [SemanticTypedPlaneFamilyDescriptorV2; FAMILY_COUNT] {
        required_families(build().profile()).map(|family| {
            SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(family, 0, Vec::new())
                .expect("explicit empty family is valid")
        })
    }

    fn input_claim(state: Coverage) -> SemanticInputClaimV2 {
        SemanticInputClaimV2::from_untrusted_claims([7; 32], ScopeRoot::from_bytes([8; 32]), state)
    }

    fn manifest() -> SemanticTypedPlaneManifestV2 {
        SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build(),
            facts(),
            input_claim(Coverage::Complete),
            UntrustedSemanticContentRootV2::from_wire_claim([9; 32]),
            UntrustedSemanticGenerationRootV2::from_wire_claim([10; 32]),
            empty_families(),
        )
        .expect("canonical empty V2 manifest")
    }

    #[test]
    fn canonical_round_trip_has_seven_explicit_empty_families_and_claim_only_witness() {
        let original = manifest();
        let bytes = original.canonical_bytes().expect("encode manifest");
        let reopened = SemanticTypedPlaneManifestV2::decode(&bytes).expect("decode manifest");

        assert_eq!(reopened, original);
        assert_eq!(reopened.families().len(), FAMILY_COUNT);
        assert!(
            reopened
                .families()
                .iter()
                .all(|family| { family.row_count() == 0 && family.segments().is_empty() })
        );
        assert_eq!(reopened.input_claim().coverage_state(), Coverage::Complete);
        let usage = reopened.resource_usage().expect("bounded resource usage");
        assert_eq!(usage.wire_bytes(), bytes.len());
        assert_eq!(usage.segment_descriptors(), 0);
        assert_eq!(usage.descriptor_validation_steps(), 0);
        assert!(
            usage.estimated_peak_resident_bytes() <= MAX_TYPED_PLANE_MANIFEST_V2_RESIDENT_BYTES
        );
        assert_ne!(
            SEMANTIC_TYPED_PLANE_MANIFEST_V2_SCHEMA,
            VERSIONED_PLANE_MANIFEST_SCHEMA
        );
    }

    #[test]
    fn decoder_rejects_trailing_bytes_and_omitted_family_descriptor() {
        let encoded = manifest().canonical_bytes().expect("encode manifest");
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(
            SemanticTypedPlaneManifestV2::decode(&trailing),
            Err(SemanticTypedPlaneManifestV2Error::TrailingBytes)
        );

        let mut omitted = encoded;
        let family_count_offset = HEADER_BYTES
            + BUILD_BYTES
            + image_facts_wire_len(facts())
            + INPUT_CLAIM_BYTES
            + 32
            + 32;
        omitted[family_count_offset] = 6;
        assert_eq!(
            SemanticTypedPlaneManifestV2::decode(&omitted),
            Err(SemanticTypedPlaneManifestV2Error::FamilyCount { observed: 6 })
        );
    }

    #[test]
    fn decoder_rejects_hostile_counts_before_allocating_segment_vectors() {
        let encoded = manifest().canonical_bytes().expect("encode manifest");
        let family_count_offset = HEADER_BYTES
            + BUILD_BYTES
            + image_facts_wire_len(facts())
            + INPUT_CLAIM_BYTES
            + 32
            + 32;
        let first_segment_count_offset = family_count_offset + 1 + 1 + 8;

        let mut excessive_count = encoded.clone();
        excessive_count[first_segment_count_offset..first_segment_count_offset + 4]
            .copy_from_slice(&500_000_u32.to_be_bytes());
        assert_eq!(
            SemanticTypedPlaneManifestV2::decode(&excessive_count),
            Err(SemanticTypedPlaneManifestV2Error::TooManySegments {
                actual: 500_000,
                maximum: MAX_TYPED_PLANE_SEGMENTS_V2,
            })
        );

        let mut truncated_under_limit = encoded;
        truncated_under_limit[first_segment_count_offset..first_segment_count_offset + 4]
            .copy_from_slice(&5_000_u32.to_be_bytes());
        assert_eq!(
            SemanticTypedPlaneManifestV2::decode(&truncated_under_limit),
            Err(SemanticTypedPlaneManifestV2Error::Truncated)
        );
    }

    #[test]
    fn untrusted_family_order_and_segment_claims_fail_closed() {
        let mut families = empty_families();
        families.swap(0, 1);
        assert!(matches!(
            SemanticTypedPlaneManifestV2::from_untrusted_claims(
                build(),
                facts(),
                input_claim(Coverage::Complete),
                UntrustedSemanticContentRootV2::from_wire_claim([9; 32]),
                UntrustedSemanticGenerationRootV2::from_wire_claim([10; 32]),
                families,
            ),
            Err(SemanticTypedPlaneManifestV2Error::FamilyOrder { index: 0, .. })
        ));

        assert!(matches!(
            SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                [2; 32],
                [1; 32],
                1,
                20,
                UntrustedSemanticSegmentId::from_raw([3; 32]),
            ),
            Err(SemanticTypedPlaneManifestV2Error::SegmentClaim { .. })
        ));
    }

    #[test]
    fn input_manifest_witness_is_canonicalized_to_a_claim() {
        let partial_claim = input_claim(Coverage::Partial);
        assert!(matches!(
            SemanticTypedPlaneManifestV2::from_untrusted_claims(
                build(),
                facts(),
                partial_claim,
                UntrustedSemanticContentRootV2::from_wire_claim([9; 32]),
                UntrustedSemanticGenerationRootV2::from_wire_claim([10; 32]),
                empty_families(),
            ),
            Err(SemanticTypedPlaneManifestV2Error::InputClaimNotComplete {
                observed: Coverage::Partial
            })
        ));

        let manifest = SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build(),
            facts(),
            input_claim(Coverage::Complete),
            UntrustedSemanticContentRootV2::from_wire_claim([9; 32]),
            UntrustedSemanticGenerationRootV2::from_wire_claim([10; 32]),
            empty_families(),
        )
        .expect("complete input claim is retained without authority capability");
        assert_eq!(manifest.input_claim(), input_claim(Coverage::Complete));
    }

    #[test]
    fn live_witness_conversion_drops_admission_details() {
        let witness = SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32]));
        let claim = SemanticInputClaimV2::from_witness(&witness);
        assert_eq!(claim, input_claim(Coverage::Complete));
        assert_eq!(claim.coverage_state(), Coverage::Complete);
        assert!(
            !claim
                .as_claimed_witness()
                .coverage()
                .is_authorized_complete()
        );
    }
}
