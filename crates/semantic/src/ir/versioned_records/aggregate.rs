//! Whole-manifest semantic checks for typed-plane V2.
//!
//! This module is the only constructor for the opaque content inventory token.
//! It validates each exact SPIR payload, enforces bounded aggregate work, and
//! reconciles all seven family censuses and references before retaining only
//! content summaries. A decoded inventory is semantic content evidence; it is
//! not an owner admission or selection capability.

use alloc::{boxed::Box, string::ToString, vec::Vec};

use crate::ir::row_index::{
    RowFamily, RowPayload, StableRowIndex, StableRowIndexError, StableRowKey,
};
use crate::ir::{
    CanonicalSemanticPlaneBoundaryFamilyVerifier, CanonicalSemanticPlaneSegmentView,
    ImageProvenance, SemanticBuildIdentity, SemanticImageAuthority, SemanticImageFacts,
    SemanticInputWitness, SemanticIrPlane, SemanticPlaneKind, SemanticPlaneRecordError,
    SemanticPlaneSegment, SemanticSegmentId, SemanticTypedPlaneFamilyDescriptorV2,
    SemanticTypedPlaneSegmentClaimV2, UntrustedSemanticSegmentId,
    decode_semantic_plane_segment_with_row_limit,
};
use crate::vocabulary::{CompileRecipeFact, LanguageProfile};
use thiserror::Error;

use super::extensions::{
    CheckedLanguageExtensionFamilyV2, CheckedLanguageExtensionFamilyV2Builder,
    LanguageExtensionFamilyValidationError,
    validate_language_extension_family_v2_with_limits_detailed,
};
use super::types::{
    CheckedTypesFamilyV2, CheckedTypesFamilyV2Builder, TypesFamilyValidationError,
    validate_types_family_v2_with_limits_detailed,
};
use super::wire::{Cursor, read_identity};
use super::{
    CanonicalPlaneSegmentBoundaryPolicy, LanguageExtensionVerificationLimitsV2,
    TypesFamilyVerificationLimitsV2, validate_language_extension_family_v2_with_limits,
    validate_types_family_v2_with_limits,
};
use crate::ir::semantic_generation::TypedPlaneSegmentSourceV2;

/// Resource ceiling for one aggregate verification window.
///
/// The standard policy is intended for local reopen and admission. The large
/// package policy permits larger complete images while keeping every bound
/// explicit. These limits size one in-memory verification window; they do not
/// define the maximum semantic image size once callers can verify/spill in
/// bounded family batches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SemanticTypedPlaneVerificationLimitsV2 {
    max_segments: usize,
    max_total_bytes: u64,
    max_total_rows: u64,
    max_references: u64,
    max_total_jumbo_value_bytes: u64,
    max_total_jumbo_leaves: u64,
    max_total_jumbo_object_reads: u64,
    max_total_jumbo_read_bytes: u64,
}

impl SemanticTypedPlaneVerificationLimitsV2 {
    /// Conservative bound for an ordinary local verification window.
    pub(crate) const fn standard() -> Self {
        Self {
            max_segments: 8_192,
            max_total_bytes: 64 * 1024 * 1024,
            max_total_rows: 250_000,
            max_references: 1_000_000,
            max_total_jumbo_value_bytes: 64 * 1024 * 1024,
            max_total_jumbo_leaves: 4_096,
            max_total_jumbo_object_reads: 8_192,
            max_total_jumbo_read_bytes: 64 * 1024 * 1024
                + 8_192 * crate::ir::jumbo_rope::ROPE_NODE_WIRE_BYTES as u64,
        }
    }

    /// Higher bounded tier for packages with larger semantic closures.
    pub(crate) const fn large_package() -> Self {
        Self {
            max_segments: 65_536,
            max_total_bytes: 512 * 1024 * 1024,
            max_total_rows: 2_000_000,
            max_references: 8_000_000,
            max_total_jumbo_value_bytes: 512 * 1024 * 1024,
            max_total_jumbo_leaves: 32_768,
            max_total_jumbo_object_reads: 65_536,
            max_total_jumbo_read_bytes: 512 * 1024 * 1024
                + 65_536 * crate::ir::jumbo_rope::ROPE_NODE_WIRE_BYTES as u64,
        }
    }

    pub(crate) const fn max_segments(self) -> usize {
        self.max_segments
    }

    pub(crate) const fn max_total_bytes(self) -> u64 {
        self.max_total_bytes
    }

    pub(crate) const fn max_total_rows(self) -> u64 {
        self.max_total_rows
    }

    pub(crate) const fn max_references(self) -> u64 {
        self.max_references
    }

    pub(crate) const fn max_total_jumbo_value_bytes(self) -> u64 {
        self.max_total_jumbo_value_bytes
    }

    pub(crate) const fn max_total_jumbo_leaves(self) -> u64 {
        self.max_total_jumbo_leaves
    }

    pub(crate) const fn max_total_jumbo_object_reads(self) -> u64 {
        self.max_total_jumbo_object_reads
    }

    pub(crate) const fn max_total_jumbo_read_bytes(self) -> u64 {
        self.max_total_jumbo_read_bytes
    }
}

const CORE_DECLARATION_TAG: u8 = 1;
const DOCUMENTATION_TAG: u8 = 2;
const RELATION_TAG: u8 = 3;
const OCCURRENCE_TAG: u8 = 4;
const SOURCE_DECLARATION_TAG: u8 = 1;
const SOURCE_RELATION_TAG: u8 = 2;
const RELATION_SOURCE_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.relation-source-key.v1\0";
const SEMANTIC_FAMILY_ROW_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.family-row-index-root.v2\0";

/// Exact descriptor and payload pair borrowed from a decoded V2 manifest.
/// The manifest owns the untrusted metadata; this adapter makes the payload
/// pairing explicit without copying the c004 bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedPlaneSegmentPayloadV2<'bytes> {
    pub(crate) first_key: [u8; 32],
    pub(crate) last_key: [u8; 32],
    pub(crate) row_count: u32,
    pub(crate) byte_length: u64,
    pub(crate) id_claim: UntrustedSemanticSegmentId,
    pub(crate) payload: &'bytes [u8],
}

impl<'bytes> TypedPlaneSegmentPayloadV2<'bytes> {
    pub(crate) const fn new(
        first_key: [u8; 32],
        last_key: [u8; 32],
        row_count: u32,
        byte_length: u64,
        id_claim: UntrustedSemanticSegmentId,
        payload: &'bytes [u8],
    ) -> Self {
        Self {
            first_key,
            last_key,
            row_count,
            byte_length,
            id_claim,
            payload,
        }
    }
}

/// One family entry from c005. Empty families remain explicit through a
/// present value with an empty segment slice and a declared zero row count.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedPlaneFamilyPayloadsV2<'bytes> {
    pub(crate) family: SemanticIrPlane,
    pub(crate) row_count: u64,
    pub(crate) boundary_policy: CanonicalPlaneSegmentBoundaryPolicy,
    pub(crate) segments: &'bytes [TypedPlaneSegmentPayloadV2<'bytes>],
}

impl<'bytes> TypedPlaneFamilyPayloadsV2<'bytes> {
    pub(crate) const fn new(
        family: SemanticIrPlane,
        row_count: u64,
        boundary_policy: CanonicalPlaneSegmentBoundaryPolicy,
        segments: &'bytes [TypedPlaneSegmentPayloadV2<'bytes>],
    ) -> Self {
        Self {
            family,
            row_count,
            boundary_policy,
            segments,
        }
    }
}

/// Exact segment facts retained after every corresponding c004 payload has
/// been independently hashed and decoded.
pub(crate) struct VerifiedTypedPlaneSegmentV2 {
    first_key: [u8; 32],
    last_key: [u8; 32],
    row_count: u32,
    byte_length: u64,
    admitted_id: SemanticSegmentId,
}

impl VerifiedTypedPlaneSegmentV2 {
    pub(crate) const fn first_key(&self) -> &[u8; 32] {
        &self.first_key
    }

    pub(crate) const fn last_key(&self) -> &[u8; 32] {
        &self.last_key
    }

    pub(crate) const fn row_count(&self) -> u32 {
        self.row_count
    }

    pub(crate) const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub(crate) const fn admitted_id(&self) -> SemanticSegmentId {
        self.admitted_id
    }
}

/// One complete family in canonical order, including explicit empty families.
pub(crate) struct VerifiedTypedPlaneFamilyV2 {
    family: SemanticIrPlane,
    segments: Box<[VerifiedTypedPlaneSegmentV2]>,
    row_count: u64,
    semantic_row_root: [u8; 32],
}

impl VerifiedTypedPlaneFamilyV2 {
    pub(crate) const fn family(&self) -> SemanticIrPlane {
        self.family
    }

    pub(crate) fn segments(&self) -> &[VerifiedTypedPlaneSegmentV2] {
        &self.segments
    }

    pub(crate) const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Returns a content root over the ordered typed rows, independent of
    /// their c004 segment partitioning.
    pub(crate) const fn semantic_row_root(&self) -> [u8; 32] {
        self.semantic_row_root
    }
}

/// Opaque proof token for one exact, complete seven-family semantic content
/// inventory. It retains c005 input/read claims as claims only. A fresh owner
/// capability and source/scope binding are required before generation
/// selection or publication.
pub(crate) struct VerifiedTypedPlaneInventoryV2 {
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: [VerifiedTypedPlaneFamilyV2; 7],
}

impl VerifiedTypedPlaneInventoryV2 {
    pub(crate) const fn build(&self) -> SemanticBuildIdentity {
        self.build
    }

    pub(crate) const fn image_facts(&self) -> SemanticImageFacts {
        self.image_facts
    }

    /// Returns the canonical input/read claim from c005. It has no live
    /// coverage capability and therefore cannot authorize selection.
    pub(crate) const fn input_witness(&self) -> SemanticInputWitness {
        self.input_witness
    }

    pub(crate) const fn families(&self) -> &[VerifiedTypedPlaneFamilyV2; 7] {
        &self.families
    }
}

/// Error while checking c005 descriptors, exact c004 bytes, or row-family
/// closure. Family-local grammar failures retain the existing strict parser
/// error; aggregate errors identify missing or contradictory semantic facts.
#[derive(Debug, Error)]
pub(crate) enum SemanticTypedPlaneInventoryV2Error {
    #[error(transparent)]
    Record(#[from] SemanticPlaneRecordError),
    #[error("typed semantic manifest has {observed} payloads; expected {expected}")]
    PayloadCount { expected: usize, observed: usize },
    #[error(
        "typed semantic family inventory differs at slot {index}: expected {expected:?}, observed {observed:?}"
    )]
    FamilyOrder {
        index: usize,
        expected: SemanticIrPlane,
        observed: SemanticIrPlane,
    },
    #[error("typed semantic family {family:?} claims {observed} rows; decoded {expected}")]
    FamilyRowCount {
        family: SemanticIrPlane,
        expected: u64,
        observed: u64,
    },
    #[error(
        "typed semantic family {family:?} segment {index} has inconsistent descriptor metadata"
    )]
    SegmentDescriptor {
        family: SemanticIrPlane,
        index: usize,
    },
    #[error(
        "typed semantic family {family:?} has overlapping or unordered segment ranges at {index}"
    )]
    SegmentOrder {
        family: SemanticIrPlane,
        index: usize,
    },
    #[error(
        "typed semantic family {family:?} segment {index} contains a row key {key:?} that does not match its typed identity"
    )]
    RowIdentityMismatch {
        family: SemanticIrPlane,
        index: usize,
        key: [u8; 32],
    },
    #[error("typed semantic manifest input/read claim is not Complete")]
    IncompleteInputClaim,
    #[error("typed semantic build and image facts disagree at {field}")]
    ImageFactsBuildMismatch { field: &'static str },
    #[error("typed semantic aggregate exceeds its {budget} budget")]
    AggregateBudget { budget: &'static str },
    #[error("typed semantic segment source failed: {0}")]
    SegmentSource(alloc::string::String),
    #[error(transparent)]
    RowIndex(#[from] StableRowIndexError),
    #[error("typed semantic cross-family closure is inconsistent: {fact}")]
    CrossFamily { fact: &'static str },
}

/// One borrowed source for the descriptor closures carried by Docs and
/// SourceProvenance rows. This small object-safe seam lets the long-standing
/// no-object verifier remain available while rejecting descriptor rows.
pub(crate) trait JumboPlaneClosureAdmissionV2 {
    fn admit(
        &mut self,
        family: SemanticIrPlane,
        descriptor: crate::ir::CheckedJumboValueDescriptor,
        documentation_reference_budget: u64,
    ) -> Result<Option<super::declarations::DocsWireReferences>, SemanticTypedPlaneInventoryV2Error>;
}

pub(crate) struct JumboObjectClosureAdmissionV2<'source, S: ?Sized> {
    source: &'source mut S,
    limits: crate::ir::JumboRopeLimits,
    aggregate_limits: SemanticTypedPlaneVerificationLimitsV2,
    work: AdmittedJumboWorkV2,
}

#[derive(Default)]
struct AdmittedJumboWorkV2 {
    value_bytes: u64,
    leaves: u64,
    object_reads: u64,
    read_bytes: u64,
}

impl AdmittedJumboWorkV2 {
    fn charge(
        &mut self,
        descriptor: crate::ir::CheckedJumboValueDescriptor,
        limits: SemanticTypedPlaneVerificationLimitsV2,
    ) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
        let nodes = descriptor.leaf_count().saturating_sub(1);
        let object_reads = descriptor.leaf_count().checked_add(nodes).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-object-reads",
            },
        )?;
        let node_bytes = nodes
            .checked_mul(crate::ir::jumbo_rope::ROPE_NODE_WIRE_BYTES as u64)
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-read-bytes",
            })?;
        let next_value_bytes = self
            .value_bytes
            .checked_add(descriptor.byte_length())
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-value-bytes",
            })?;
        let next_leaves = self.leaves.checked_add(descriptor.leaf_count()).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-leaf-count",
            },
        )?;
        let next_object_reads = self.object_reads.checked_add(object_reads).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-object-reads",
            },
        )?;
        let next_read_bytes = self
            .read_bytes
            .checked_add(descriptor.byte_length())
            .and_then(|bytes| bytes.checked_add(node_bytes))
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-read-bytes",
            })?;
        if next_value_bytes > limits.max_total_jumbo_value_bytes {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-value-bytes",
            });
        }
        if next_leaves > limits.max_total_jumbo_leaves {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-leaf-count",
            });
        }
        if next_object_reads > limits.max_total_jumbo_object_reads {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-object-reads",
            });
        }
        if next_read_bytes > limits.max_total_jumbo_read_bytes {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-read-bytes",
            });
        }
        self.value_bytes = next_value_bytes;
        self.leaves = next_leaves;
        self.object_reads = next_object_reads;
        self.read_bytes = next_read_bytes;
        Ok(())
    }
}

impl<'source, S: ?Sized> JumboObjectClosureAdmissionV2<'source, S> {
    pub(crate) const fn new(
        source: &'source mut S,
        limits: crate::ir::JumboRopeLimits,
        aggregate_limits: SemanticTypedPlaneVerificationLimitsV2,
    ) -> Self {
        Self {
            source,
            limits,
            aggregate_limits,
            work: AdmittedJumboWorkV2 {
                value_bytes: 0,
                leaves: 0,
                object_reads: 0,
                read_bytes: 0,
            },
        }
    }
}

impl<S> JumboPlaneClosureAdmissionV2 for JumboObjectClosureAdmissionV2<'_, S>
where
    S: crate::ir::JumboRopeObjectSource + ?Sized,
    S::Error: core::fmt::Display,
{
    fn admit(
        &mut self,
        family: SemanticIrPlane,
        descriptor: crate::ir::CheckedJumboValueDescriptor,
        documentation_reference_budget: u64,
    ) -> Result<Option<super::declarations::DocsWireReferences>, SemanticTypedPlaneInventoryV2Error>
    {
        // Row decoding applies the default hard policy. Re-check the exact
        // wire claim here so callers may impose a stricter admission window.
        let descriptor =
            crate::ir::UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
                .map_err(SemanticPlaneRecordError::from)?
                .check(self.limits)
                .map_err(SemanticPlaneRecordError::from)?;
        self.work.charge(descriptor, self.aggregate_limits)?;
        match family {
            SemanticIrPlane::Documentation => {
                let mut validator = super::declarations::DocsWireValidator::with_reference_limit(
                    documentation_reference_budget,
                );
                descriptor
                    .admit_stored_closure_to(self.source, &mut validator)
                    .map_err(map_jumbo_source_error)?;
                let references = validator.finish().map_err(|error| match error {
                    super::declarations::DocsWireValidationError::ReferenceLimitExceeded => {
                        SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                            budget: "reference-count",
                        }
                    }
                    error => SemanticPlaneRecordError::from(error).into(),
                })?;
                Ok(Some(references))
            }
            SemanticIrPlane::SourceProvenance => descriptor
                .admit_stored_closure(self.source)
                .map(|_| None)
                .map_err(map_jumbo_source_error)
                .map_err(Into::into),
            _ => Err(SemanticPlaneRecordError::RowGrammar.into()),
        }
    }
}

/// Verifies the exact flattened c004 payload sequence named by c005, all seven
/// family descriptors, and every cross-family semantic join. The supplied
/// input witness is the manifest's decoded claim only; its coverage capability
/// is not consulted or reconstructed here.
pub(crate) fn verify_semantic_typed_plane_inventory_v2(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[TypedPlaneFamilyPayloadsV2<'_>; 7],
    ordered_payloads: &[&[u8]],
    limits: SemanticTypedPlaneVerificationLimitsV2,
) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error> {
    verify_semantic_typed_plane_inventory_v2_with_admission(
        build,
        image_facts,
        input_witness,
        families,
        ordered_payloads,
        limits,
        None,
    )
}

/// Source-aware inventory verifier used by cold c007 reopen. Every descriptor
/// row is context-checked and its full rope closure is authenticated before
/// the inventory token can be returned.
pub(crate) fn verify_semantic_typed_plane_inventory_v2_with_jumbo_source<S>(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[TypedPlaneFamilyPayloadsV2<'_>; 7],
    ordered_payloads: &[&[u8]],
    limits: SemanticTypedPlaneVerificationLimitsV2,
    jumbo_limits: crate::ir::JumboRopeLimits,
    source: &mut S,
) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>
where
    S: crate::ir::JumboRopeObjectSource + ?Sized,
    S::Error: core::fmt::Display,
{
    let mut admission = JumboObjectClosureAdmissionV2::new(source, jumbo_limits, limits);
    verify_semantic_typed_plane_inventory_v2_with_admission(
        build,
        image_facts,
        input_witness,
        families,
        ordered_payloads,
        limits,
        Some(&mut admission),
    )
}

pub(crate) fn verify_semantic_typed_plane_inventory_v2_with_admission(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[TypedPlaneFamilyPayloadsV2<'_>; 7],
    ordered_payloads: &[&[u8]],
    limits: SemanticTypedPlaneVerificationLimitsV2,
    mut jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error> {
    let input_witness = normalize_input_claim(input_witness)?;
    verify_manifest_claims(
        build,
        image_facts,
        input_witness,
        families,
        ordered_payloads,
        limits,
    )?;

    let mut decoded_families: [Vec<CanonicalSemanticPlaneSegmentView<'_>>; 7] =
        core::array::from_fn(|_| Vec::new());
    let mut verified_families = Vec::new();
    verified_families
        .try_reserve_exact(7)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    let mut verified_ids = Vec::new();
    let mut payload_index = 0_usize;
    let mut jumbo_documentation_references = Vec::new();
    let mut jumbo_documentation_reference_count = 0_u64;

    for (family_index, family) in families.iter().enumerate() {
        let kind = SemanticPlaneKind::Ir(family.family);
        let mut verified_segments = Vec::new();
        verified_segments
            .try_reserve_exact(family.segments.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        let mut observed_rows = 0_u64;
        let row_family = stable_row_family(family.family);
        let mut row_index_builder = StableRowIndex::builder();
        let mut boundary_verifier = CanonicalSemanticPlaneBoundaryFamilyVerifier::begin_family(
            family.family,
            family.boundary_policy,
        );
        for (segment_index, segment) in family.segments.iter().enumerate() {
            let Some(payload) = ordered_payloads.get(payload_index).copied() else {
                return Err(SemanticTypedPlaneInventoryV2Error::PayloadCount {
                    expected: payload_index + 1,
                    observed: ordered_payloads.len(),
                });
            };
            payload_index += 1;
            let descriptor = SemanticPlaneSegment::from_payload(
                kind,
                segment.first_key,
                segment.last_key,
                segment.row_count,
                payload,
            )
            .map_err(SemanticPlaneRecordError::from)?;
            let Some(admitted_id) = descriptor.admitted_id() else {
                return Err(SemanticPlaneRecordError::MissingAdmittedId.into());
            };
            if descriptor.byte_length() != segment.byte_length
                || segment.byte_length
                    != u64::try_from(payload.len()).map_err(|_| {
                        SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                            budget: "payload-bytes",
                        }
                    })?
                || admitted_id.as_bytes() != segment.id_claim.as_bytes()
            {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor {
                    family: family.family,
                    index: segment_index,
                });
            }
            let view = decode_semantic_plane_segment_with_row_limit(
                kind,
                &descriptor,
                payload,
                family.boundary_policy.maximum_bytes(),
            )?;
            boundary_verifier.push_segment(view)?;
            if let Some(previous) = decoded_families[family_index].last()
                && previous.last_key() >= view.first_key()
            {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentOrder {
                    family: family.family,
                    index: segment_index,
                });
            }
            observed_rows = observed_rows
                .checked_add(u64::from(view.row_count()))
                .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "row-count",
                })?;
            for record in view.records() {
                let jumbo = match family.family {
                    SemanticIrPlane::Documentation => {
                        super::declarations::jumbo_descriptor_for_record_with_row_limit(
                            record,
                            family.boundary_policy.maximum_bytes(),
                        )?
                    }
                    SemanticIrPlane::SourceProvenance => {
                        super::source_provenance::jumbo_descriptor_for_record_with_row_limit(
                            record,
                            family.boundary_policy.maximum_bytes(),
                        )?
                    }
                    _ => None,
                };
                if let Some(descriptor) = jumbo {
                    let admission = jumbo_admission
                        .as_deref_mut()
                        .ok_or(SemanticPlaneRecordError::JumboObjectStoreRequired)?;
                    let remaining_reference_budget = limits
                        .max_references
                        .saturating_sub(jumbo_documentation_reference_count);
                    if let Some(references) =
                        admission.admit(family.family, descriptor, remaining_reference_budget)?
                    {
                        let local_count = u64::try_from(references.local.len()).map_err(|_| {
                            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                budget: "reference-count",
                            }
                        })?;
                        let external_count =
                            u64::try_from(references.external.len()).map_err(|_| {
                                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                    budget: "reference-count",
                                }
                            })?;
                        let added = local_count.checked_add(external_count).ok_or(
                            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                budget: "reference-count",
                            },
                        )?;
                        jumbo_documentation_reference_count = jumbo_documentation_reference_count
                            .checked_add(added)
                            .filter(|count| *count <= limits.max_references)
                            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                budget: "reference-count",
                            })?;
                        jumbo_documentation_references
                            .try_reserve(1)
                            .map_err(SemanticPlaneRecordError::Allocation)?;
                        jumbo_documentation_references.push((record.key(), references));
                    }
                }
                let key = StableRowKey::new(row_family, record.key());
                let row_payload = RowPayload::from_tagged_bytes(record.tag(), record.payload())?;
                row_index_builder.push(key, row_payload)?;
            }
            decoded_families[family_index]
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            decoded_families[family_index].push(view);
            verified_ids
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            verified_ids.push(admitted_id);
            verified_segments.push(VerifiedTypedPlaneSegmentV2 {
                first_key: *descriptor.first_key(),
                last_key: *descriptor.last_key(),
                row_count: descriptor.row_count(),
                byte_length: descriptor.byte_length(),
                admitted_id,
            });
        }
        boundary_verifier.finish(
            family.row_count,
            u64::try_from(family.segments.len()).map_err(|_| {
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "segment-count",
                }
            })?,
        )?;
        if observed_rows != family.row_count {
            return Err(SemanticTypedPlaneInventoryV2Error::FamilyRowCount {
                family: family.family,
                expected: family.row_count,
                observed: observed_rows,
            });
        }
        let row_index = row_index_builder.finish()?;
        if row_index.row_count() != observed_rows {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "semantic row-index census",
            });
        }
        let semantic_row_root =
            semantic_family_row_root(family.family, observed_rows, row_index.root().as_bytes());
        verified_families.push(VerifiedTypedPlaneFamilyV2 {
            family: family.family,
            segments: verified_segments.into_boxed_slice(),
            row_count: observed_rows,
            semantic_row_root,
        });
    }
    if payload_index != ordered_payloads.len() {
        return Err(SemanticTypedPlaneInventoryV2Error::PayloadCount {
            expected: payload_index,
            observed: ordered_payloads.len(),
        });
    }
    verified_ids.sort_unstable();
    if verified_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }

    jumbo_documentation_references.sort_unstable_by_key(|(owner, _)| *owner);
    if jumbo_documentation_references
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    let family_row_limits =
        core::array::from_fn(|index| families[index].boundary_policy.maximum_bytes());
    validate_cross_family_closure(
        build.profile(),
        &decoded_families,
        limits,
        family_row_limits,
        &mut jumbo_documentation_references,
        jumbo_documentation_reference_count,
    )?;
    if matches!(image_facts.authority, SemanticImageAuthority::Shared) && families[6].row_count != 0
    {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "shared image has language extension rows",
        });
    }

    let families: [VerifiedTypedPlaneFamilyV2; 7] = verified_families.try_into().map_err(|_| {
        SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "seven-family inventory conversion",
        }
    })?;
    Ok(VerifiedTypedPlaneInventoryV2 {
        build,
        image_facts,
        input_witness,
        families,
    })
}

/// Verifies a typed V2 inventory through a lending source that yields each
/// exact c004 payload once, in manifest order. Jumbo closure admission,
/// semantic row-root construction, and family decoding happen while that
/// segment borrow is live.
pub(crate) fn verify_semantic_typed_plane_inventory_v2_with_segment_source<S>(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[SemanticTypedPlaneFamilyDescriptorV2; 7],
    source: &mut S,
    limits: SemanticTypedPlaneVerificationLimitsV2,
    mut jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>
where
    S: TypedPlaneSegmentSourceV2 + ?Sized,
{
    verify_semantic_typed_plane_inventory_v2_with_segment_source_inner(
        build,
        image_facts,
        input_witness,
        families,
        source,
        limits,
        jumbo_admission,
        InputClaimValidation::Complete,
    )
}

/// Verifies the same complete seven-family inventory for durable semantic
/// history while preserving its selected generation's claim-only input
/// coverage state. The result is content evidence only; it does not establish
/// current selection or authorize read-frontier reuse.
pub(crate) fn verify_semantic_typed_plane_history_v3_with_segment_source<S>(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_claim: SemanticInputWitness,
    families: &[SemanticTypedPlaneFamilyDescriptorV2; 7],
    source: &mut S,
    limits: SemanticTypedPlaneVerificationLimitsV2,
    jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>
where
    S: TypedPlaneSegmentSourceV2 + ?Sized,
{
    verify_semantic_typed_plane_inventory_v2_with_segment_source_inner(
        build,
        image_facts,
        input_claim,
        families,
        source,
        limits,
        jumbo_admission,
        InputClaimValidation::PersistedHistory,
    )
}

#[derive(Clone, Copy)]
enum InputClaimValidation {
    Complete,
    PersistedHistory,
}

fn verify_semantic_typed_plane_inventory_v2_with_segment_source_inner<S>(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[SemanticTypedPlaneFamilyDescriptorV2; 7],
    source: &mut S,
    limits: SemanticTypedPlaneVerificationLimitsV2,
    mut jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
    input_claim_validation: InputClaimValidation,
) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>
where
    S: TypedPlaneSegmentSourceV2 + ?Sized,
{
    let input_witness =
        normalize_input_claim_for_validation(input_witness, input_claim_validation)?;
    verify_stream_manifest_claims(
        build,
        image_facts,
        input_witness,
        families,
        limits,
        input_claim_validation,
    )?;

    let mut jumbo_documentation_row_count = 0_usize;
    let mut jumbo_documentation_reference_count = 0_u64;
    let mut global_segment_index = 0_usize;
    let mut reference_scratch = AggregateReferenceScratchV2::new(limits.max_references, 0)?;
    let mut verified_families = Vec::new();
    verified_families
        .try_reserve_exact(7)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    let mut verified_ids = Vec::new();
    let mut core = CoreFamilyFacts::default();
    let mut types_family = None;
    let mut relations = RelationFamilyFacts::default();
    let mut occurrences = OccurrenceFamilyFacts::default();
    let mut docs = DocumentationFamilyFacts::default();
    let mut source_facts = SourceFamilyFacts::default();
    let mut extensions = None;

    for (family_index, family) in families.iter().enumerate() {
        let kind = SemanticPlaneKind::Ir(family.family());
        let type_limits = if family_index == 1 {
            Some(TypesFamilyVerificationLimitsV2::bounded(
                limits.max_total_bytes,
                limits.max_total_rows,
                reference_scratch.remaining().min(limits.max_references),
            ))
        } else {
            None
        };
        let mut type_builder = type_limits
            .map(CheckedTypesFamilyV2Builder::new)
            .transpose()?;
        let extension_limits = if family_index == 6 {
            Some(LanguageExtensionVerificationLimitsV2::bounded(
                limits.max_total_bytes,
                limits.max_total_rows,
                reference_scratch.remaining().min(limits.max_references),
            ))
        } else {
            None
        };
        let mut extension_builder = extension_limits.map(|family_limits| {
            CheckedLanguageExtensionFamilyV2Builder::new(build.profile(), family_limits)
        });
        let mut verified_segments = Vec::new();
        verified_segments
            .try_reserve_exact(family.segments().len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        let mut observed_rows = 0_u64;
        let mut previous_segment_last = None;
        // Source-derived residency: this sorted-row builder retains the whole
        // current family, so borrowed c004 payloads do not make row-index
        // scratch segment-bounded. On the current 64-bit layout, 250k rows ×
        // 88-byte slots is about 21 MiB (168 MiB at the 2m-row large-tier
        // ceiling); the owned tree build can briefly duplicate row slots while
        // forming leaf slabs. A lower-memory path needs an incremental
        // canonical root builder in backend-version.
        let mut row_index_builder = StableRowIndex::builder();
        let mut boundary_verifier = CanonicalSemanticPlaneBoundaryFamilyVerifier::begin_family(
            family.family(),
            family.boundary_policy(),
        );

        for (segment_index, segment) in family.segments().iter().enumerate() {
            let mut segment_jumbo_documentation_references = Vec::new();
            let payload = source
                .segment(global_segment_index, segment)
                .map_err(|error| {
                    SemanticTypedPlaneInventoryV2Error::SegmentSource(error.to_string())
                })?;
            let row_limit = family.boundary_policy().maximum_bytes();
            let (view, admitted_id) = admitted_stream_segment(
                kind,
                family.family(),
                segment_index,
                segment,
                payload,
                row_limit,
                &mut boundary_verifier,
            )?;
            if previous_segment_last.is_some_and(|previous| previous >= view.first_key()) {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentOrder {
                    family: family.family(),
                    index: segment_index,
                });
            }
            previous_segment_last = Some(view.last_key());
            observed_rows = observed_rows
                .checked_add(u64::from(view.row_count()))
                .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "row-count",
                })?;
            for record in view.records() {
                if matches!(
                    family.family(),
                    SemanticIrPlane::Documentation | SemanticIrPlane::SourceProvenance
                ) {
                    let descriptor = match family.family() {
                        SemanticIrPlane::Documentation => {
                            super::declarations::jumbo_descriptor_for_record_with_row_limit(
                                record, row_limit,
                            )?
                        }
                        SemanticIrPlane::SourceProvenance => {
                            super::source_provenance::jumbo_descriptor_for_record_with_row_limit(
                                record, row_limit,
                            )?
                        }
                        _ => None,
                    };
                    if let Some(descriptor) = descriptor {
                        let admission = jumbo_admission
                            .as_deref_mut()
                            .ok_or(SemanticPlaneRecordError::JumboObjectStoreRequired)?;
                        let reference_budget =
                            reference_scratch.remaining().min(limits.max_references);
                        if let Some(references) =
                            admission.admit(family.family(), descriptor, reference_budget)?
                        {
                            let local_count =
                                u64::try_from(references.local.len()).map_err(|_| {
                                    SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                        budget: "reference-count",
                                    }
                                })?;
                            let external_count =
                                u64::try_from(references.external.len()).map_err(|_| {
                                    SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                        budget: "reference-count",
                                    }
                                })?;
                            let added = local_count.checked_add(external_count).ok_or(
                                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                    budget: "reference-count",
                                },
                            )?;
                            jumbo_documentation_reference_count =
                                jumbo_documentation_reference_count
                                    .checked_add(added)
                                    .filter(|count| *count <= limits.max_references)
                                    .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                        budget: "reference-count",
                                    })?;
                            reference_scratch.charge_u64(added)?;
                            if family.family() == SemanticIrPlane::Documentation {
                                segment_jumbo_documentation_references
                                    .try_reserve(1)
                                    .map_err(SemanticPlaneRecordError::Allocation)?;
                                segment_jumbo_documentation_references
                                    .push((record.key(), references));
                                jumbo_documentation_row_count = jumbo_documentation_row_count
                                    .checked_add(1)
                                    .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                                        budget: "row-count",
                                    })?;
                            }
                        }
                    }
                }
                let key = StableRowKey::new(stable_row_family(family.family()), record.key());
                let row_payload = RowPayload::from_tagged_bytes(record.tag(), record.payload())?;
                row_index_builder.push(key, row_payload)?;
                match family_index {
                    1 => type_builder
                        .as_mut()
                        .ok_or(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                            fact: "missing Types stream builder",
                        })?
                        .push(record.key(), record.tag(), record.payload())
                        .map_err(map_types_family_validation_error)?,
                    6 => extension_builder
                        .as_mut()
                        .ok_or(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                            fact: "missing language-extension stream builder",
                        })?
                        .push(
                            record.key(),
                            record.tag(),
                            record.payload(),
                            types_family.as_ref().ok_or(
                                SemanticTypedPlaneInventoryV2Error::CrossFamily {
                                    fact: "missing checked Types family",
                                },
                            )?,
                        )
                        .map_err(map_extension_family_validation_error)?,
                    _ => {}
                }
            }
            match family_index {
                0 => merge_core_facts(&mut core, decode_core(&[view], &mut reference_scratch)?)?,
                2 => merge_relation_facts(
                    &mut relations,
                    decode_relations(&[view], &mut reference_scratch)?,
                )?,
                3 => merge_occurrence_facts(
                    &mut occurrences,
                    decode_occurrences(&[view], &mut reference_scratch)?,
                )?,
                4 => merge_documentation_facts(
                    &mut docs,
                    decode_documentation(
                        &[view],
                        &mut segment_jumbo_documentation_references,
                        &mut reference_scratch,
                        true,
                        family.boundary_policy().maximum_bytes(),
                    )?,
                )?,
                5 => merge_source_facts(
                    &mut source_facts,
                    decode_source_provenance(
                        &[view],
                        &mut reference_scratch,
                        family.boundary_policy().maximum_bytes(),
                    )?,
                )?,
                _ => {}
            }
            verified_ids
                .try_reserve(1)
                .map_err(SemanticPlaneRecordError::Allocation)?;
            verified_ids.push(admitted_id);
            verified_segments.push(VerifiedTypedPlaneSegmentV2 {
                first_key: *segment.first_key(),
                last_key: *segment.last_key(),
                row_count: segment.row_count(),
                byte_length: segment.byte_length(),
                admitted_id,
            });
            global_segment_index = global_segment_index.checked_add(1).ok_or(
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "segment-count",
                },
            )?;
        }
        boundary_verifier.finish(
            family.row_count(),
            u64::try_from(family.segments().len()).map_err(|_| {
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "segment-count",
                }
            })?,
        )?;
        if observed_rows != family.row_count() {
            return Err(SemanticTypedPlaneInventoryV2Error::FamilyRowCount {
                family: family.family(),
                expected: family.row_count(),
                observed: observed_rows,
            });
        }
        let row_index = row_index_builder.finish()?;
        if row_index.row_count() != observed_rows {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "semantic row-index census",
            });
        }
        let semantic_row_root =
            semantic_family_row_root(family.family(), observed_rows, row_index.root().as_bytes());
        verified_families.push(VerifiedTypedPlaneFamilyV2 {
            family: family.family(),
            segments: verified_segments.into_boxed_slice(),
            row_count: observed_rows,
            semantic_row_root,
        });
        if let Some(builder) = type_builder {
            let checked = builder.finish()?;
            reference_scratch.charge_u64(checked.reference_count())?;
            types_family = Some(checked);
        }
        if let Some(builder) = extension_builder {
            let checked = builder.finish(&core.captured_extension_owners)?;
            reference_scratch.charge_u64(checked.reference_count())?;
            extensions = Some(checked);
        }
    }
    if global_segment_index != verified_ids.len() {
        return Err(SemanticTypedPlaneInventoryV2Error::PayloadCount {
            expected: global_segment_index,
            observed: verified_ids.len(),
        });
    }
    verified_ids.sort_unstable();
    if verified_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    normalize_streamed_facts(
        &mut core,
        &mut relations,
        &mut occurrences,
        &mut docs,
        &mut source_facts,
    )?;
    let types_family = types_family.ok_or(SemanticTypedPlaneInventoryV2Error::CrossFamily {
        fact: "missing checked Types family",
    })?;
    let extensions = extensions.ok_or(SemanticTypedPlaneInventoryV2Error::CrossFamily {
        fact: "missing checked language-extension family",
    })?;
    validate_streamed_cross_family_closure(
        build.profile(),
        &core,
        &types_family,
        &relations,
        &occurrences,
        &docs,
        &source_facts,
        &extensions,
        jumbo_documentation_row_count,
        jumbo_documentation_reference_count,
        limits,
    )?;
    if matches!(image_facts.authority, SemanticImageAuthority::Shared)
        && families[6].row_count() != 0
    {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "shared image has language extension rows",
        });
    }
    let families: [VerifiedTypedPlaneFamilyV2; 7] = verified_families.try_into().map_err(|_| {
        SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "seven-family inventory conversion",
        }
    })?;
    Ok(VerifiedTypedPlaneInventoryV2 {
        build,
        image_facts,
        input_witness,
        families,
    })
}

fn verify_stream_manifest_claims(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[SemanticTypedPlaneFamilyDescriptorV2; 7],
    limits: SemanticTypedPlaneVerificationLimitsV2,
    input_claim_validation: InputClaimValidation,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    let expected_families = [
        SemanticIrPlane::Core,
        SemanticIrPlane::Types,
        SemanticIrPlane::Relations,
        SemanticIrPlane::Occurrences,
        SemanticIrPlane::Documentation,
        SemanticIrPlane::SourceProvenance,
        SemanticIrPlane::LanguageExtensions(build.profile()),
    ];
    let mut total_segments = 0_usize;
    let mut total_bytes = 0_u64;
    let mut total_rows = 0_u64;
    for (index, family) in families.iter().enumerate() {
        let expected = expected_families[index];
        if family.family() != expected {
            return Err(SemanticTypedPlaneInventoryV2Error::FamilyOrder {
                index,
                expected,
                observed: family.family(),
            });
        }
        let mut family_rows = 0_u64;
        let mut previous = None;
        for (segment_index, segment) in family.segments().iter().enumerate() {
            total_segments = total_segments.checked_add(1).ok_or(
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "segment-count",
                },
            )?;
            if total_segments > limits.max_segments
                || segment.first_key() > segment.last_key()
                || segment.row_count() == 0
                || segment.byte_length() == 0
                || segment.byte_length() > crate::ir::MAX_SEMANTIC_SEGMENT_BYTES as u64
                || previous.is_some_and(|last| last >= segment.first_key())
            {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentOrder {
                    family: family.family(),
                    index: segment_index,
                });
            }
            family_rows = family_rows
                .checked_add(u64::from(segment.row_count()))
                .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "row-count",
                })?;
            total_bytes = total_bytes.checked_add(segment.byte_length()).ok_or(
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "payload-bytes",
                },
            )?;
            previous = Some(segment.last_key());
        }
        if family_rows != family.row_count() {
            return Err(SemanticTypedPlaneInventoryV2Error::FamilyRowCount {
                family: family.family(),
                expected: family.row_count(),
                observed: family_rows,
            });
        }
        total_rows = total_rows.checked_add(family_rows).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "row-count",
            },
        )?;
    }
    if total_bytes > limits.max_total_bytes || total_rows > limits.max_total_rows {
        return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: if total_bytes > limits.max_total_bytes {
                "payload-bytes"
            } else {
                "row-count"
            },
        });
    }
    if matches!(input_claim_validation, InputClaimValidation::Complete)
        && input_witness.coverage().state() != backend_version::Coverage::Complete
    {
        return Err(SemanticTypedPlaneInventoryV2Error::IncompleteInputClaim);
    }
    validate_image_facts(build, image_facts)
}

fn admitted_stream_segment<'payload>(
    kind: SemanticPlaneKind,
    family: SemanticIrPlane,
    index: usize,
    claim: &SemanticTypedPlaneSegmentClaimV2,
    payload: &'payload [u8],
    maximum_inline_row_bytes: usize,
    boundary_verifier: &mut CanonicalSemanticPlaneBoundaryFamilyVerifier,
) -> Result<
    (
        CanonicalSemanticPlaneSegmentView<'payload>,
        SemanticSegmentId,
    ),
    SemanticTypedPlaneInventoryV2Error,
> {
    if u64::try_from(payload.len()).map_err(|_| {
        SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "payload-bytes",
        }
    })? != claim.byte_length()
    {
        return Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor { family, index });
    }
    let descriptor = SemanticPlaneSegment::from_payload(
        kind,
        *claim.first_key(),
        *claim.last_key(),
        claim.row_count(),
        payload,
    )
    .map_err(SemanticPlaneRecordError::from)?;
    let Some(admitted_id) = descriptor.admitted_id() else {
        return Err(SemanticPlaneRecordError::MissingAdmittedId.into());
    };
    if descriptor.byte_length() != claim.byte_length()
        || admitted_id.as_bytes() != claim.id_claim().as_bytes()
    {
        return Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor { family, index });
    }
    boundary_verifier.begin_segment(kind, descriptor.row_count(), payload.len())?;
    // `SemanticPlaneSegment::from_payload` computed the sole segment identity
    // hash over these exact bytes; comparing `admitted_id` with the untrusted
    // claim binds the claim to this payload before parsing. The structural
    // visitor then checks the claimed row count/key range against the grammar.
    // Re-entering the public decoder here would hash the payload a second time.
    let view = super::decode_semantic_plane_segment_structure_with_record_visitor(
        kind,
        &descriptor,
        payload,
        maximum_inline_row_bytes,
        |record| boundary_verifier.push_record(record.key(), record.encoded_len()),
    )
    .map_err(|error| match error {
        SemanticPlaneRecordError::StableKeyMismatchAt { key } => {
            SemanticTypedPlaneInventoryV2Error::RowIdentityMismatch { family, index, key }
        }
        error => error.into(),
    })?;
    boundary_verifier.finish_segment()?;
    Ok((view, admitted_id))
}

fn append_owned<T>(
    destination: &mut Vec<T>,
    source: &mut Vec<T>,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    destination
        .try_reserve(source.len())
        .map_err(SemanticPlaneRecordError::Allocation)?;
    destination.append(source);
    Ok(())
}

fn merge_core_facts(
    destination: &mut CoreFamilyFacts,
    mut source: CoreFamilyFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    append_owned(&mut destination.declarations, &mut source.declarations)?;
    append_owned(
        &mut destination.local_references,
        &mut source.local_references,
    )?;
    append_owned(
        &mut destination.captured_extension_owners,
        &mut source.captured_extension_owners,
    )?;
    Ok(())
}

fn merge_relation_facts(
    destination: &mut RelationFamilyFacts,
    mut source: RelationFamilyFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    append_owned(&mut destination.keys, &mut source.keys)?;
    append_owned(&mut destination.references, &mut source.references)?;
    append_owned(
        &mut destination.external_references,
        &mut source.external_references,
    )?;
    Ok(())
}

fn merge_occurrence_facts(
    destination: &mut OccurrenceFamilyFacts,
    mut source: OccurrenceFamilyFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    append_owned(
        &mut destination.relation_references,
        &mut source.relation_references,
    )?;
    Ok(())
}

fn merge_documentation_facts(
    destination: &mut DocumentationFamilyFacts,
    mut source: DocumentationFamilyFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    append_owned(&mut destination.declarations, &mut source.declarations)?;
    append_owned(
        &mut destination.local_references,
        &mut source.local_references,
    )?;
    append_owned(
        &mut destination.external_references,
        &mut source.external_references,
    )?;
    append_owned(
        &mut destination.jumbo_references,
        &mut source.jumbo_references,
    )?;
    destination.reference_count = destination
        .reference_count
        .checked_add(source.reference_count)
        .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "reference-count",
        })?;
    destination.jumbo_rows = destination
        .jumbo_rows
        .checked_add(source.jumbo_rows)
        .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "row-count",
        })?;
    Ok(())
}

fn merge_source_facts(
    destination: &mut SourceFamilyFacts,
    mut source: SourceFamilyFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    append_owned(
        &mut destination.declaration_keys,
        &mut source.declaration_keys,
    )?;
    append_owned(
        &mut destination.declaration_source,
        &mut source.declaration_source,
    )?;
    append_owned(&mut destination.relation_keys, &mut source.relation_keys)?;
    Ok(())
}

fn normalize_streamed_facts(
    core: &mut CoreFamilyFacts,
    relations: &mut RelationFamilyFacts,
    occurrences: &mut OccurrenceFamilyFacts,
    docs: &mut DocumentationFamilyFacts,
    source: &mut SourceFamilyFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    core.declarations.sort_unstable_by_key(|row| row.identity);
    if core
        .declarations
        .windows(2)
        .any(|pair| pair[0].identity == pair[1].identity)
    {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    core.local_references.sort_unstable();
    core.local_references.dedup();
    core.captured_extension_owners.sort_unstable();

    relations.keys.sort_unstable();
    reject_duplicates(&relations.keys)?;
    relations.references.sort_unstable();
    relations.references.dedup();
    relations.external_references.sort_unstable();
    relations.external_references.dedup();
    relations.reference_count = reference_count_for_items(
        relations
            .references
            .len()
            .checked_add(relations.external_references.len())
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            })?,
    )?;

    occurrences.relation_references.sort_unstable();
    occurrences.relation_references.dedup();
    occurrences.reference_count = reference_count_for_items(occurrences.relation_references.len())?;

    docs.declarations
        .sort_unstable_by_key(|(identity, _)| *identity);
    if docs
        .declarations
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    if docs.local_references.len() > 1 {
        docs.local_references.sort_unstable();
    }
    if docs.external_references.len() > 1 {
        docs.external_references.sort_unstable();
    }

    source.declaration_keys.sort_unstable();
    source.relation_keys.sort_unstable();
    source
        .declaration_source
        .sort_unstable_by_key(|(identity, _)| *identity);
    reject_duplicates(&source.declaration_keys)?;
    reject_duplicates(&source.relation_keys)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_streamed_cross_family_closure(
    profile: LanguageProfile,
    core: &CoreFamilyFacts,
    types: &CheckedTypesFamilyV2,
    relations: &RelationFamilyFacts,
    occurrences: &OccurrenceFamilyFacts,
    docs: &DocumentationFamilyFacts,
    source: &SourceFamilyFacts,
    extensions: &CheckedLanguageExtensionFamilyV2,
    jumbo_documentation_row_count: usize,
    jumbo_documentation_reference_count: u64,
    limits: SemanticTypedPlaneVerificationLimitsV2,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    types.verify_reachable_closure(extensions.types_references())?;
    if docs.jumbo_rows != jumbo_documentation_row_count {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "jumbo documentation closure census",
        });
    }
    require_core_keys(
        &core.declarations,
        &source.declaration_keys,
        "declaration source census",
    )?;
    require_documentation_keys(&core.declarations, &docs.declarations)?;
    require_core_keys(
        &core.declarations,
        types.root_identities(),
        "Types entity root census",
    )?;
    if core.captured_extension_owners.as_slice() != extensions.owner_identities() {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "language extension owner census",
        });
    }
    if relations.keys != source.relation_keys {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "relation source census",
        });
    }
    for row in &core.declarations {
        let source_fact = find_availability(&source.declaration_source, row.identity).ok_or(
            SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "missing declaration source row",
            },
        )?;
        if source_fact != row.source || row.source != row.source_file {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "declaration source availability",
            });
        }
        let docs_fact = find_availability(&docs.declarations, row.identity).ok_or(
            SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "missing documentation row",
            },
        )?;
        if docs_fact != row.documentation {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "documentation availability",
            });
        }
    }
    let root_presence = types.root_type_presence();
    if root_presence.len() != core.declarations.len() {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "Types root availability census",
        });
    }
    for (row, (identity, present)) in core.declarations.iter().zip(root_presence) {
        if row.identity != *identity || row.semantic_type != *present {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "semantic type availability",
            });
        }
    }
    require_core_refs(
        &core.local_references,
        &core.declarations,
        "core parent/member reference",
    )?;
    require_core_refs(
        &relations.references,
        &core.declarations,
        "relation declaration reference",
    )?;
    require_core_refs(
        types.declaration_references(),
        &core.declarations,
        "Types declaration reference",
    )?;
    require_core_refs(
        extensions.declaration_references(),
        &core.declarations,
        "language extension declaration reference",
    )?;
    require_core_refs_iter(
        docs.local_reference_iter(),
        &core.declarations,
        "documentation local link",
    )?;
    let external_keys = types.external_target_keys();
    require_local_refs(
        &relations.external_references,
        external_keys,
        "relation external target",
    )?;
    require_local_refs_iter(
        docs.external_reference_iter(),
        external_keys,
        "documentation external link",
    )?;
    require_local_refs(
        &occurrences.relation_references,
        &relations.keys,
        "occurrence relation reference",
    )?;
    if extensions.profile() != profile {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "language extension profile",
        });
    }
    let source_reference_count = reference_count_for_items(
        source
            .declaration_keys
            .len()
            .checked_add(source.relation_keys.len())
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            })?,
    )?;
    let core_reference_count = reference_count_for_items(core.local_references.len())?;
    let reference_count = [
        core_reference_count,
        relations.reference_count,
        occurrences.reference_count,
        docs.reference_count,
        source_reference_count,
        types.reference_count(),
        extensions.reference_count(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, family_count| total.checked_add(family_count))
    .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
        budget: "reference-count",
    })?;
    if reference_count > limits.max_references
        || jumbo_documentation_reference_count > limits.max_references
    {
        return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "reference-count",
        });
    }
    Ok(())
}

fn map_jumbo_source_error<E: core::fmt::Display>(
    error: crate::ir::JumboOperationError<E>,
) -> SemanticPlaneRecordError {
    match error {
        crate::ir::JumboOperationError::Rope(error) => SemanticPlaneRecordError::JumboRope(error),
        crate::ir::JumboOperationError::Store(error) => {
            SemanticPlaneRecordError::JumboObjectStore(error.to_string())
        }
        crate::ir::JumboOperationError::Input(_) | crate::ir::JumboOperationError::Output(_) => {
            SemanticPlaneRecordError::JumboStream
        }
    }
}

fn map_types_family_validation_error(
    error: TypesFamilyValidationError,
) -> SemanticTypedPlaneInventoryV2Error {
    match error {
        TypesFamilyValidationError::Record(error) => error.into(),
        TypesFamilyValidationError::ReferenceLimitExceeded => {
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            }
        }
    }
}

fn map_extension_family_validation_error(
    error: LanguageExtensionFamilyValidationError,
) -> SemanticTypedPlaneInventoryV2Error {
    match error {
        LanguageExtensionFamilyValidationError::Record(error) => error.into(),
        LanguageExtensionFamilyValidationError::ReferenceLimitExceeded => {
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            }
        }
    }
}

fn verify_manifest_claims(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
    input_witness: SemanticInputWitness,
    families: &[TypedPlaneFamilyPayloadsV2<'_>; 7],
    ordered_payloads: &[&[u8]],
    limits: SemanticTypedPlaneVerificationLimitsV2,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    let expected_families = [
        SemanticIrPlane::Core,
        SemanticIrPlane::Types,
        SemanticIrPlane::Relations,
        SemanticIrPlane::Occurrences,
        SemanticIrPlane::Documentation,
        SemanticIrPlane::SourceProvenance,
        SemanticIrPlane::LanguageExtensions(build.profile()),
    ];
    let mut expected_payloads = 0_usize;
    let mut total_bytes = 0_u64;
    let mut total_rows = 0_u64;
    for (index, family) in families.iter().enumerate() {
        let expected = expected_families[index];
        if family.family != expected {
            return Err(SemanticTypedPlaneInventoryV2Error::FamilyOrder {
                index,
                expected,
                observed: family.family,
            });
        }
        expected_payloads = expected_payloads.checked_add(family.segments.len()).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "segment-count",
            },
        )?;
        if expected_payloads > limits.max_segments {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "segment-count",
            });
        }
        let mut family_rows = 0_u64;
        for (segment_index, segment) in family.segments.iter().enumerate() {
            if segment.first_key > segment.last_key {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentOrder {
                    family: family.family,
                    index: segment_index,
                });
            }
            if let Some(previous) = segment_index
                .checked_sub(1)
                .and_then(|previous| family.segments.get(previous))
                && previous.last_key >= segment.first_key
            {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentOrder {
                    family: family.family,
                    index: segment_index,
                });
            }
            if segment.row_count == 0
                || segment.byte_length == 0
                || segment.byte_length
                    > u64::try_from(crate::ir::MAX_SEMANTIC_SEGMENT_BYTES).map_err(|_| {
                        SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                            budget: "segment-bytes",
                        }
                    })?
            {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor {
                    family: family.family,
                    index: segment_index,
                });
            }
            let payload = ordered_payloads
                .get(expected_payloads - family.segments.len() + segment_index)
                .copied()
                .ok_or(SemanticTypedPlaneInventoryV2Error::PayloadCount {
                    expected: expected_payloads,
                    observed: ordered_payloads.len(),
                })?;
            if u64::try_from(payload.len()).map_err(|_| {
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "payload-bytes",
                }
            })? != segment.byte_length
            {
                return Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor {
                    family: family.family,
                    index: segment_index,
                });
            }
            total_bytes = total_bytes.checked_add(segment.byte_length).ok_or(
                SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "payload-bytes",
                },
            )?;
            family_rows = family_rows
                .checked_add(u64::from(segment.row_count))
                .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "row-count",
                })?;
        }
        if family_rows != family.row_count {
            return Err(SemanticTypedPlaneInventoryV2Error::FamilyRowCount {
                family: family.family,
                expected: family.row_count,
                observed: family_rows,
            });
        }
        total_rows = total_rows.checked_add(family_rows).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "row-count",
            },
        )?;
    }
    if expected_payloads != ordered_payloads.len() {
        return Err(SemanticTypedPlaneInventoryV2Error::PayloadCount {
            expected: expected_payloads,
            observed: ordered_payloads.len(),
        });
    }
    if total_bytes > limits.max_total_bytes {
        return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "payload-bytes",
        });
    }
    if total_rows > limits.max_total_rows {
        return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "row-count",
        });
    }
    if input_witness.coverage().state() != backend_version::Coverage::Complete {
        return Err(SemanticTypedPlaneInventoryV2Error::IncompleteInputClaim);
    }
    validate_image_facts(build, image_facts)?;
    Ok(())
}

fn validate_image_facts(
    build: SemanticBuildIdentity,
    image_facts: SemanticImageFacts,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    if let SemanticImageAuthority::Language(profile) = image_facts.authority
        && profile != build.profile()
    {
        return Err(
            SemanticTypedPlaneInventoryV2Error::ImageFactsBuildMismatch {
                field: "language authority profile",
            },
        );
    }
    if let ImageProvenance::Captured { source, recipe, .. } = image_facts.provenance {
        if recipe.profile != build.profile() {
            return Err(
                SemanticTypedPlaneInventoryV2Error::ImageFactsBuildMismatch {
                    field: "recipe profile",
                },
            );
        }
        if recipe.stage != build.stage() {
            return Err(
                SemanticTypedPlaneInventoryV2Error::ImageFactsBuildMismatch {
                    field: "recipe stage",
                },
            );
        }
        if recipe.identity.as_ref() != build.recipe() {
            return Err(
                SemanticTypedPlaneInventoryV2Error::ImageFactsBuildMismatch {
                    field: "recipe identity",
                },
            );
        }
        if recipe.toolchain.as_ref() != build.toolchain() {
            return Err(
                SemanticTypedPlaneInventoryV2Error::ImageFactsBuildMismatch {
                    field: "recipe toolchain",
                },
            );
        }
        let expected = CompileRecipeFact::derive(
            recipe.profile,
            recipe.stage,
            recipe.tool,
            source.identity,
            recipe.toolchain,
        );
        if recipe.identity != expected.identity {
            return Err(
                SemanticTypedPlaneInventoryV2Error::ImageFactsBuildMismatch {
                    field: "source-bound recipe identity",
                },
            );
        }
    }
    Ok(())
}

fn normalize_input_claim(
    input_witness: SemanticInputWitness,
) -> Result<SemanticInputWitness, SemanticTypedPlaneInventoryV2Error> {
    if input_witness.coverage().state() != backend_version::Coverage::Complete {
        return Err(SemanticTypedPlaneInventoryV2Error::IncompleteInputClaim);
    }
    Ok(SemanticInputWitness::claimed(
        *input_witness.input_root(),
        input_witness.read_manifest_root(),
    ))
}

fn normalize_input_claim_for_validation(
    input_witness: SemanticInputWitness,
    validation: InputClaimValidation,
) -> Result<SemanticInputWitness, SemanticTypedPlaneInventoryV2Error> {
    if matches!(validation, InputClaimValidation::Complete)
        && input_witness.coverage().state() != backend_version::Coverage::Complete
    {
        return Err(SemanticTypedPlaneInventoryV2Error::IncompleteInputClaim);
    }
    Ok(SemanticInputWitness::claimed_state(
        *input_witness.input_root(),
        input_witness.read_manifest_root(),
        input_witness.coverage().state(),
    ))
}

#[derive(Clone, Copy)]
struct CoreDeclarationFacts {
    identity: [u8; 32],
    source: bool,
    source_file: bool,
    semantic_type: bool,
    documentation: bool,
}

#[derive(Default)]
struct CoreFamilyFacts {
    declarations: Vec<CoreDeclarationFacts>,
    local_references: Vec<[u8; 32]>,
    captured_extension_owners: Vec<[u8; 32]>,
}

#[derive(Default)]
struct SourceFamilyFacts {
    declaration_keys: Vec<[u8; 32]>,
    declaration_source: Vec<([u8; 32], bool)>,
    relation_keys: Vec<[u8; 32]>,
}

#[derive(Default)]
struct RelationFamilyFacts {
    keys: Vec<[u8; 32]>,
    references: Vec<[u8; 32]>,
    external_references: Vec<[u8; 32]>,
    reference_count: u64,
}

#[derive(Default)]
struct DocumentationFamilyFacts {
    declarations: Vec<([u8; 32], bool)>,
    local_references: Vec<[u8; 32]>,
    external_references: Vec<[u8; 32]>,
    jumbo_references: Vec<super::declarations::DocsWireReferences>,
    reference_count: u64,
    jumbo_rows: usize,
}

impl DocumentationFamilyFacts {
    fn local_reference_iter<'a>(&'a self) -> impl Iterator<Item = &'a [u8; 32]> + 'a {
        self.local_references.iter().chain(
            self.jumbo_references
                .iter()
                .flat_map(|references| references.local.iter()),
        )
    }

    fn external_reference_iter<'a>(&'a self) -> impl Iterator<Item = &'a [u8; 32]> + 'a {
        self.external_references.iter().chain(
            self.jumbo_references
                .iter()
                .flat_map(|references| references.external.iter()),
        )
    }

    fn jumbo_reference_count(&self) -> Result<usize, SemanticTypedPlaneInventoryV2Error> {
        self.jumbo_references
            .iter()
            .try_fold(0_usize, |count, references| {
                count
                    .checked_add(references.local.len())
                    .and_then(|count| count.checked_add(references.external.len()))
                    .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                        budget: "reference-count",
                    })
            })
    }
}

#[derive(Default)]
struct OccurrenceFamilyFacts {
    relation_references: Vec<[u8; 32]>,
    reference_count: u64,
}

struct AggregateReferenceScratchV2 {
    // The semantic allowance is shared exactly across all seven families.
    // Scratch also bounds duplicated cross-family fact views. Jumbo Docs
    // references move into Docs facts and are charged once.
    maximum_scratch: u64,
    charged_scratch: u64,
    maximum_semantic: u64,
    charged_semantic: u64,
}

impl AggregateReferenceScratchV2 {
    fn new(
        semantic_reference_limit: u64,
        already_retained: u64,
    ) -> Result<Self, SemanticTypedPlaneInventoryV2Error> {
        let maximum_scratch = semantic_reference_limit.checked_mul(2).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-scratch",
            },
        )?;
        if already_retained > maximum_scratch || already_retained > semantic_reference_limit {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-scratch",
            });
        }
        Ok(Self {
            maximum_scratch,
            charged_scratch: already_retained,
            maximum_semantic: semantic_reference_limit,
            charged_semantic: already_retained,
        })
    }

    fn remaining(&self) -> u64 {
        self.maximum_scratch
            .saturating_sub(self.charged_scratch)
            .min(self.maximum_semantic.saturating_sub(self.charged_semantic))
    }

    fn charge(&mut self, count: usize) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
        self.charge_u64(u64::try_from(count).map_err(|_| {
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-scratch",
            }
        })?)
    }

    fn charge_u64(&mut self, count: u64) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
        self.charge_scratch_u64(count)?;
        let charged = self.charged_semantic.checked_add(count).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-scratch",
            },
        )?;
        if charged > self.maximum_semantic {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            });
        }
        self.charged_semantic = charged;
        Ok(())
    }

    fn charge_scratch_u64(&mut self, count: u64) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
        let charged = self.charged_scratch.checked_add(count).ok_or(
            SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-scratch",
            },
        )?;
        if charged > self.maximum_scratch {
            return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-scratch",
            });
        }
        self.charged_scratch = charged;
        Ok(())
    }

    fn try_push<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
    ) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
        self.charge(1)?;
        values
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        values.push(value);
        Ok(())
    }

    fn try_push_scratch<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
    ) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
        self.charge_scratch_u64(1)?;
        values
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        values.push(value);
        Ok(())
    }
}

fn validate_cross_family_closure(
    profile: LanguageProfile,
    families: &[Vec<CanonicalSemanticPlaneSegmentView<'_>>; 7],
    limits: SemanticTypedPlaneVerificationLimitsV2,
    family_row_limits: [usize; 7],
    jumbo_documentation_references: &mut [([u8; 32], super::declarations::DocsWireReferences)],
    jumbo_documentation_reference_count: u64,
) -> Result<u64, SemanticTypedPlaneInventoryV2Error> {
    let mut reference_scratch = AggregateReferenceScratchV2::new(
        limits.max_references,
        jumbo_documentation_reference_count,
    )?;
    let mut core = decode_core(&families[0], &mut reference_scratch)?;
    core.declarations.sort_unstable_by_key(|row| row.identity);
    if core
        .declarations
        .windows(2)
        .any(|pair| pair[0].identity == pair[1].identity)
    {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    let core_reference_count = reference_count_for_items(core.local_references.len())?;
    core.local_references.sort_unstable();
    core.local_references.dedup();
    core.captured_extension_owners.sort_unstable();

    let family_limits = TypesFamilyVerificationLimitsV2::bounded(
        limits.max_total_bytes,
        limits.max_total_rows,
        reference_scratch.remaining().min(limits.max_references),
    );
    let types =
        validate_types_family_v2_with_limits_detailed(families[1].iter().copied(), family_limits)
            .map_err(map_types_family_validation_error)?;
    reference_scratch.charge_u64(types.reference_count())?;
    let relations = decode_relations(&families[2], &mut reference_scratch)?;
    let occurrences = decode_occurrences(&families[3], &mut reference_scratch)?;
    let docs = decode_documentation(
        &families[4],
        jumbo_documentation_references,
        &mut reference_scratch,
        true,
        family_row_limits[4],
    )?;
    let source =
        decode_source_provenance(&families[5], &mut reference_scratch, family_row_limits[5])?;
    let extension_limits = LanguageExtensionVerificationLimitsV2::bounded(
        limits.max_total_bytes,
        limits.max_total_rows,
        reference_scratch.remaining().min(limits.max_references),
    );
    let extensions = validate_language_extension_family_v2_with_limits_detailed(
        profile,
        families[6].iter().copied(),
        &types,
        &core.captured_extension_owners,
        extension_limits,
    )
    .map_err(map_extension_family_validation_error)?;
    reference_scratch.charge_u64(extensions.reference_count())?;
    types.verify_reachable_closure(extensions.types_references())?;

    require_core_keys(
        &core.declarations,
        &source.declaration_keys,
        "declaration source census",
    )?;
    require_documentation_keys(&core.declarations, &docs.declarations)?;
    require_core_keys(
        &core.declarations,
        types.root_identities(),
        "Types entity root census",
    )?;
    if core.captured_extension_owners.as_slice() != extensions.owner_identities() {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "language extension owner census",
        });
    }
    if relations.keys != source.relation_keys {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "relation source census",
        });
    }

    for row in &core.declarations {
        let source_fact = find_availability(&source.declaration_source, row.identity).ok_or(
            SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "missing declaration source row",
            },
        )?;
        if source_fact != row.source || row.source != row.source_file {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "declaration source availability",
            });
        }
        let docs_fact = find_availability(&docs.declarations, row.identity).ok_or(
            SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "missing documentation row",
            },
        )?;
        if docs_fact != row.documentation {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "documentation availability",
            });
        }
    }
    let root_presence = types.root_type_presence();
    if root_presence.len() != core.declarations.len() {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "Types root availability census",
        });
    }
    for (row, (identity, present)) in core.declarations.iter().zip(root_presence) {
        if row.identity != *identity || row.semantic_type != *present {
            return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "semantic type availability",
            });
        }
    }

    require_core_refs(
        &core.local_references,
        &core.declarations,
        "core parent/member reference",
    )?;
    require_core_refs(
        &relations.references,
        &core.declarations,
        "relation declaration reference",
    )?;
    require_core_refs(
        types.declaration_references(),
        &core.declarations,
        "Types declaration reference",
    )?;
    require_core_refs(
        extensions.declaration_references(),
        &core.declarations,
        "language extension declaration reference",
    )?;
    require_core_refs_iter(
        docs.local_reference_iter(),
        &core.declarations,
        "documentation local link",
    )?;

    let external_keys = types.external_target_keys();
    require_local_refs(
        &relations.external_references,
        external_keys,
        "relation external target",
    )?;
    require_local_refs_iter(
        docs.external_reference_iter(),
        external_keys,
        "documentation external link",
    )?;

    require_local_refs(
        &occurrences.relation_references,
        &relations.keys,
        "occurrence relation reference",
    )?;

    if extensions.profile() != profile {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "language extension profile",
        });
    }
    let source_reference_count = reference_count_for_items(
        source
            .declaration_keys
            .len()
            .checked_add(source.relation_keys.len())
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            })?,
    )?;
    let reference_count = [
        core_reference_count,
        relations.reference_count,
        occurrences.reference_count,
        docs.reference_count,
        source_reference_count,
        types.reference_count(),
        extensions.reference_count(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, family_count| total.checked_add(family_count))
    .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
        budget: "reference-count",
    })?;
    if reference_count > limits.max_references {
        return Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
            budget: "reference-count",
        });
    }
    Ok(reference_count)
}

fn decode_core(
    segments: &[CanonicalSemanticPlaneSegmentView<'_>],
    reference_scratch: &mut AggregateReferenceScratchV2,
) -> Result<CoreFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = CoreFamilyFacts::default();
    for (segment_index, segment) in segments.iter().enumerate() {
        for record in segment.records() {
            if record.tag() != CORE_DECLARATION_TAG {
                return Err(SemanticPlaneRecordError::RowGrammar.into());
            }
            let mut cursor = Cursor::new(record.payload());
            let identity = identity_key(read_identity(&mut cursor)?);
            let _name = cursor.bytes32()?;
            let _kind = cursor.u16()?;
            let visibility = cursor.u8()?;
            if visibility > 4 {
                return Err(SemanticPlaneRecordError::RowGrammar.into());
            }
            match cursor.u8()? {
                0 => {}
                1 => reference_scratch.try_push(
                    &mut facts.local_references,
                    identity_key(read_identity(&mut cursor)?),
                )?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
            match cursor.u8()? {
                0 | 1 => {}
                2 => reference_scratch.try_push(
                    &mut facts.local_references,
                    identity_key(read_identity(&mut cursor)?),
                )?,
                3 => {
                    let _ = cursor.take(16)?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
            let mut availability = [false; 8];
            for value in &mut availability {
                *value = match cursor.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
                };
            }
            let members = cursor.u32()?;
            for _ in 0..members {
                reference_scratch.try_push(
                    &mut facts.local_references,
                    identity_key(read_identity(&mut cursor)?),
                )?;
            }
            let attributes = cursor.u32()?;
            for _ in 0..attributes {
                let _ = cursor.bytes32()?;
            }
            if !cursor.is_empty() || identity != record.key() {
                return Err(SemanticTypedPlaneInventoryV2Error::RowIdentityMismatch {
                    family: SemanticIrPlane::Core,
                    index: segment_index,
                    key: record.key(),
                });
            }
            if availability[7] {
                try_push(&mut facts.captured_extension_owners, identity)?;
            }
            try_push(
                &mut facts.declarations,
                CoreDeclarationFacts {
                    identity,
                    source: availability[0],
                    source_file: availability[1],
                    semantic_type: availability[3],
                    documentation: availability[4],
                },
            )?;
        }
    }
    Ok(facts)
}

fn decode_documentation(
    segments: &[CanonicalSemanticPlaneSegmentView<'_>],
    jumbo_references: &mut [([u8; 32], super::declarations::DocsWireReferences)],
    reference_scratch: &mut AggregateReferenceScratchV2,
    require_complete_jumbo_set: bool,
    maximum_inline_row_bytes: usize,
) -> Result<DocumentationFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = DocumentationFamilyFacts::default();
    let mut observed_jumbo_rows = 0_usize;
    for (segment_index, segment) in segments.iter().enumerate() {
        for record in segment.records() {
            let mut cursor = Cursor::new(record.payload());
            let mut jumbo_reference_index = None;
            let identity = identity_key(read_identity(&mut cursor)?);
            let available = read_availability(&mut cursor)?;
            match record.tag() {
                DOCUMENTATION_TAG => {
                    let fragments = cursor.u32()?;
                    for _ in 0..fragments {
                        match cursor.u8()? {
                            0 | 1 => {
                                let _ = cursor.utf8()?;
                            }
                            2 => {
                                let _ = cursor.utf8()?;
                                match cursor.u8()? {
                                    0 => reference_scratch.try_push(
                                        &mut facts.local_references,
                                        identity_key(read_identity(&mut cursor)?),
                                    )?,
                                    1 => reference_scratch.try_push(
                                        &mut facts.external_references,
                                        cursor
                                            .take(32)?
                                            .try_into()
                                            .map_err(|_| SemanticPlaneRecordError::Truncated)?,
                                    )?,
                                    _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
                                }
                            }
                            3 | 4 => {}
                            _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
                        }
                    }
                }
                super::declarations::DOCS_JUMBO_TAG => {
                    let descriptor =
                        super::declarations::jumbo_descriptor_for_record_with_row_limit(
                            record,
                            maximum_inline_row_bytes,
                        )?;
                    if descriptor.is_none() {
                        return Err(SemanticPlaneRecordError::RowGrammar.into());
                    }
                    let _ = cursor.take(crate::ir::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES)?;
                    let index = jumbo_references
                        .binary_search_by_key(&identity, |(owner, _)| *owner)
                        .map_err(|_| SemanticTypedPlaneInventoryV2Error::CrossFamily {
                            fact: "jumbo documentation closure census",
                        })?;
                    jumbo_reference_index = Some(index);
                    observed_jumbo_rows = observed_jumbo_rows.checked_add(1).ok_or(
                        SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                            budget: "row-count",
                        },
                    )?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
            if !cursor.is_empty() || identity != record.key() {
                return Err(SemanticTypedPlaneInventoryV2Error::RowIdentityMismatch {
                    family: SemanticIrPlane::Documentation,
                    index: segment_index,
                    key: record.key(),
                });
            }
            if let Some(index) = jumbo_reference_index {
                let Some((_, references)) = jumbo_references.get_mut(index) else {
                    return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                        fact: "jumbo documentation closure census",
                    });
                };
                facts
                    .jumbo_references
                    .try_reserve(1)
                    .map_err(SemanticPlaneRecordError::Allocation)?;
                facts.jumbo_references.push(core::mem::replace(
                    references,
                    super::declarations::DocsWireReferences {
                        local: Vec::new(),
                        external: Vec::new(),
                    },
                ));
            }
            reference_scratch.try_push(&mut facts.declarations, (identity, available))?;
        }
    }
    if require_complete_jumbo_set && observed_jumbo_rows != jumbo_references.len() {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "jumbo documentation closure census",
        });
    }
    facts.jumbo_rows = observed_jumbo_rows;
    let jumbo_reference_count = facts.jumbo_reference_count()?;
    facts.reference_count = reference_count_for_items(
        facts
            .declarations
            .len()
            .checked_add(facts.local_references.len())
            .and_then(|count| count.checked_add(facts.external_references.len()))
            .and_then(|count| count.checked_add(jumbo_reference_count))
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            })?,
    )?;
    facts
        .declarations
        .sort_unstable_by_key(|(identity, _)| *identity);
    if facts
        .declarations
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0)
    {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    Ok(facts)
}

fn decode_source_provenance(
    segments: &[CanonicalSemanticPlaneSegmentView<'_>],
    reference_scratch: &mut AggregateReferenceScratchV2,
    maximum_inline_row_bytes: usize,
) -> Result<SourceFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = SourceFamilyFacts::default();
    for (segment_index, segment) in segments.iter().enumerate() {
        for record in segment.records() {
            let mut cursor = Cursor::new(record.payload());
            match record.tag() {
                SOURCE_DECLARATION_TAG | super::source_provenance::DECLARATION_SOURCE_JUMBO_TAG => {
                    let identity = identity_key(read_identity(&mut cursor)?);
                    let available = read_availability(&mut cursor)?;
                    if record.tag() == super::source_provenance::DECLARATION_SOURCE_JUMBO_TAG {
                        let descriptor =
                            super::source_provenance::jumbo_descriptor_for_record_with_row_limit(
                                record,
                                maximum_inline_row_bytes,
                            )?;
                        if descriptor.is_none() {
                            return Err(SemanticPlaneRecordError::RowGrammar.into());
                        }
                        let _ = cursor.take(crate::ir::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES)?;
                        let start = cursor.u32()?;
                        let end = cursor.u32()?;
                        if start > end {
                            return Err(SemanticPlaneRecordError::RowGrammar.into());
                        }
                    } else if available {
                        let _source_file = cursor.bytes32()?;
                        let start = cursor.u32()?;
                        let end = cursor.u32()?;
                        if start > end {
                            return Err(SemanticPlaneRecordError::RowGrammar.into());
                        }
                    }
                    if !cursor.is_empty() || identity != record.key() {
                        return Err(SemanticTypedPlaneInventoryV2Error::RowIdentityMismatch {
                            family: SemanticIrPlane::SourceProvenance,
                            index: segment_index,
                            key: record.key(),
                        });
                    }
                    reference_scratch.try_push(&mut facts.declaration_keys, identity)?;
                    reference_scratch
                        .try_push_scratch(&mut facts.declaration_source, (identity, available))?;
                }
                SOURCE_RELATION_TAG | super::source_provenance::RELATION_SOURCE_JUMBO_TAG => {
                    let relation: [u8; 32] = cursor
                        .take(32)?
                        .try_into()
                        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
                    let available = read_availability(&mut cursor)?;
                    if available {
                        if record.tag() == super::source_provenance::RELATION_SOURCE_JUMBO_TAG {
                            let descriptor = super::source_provenance::
                                jumbo_descriptor_for_record_with_row_limit(
                                    record,
                                    maximum_inline_row_bytes,
                                )?;
                            if descriptor.is_none() {
                                return Err(SemanticPlaneRecordError::RowGrammar.into());
                            }
                            let _ = cursor.take(crate::ir::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES)?;
                        } else {
                            let _ = cursor.bytes32()?;
                        }
                        let start = cursor.u32()?;
                        let end = cursor.u32()?;
                        if start > end {
                            return Err(SemanticPlaneRecordError::RowGrammar.into());
                        }
                    }
                    if !cursor.is_empty() || relation_source_row_key(relation) != record.key() {
                        return Err(SemanticTypedPlaneInventoryV2Error::RowIdentityMismatch {
                            family: SemanticIrPlane::SourceProvenance,
                            index: segment_index,
                            key: record.key(),
                        });
                    }
                    reference_scratch.try_push(&mut facts.relation_keys, relation)?;
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
        }
    }
    facts.declaration_keys.sort_unstable();
    facts.relation_keys.sort_unstable();
    facts
        .declaration_source
        .sort_unstable_by_key(|(key, _)| *key);
    reject_duplicates(&facts.declaration_keys)?;
    reject_duplicates(&facts.relation_keys)?;
    Ok(facts)
}

fn decode_relations(
    segments: &[CanonicalSemanticPlaneSegmentView<'_>],
    reference_scratch: &mut AggregateReferenceScratchV2,
) -> Result<RelationFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = RelationFamilyFacts::default();
    for segment in segments {
        for record in segment.records() {
            if record.tag() != RELATION_TAG {
                return Err(SemanticPlaneRecordError::RowGrammar.into());
            }
            let payload = record.payload();
            if payload.len() != 67 {
                return Err(SemanticPlaneRecordError::RowGrammar.into());
            }
            let from: [u8; 32] = payload[..32]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?;
            let target_kind = payload[32];
            let target: [u8; 32] = payload[33..65]
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?;
            reference_scratch.try_push(&mut facts.references, from)?;
            match target_kind {
                0 => reference_scratch.try_push(&mut facts.references, target)?,
                1 => reference_scratch.try_push(&mut facts.external_references, target)?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
            reference_scratch.try_push_scratch(&mut facts.keys, record.key())?;
        }
    }
    facts.reference_count = reference_count_for_items(
        facts
            .references
            .len()
            .checked_add(facts.external_references.len())
            .ok_or(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count",
            })?,
    )?;
    facts.keys.sort_unstable();
    facts.references.sort_unstable();
    facts.references.dedup();
    facts.external_references.sort_unstable();
    facts.external_references.dedup();
    reject_duplicates(&facts.keys)?;
    Ok(facts)
}

fn decode_occurrences(
    segments: &[CanonicalSemanticPlaneSegmentView<'_>],
    reference_scratch: &mut AggregateReferenceScratchV2,
) -> Result<OccurrenceFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut references = Vec::new();
    for segment in segments {
        for record in segment.records() {
            if record.tag() != OCCURRENCE_TAG {
                return Err(SemanticPlaneRecordError::RowGrammar.into());
            }
            let relation: [u8; 32] = record
                .payload()
                .get(..32)
                .ok_or(SemanticPlaneRecordError::Truncated)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?;
            reference_scratch.try_push(&mut references, relation)?;
        }
    }
    let reference_count = reference_count_for_items(references.len())?;
    references.sort_unstable();
    references.dedup();
    Ok(OccurrenceFamilyFacts {
        relation_references: references,
        reference_count,
    })
}

fn reference_count_for_items(count: usize) -> Result<u64, SemanticTypedPlaneInventoryV2Error> {
    u64::try_from(count).map_err(|_| SemanticTypedPlaneInventoryV2Error::AggregateBudget {
        budget: "reference-count",
    })
}

fn find_availability(rows: &[([u8; 32], bool)], key: [u8; 32]) -> Option<bool> {
    rows.binary_search_by_key(&key, |(identity, _)| *identity)
        .ok()
        .and_then(|index| rows.get(index).map(|(_, available)| *available))
}

fn require_core_keys(
    expected: &[CoreDeclarationFacts],
    observed: &[[u8; 32]],
    fact: &'static str,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    if expected.len() != observed.len()
        || expected
            .iter()
            .zip(observed)
            .any(|(expected, observed)| expected.identity != *observed)
        || observed.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily { fact });
    }
    Ok(())
}

fn require_documentation_keys(
    expected: &[CoreDeclarationFacts],
    observed: &[([u8; 32], bool)],
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    if expected.len() != observed.len()
        || expected
            .iter()
            .zip(observed)
            .any(|(expected, (observed, _))| expected.identity != *observed)
    {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
            fact: "documentation declaration census",
        });
    }
    Ok(())
}

fn require_core_refs(
    references: &[[u8; 32]],
    identities: &[CoreDeclarationFacts],
    fact: &'static str,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    require_core_refs_iter(references.iter(), identities, fact)
}

fn require_core_refs_iter<'a>(
    references: impl IntoIterator<Item = &'a [u8; 32]>,
    identities: &[CoreDeclarationFacts],
    fact: &'static str,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    if references.into_iter().any(|identity| {
        identities
            .binary_search_by_key(identity, |row| row.identity)
            .is_err()
    }) {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily { fact });
    }
    Ok(())
}

fn require_local_refs(
    references: &[[u8; 32]],
    identities: &[[u8; 32]],
    fact: &'static str,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    require_local_refs_iter(references.iter(), identities, fact)
}

fn require_local_refs_iter<'a>(
    references: impl IntoIterator<Item = &'a [u8; 32]>,
    identities: &[[u8; 32]],
    fact: &'static str,
) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    if references
        .into_iter()
        .any(|identity| identities.binary_search(identity).is_err())
    {
        return Err(SemanticTypedPlaneInventoryV2Error::CrossFamily { fact });
    }
    Ok(())
}

fn reject_duplicates(values: &[[u8; 32]]) -> Result<(), SemanticTypedPlaneInventoryV2Error> {
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(SemanticPlaneRecordError::StableKeyCollision.into());
    }
    Ok(())
}

fn read_availability(cursor: &mut Cursor<'_>) -> Result<bool, SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn try_push<T>(values: &mut Vec<T>, value: T) -> Result<(), SemanticPlaneRecordError> {
    values
        .try_reserve(1)
        .map_err(SemanticPlaneRecordError::Allocation)?;
    values.push(value);
    Ok(())
}

fn identity_key(identity: crate::ir::DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(identity.family.as_bytes());
    bytes[16..].copy_from_slice(identity.variant.as_bytes());
    bytes
}

fn relation_source_row_key(relation: [u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RELATION_SOURCE_KEY_DOMAIN);
    hasher.update(&relation);
    *hasher.finalize().as_bytes()
}

fn stable_row_family(family: SemanticIrPlane) -> RowFamily {
    match family {
        SemanticIrPlane::Core => RowFamily::Core,
        SemanticIrPlane::Types => RowFamily::Types,
        SemanticIrPlane::Relations => RowFamily::Relations,
        SemanticIrPlane::Occurrences => RowFamily::Occurrences,
        SemanticIrPlane::Documentation => RowFamily::Documentation,
        SemanticIrPlane::SourceProvenance => RowFamily::SourceProvenance,
        SemanticIrPlane::LanguageExtensions(_) => RowFamily::LanguageExtensions,
    }
}

fn semantic_family_row_root(
    family: SemanticIrPlane,
    row_count: u64,
    row_index_root: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SEMANTIC_FAMILY_ROW_ROOT_DOMAIN);
    hasher.update(&[2, stable_row_family(family).code()]);
    if let SemanticIrPlane::LanguageExtensions(profile) = family {
        hasher.update(&<[u8; 2]>::from(profile));
    }
    hasher.update(&row_count.to_be_bytes());
    hasher.update(&row_index_root);
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use alloc::{collections::BTreeMap, vec, vec::Vec};

    use backend_version::ScopeRoot;

    use super::*;
    use crate::ir::{DeclarationFamilyId, SemanticPlaneKind, TypeScriptSource, VariantFingerprint};
    use crate::vocabulary::{LanguageProfile, Stage};

    type Row = ([u8; 32], u8, Vec<u8>);

    const TYPES_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.types-row-key.v1\0";
    const RELATION_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.relation-key.v1\0";
    const OCCURRENCE_BASE_DOMAIN: &[u8] = b"backend.semantic.ir.occurrence-base.v1\0";
    const OCCURRENCE_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.occurrence-row.v1\0";
    const EXTENSION_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.language-extension-row.v1\0";

    struct EncodedSegment {
        first_key: [u8; 32],
        last_key: [u8; 32],
        row_count: u32,
        id: SemanticSegmentId,
        bytes: Vec<u8>,
    }

    #[derive(Clone, Default)]
    struct TestJumboObjects {
        leaves: BTreeMap<crate::ir::JumboRopeObjectId, Vec<u8>>,
        interiors: BTreeMap<
            crate::ir::JumboRopeObjectId,
            [u8; crate::ir::jumbo_rope::ROPE_NODE_WIRE_BYTES],
        >,
        leaf_order: Vec<crate::ir::JumboRopeObjectId>,
        leaf_reads: u64,
        interior_reads: u64,
    }

    impl crate::ir::JumboRopeObjectSink for TestJumboObjects {
        type Error = SemanticPlaneRecordError;

        fn write_leaf(&mut self, leaf: crate::ir::JumboRopeLeafRef<'_>) -> Result<(), Self::Error> {
            let bytes = leaf.bytes().to_vec();
            if let Some(previous) = self.leaves.insert(leaf.id(), bytes.clone()) {
                assert_eq!(previous, bytes, "content-addressed leaves are immutable");
            }
            self.leaf_order.push(leaf.id());
            Ok(())
        }

        fn write_interior(&mut self, node: &crate::ir::JumboRopeNode) -> Result<(), Self::Error> {
            let id = node.id();
            let wire = node.encode_wire();
            if let Some(previous) = self.interiors.insert(id, wire) {
                assert_eq!(previous, wire, "content-addressed nodes are immutable");
            }
            Ok(())
        }
    }

    impl crate::ir::JumboRopeObjectSource for TestJumboObjects {
        type Error = SemanticPlaneRecordError;

        fn read_leaf(
            &mut self,
            id: crate::ir::JumboRopeObjectId,
            output: &mut [u8; crate::ir::JUMBO_ROPE_MAX_LEAF_BYTES],
        ) -> Result<Option<usize>, Self::Error> {
            self.leaf_reads = self.leaf_reads.saturating_add(1);
            let Some(bytes) = self.leaves.get(&id) else {
                return Ok(None);
            };
            if bytes.len() > output.len() {
                return Ok(Some(output.len() + 1));
            }
            output[..bytes.len()].copy_from_slice(bytes);
            Ok(Some(bytes.len()))
        }

        fn read_interior(
            &mut self,
            id: crate::ir::JumboRopeObjectId,
        ) -> Result<Option<[u8; crate::ir::jumbo_rope::ROPE_NODE_WIRE_BYTES]>, Self::Error>
        {
            self.interior_reads = self.interior_reads.saturating_add(1);
            Ok(self.interiors.get(&id).copied())
        }
    }

    fn profile() -> LanguageProfile {
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript)
    }

    fn build() -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            [0x10; 32],
            [0x20; 32],
            profile(),
            Stage::LowerIr,
            [0x30; 32],
            [0x40; 32],
            [0x50; 32],
            [0x60; 32],
        )
    }

    fn image_facts() -> SemanticImageFacts {
        SemanticImageFacts {
            authority: SemanticImageAuthority::Language(profile()),
            provenance: ImageProvenance::Unavailable,
        }
    }

    fn input_claim() -> SemanticInputWitness {
        SemanticInputWitness::claimed([0x70; 32], ScopeRoot::from_bytes([0x80; 32]))
    }

    fn identity(seed: u8) -> [u8; 32] {
        let family = core::array::from_fn(|index| {
            seed.wrapping_add(u8::try_from(index).expect("family byte index") * 3)
        });
        let variant = core::array::from_fn(|index| {
            seed.wrapping_add(0x41 + u8::try_from(index).expect("variant byte index") * 5)
        });
        let identity = crate::ir::DeclarationIdentity {
            family: DeclarationFamilyId::from_raw(family),
            variant: VariantFingerprint::from_raw(variant),
        };
        identity_key(identity)
    }

    fn append_bytes(value: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(
            &u32::try_from(value.len())
                .expect("fixture length")
                .to_be_bytes(),
        );
        out.extend_from_slice(value);
    }

    fn core_row(identity: [u8; 32], extension: bool) -> Row {
        let mut payload = Vec::new();
        payload.extend_from_slice(&identity);
        append_bytes(b"fixture-name", &mut payload);
        payload.extend_from_slice(&0_u16.to_be_bytes()); // Function
        payload.push(0); // visibility
        payload.push(0); // no local parent
        payload.push(1); // authority root
        payload.extend_from_slice(&[
            0,
            0,
            0,
            0,
            1, // documentation captured
            0,
            0,
            u8::from(extension),
        ]);
        payload.extend_from_slice(&0_u32.to_be_bytes()); // members
        payload.extend_from_slice(&0_u32.to_be_bytes()); // attributes
        (identity, CORE_DECLARATION_TAG, payload)
    }

    fn documentation_row(identity: [u8; 32]) -> Row {
        let mut payload = Vec::new();
        payload.extend_from_slice(&identity);
        payload.push(1); // documentation captured
        payload.extend_from_slice(&0_u32.to_be_bytes()); // empty docs
        (identity, DOCUMENTATION_TAG, payload)
    }

    fn source_declaration_row(identity: [u8; 32]) -> Row {
        let mut payload = identity.to_vec();
        payload.push(0); // source unavailable
        (identity, SOURCE_DECLARATION_TAG, payload)
    }

    fn source_declaration_row_with_inline_source(
        identity: [u8; 32],
        path: &[u8],
        start: u32,
        end: u32,
    ) -> Row {
        let mut payload = identity.to_vec();
        payload.push(1); // source captured
        append_bytes(path, &mut payload);
        payload.extend_from_slice(&start.to_be_bytes());
        payload.extend_from_slice(&end.to_be_bytes());
        (identity, SOURCE_DECLARATION_TAG, payload)
    }

    fn relation_key(from: [u8; 32], target: [u8; 32]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(RELATION_KEY_DOMAIN);
        hasher.update(&from);
        hasher.update(&[0]); // local target
        hasher.update(&target);
        hasher.update(&[0]); // Calls
        *hasher.finalize().as_bytes()
    }

    fn relation_row(from: [u8; 32], target: [u8; 32]) -> ([u8; 32], Row) {
        let key = relation_key(from, target);
        let mut payload = Vec::new();
        payload.extend_from_slice(&from);
        payload.push(0); // local target
        payload.extend_from_slice(&target);
        payload.push(0); // Calls
        payload.push(4); // compiler confidence
        (key, (key, RELATION_TAG, payload))
    }

    fn occurrence_row(relation: [u8; 32]) -> Row {
        occurrence_row_with_duplicate_rank(relation, 0)
    }

    fn occurrence_row_with_duplicate_rank(relation: [u8; 32], duplicate_rank: u32) -> Row {
        let mut base = Vec::new();
        base.extend_from_slice(&relation);
        base.push(0); // syntactic confidence
        base.push(0); // source unavailable
        base.push(0); // no source span
        let mut base_hasher = blake3::Hasher::new();
        base_hasher.update(OCCURRENCE_BASE_DOMAIN);
        base_hasher.update(&base);
        let digest = *base_hasher.finalize().as_bytes();
        let mut key_hasher = blake3::Hasher::new();
        key_hasher.update(OCCURRENCE_KEY_DOMAIN);
        key_hasher.update(&digest);
        key_hasher.update(&duplicate_rank.to_be_bytes());
        let key = *key_hasher.finalize().as_bytes();
        base.extend_from_slice(&duplicate_rank.to_be_bytes());
        (key, OCCURRENCE_TAG, base)
    }

    fn empty_types_key() -> [u8; 32] {
        let payload = [0_u8; 4];
        let mut hasher = blake3::Hasher::new();
        hasher.update(TYPES_KEY_DOMAIN);
        hasher.update(&[2, 8]); // Types plane, TypeParameters tag
        hasher.update(&payload);
        *hasher.finalize().as_bytes()
    }

    fn atom_row(bytes: &[u8]) -> Row {
        let mut payload = Vec::new();
        append_bytes(bytes, &mut payload);
        let mut hasher = blake3::Hasher::new();
        hasher.update(TYPES_KEY_DOMAIN);
        hasher.update(&[2, 11]); // Types plane, Atom tag
        hasher.update(&payload);
        (*hasher.finalize().as_bytes(), 11, payload)
    }

    fn types_rows(identities: &[[u8; 32]]) -> Vec<Row> {
        let mut rows = Vec::new();
        for identity in identities {
            let mut payload = Vec::new();
            payload.extend_from_slice(identity);
            payload.push(0); // no semantic type
            payload.extend_from_slice(&[0; 32]);
            rows.push((*identity, 1, payload)); // entity-root tag
        }
        rows.push((empty_types_key(), 8, vec![0; 4]));
        rows
    }

    fn extension_row(identity: [u8; 32]) -> Row {
        let profile_bytes = <[u8; 2]>::from(profile());
        let mut key_hasher = blake3::Hasher::new();
        key_hasher.update(EXTENSION_KEY_DOMAIN);
        key_hasher.update(&profile_bytes);
        key_hasher.update(&identity);
        key_hasher.update(&[1]); // TypeScript role
        let key = *key_hasher.finalize().as_bytes();
        let mut payload = Vec::new();
        payload.extend_from_slice(&identity);
        payload.push(1); // captured
        payload.push(8); // TypeParameters domain
        payload.extend_from_slice(&empty_types_key());
        payload.push(0); // no declared type
        payload.push(0); // no observed type
        (key, 1, payload)
    }

    fn valid_rows(
        relation_from: [u8; 32],
        omit_source: Option<[u8; 32]>,
        omit_documentation: Option<[u8; 32]>,
        add_unclaimed_extension_owner: bool,
        include_relation: bool,
    ) -> [Vec<Row>; 7] {
        let first = identity(0x13);
        let second = identity(0x72);
        let mut rows: [Vec<Row>; 7] = core::array::from_fn(|_| Vec::new());
        rows[0].push(core_row(first, true));
        rows[0].push(core_row(second, false));
        rows[1] = types_rows(&[first, second]);
        rows[4].extend(
            [first, second]
                .into_iter()
                .filter(|identity| Some(*identity) != omit_documentation)
                .map(documentation_row),
        );
        rows[5].extend(
            [first, second]
                .into_iter()
                .filter(|identity| Some(*identity) != omit_source)
                .map(source_declaration_row),
        );
        rows[6].push(extension_row(first));
        if add_unclaimed_extension_owner {
            rows[6].push(extension_row(second));
        }
        if include_relation {
            let (relation, row) = relation_row(relation_from, second);
            rows[2].push(row);
            rows[3].push(occurrence_row(relation));
            let source_key = relation_source_row_key(relation);
            let mut payload = relation.to_vec();
            payload.push(0); // relation source unavailable
            rows[5].push((source_key, SOURCE_RELATION_TAG, payload));
        }
        rows
    }

    fn plane_code(family: SemanticIrPlane) -> u8 {
        match family {
            SemanticIrPlane::Core => 1,
            SemanticIrPlane::Types => 2,
            SemanticIrPlane::Relations => 3,
            SemanticIrPlane::Occurrences => 4,
            SemanticIrPlane::Documentation => 5,
            SemanticIrPlane::SourceProvenance => 6,
            SemanticIrPlane::LanguageExtensions(_) => 7,
        }
    }

    fn encode_family_segment(family: SemanticIrPlane, mut rows: Vec<Row>) -> EncodedSegment {
        rows.sort_unstable_by_key(|(key, _, _)| *key);
        let first_key = rows.first().expect("non-empty fixture family").0;
        let last_key = rows.last().expect("non-empty fixture family").0;
        let row_count = u32::try_from(rows.len()).expect("fixture row count");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SPIR");
        bytes.extend_from_slice(&super::super::VERSION.to_be_bytes());
        bytes.push(plane_code(family));
        bytes.extend_from_slice(&row_count.to_be_bytes());
        for (key, tag, payload) in rows {
            bytes.extend_from_slice(&key);
            bytes.push(tag);
            bytes.extend_from_slice(
                &u32::try_from(payload.len())
                    .expect("fixture payload length")
                    .to_be_bytes(),
            );
            bytes.extend_from_slice(&payload);
        }
        let descriptor = SemanticPlaneSegment::from_payload(
            SemanticPlaneKind::Ir(family),
            first_key,
            last_key,
            row_count,
            &bytes,
        )
        .expect("fixture descriptor");
        EncodedSegment {
            first_key,
            last_key,
            row_count,
            id: descriptor.admitted_id().expect("fixture ID"),
            bytes,
        }
    }

    fn family_kinds() -> [SemanticIrPlane; 7] {
        [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(profile()),
        ]
    }

    fn terminal_boundary_policy() -> CanonicalPlaneSegmentBoundaryPolicy {
        CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES as u32,
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES as u32,
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES as u32,
        )
        .expect("fixed terminal-only test boundary policy")
    }

    fn split_boundary_policy() -> CanonicalPlaneSegmentBoundaryPolicy {
        CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            11,
            64,
            crate::ir::MAX_SEMANTIC_SEGMENT_BYTES as u32,
        )
        .expect("fixture row-size split boundary policy")
    }

    fn fixture_policy_splits(family: SemanticIrPlane, sorted_rows: &[Row]) -> Vec<usize> {
        let policy = split_boundary_policy();
        let mut splits = Vec::new();
        let mut current_segment_bytes = 0_usize;
        for (index, (key, _, payload)) in sorted_rows.iter().enumerate() {
            let row_bytes = 32 + 1 + 4 + payload.len();
            let projected = current_segment_bytes
                .checked_add(row_bytes)
                .expect("fixture row byte count fits");
            if index > 0
                && (projected > policy.maximum_bytes()
                    || policy.cuts_before(family, current_segment_bytes, key))
            {
                splits.push(index);
                current_segment_bytes = super::super::SPIR_HEADER_BYTES;
            } else if index == 0 {
                current_segment_bytes = super::super::SPIR_HEADER_BYTES;
            }
            current_segment_bytes = current_segment_bytes
                .checked_add(row_bytes)
                .expect("fixture segment byte count fits");
        }
        splits
    }

    fn verify_rows(
        rows: [Vec<Row>; 7],
    ) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error> {
        verify_rows_with_split(rows, None)
    }

    fn verify_rows_with_split(
        rows: [Vec<Row>; 7],
        split: Option<(usize, usize)>,
    ) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error> {
        verify_rows_with_policy(
            rows,
            split,
            SemanticTypedPlaneVerificationLimitsV2::standard(),
        )
    }

    fn verify_rows_with_policy(
        rows: [Vec<Row>; 7],
        split: Option<(usize, usize)>,
        limits: SemanticTypedPlaneVerificationLimitsV2,
    ) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error> {
        let mut split_points_by_family: [Vec<usize>; 7] = core::array::from_fn(|_| Vec::new());
        if let Some((family, split_at)) = split {
            split_points_by_family[family].push(split_at);
        }
        verify_rows_with_family_splits(rows, split_points_by_family, limits)
    }

    fn verify_rows_with_family_splits(
        rows: [Vec<Row>; 7],
        split_points_by_family: [Vec<usize>; 7],
        limits: SemanticTypedPlaneVerificationLimitsV2,
    ) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error> {
        let kinds = family_kinds();
        let row_counts = rows
            .each_ref()
            .map(|family| u64::try_from(family.len()).expect("fixture row count fits u64"));
        let encoded: [Vec<EncodedSegment>; 7] = core::array::from_fn(|index| {
            let mut family_rows = rows[index].clone();
            family_rows.sort_unstable_by_key(|(key, _, _)| *key);
            let mut segments = Vec::new();
            let mut start = 0;
            for end in split_points_by_family[index]
                .iter()
                .copied()
                .chain(core::iter::once(family_rows.len()))
            {
                if end > start && end <= family_rows.len() {
                    segments.push(encode_family_segment(
                        kinds[index],
                        family_rows[start..end].to_vec(),
                    ));
                    start = end;
                }
            }
            segments
        });
        let segment_claims: [Vec<TypedPlaneSegmentPayloadV2<'_>>; 7] =
            core::array::from_fn(|index| {
                encoded[index]
                    .iter()
                    .map(|segment| TypedPlaneSegmentPayloadV2 {
                        first_key: segment.first_key,
                        last_key: segment.last_key,
                        row_count: segment.row_count,
                        byte_length: u64::try_from(segment.bytes.len())
                            .expect("fixture payload length fits u64"),
                        id_claim: UntrustedSemanticSegmentId::from_raw(*segment.id.as_bytes()),
                        payload: &segment.bytes,
                    })
                    .collect()
            });
        let families = core::array::from_fn(|index| TypedPlaneFamilyPayloadsV2 {
            family: kinds[index],
            row_count: row_counts[index],
            boundary_policy: if split_points_by_family[index].is_empty() {
                terminal_boundary_policy()
            } else {
                split_boundary_policy()
            },
            segments: &segment_claims[index],
        });
        let payloads = encoded
            .iter()
            .flat_map(|segments| segments.iter().map(|segment| segment.bytes.as_slice()))
            .collect::<Vec<_>>();
        verify_semantic_typed_plane_inventory_v2(
            build(),
            image_facts(),
            input_claim(),
            &families,
            &payloads,
            limits,
        )
    }

    struct BorrowedTestSegmentSource<'a> {
        payloads: Vec<&'a [u8]>,
        calls: usize,
        requested_indices: Vec<usize>,
        tamper: Option<usize>,
        corrupted_payload: Vec<u8>,
    }

    impl TypedPlaneSegmentSourceV2 for BorrowedTestSegmentSource<'_> {
        type Error = &'static str;

        fn segment<'source>(
            &'source mut self,
            index: usize,
            _claim: &SemanticTypedPlaneSegmentClaimV2,
        ) -> Result<&'source [u8], Self::Error> {
            let selected = if self.tamper == Some(index) {
                index.checked_add(1).ok_or("test segment index overflow")?
            } else {
                index
            };
            let payload = self
                .payloads
                .get(selected)
                .copied()
                .ok_or("test segment source index out of range")?;
            self.calls = self
                .calls
                .checked_add(1)
                .ok_or("test source call overflow")?;
            self.requested_indices.push(index);
            if self.tamper == Some(usize::MAX - 1) && index == 0 {
                self.corrupted_payload.clear();
                self.corrupted_payload.extend_from_slice(payload);
                let last = self.corrupted_payload.last_mut().ok_or("empty fixture")?;
                *last ^= 1;
                return Ok(&self.corrupted_payload);
            }
            if self.tamper == Some(usize::MAX) && index == 0 {
                return payload
                    .get(..payload.len().saturating_sub(1))
                    .ok_or("empty fixture");
            }
            Ok(payload)
        }
    }

    fn verify_rows_from_borrowed_source(
        rows: &[Vec<Row>; 7],
        tamper: Option<usize>,
    ) -> (
        Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>,
        Vec<usize>,
        usize,
    ) {
        verify_rows_from_borrowed_source_with_admission(
            rows,
            tamper,
            SemanticTypedPlaneVerificationLimitsV2::standard(),
            None,
        )
    }

    fn verify_rows_from_borrowed_source_with_admission(
        rows: &[Vec<Row>; 7],
        tamper: Option<usize>,
        limits: SemanticTypedPlaneVerificationLimitsV2,
        jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
    ) -> (
        Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>,
        Vec<usize>,
        usize,
    ) {
        verify_rows_from_borrowed_source_with_admission_and_docs_split(
            rows,
            tamper,
            limits,
            jumbo_admission,
            None,
        )
    }

    fn verify_rows_from_borrowed_source_with_admission_and_docs_split(
        rows: &[Vec<Row>; 7],
        tamper: Option<usize>,
        limits: SemanticTypedPlaneVerificationLimitsV2,
        jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
        split_documentation_at: Option<usize>,
    ) -> (
        Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>,
        Vec<usize>,
        usize,
    ) {
        let mut split_points_by_family: [Vec<usize>; 7] = core::array::from_fn(|_| Vec::new());
        if let Some(split_at) = split_documentation_at {
            split_points_by_family[4].push(split_at);
        }
        verify_rows_from_borrowed_source_with_family_splits(
            rows,
            tamper,
            limits,
            jumbo_admission,
            split_points_by_family,
        )
    }

    fn verify_rows_from_borrowed_source_with_family_splits(
        rows: &[Vec<Row>; 7],
        tamper: Option<usize>,
        limits: SemanticTypedPlaneVerificationLimitsV2,
        mut jumbo_admission: Option<&mut dyn JumboPlaneClosureAdmissionV2>,
        split_points_by_family: [Vec<usize>; 7],
    ) -> (
        Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>,
        Vec<usize>,
        usize,
    ) {
        let kinds = family_kinds();
        let row_counts = rows
            .each_ref()
            .map(|family| u64::try_from(family.len()).expect("fixture row count fits u64"));
        let encoded: [Vec<EncodedSegment>; 7] = core::array::from_fn(|index| {
            let mut family_rows = rows[index].clone();
            family_rows.sort_unstable_by_key(|(key, _, _)| *key);
            let mut segments = Vec::new();
            let mut start = 0;
            for end in split_points_by_family[index]
                .iter()
                .copied()
                .chain(core::iter::once(family_rows.len()))
            {
                if end > start && end <= family_rows.len() {
                    segments.push(encode_family_segment(
                        kinds[index],
                        family_rows[start..end].to_vec(),
                    ));
                    start = end;
                }
            }
            segments
        });
        let families = core::array::from_fn(|index| {
            let segments = encoded[index]
                .iter()
                .map(|segment| {
                    SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                        segment.first_key,
                        segment.last_key,
                        segment.row_count,
                        u64::try_from(segment.bytes.len()).expect("fixture byte length fits"),
                        UntrustedSemanticSegmentId::from_raw(*segment.id.as_bytes()),
                    )
                    .expect("fixture segment claim")
                })
                .collect();
            SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                kinds[index],
                row_counts[index],
                if split_points_by_family[index].is_empty() {
                    terminal_boundary_policy()
                } else {
                    split_boundary_policy()
                },
                segments,
            )
            .expect("fixture family descriptor")
        });
        let payloads = encoded
            .iter()
            .flat_map(|segments| segments.iter().map(|segment| segment.bytes.as_slice()))
            .collect::<Vec<_>>();
        let expected_calls = payloads.len();
        let mut source = BorrowedTestSegmentSource {
            payloads,
            calls: 0,
            requested_indices: Vec::new(),
            tamper,
            corrupted_payload: Vec::new(),
        };
        let result = verify_semantic_typed_plane_inventory_v2_with_segment_source(
            build(),
            image_facts(),
            input_claim(),
            &families,
            &mut source,
            limits,
            jumbo_admission,
        );
        debug_assert_eq!(source.calls, source.requested_indices.len());
        (result, source.requested_indices, expected_calls)
    }

    fn docs_wire(text: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(text.len() + 9);
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.push(0); // text fragment
        bytes.extend_from_slice(
            &u32::try_from(text.len())
                .expect("fixture text length fits u32")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(text);
        bytes
    }

    fn jumbo_docs_row_with_context(
        identity: [u8; 32],
        text: &[u8],
        context: crate::ir::JumboValueContext,
        objects: &mut TestJumboObjects,
    ) -> Row {
        jumbo_docs_row_with_value(identity, &docs_wire(text), context, objects)
    }

    fn jumbo_docs_row_with_value(
        identity: [u8; 32],
        value: &[u8],
        context: crate::ir::JumboValueContext,
        objects: &mut TestJumboObjects,
    ) -> Row {
        let receipt = crate::ir::write_jumbo_value(
            context,
            value,
            crate::ir::JumboRopeLimits::default(),
            objects,
        )
        .expect("write jumbo fixture objects");
        let mut payload = identity.to_vec();
        payload.push(1); // documentation captured
        payload.extend_from_slice(&receipt.verified().descriptor().encode_wire());
        (
            identity,
            super::super::declarations::DOCS_JUMBO_TAG,
            payload,
        )
    }

    fn jumbo_source_row_with_context(
        identity: [u8; 32],
        path: &[u8],
        context: crate::ir::JumboValueContext,
        objects: &mut TestJumboObjects,
    ) -> Row {
        let receipt = crate::ir::write_jumbo_value(
            context,
            path,
            crate::ir::JumboRopeLimits::default(),
            objects,
        )
        .expect("write jumbo source fixture objects");
        let mut payload = identity.to_vec();
        payload.push(1); // source captured
        payload.extend_from_slice(&receipt.verified().descriptor().encode_wire());
        payload.extend_from_slice(&3_u32.to_be_bytes());
        payload.extend_from_slice(&17_u32.to_be_bytes());
        (
            identity,
            super::super::source_provenance::DECLARATION_SOURCE_JUMBO_TAG,
            payload,
        )
    }

    fn jumbo_relation_source_row(
        relation: [u8; 32],
        path: &[u8],
        objects: &mut TestJumboObjects,
    ) -> Row {
        let owner = relation_source_row_key(relation);
        let context = crate::ir::JumboValueContext::new(
            owner,
            crate::ir::JumboValueFamily::SourceProvenance,
            0,
            crate::ir::JumboValueEncoding::Bytes,
        );
        let receipt = crate::ir::write_jumbo_value(
            context,
            path,
            crate::ir::JumboRopeLimits::default(),
            objects,
        )
        .expect("write jumbo relation source fixture objects");
        let mut payload = relation.to_vec();
        payload.push(1); // source captured
        payload.extend_from_slice(&receipt.verified().descriptor().encode_wire());
        payload.extend_from_slice(&3_u32.to_be_bytes());
        payload.extend_from_slice(&17_u32.to_be_bytes());
        (
            owner,
            super::super::source_provenance::RELATION_SOURCE_JUMBO_TAG,
            payload,
        )
    }

    fn replace_documentation_row(rows: &mut [Vec<Row>; 7], key: [u8; 32], row: Row) {
        let slot = rows[4]
            .iter_mut()
            .find(|row| row.0 == key)
            .expect("fixture documentation row exists");
        *slot = row;
    }

    fn verify_rows_with_jumbo_source<S>(
        rows: [Vec<Row>; 7],
        source: &mut S,
        jumbo_limits: crate::ir::JumboRopeLimits,
    ) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>
    where
        S: crate::ir::JumboRopeObjectSource + ?Sized,
        S::Error: core::fmt::Display,
    {
        verify_rows_with_jumbo_source_and_policy(
            rows,
            source,
            jumbo_limits,
            SemanticTypedPlaneVerificationLimitsV2::standard(),
        )
    }

    fn verify_rows_with_jumbo_source_and_policy<S>(
        rows: [Vec<Row>; 7],
        source: &mut S,
        jumbo_limits: crate::ir::JumboRopeLimits,
        limits: SemanticTypedPlaneVerificationLimitsV2,
    ) -> Result<VerifiedTypedPlaneInventoryV2, SemanticTypedPlaneInventoryV2Error>
    where
        S: crate::ir::JumboRopeObjectSource + ?Sized,
        S::Error: core::fmt::Display,
    {
        let kinds = family_kinds();
        let row_counts = rows
            .each_ref()
            .map(|family| u64::try_from(family.len()).expect("fixture row count fits u64"));
        let encoded: [Vec<EncodedSegment>; 7] = core::array::from_fn(|index| {
            let family_rows = rows[index].clone();
            if family_rows.is_empty() {
                Vec::new()
            } else {
                vec![encode_family_segment(kinds[index], family_rows)]
            }
        });
        let segment_claims: [Vec<TypedPlaneSegmentPayloadV2<'_>>; 7] =
            core::array::from_fn(|index| {
                encoded[index]
                    .iter()
                    .map(|segment| TypedPlaneSegmentPayloadV2 {
                        first_key: segment.first_key,
                        last_key: segment.last_key,
                        row_count: segment.row_count,
                        byte_length: u64::try_from(segment.bytes.len())
                            .expect("fixture payload length fits u64"),
                        id_claim: UntrustedSemanticSegmentId::from_raw(*segment.id.as_bytes()),
                        payload: &segment.bytes,
                    })
                    .collect()
            });
        let families = core::array::from_fn(|index| TypedPlaneFamilyPayloadsV2 {
            family: kinds[index],
            row_count: row_counts[index],
            boundary_policy: terminal_boundary_policy(),
            segments: &segment_claims[index],
        });
        let payloads = encoded
            .iter()
            .flat_map(|segments| segments.iter().map(|segment| segment.bytes.as_slice()))
            .collect::<Vec<_>>();
        verify_semantic_typed_plane_inventory_v2_with_jumbo_source(
            build(),
            image_facts(),
            input_claim(),
            &families,
            &payloads,
            limits,
            jumbo_limits,
            source,
        )
    }

    fn encode_all_rows(rows: &[Vec<Row>; 7]) -> ([Vec<EncodedSegment>; 7], [u64; 7]) {
        let kinds = family_kinds();
        let row_counts = rows
            .each_ref()
            .map(|family| u64::try_from(family.len()).expect("fixture row count fits u64"));
        let encoded = core::array::from_fn(|index| {
            if rows[index].is_empty() {
                Vec::new()
            } else {
                vec![encode_family_segment(kinds[index], rows[index].clone())]
            }
        });
        (encoded, row_counts)
    }

    fn cold_manifest_for_encoded_rows(
        encoded: &[Vec<EncodedSegment>; 7],
        row_counts: [u64; 7],
        content_root: [u8; 32],
        generation_root: [u8; 32],
    ) -> crate::ir::SemanticTypedPlaneManifestV2 {
        let kinds = family_kinds();
        let families = core::array::from_fn(|index| {
            let segments = encoded[index]
                .iter()
                .map(|segment| {
                    crate::ir::SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                        segment.first_key,
                        segment.last_key,
                        segment.row_count,
                        u64::try_from(segment.bytes.len()).expect("fixture byte count fits"),
                        UntrustedSemanticSegmentId::from_raw(*segment.id.as_bytes()),
                    )
                    .expect("fixture segment claim is bounded")
                })
                .collect();
            crate::ir::SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                kinds[index],
                row_counts[index],
                terminal_boundary_policy(),
                segments,
            )
            .expect("fixture family claim is canonical")
        });
        crate::ir::SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build(),
            image_facts(),
            crate::ir::SemanticInputClaimV2::from_witness(&input_claim()),
            crate::ir::UntrustedSemanticContentRootV2::from_wire_claim(content_root),
            crate::ir::UntrustedSemanticGenerationRootV2::from_wire_claim(generation_root),
            families,
        )
        .expect("fixture c007 claims are canonical")
    }

    fn patterned_text(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| b'a' + u8::try_from((index * 37 + index / 97) % 26).expect("ASCII offset"))
            .collect()
    }

    fn jumbo_docs_context(identity: [u8; 32]) -> crate::ir::JumboValueContext {
        crate::ir::JumboValueContext::new(
            identity,
            crate::ir::JumboValueFamily::Documentation,
            0,
            crate::ir::JumboValueEncoding::Bytes,
        )
    }

    fn jumbo_docs_local_link(target: [u8; 32], filler: &[u8]) -> Vec<u8> {
        jumbo_docs_local_links(target, filler, 1)
    }

    fn jumbo_docs_local_links(target: [u8; 32], filler: &[u8], link_count: u32) -> Vec<u8> {
        let mut value = Vec::new();
        value.extend_from_slice(&(link_count + 1).to_be_bytes());
        for _ in 0..link_count {
            value.push(2); // link fragment
            append_bytes(b"label", &mut value);
            value.push(0); // local declaration target
            value.extend_from_slice(&target);
        }
        value.push(0); // trailing text fragment
        append_bytes(filler, &mut value);
        value
    }

    fn rows_with_jumbo_docs(text: &[u8], objects: &mut TestJumboObjects) -> [Vec<Row>; 7] {
        let mut rows = valid_rows(identity(0x13), None, None, false, true);
        let owner = identity(0x13);
        let row = jumbo_docs_row_with_context(owner, text, jumbo_docs_context(owner), objects);
        replace_documentation_row(&mut rows, owner, row);
        rows
    }

    fn rows_with_jumbo_source(path: &[u8], objects: &mut TestJumboObjects) -> [Vec<Row>; 7] {
        let owner = identity(0x13);
        let mut rows = valid_rows(owner, None, None, false, true);
        let context = crate::ir::JumboValueContext::new(
            owner,
            crate::ir::JumboValueFamily::SourceProvenance,
            0,
            crate::ir::JumboValueEncoding::Bytes,
        );
        let jumbo = jumbo_source_row_with_context(owner, path, context, objects);
        let slot = rows[5]
            .iter_mut()
            .find(|row| row.0 == owner)
            .expect("fixture source row exists");
        *slot = jumbo;
        mark_core_source_available(&mut rows, owner);
        rows
    }

    fn mark_core_source_available(rows: &mut [Vec<Row>; 7], identity: [u8; 32]) {
        let (_, _, payload) = rows[0]
            .iter_mut()
            .find(|row| row.0 == identity)
            .expect("fixture core row exists");
        // Core layout: 32-byte identity, length-prefixed name, kind,
        // visibility, parent, parent authority, then eight availability bits.
        let name_length = usize::try_from(u32::from_be_bytes(
            payload[32..36]
                .try_into()
                .expect("fixture name length is fixed width"),
        ))
        .expect("fixture name length fits usize");
        let availability_start = 32 + 4 + name_length + 2 + 1 + 1 + 1;
        payload[availability_start] = 1; // source captured
        payload[availability_start + 1] = 1; // source-file identity captured
    }

    #[test]
    fn aggregate_requires_a_complete_jumbo_object_closure_before_inventory() {
        let text = patterned_text(1_200 * 1024);
        let mut persisted = TestJumboObjects::default();
        let rows = rows_with_jumbo_docs(&text, &mut persisted);

        assert!(matches!(
            verify_rows(rows.clone()),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::JumboObjectStoreRequired
            ))
        ));
        let missing_leaf = *persisted.leaf_order.first().expect("stored jumbo leaf");
        let mut absent = persisted.clone();
        absent.leaves.remove(&missing_leaf);
        assert!(matches!(
            verify_rows_with_jumbo_source(rows, &mut absent, crate::ir::JumboRopeLimits::default()),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::JumboRope(crate::ir::JumboRopeError::MissingStoredLeaf)
            ))
        ));
    }

    #[test]
    fn aggregate_applies_explicit_jumbo_limits_before_object_reads() {
        let text = patterned_text(1_200 * 1024);
        let mut objects = TestJumboObjects::default();
        let rows = rows_with_jumbo_docs(&text, &mut objects);
        assert!(matches!(
            verify_rows_with_jumbo_source(
                rows,
                &mut objects,
                crate::ir::JumboRopeLimits::new(1, 1, 1024)
            ),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::JumboRope(
                    crate::ir::JumboRopeError::ValueTooLarge { .. }
                )
            ))
        ));
    }

    #[test]
    fn aggregate_charges_cumulative_jumbo_work_before_each_object_closure() {
        let first = identity(0x13);
        let second = identity(0x72);
        let text = patterned_text(1_200 * 1024);
        let mut objects = TestJumboObjects::default();
        let mut rows = valid_rows(first, None, None, false, false);
        let first_row =
            jumbo_docs_row_with_context(first, &text, jumbo_docs_context(first), &mut objects);
        let second_row =
            jumbo_docs_row_with_context(second, &text, jumbo_docs_context(second), &mut objects);
        let first_descriptor = crate::ir::UntrustedJumboValueDescriptor::decode_wire(
            &first_row.2[33..33 + crate::ir::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES],
        )
        .expect("fixture descriptor bytes decode")
        .check(crate::ir::JumboRopeLimits::default())
        .expect("fixture descriptor is structurally valid");
        replace_documentation_row(&mut rows, first, first_row);
        replace_documentation_row(&mut rows, second, second_row);

        let mut limits = SemanticTypedPlaneVerificationLimitsV2::standard();
        limits.max_total_jumbo_value_bytes = first_descriptor.byte_length();
        limits.max_total_jumbo_leaves = first_descriptor.leaf_count();
        limits.max_total_jumbo_object_reads = first_descriptor
            .leaf_count()
            .saturating_mul(2)
            .saturating_sub(1);
        limits.max_total_jumbo_read_bytes = first_descriptor.byte_length().saturating_add(
            first_descriptor
                .leaf_count()
                .saturating_sub(1)
                .saturating_mul(crate::ir::jumbo_rope::ROPE_NODE_WIRE_BYTES as u64),
        );

        assert!(matches!(
            verify_rows_with_jumbo_source_and_policy(
                rows,
                &mut objects,
                crate::ir::JumboRopeLimits::default(),
                limits,
            ),
            Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "jumbo-value-bytes"
            })
        ));
        assert_eq!(objects.leaf_reads, first_descriptor.leaf_count());
        assert_eq!(
            objects.interior_reads,
            first_descriptor.leaf_count().saturating_sub(1)
        );
    }

    #[test]
    fn aggregate_rejects_wrong_jumbo_owner_and_field_ordinal() {
        let owner = identity(0x13);
        let text = patterned_text(1_200 * 1024);
        for context in [
            crate::ir::JumboValueContext::new(
                [0xEE; 32],
                crate::ir::JumboValueFamily::Documentation,
                0,
                crate::ir::JumboValueEncoding::Bytes,
            ),
            crate::ir::JumboValueContext::new(
                owner,
                crate::ir::JumboValueFamily::Documentation,
                1,
                crate::ir::JumboValueEncoding::Bytes,
            ),
            crate::ir::JumboValueContext::new(
                owner,
                crate::ir::JumboValueFamily::SourceProvenance,
                0,
                crate::ir::JumboValueEncoding::Bytes,
            ),
            crate::ir::JumboValueContext::new(
                owner,
                crate::ir::JumboValueFamily::Documentation,
                0,
                crate::ir::JumboValueEncoding::Utf8,
            ),
        ] {
            let mut objects = TestJumboObjects::default();
            let mut rows = valid_rows(owner, None, None, false, true);
            // These descriptors must fail the owner/family/field/encoding
            // grammar before closure admission. Keep the stored value valid
            // UTF-8 even for the wrong-encoding case; `docs_wire` contains a
            // binary length prefix and is not necessarily valid UTF-8.
            let row = jumbo_docs_row_with_value(owner, &text, context, &mut objects);
            replace_documentation_row(&mut rows, owner, row);
            assert!(matches!(
                verify_rows_with_jumbo_source(
                    rows,
                    &mut objects,
                    crate::ir::JumboRopeLimits::default()
                ),
                Err(SemanticTypedPlaneInventoryV2Error::Record(
                    SemanticPlaneRecordError::RowGrammar
                ))
            ));
        }
    }

    #[test]
    fn aggregate_rejects_tampered_jumbo_length_and_root_claims() {
        let owner = identity(0x13);
        let text = patterned_text(1_200 * 1024);
        for mutation in ["length", "root"] {
            let mut objects = TestJumboObjects::default();
            let mut rows = valid_rows(owner, None, None, false, true);
            let mut row =
                jumbo_docs_row_with_context(owner, &text, jumbo_docs_context(owner), &mut objects);
            let descriptor = &mut row.2[33..33 + crate::ir::JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES];
            if mutation == "length" {
                let observed = u64::from_be_bytes(
                    descriptor[43..51]
                        .try_into()
                        .expect("fixed-width descriptor length"),
                );
                descriptor[43..51].copy_from_slice(
                    &observed
                        .checked_add(1)
                        .expect("fixture descriptor length does not overflow")
                        .to_be_bytes(),
                );
            } else {
                descriptor[59] ^= 1;
            }
            replace_documentation_row(&mut rows, owner, row);
            assert!(matches!(
                verify_rows_with_jumbo_source(
                    rows,
                    &mut objects,
                    crate::ir::JumboRopeLimits::default()
                ),
                Err(SemanticTypedPlaneInventoryV2Error::Record(
                    SemanticPlaneRecordError::JumboRope(_)
                ))
            ));
        }
    }

    #[test]
    fn aggregate_rejects_swapped_content_addressed_leaves() {
        let text = patterned_text(1_200 * 1024);
        let mut objects = TestJumboObjects::default();
        let rows = rows_with_jumbo_docs(&text, &mut objects);
        let first = *objects.leaf_order.first().expect("first stored leaf");
        let last = *objects.leaf_order.last().expect("last stored leaf");
        assert_ne!(first, last, "fixture leaves have distinct content IDs");
        let first_bytes = objects
            .leaves
            .get(&first)
            .expect("first leaf bytes")
            .clone();
        let last_bytes = objects.leaves.get(&last).expect("last leaf bytes").clone();
        objects.leaves.insert(first, last_bytes);
        objects.leaves.insert(last, first_bytes);

        assert!(matches!(
            verify_rows_with_jumbo_source(
                rows,
                &mut objects,
                crate::ir::JumboRopeLimits::default()
            ),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::JumboRope(crate::ir::JumboRopeError::LeafObjectCorrupt)
            ))
        ));
    }

    #[test]
    fn jumbo_documentation_links_join_the_exact_core_declaration_set() {
        let owner = identity(0x13);
        let target = identity(0x72);
        let filler = patterned_text(1_200 * 1024);
        let value = jumbo_docs_local_link(target, &filler);
        let mut objects = TestJumboObjects::default();
        let mut rows = valid_rows(owner, None, None, false, true);
        let row = jumbo_docs_row_with_value(owner, &value, jumbo_docs_context(owner), &mut objects);
        replace_documentation_row(&mut rows, owner, row);
        verify_rows_with_jumbo_source(rows, &mut objects, crate::ir::JumboRopeLimits::default())
            .expect("streamed jumbo link resolves to a declared core row");

        let unknown = identity(0xD1);
        let mut dangling_objects = TestJumboObjects::default();
        let mut dangling_rows = valid_rows(owner, None, None, false, true);
        let dangling_value = jumbo_docs_local_link(unknown, &filler);
        let row = jumbo_docs_row_with_value(
            owner,
            &dangling_value,
            jumbo_docs_context(owner),
            &mut dangling_objects,
        );
        replace_documentation_row(&mut dangling_rows, owner, row);
        assert!(matches!(
            verify_rows_with_jumbo_source(
                dangling_rows,
                &mut dangling_objects,
                crate::ir::JumboRopeLimits::default()
            ),
            Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "documentation local link"
            })
        ));
    }

    #[test]
    fn jumbo_documentation_duplicate_links_are_joined_and_counted_exactly() {
        let owner = identity(0x13);
        let target = identity(0x72);
        let filler = patterned_text(1_200 * 1024);
        let exact_limits = SemanticTypedPlaneVerificationLimitsV2 {
            max_references: 14,
            ..SemanticTypedPlaneVerificationLimitsV2::standard()
        };

        let mut duplicate_objects = TestJumboObjects::default();
        let mut duplicate_rows = valid_rows(owner, None, None, false, true);
        let duplicate_value = jumbo_docs_local_links(target, &filler, 2);
        let row = jumbo_docs_row_with_value(
            owner,
            &duplicate_value,
            jumbo_docs_context(owner),
            &mut duplicate_objects,
        );
        replace_documentation_row(&mut duplicate_rows, owner, row);
        verify_rows_with_jumbo_source_and_policy(
            duplicate_rows,
            &mut duplicate_objects,
            crate::ir::JumboRopeLimits::default(),
            exact_limits,
        )
        .expect("duplicate links to an exact core declaration remain two references");

        let mut one_link_objects = TestJumboObjects::default();
        let mut one_link_rows = valid_rows(owner, None, None, false, true);
        let one_link_value = jumbo_docs_local_links(target, &filler, 1);
        let row = jumbo_docs_row_with_value(
            owner,
            &one_link_value,
            jumbo_docs_context(owner),
            &mut one_link_objects,
        );
        replace_documentation_row(&mut one_link_rows, owner, row);
        let one_link_limits = SemanticTypedPlaneVerificationLimitsV2 {
            max_references: 14,
            ..exact_limits
        };
        verify_rows_with_jumbo_source_and_policy(
            one_link_rows,
            &mut one_link_objects,
            crate::ir::JumboRopeLimits::default(),
            one_link_limits,
        )
        .expect("one jumbo link fits the exact one-reference increment");

        let mut under_budget_objects = TestJumboObjects::default();
        let mut under_budget_rows = valid_rows(owner, None, None, false, true);
        let duplicate_value = jumbo_docs_local_links(target, &filler, 2);
        let row = jumbo_docs_row_with_value(
            owner,
            &duplicate_value,
            jumbo_docs_context(owner),
            &mut under_budget_objects,
        );
        replace_documentation_row(&mut under_budget_rows, owner, row);
        let under_budget = SemanticTypedPlaneVerificationLimitsV2 {
            max_references: 13,
            ..exact_limits
        };
        assert!(matches!(
            verify_rows_with_jumbo_source_and_policy(
                under_budget_rows,
                &mut under_budget_objects,
                crate::ir::JumboRopeLimits::default(),
                under_budget,
            ),
            Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count"
            })
        ));
    }

    #[test]
    fn aggregate_verifies_source_provenance_rope_before_inventory() {
        let path = patterned_text(1_200 * 1024);
        let mut objects = TestJumboObjects::default();
        let rows = rows_with_jumbo_source(&path, &mut objects);
        let inventory = verify_rows_with_jumbo_source(
            rows,
            &mut objects,
            crate::ir::JumboRopeLimits::default(),
        )
        .expect("binary source path closure admits in the seven-family inventory");
        assert_eq!(inventory.families()[5].row_count(), 3);
    }

    #[test]
    fn aggregate_decodes_captured_inline_declaration_source_fields() {
        let owner = identity(0x13);
        let mut rows = valid_rows(owner, None, None, false, true);
        let captured_source =
            source_declaration_row_with_inline_source(owner, b"src/captured.rs", 3, 17);
        let source_row = rows[5]
            .iter_mut()
            .find(|row| row.0 == owner)
            .expect("fixture source row exists");
        *source_row = captured_source;
        mark_core_source_available(&mut rows, owner);

        let inventory = verify_rows(rows)
            .expect("inline source path and span are consumed by aggregate verification");
        assert_eq!(inventory.families()[5].row_count(), 3);
    }

    #[test]
    fn aggregate_rejects_captured_inline_declaration_with_reversed_span() {
        let owner = identity(0x13);
        let mut rows = valid_rows(owner, None, None, false, true);
        let malformed = source_declaration_row_with_inline_source(owner, b"src/captured.rs", 17, 3);
        let source_row = rows[5]
            .iter_mut()
            .find(|row| row.0 == owner)
            .expect("fixture source row exists");
        *source_row = malformed;
        mark_core_source_available(&mut rows, owner);

        assert!(matches!(
            verify_rows(rows),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::RowGrammar
            ))
        ));
    }

    #[test]
    fn aggregate_verifies_jumbo_relation_source_rope_before_inventory() {
        let owner = identity(0x13);
        let path = patterned_text(1_200 * 1024);
        let mut objects = TestJumboObjects::default();
        let mut rows = valid_rows(owner, None, None, false, true);
        let source_relation = rows[5]
            .iter()
            .find(|row| row.1 == SOURCE_RELATION_TAG)
            .expect("fixture relation source row exists");
        let relation: [u8; 32] = source_relation.2[..32]
            .try_into()
            .expect("fixed-width relation identity");
        let jumbo = jumbo_relation_source_row(relation, &path, &mut objects);
        let slot = rows[5]
            .iter_mut()
            .find(|row| row.0 == jumbo.0)
            .expect("fixture relation source slot exists");
        *slot = jumbo;
        let inventory = verify_rows_with_jumbo_source(
            rows,
            &mut objects,
            crate::ir::JumboRopeLimits::default(),
        )
        .expect("jumbo relation source closure preserves the exact relation join");
        assert_eq!(inventory.families()[5].row_count(), 3);
    }

    #[test]
    fn cold_c007_reopen_admits_docs_and_source_jumbo_closures() {
        let owner = identity(0x13);
        let text = patterned_text(1_200 * 1024);
        let path = patterned_text(1_200 * 1024);
        let mut objects = TestJumboObjects::default();
        let mut rows = valid_rows(owner, None, None, false, true);
        let docs_row =
            jumbo_docs_row_with_context(owner, &text, jumbo_docs_context(owner), &mut objects);
        replace_documentation_row(&mut rows, owner, docs_row);
        let source_context = crate::ir::JumboValueContext::new(
            owner,
            crate::ir::JumboValueFamily::SourceProvenance,
            0,
            crate::ir::JumboValueEncoding::Bytes,
        );
        let source_row = jumbo_source_row_with_context(owner, &path, source_context, &mut objects);
        let source_slot = rows[5]
            .iter_mut()
            .find(|row| row.0 == owner)
            .expect("fixture source row exists");
        *source_slot = source_row;
        let relation: [u8; 32] = rows[5]
            .iter()
            .find(|row| row.1 == SOURCE_RELATION_TAG)
            .map(|row| {
                row.2[..32]
                    .try_into()
                    .expect("fixed-width relation identity")
            })
            .expect("fixture relation source row exists");
        let jumbo_relation = jumbo_relation_source_row(relation, &path, &mut objects);
        let relation_slot = rows[5]
            .iter_mut()
            .find(|row| row.0 == jumbo_relation.0)
            .expect("fixture relation source slot exists");
        *relation_slot = jumbo_relation;
        mark_core_source_available(&mut rows, owner);

        let (encoded, row_counts) = encode_all_rows(&rows);
        let segment_claims: [Vec<TypedPlaneSegmentPayloadV2<'_>>; 7] =
            core::array::from_fn(|index| {
                encoded[index]
                    .iter()
                    .map(|segment| {
                        TypedPlaneSegmentPayloadV2::new(
                            segment.first_key,
                            segment.last_key,
                            segment.row_count,
                            u64::try_from(segment.bytes.len()).expect("fixture size fits u64"),
                            UntrustedSemanticSegmentId::from_raw(*segment.id.as_bytes()),
                            &segment.bytes,
                        )
                    })
                    .collect()
            });
        let families = core::array::from_fn(|index| {
            TypedPlaneFamilyPayloadsV2::new(
                family_kinds()[index],
                row_counts[index],
                terminal_boundary_policy(),
                &segment_claims[index],
            )
        });
        let payloads = encoded
            .iter()
            .flat_map(|segments| segments.iter().map(|segment| segment.bytes.as_slice()))
            .collect::<Vec<_>>();
        let inventory = verify_semantic_typed_plane_inventory_v2_with_jumbo_source(
            build(),
            image_facts(),
            input_claim(),
            &families,
            &payloads,
            SemanticTypedPlaneVerificationLimitsV2::standard(),
            crate::ir::JumboRopeLimits::default(),
            &mut objects,
        )
        .expect("aggregate joins both jumbo owner families");
        let computed = crate::ir::VerifiedTypedPlaneContentV2::from_verified_inventory(inventory)
            .expect("verified inventory computes roots");
        let claims = cold_manifest_for_encoded_rows(
            &encoded,
            row_counts,
            *computed.content_root().as_bytes(),
            *computed.generation_root().as_bytes(),
        );
        let wire = claims.canonical_bytes().expect("encode c007 manifest");
        let cold = crate::ir::SemanticTypedPlaneManifestV2::decode(&wire)
            .expect("cold c007 manifest decodes");
        let reopened = crate::ir::verify_typed_plane_content_v2_with_jumbo_source(
            &cold,
            &payloads,
            crate::ir::SemanticTypedPlaneVerificationTierV2::Standard,
            crate::ir::JumboRopeLimits::default(),
            &mut objects,
        )
        .expect("cold verifier reopens exact ordered jumbo objects");
        assert_eq!(reopened.content_root(), computed.content_root());
        assert_eq!(reopened.generation_root(), computed.generation_root());

        assert!(matches!(
            crate::ir::verify_typed_plane_content_v2(&cold, &payloads),
            Err(crate::ir::SemanticGenerationProofError::JumboObjectSourceRequired)
        ));
        let missing = *objects.leaf_order.first().expect("stored jumbo leaf");
        objects.leaves.remove(&missing);
        assert!(matches!(
            crate::ir::verify_typed_plane_content_v2_with_jumbo_source(
                &cold,
                &payloads,
                crate::ir::SemanticTypedPlaneVerificationTierV2::Standard,
                crate::ir::JumboRopeLimits::default(),
                &mut objects,
            ),
            Err(crate::ir::SemanticGenerationProofError::TypedPlaneInventoryRejected { .. })
        ));
    }

    #[test]
    fn jumbo_documentation_edit_changes_only_its_family_root() {
        let original_text = patterned_text(1_200 * 1024);
        let mut edited_text = original_text.clone();
        edited_text[320 * 1024] = b'z';
        let mut original_objects = TestJumboObjects::default();
        let original_rows = rows_with_jumbo_docs(&original_text, &mut original_objects);
        let mut edited_objects = TestJumboObjects::default();
        let edited_rows = rows_with_jumbo_docs(&edited_text, &mut edited_objects);
        let original = verify_rows_with_jumbo_source(
            original_rows,
            &mut original_objects,
            crate::ir::JumboRopeLimits::default(),
        )
        .expect("original closure admits");
        let edited = verify_rows_with_jumbo_source(
            edited_rows,
            &mut edited_objects,
            crate::ir::JumboRopeLimits::default(),
        )
        .expect("edited closure admits");
        for (index, (before, after)) in original
            .families()
            .iter()
            .zip(edited.families())
            .enumerate()
        {
            if index == 4 {
                assert_ne!(before.semantic_row_root(), after.semantic_row_root());
            } else {
                assert_eq!(before.semantic_row_root(), after.semantic_row_root());
            }
        }
    }

    #[test]
    fn independent_reference_inventory_checks_exact_seven_family_census() {
        let first = identity(0x13);
        let rows = valid_rows(first, None, None, false, true);
        let expected_counts = [2, 3, 1, 1, 2, 3, 1];
        let inventory = verify_rows(rows).expect("complete reference inventory");
        for (family, expected_count) in inventory.families().iter().zip(expected_counts) {
            assert_eq!(family.row_count(), expected_count);
            assert_eq!(family.segments().len(), 1);
            assert!(family.segments()[0].admitted_id().as_bytes() != &[0; 32]);
        }
    }

    #[test]
    fn exact_reference_budget_does_not_scale_with_payload_bytes() {
        let first = identity(0x13);
        let rows = valid_rows(first, None, None, false, true);
        let inventory = verify_rows_with_policy(
            rows,
            None,
            SemanticTypedPlaneVerificationLimitsV2 {
                max_references: 12,
                ..SemanticTypedPlaneVerificationLimitsV2::standard()
            },
        )
        .expect("twelve explicit references fit despite larger payload framing");
        assert_eq!(inventory.families()[2].row_count(), 1);
    }

    #[test]
    fn aggregate_reference_budget_is_charged_before_duplicate_appends() {
        let first = identity(0x13);
        let second = identity(0x72);
        let mut rows = valid_rows(first, None, None, false, true);
        let relation = relation_key(first, second);
        rows[3].clear();
        for duplicate_rank in 0..32 {
            rows[3].push(occurrence_row_with_duplicate_rank(relation, duplicate_rank));
        }

        assert!(matches!(
            verify_rows_with_policy(
                rows,
                None,
                SemanticTypedPlaneVerificationLimitsV2 {
                    max_references: 12,
                    ..SemanticTypedPlaneVerificationLimitsV2::standard()
                },
            ),
            Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                budget: "reference-count"
            })
        ));
    }

    #[test]
    fn explicit_empty_occurrence_family_is_admitted() {
        let first = identity(0x13);
        let rows = valid_rows(first, None, None, false, false);
        let inventory = verify_rows(rows).expect("explicit empty relation-side families");
        assert_eq!(inventory.families()[2].row_count(), 0);
        assert!(inventory.families()[2].segments().is_empty());
        assert_eq!(inventory.families()[3].row_count(), 0);
        assert!(inventory.families()[3].segments().is_empty());
    }

    #[test]
    fn cold_reopen_accepts_seven_explicit_empty_families() {
        let rows: [Vec<Row>; 7] = core::array::from_fn(|_| Vec::new());
        let inventory = verify_rows(rows).expect("empty families remain explicit in c005");
        assert!(
            inventory
                .families()
                .iter()
                .all(|family| family.row_count() == 0 && family.segments().is_empty())
        );
    }

    #[test]
    fn lending_segment_verifier_matches_exact_seven_family_proof() {
        let rows = valid_rows(identity(0x13), None, None, false, true);
        let expected = verify_rows(rows.clone()).expect("materialized reference proof");
        let (observed, calls, expected_calls) = verify_rows_from_borrowed_source(&rows, None);
        let observed = observed.expect("lending source proof");
        let previous_two_pass_reads = expected_calls.saturating_mul(2);
        assert_eq!(
            calls.len(),
            expected_calls,
            "source reads fall from {previous_two_pass_reads} to one per descriptor"
        );
        assert_eq!(calls, (0..expected_calls).collect::<Vec<_>>());
        assert_eq!(observed.families().len(), expected.families().len());
        for (streamed, materialized) in observed.families().iter().zip(expected.families()) {
            assert_eq!(streamed.family(), materialized.family());
            assert_eq!(streamed.row_count(), materialized.row_count());
            assert_eq!(
                streamed.semantic_row_root(),
                materialized.semantic_row_root()
            );
            assert_eq!(streamed.segments().len(), materialized.segments().len());
            for (streamed_segment, materialized_segment) in
                streamed.segments().iter().zip(materialized.segments())
            {
                assert_eq!(
                    streamed_segment.admitted_id(),
                    materialized_segment.admitted_id()
                );
                assert_eq!(
                    streamed_segment.byte_length(),
                    materialized_segment.byte_length()
                );
            }
        }
    }

    #[test]
    fn lending_segment_verifier_matches_materialized_proof_across_multisegment_families() {
        let first = identity(0x13);
        let second = identity(0x72);
        let mut rows = valid_rows(first, None, None, false, true);
        rows[0] = vec![core_row(first, true), core_row(second, true)];
        rows[6].push(extension_row(second));
        mark_core_source_available(&mut rows, first);
        let source_slot = rows[5]
            .iter_mut()
            .find(|row| row.0 == first)
            .expect("fixture source declaration exists");
        *source_slot = source_declaration_row_with_inline_source(first, b"src/captured.rs", 3, 17);

        let kinds = family_kinds();
        let split_points_by_family = core::array::from_fn(|index| {
            let mut sorted_rows = rows[index].clone();
            sorted_rows.sort_unstable_by_key(|(key, _, _)| *key);
            fixture_policy_splits(kinds[index], &sorted_rows)
        });
        for family in [0, 1, 4, 5, 6] {
            assert!(
                !split_points_by_family[family].is_empty(),
                "family {:?} must exercise a split boundary",
                kinds[family]
            );
        }

        let materialized = verify_rows_with_family_splits(
            rows.clone(),
            split_points_by_family.clone(),
            SemanticTypedPlaneVerificationLimitsV2::standard(),
        )
        .expect("materialized independent proof");
        let mut sorted_core_rows = rows[0].clone();
        sorted_core_rows.sort_unstable_by_key(|(key, _, _)| *key);
        assert_eq!(split_points_by_family[0], [1]);
        assert_eq!(
            11 + 32 + 1 + 4 + sorted_core_rows[0].2.len(),
            11 + 32 + 1 + 4 + sorted_core_rows[1].2.len(),
            "the swapped Core payload has the exact same c004 byte length"
        );

        let total_rows = rows.iter().map(Vec::len).sum::<usize>();
        let decoder_rows = [0, 2, 3, 4, 5]
            .into_iter()
            .map(|family| rows[family].len())
            .sum::<usize>();
        let expected_record_visits =
            u64::try_from(total_rows * 2 + decoder_rows).expect("fixture visit count fits");
        let previous_record_visits = expected_record_visits
            .checked_add(u64::try_from(total_rows).expect("fixture row count fits"))
            .expect("previous traversal count fits");
        let encoded_row_bytes = rows
            .iter()
            .flatten()
            .map(|(_, _, payload)| u64::try_from(32 + 1 + 4 + payload.len()))
            .try_fold(0_u64, |total, bytes| total.checked_add(bytes.ok()?))
            .expect("fixture encoded row bytes fit");
        let decoder_row_bytes = [0, 2, 3, 4, 5]
            .into_iter()
            .flat_map(|family| rows[family].iter())
            .map(|(_, _, payload)| u64::try_from(32 + 1 + 4 + payload.len()))
            .try_fold(0_u64, |total, bytes| total.checked_add(bytes.ok()?))
            .expect("fixture decoder bytes fit");
        let expected_encoded_row_bytes = encoded_row_bytes
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(decoder_row_bytes))
            .expect("fixture traversed bytes fit");
        let previous_encoded_row_bytes = expected_encoded_row_bytes
            .checked_add(encoded_row_bytes)
            .expect("previous traversed bytes fit");

        super::super::reset_record_traversal_metrics();
        let expected_calls = split_points_by_family
            .iter()
            .enumerate()
            .map(|(index, splits)| (if rows[index].is_empty() { 0 } else { 1 }) + splits.len())
            .sum::<usize>();
        let (streamed, calls, observed_expected_calls) =
            verify_rows_from_borrowed_source_with_family_splits(
                &rows,
                None,
                SemanticTypedPlaneVerificationLimitsV2::standard(),
                None,
                split_points_by_family.clone(),
            );
        let streamed = streamed.expect("lending proof over split families");
        assert_eq!(observed_expected_calls, expected_calls);
        assert_eq!(calls.len(), expected_calls, "one source fetch per c004");
        assert_eq!(calls, (0..expected_calls).collect::<Vec<_>>());
        assert_eq!(
            super::super::record_traversal_metrics(),
            (expected_record_visits, expected_encoded_row_bytes),
            "record traversal work is measured separately from source fetches"
        );
        assert_eq!(
            previous_record_visits - expected_record_visits,
            u64::try_from(total_rows).expect("fixture row count fits"),
            "fusing the boundary stage removes one complete record walk"
        );
        assert_eq!(
            previous_encoded_row_bytes - expected_encoded_row_bytes,
            encoded_row_bytes,
            "the removed row walk is counted independently of the single source fetch"
        );

        let (wrong_id, _, _) = verify_rows_from_borrowed_source_with_family_splits(
            &rows,
            Some(0),
            SemanticTypedPlaneVerificationLimitsV2::standard(),
            None,
            split_points_by_family.clone(),
        );
        assert!(matches!(
            wrong_id,
            Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor {
                family: SemanticIrPlane::Core,
                index: 0,
            })
        ));
        let (corrupted, _, _) = verify_rows_from_borrowed_source_with_family_splits(
            &rows,
            Some(usize::MAX - 1),
            SemanticTypedPlaneVerificationLimitsV2::standard(),
            None,
            split_points_by_family,
        );
        assert!(matches!(
            corrupted,
            Err(SemanticTypedPlaneInventoryV2Error::SegmentDescriptor {
                family: SemanticIrPlane::Core,
                index: 0,
            })
        ));

        assert_eq!(streamed.families().len(), materialized.families().len());
        for (streamed_family, materialized_family) in
            streamed.families().iter().zip(materialized.families())
        {
            assert_eq!(streamed_family.family(), materialized_family.family());
            assert_eq!(streamed_family.row_count(), materialized_family.row_count());
            assert_eq!(
                streamed_family.semantic_row_root(),
                materialized_family.semantic_row_root()
            );
            assert_eq!(
                streamed_family.segments().len(),
                materialized_family.segments().len()
            );
            for (streamed_segment, materialized_segment) in streamed_family
                .segments()
                .iter()
                .zip(materialized_family.segments())
            {
                assert_eq!(
                    streamed_segment.admitted_id(),
                    materialized_segment.admitted_id()
                );
                assert_eq!(
                    streamed_segment.byte_length(),
                    materialized_segment.byte_length()
                );
            }
        }
    }

    #[test]
    fn lending_segment_verifier_reads_once_and_preserves_jumbo_cross_family_admission() {
        let owner = identity(0x13);
        let target = identity(0x72);
        let filler = patterned_text(1_200 * 1024);
        let mut persisted = TestJumboObjects::default();
        let mut rows = valid_rows(owner, None, None, false, true);
        let value = jumbo_docs_local_links(target, &filler, 2);
        let row =
            jumbo_docs_row_with_value(owner, &value, jumbo_docs_context(owner), &mut persisted);
        replace_documentation_row(&mut rows, owner, row);
        let exact_limits = SemanticTypedPlaneVerificationLimitsV2 {
            max_references: 14,
            ..SemanticTypedPlaneVerificationLimitsV2::standard()
        };

        let mut expected_objects = persisted.clone();
        let expected = verify_rows_with_jumbo_source_and_policy(
            rows.clone(),
            &mut expected_objects,
            crate::ir::JumboRopeLimits::default(),
            exact_limits,
        )
        .expect("jumbo local link and asymmetric family joins are valid");

        let mut streamed_objects = persisted.clone();
        let mut admission = JumboObjectClosureAdmissionV2::new(
            &mut streamed_objects,
            crate::ir::JumboRopeLimits::default(),
            exact_limits,
        );
        let (observed, calls, expected_calls) = verify_rows_from_borrowed_source_with_admission(
            &rows,
            None,
            exact_limits,
            Some(&mut admission),
        );
        let observed = observed.expect("one-pass jumbo inventory is valid");
        let previous_two_pass_reads = expected_calls.saturating_mul(2);
        assert_eq!(
            calls.len(),
            expected_calls,
            "jumbo source reads fall from {previous_two_pass_reads} to one per descriptor"
        );
        assert_eq!(calls, (0..expected_calls).collect::<Vec<_>>());
        for (streamed, materialized) in observed.families().iter().zip(expected.families()) {
            assert_eq!(streamed.family(), materialized.family());
            assert_eq!(streamed.row_count(), materialized.row_count());
            assert_eq!(
                streamed.semantic_row_root(),
                materialized.semantic_row_root()
            );
        }

        let under_budget_limits = SemanticTypedPlaneVerificationLimitsV2 {
            max_references: 13,
            ..SemanticTypedPlaneVerificationLimitsV2::standard()
        };
        let mut under_budget_objects = persisted.clone();
        let mut admission = JumboObjectClosureAdmissionV2::new(
            &mut under_budget_objects,
            crate::ir::JumboRopeLimits::default(),
            under_budget_limits,
        );
        let (under_budget, _, _) = verify_rows_from_borrowed_source_with_admission(
            &rows,
            None,
            under_budget_limits,
            Some(&mut admission),
        );
        assert!(
            matches!(
                under_budget,
                Err(SemanticTypedPlaneInventoryV2Error::AggregateBudget {
                    budget: "reference-count"
                })
            ),
            "unexpected reference-budget error: {:?}",
            under_budget.as_ref().err()
        );

        let missing_leaf = *persisted.leaf_order.first().expect("stored jumbo leaf");
        let mut partial_closure = persisted;
        partial_closure.leaves.remove(&missing_leaf);
        let mut admission = JumboObjectClosureAdmissionV2::new(
            &mut partial_closure,
            crate::ir::JumboRopeLimits::default(),
            exact_limits,
        );
        let (rejected, calls, _) = verify_rows_from_borrowed_source_with_admission(
            &rows,
            None,
            exact_limits,
            Some(&mut admission),
        );
        assert!(matches!(
            rejected,
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::JumboRope(crate::ir::JumboRopeError::MissingStoredLeaf)
            ))
        ));
        assert_eq!(calls, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn lending_segment_verifier_scopes_jumbo_admission_to_each_docs_segment() {
        let first = identity(0x13);
        let second = identity(0x72);
        let filler = patterned_text(1_200 * 1024);
        let mut persisted = TestJumboObjects::default();
        let mut rows = valid_rows(first, None, None, false, true);
        for (owner, target) in [(first, second), (second, first)] {
            let value = jumbo_docs_local_link(target, &filler);
            let row =
                jumbo_docs_row_with_value(owner, &value, jumbo_docs_context(owner), &mut persisted);
            replace_documentation_row(&mut rows, owner, row);
        }
        let limits = SemanticTypedPlaneVerificationLimitsV2 {
            max_references: 14,
            ..SemanticTypedPlaneVerificationLimitsV2::standard()
        };

        let mut expected_objects = persisted.clone();
        let expected = verify_rows_with_jumbo_source_and_policy(
            rows.clone(),
            &mut expected_objects,
            crate::ir::JumboRopeLimits::default(),
            limits,
        )
        .expect("both jumbo Docs rows and cross-family links are valid");

        let mut streamed_objects = persisted;
        let mut admission = JumboObjectClosureAdmissionV2::new(
            &mut streamed_objects,
            crate::ir::JumboRopeLimits::default(),
            limits,
        );
        let (observed, calls, expected_calls) =
            verify_rows_from_borrowed_source_with_admission_and_docs_split(
                &rows,
                None,
                limits,
                Some(&mut admission),
                Some(1),
            );
        let observed = observed.expect("two-segment Docs inventory is valid");
        assert_eq!(observed.families()[4].segments().len(), 2);
        assert_eq!(expected.families()[4].segments().len(), 1);
        assert_eq!(calls.len(), expected_calls);
        assert_eq!(calls, (0..expected_calls).collect::<Vec<_>>());
        assert_eq!(expected_calls, 8);
        assert_eq!(&calls[4..6], &[4, 5]);
        for (streamed, materialized) in observed.families().iter().zip(expected.families()) {
            assert_eq!(streamed.family(), materialized.family());
            assert_eq!(streamed.row_count(), materialized.row_count());
            assert_eq!(
                streamed.semantic_row_root(),
                materialized.semantic_row_root()
            );
        }
    }

    #[test]
    fn lending_segment_verifier_rejects_reordered_and_truncated_payloads() {
        let rows = valid_rows(identity(0x13), None, None, false, true);
        let (reordered, _, _) = verify_rows_from_borrowed_source(&rows, Some(0));
        assert!(
            reordered.is_err(),
            "reordered segment source must not mint proof"
        );
        let (truncated, _, _) = verify_rows_from_borrowed_source(&rows, Some(usize::MAX));
        assert!(
            truncated.is_err(),
            "truncated segment source must not mint proof"
        );
    }

    #[test]
    fn family_content_roots_ignore_segment_partitioning() {
        let first = identity(0x13);
        let rows = valid_rows(first, None, None, false, true);
        let unsplit = verify_rows(rows.clone()).expect("unsplit complete inventory");
        let split = verify_rows_with_split(rows, Some((0, 1))).expect("split complete inventory");

        for (unsplit_family, split_family) in unsplit.families().iter().zip(split.families()) {
            assert_eq!(
                unsplit_family.semantic_row_root(),
                split_family.semantic_row_root()
            );
            assert_eq!(unsplit_family.row_count(), split_family.row_count());
        }
        assert_eq!(unsplit.families()[0].segments().len(), 1);
        assert_eq!(split.families()[0].segments().len(), 2);
        assert_ne!(
            unsplit.families()[0].segments()[0].admitted_id(),
            split.families()[0].segments()[0].admitted_id()
        );
        let unsplit_content =
            crate::ir::VerifiedTypedPlaneContentV2::from_verified_inventory(unsplit)
                .expect("unsplit content identity");
        let split_content = crate::ir::VerifiedTypedPlaneContentV2::from_verified_inventory(split)
            .expect("split content identity");
        assert_eq!(unsplit_content.content_root(), split_content.content_root());
        assert_eq!(
            unsplit_content.generation_root(),
            split_content.generation_root()
        );
    }

    #[test]
    fn lower_hashes_cannot_hide_a_relation_to_unknown_declaration() {
        let unknown = identity(0xd1);
        let rows = valid_rows(unknown, None, None, false, true);
        assert!(matches!(
            verify_rows(rows),
            Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "relation declaration reference"
            })
        ));
    }

    #[test]
    fn lower_hashes_cannot_hide_missing_source_or_documentation_rows() {
        let first = identity(0x13);
        let missing_source = valid_rows(first, Some(identity(0x72)), None, false, true);
        assert!(matches!(
            verify_rows(missing_source),
            Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "declaration source census"
            })
        ));

        let missing_docs = valid_rows(first, None, Some(identity(0x72)), false, true);
        assert!(matches!(
            verify_rows(missing_docs),
            Err(SemanticTypedPlaneInventoryV2Error::CrossFamily {
                fact: "documentation declaration census"
            })
        ));
    }

    #[test]
    fn aggregate_rejects_valid_but_orphaned_types_rows() {
        let first = identity(0x13);
        let mut rows = valid_rows(first, None, None, false, true);
        rows[1].push(atom_row(b"unreferenced atom"));

        assert!(matches!(
            verify_rows(rows),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::ReaderReference
            ))
        ));
    }

    #[test]
    fn extension_owner_must_match_core_captured_availability() {
        let first = identity(0x13);
        let rows = valid_rows(first, None, None, true, true);
        assert!(matches!(
            verify_rows(rows),
            Err(SemanticTypedPlaneInventoryV2Error::Record(
                SemanticPlaneRecordError::RowGrammar
            ))
        ));
    }
}
