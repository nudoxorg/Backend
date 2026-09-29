//! Whole-manifest semantic checks for typed-plane V2.
//!
//! This module is the only constructor for the opaque content inventory token.
//! It validates each exact SPIR payload, enforces bounded aggregate work, and
//! reconciles all seven family censuses and references before retaining only
//! content summaries. A decoded inventory is semantic content evidence; it is
//! not an owner admission or selection capability.

use alloc::{boxed::Box, vec::Vec};

use crate::ir::row_index::{
    RowFamily, RowPayload, StableRowIndex, StableRowIndexError, StableRowKey,
};
use crate::ir::{
    CanonicalSemanticPlaneSegmentView, ImageProvenance, SemanticBuildIdentity,
    SemanticImageAuthority, SemanticImageFacts, SemanticInputWitness, SemanticIrPlane,
    SemanticPlaneKind, SemanticPlaneRecordError, SemanticPlaneSegment, SemanticSegmentId,
    UntrustedSemanticSegmentId, decode_semantic_plane_segment,
};
use crate::vocabulary::{CompileRecipeFact, LanguageProfile};
use thiserror::Error;

use super::wire::{Cursor, read_identity};
use super::{
    LanguageExtensionVerificationLimitsV2, TypesFamilyVerificationLimitsV2,
    validate_language_extension_family_v2_with_limits, validate_types_family_v2_with_limits,
};

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
}

impl SemanticTypedPlaneVerificationLimitsV2 {
    /// Conservative bound for an ordinary local verification window.
    pub(crate) const fn standard() -> Self {
        Self {
            max_segments: 8_192,
            max_total_bytes: 64 * 1024 * 1024,
            max_total_rows: 250_000,
            max_references: 1_000_000,
        }
    }

    /// Higher bounded tier for packages with larger semantic closures.
    pub(crate) const fn large_package() -> Self {
        Self {
            max_segments: 65_536,
            max_total_bytes: 512 * 1024 * 1024,
            max_total_rows: 2_000_000,
            max_references: 8_000_000,
        }
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
    pub(crate) segments: &'bytes [TypedPlaneSegmentPayloadV2<'bytes>],
}

impl<'bytes> TypedPlaneFamilyPayloadsV2<'bytes> {
    pub(crate) const fn new(
        family: SemanticIrPlane,
        row_count: u64,
        segments: &'bytes [TypedPlaneSegmentPayloadV2<'bytes>],
    ) -> Self {
        Self {
            family,
            row_count,
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
    #[error("typed semantic manifest input/read claim is not Complete")]
    IncompleteInputClaim,
    #[error("typed semantic build and image facts disagree at {field}")]
    ImageFactsBuildMismatch { field: &'static str },
    #[error("typed semantic aggregate exceeds its {budget} budget")]
    AggregateBudget { budget: &'static str },
    #[error(transparent)]
    RowIndex(#[from] StableRowIndexError),
    #[error("typed semantic cross-family closure is inconsistent: {fact}")]
    CrossFamily { fact: &'static str },
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

    for (family_index, family) in families.iter().enumerate() {
        let kind = SemanticPlaneKind::Ir(family.family);
        let mut verified_segments = Vec::new();
        verified_segments
            .try_reserve_exact(family.segments.len())
            .map_err(SemanticPlaneRecordError::Allocation)?;
        let mut observed_rows = 0_u64;
        let row_family = stable_row_family(family.family);
        let mut row_index_builder = StableRowIndex::builder();
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
            )?;
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
            let view = decode_semantic_plane_segment(kind, &descriptor, payload)?;
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
                let key = StableRowKey::new(row_family, *record.key());
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

    validate_cross_family_closure(build.profile(), &decoded_families, limits)?;
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
    reference_count: u64,
}

#[derive(Default)]
struct OccurrenceFamilyFacts {
    relation_references: Vec<[u8; 32]>,
    reference_count: u64,
}

fn validate_cross_family_closure(
    profile: LanguageProfile,
    families: &[Vec<CanonicalSemanticPlaneSegmentView<'_>>; 7],
    limits: SemanticTypedPlaneVerificationLimitsV2,
) -> Result<u64, SemanticTypedPlaneInventoryV2Error> {
    let mut core = decode_core(&families[0])?;
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
        limits.max_references,
    );
    let types = validate_types_family_v2_with_limits(families[1].iter().copied(), family_limits)?;
    let relations = decode_relations(&families[2])?;
    let occurrences = decode_occurrences(&families[3])?;
    let docs = decode_documentation(&families[4])?;
    let source = decode_source_provenance(&families[5])?;
    let extension_limits = LanguageExtensionVerificationLimitsV2::bounded(
        limits.max_total_bytes,
        limits.max_total_rows,
        limits.max_references,
    );
    let extensions = validate_language_extension_family_v2_with_limits(
        profile,
        families[6].iter().copied(),
        &types,
        &core.captured_extension_owners,
        extension_limits,
    )?;
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
    require_core_refs(
        &docs.local_references,
        &core.declarations,
        "documentation local link",
    )?;

    let external_keys = types.external_target_keys();
    require_local_refs(
        &relations.external_references,
        external_keys,
        "relation external target",
    )?;
    require_local_refs(
        &docs.external_references,
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
) -> Result<CoreFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = CoreFamilyFacts::default();
    for segment in segments {
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
                1 => try_push(
                    &mut facts.local_references,
                    identity_key(read_identity(&mut cursor)?),
                )?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
            match cursor.u8()? {
                0 | 1 => {}
                2 => try_push(
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
                try_push(
                    &mut facts.local_references,
                    identity_key(read_identity(&mut cursor)?),
                )?;
            }
            let attributes = cursor.u32()?;
            for _ in 0..attributes {
                let _ = cursor.bytes32()?;
            }
            if !cursor.is_empty() || identity != record.key() {
                return Err(SemanticPlaneRecordError::StableKeyMismatch.into());
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
) -> Result<DocumentationFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = DocumentationFamilyFacts::default();
    for segment in segments {
        for record in segment.records() {
            if record.tag() != DOCUMENTATION_TAG {
                return Err(SemanticPlaneRecordError::RowGrammar.into());
            }
            let mut cursor = Cursor::new(record.payload());
            let identity = identity_key(read_identity(&mut cursor)?);
            let available = read_availability(&mut cursor)?;
            let fragments = cursor.u32()?;
            for _ in 0..fragments {
                match cursor.u8()? {
                    0 | 1 => {
                        let _ = cursor.utf8()?;
                    }
                    2 => {
                        let _ = cursor.utf8()?;
                        match cursor.u8()? {
                            0 => try_push(
                                &mut facts.local_references,
                                identity_key(read_identity(&mut cursor)?),
                            )?,
                            1 => try_push(
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
            if !cursor.is_empty() || identity != record.key() {
                return Err(SemanticPlaneRecordError::StableKeyMismatch.into());
            }
            try_push(&mut facts.declarations, (identity, available))?;
        }
    }
    facts.reference_count = reference_count_for_items(
        facts
            .declarations
            .len()
            .checked_add(facts.local_references.len())
            .and_then(|count| count.checked_add(facts.external_references.len()))
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
) -> Result<SourceFamilyFacts, SemanticTypedPlaneInventoryV2Error> {
    let mut facts = SourceFamilyFacts::default();
    for segment in segments {
        for record in segment.records() {
            let mut cursor = Cursor::new(record.payload());
            match record.tag() {
                SOURCE_DECLARATION_TAG => {
                    let identity = identity_key(read_identity(&mut cursor)?);
                    let available = read_availability(&mut cursor)?;
                    if !cursor.is_empty() || identity != record.key() {
                        return Err(SemanticPlaneRecordError::StableKeyMismatch.into());
                    }
                    try_push(&mut facts.declaration_keys, identity)?;
                    try_push(&mut facts.declaration_source, (identity, available))?;
                }
                SOURCE_RELATION_TAG => {
                    let relation: [u8; 32] = cursor
                        .take(32)?
                        .try_into()
                        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
                    let available = read_availability(&mut cursor)?;
                    if available {
                        let _ = cursor.bytes32()?;
                        let start = cursor.u32()?;
                        let end = cursor.u32()?;
                        if start > end {
                            return Err(SemanticPlaneRecordError::RowGrammar.into());
                        }
                    }
                    if !cursor.is_empty() || relation_source_row_key(relation) != record.key() {
                        return Err(SemanticPlaneRecordError::StableKeyMismatch.into());
                    }
                    try_push(&mut facts.relation_keys, relation)?;
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
            try_push(&mut facts.references, from)?;
            match target_kind {
                0 => try_push(&mut facts.references, target)?,
                1 => try_push(&mut facts.external_references, target)?,
                _ => return Err(SemanticPlaneRecordError::RowGrammar.into()),
            }
            try_push(&mut facts.keys, record.key())?;
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
            try_push(&mut references, relation)?;
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
    if references.iter().any(|identity| {
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
    if references
        .iter()
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
    use alloc::{vec, vec::Vec};

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
        key_hasher.update(&0_u32.to_be_bytes());
        let key = *key_hasher.finalize().as_bytes();
        base.extend_from_slice(&0_u32.to_be_bytes()); // duplicate rank
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
        bytes.extend_from_slice(&1_u16.to_be_bytes());
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
        let kinds = family_kinds();
        let row_counts = rows
            .each_ref()
            .map(|family| u64::try_from(family.len()).expect("fixture row count fits u64"));
        let encoded: [Vec<EncodedSegment>; 7] = core::array::from_fn(|index| {
            let mut family_rows = rows[index].clone();
            family_rows.sort_unstable_by_key(|(key, _, _)| *key);
            if let Some((split_family, split_at)) = split
                && split_family == index
                && split_at > 0
                && split_at < family_rows.len()
            {
                let right_rows = family_rows.split_off(split_at);
                vec![
                    encode_family_segment(kinds[index], family_rows),
                    encode_family_segment(kinds[index], right_rows),
                ]
            } else if family_rows.is_empty() {
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
                max_segments: 8_192,
                max_total_bytes: 64 * 1024 * 1024,
                max_total_rows: 250_000,
                max_references: 12,
            },
        )
        .expect("twelve explicit references fit despite larger payload framing");
        assert_eq!(inventory.families()[2].row_count(), 1);
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
