//! Normalized reachable typed closure for the SPIR Types plane.
//!
//! The closure seeds entity type associations, typed/list references owned by
//! language-extension facts, and the complete external-target catalog. It
//! follows every child row iteratively. Unreferenced type/list/atom rows are
//! intentionally omitted: this is V2 normalized reachable semantics, not a
//! bytewise or census-equivalent reconstruction of legacy NXFI pools.
//! `TextId` is documentation-owned and has no typed/extension reference here;
//! documentation rows retain their UTF-8 values in the Documentation family.

mod catalog;
mod plan;
#[cfg(test)]
mod tests;
mod wire;

pub(super) use crate::ir::declaration_plane_key;
pub(super) use catalog::CheckedTypesFamilyV2Builder;
pub use catalog::{
    CheckedTypesFamilyV2, TypesFamilyVerificationLimitsV2, TypesReferenceV2, TypesRowDomainV2,
    validate_types_family_v2, validate_types_family_v2_with_limits,
};
pub(super) use catalog::{
    TypesFamilyValidationError, validate_types_family_v2_with_limits_detailed,
};
pub use plan::{TypedRecordPlan, TypesClosureSemantics, TypesRowHandle, TypesRows};
pub(super) fn validate_record(
    kind: crate::ir::SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), crate::ir::SemanticPlaneRecordError> {
    wire::validate_record(kind, key, tag, payload)
}

const ROOT_TAG: u8 = 1;
const TYPE_TAG: u8 = 2;
const TYPE_LIST_TAG: u8 = 3;
const TUPLE_ELEMENTS_TAG: u8 = 4;
const OBJECT_MEMBERS_TAG: u8 = 5;
const TEMPLATE_PARTS_TAG: u8 = 6;
const ATOM_LIST_TAG: u8 = 7;
const TYPE_PARAMETERS_TAG: u8 = 8;
const TYPE_PARAMETER_BOUNDS_TAG: u8 = 9;
const FREE_PREDICATES_TAG: u8 = 10;
const ATOM_TAG: u8 = 11;
const EXTERNAL_TARGET_TAG: u8 = 12;
const TYPES_PLANE_CODE: u8 = 2;
const TYPES_ROW_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.types-row-key.v1\0";
