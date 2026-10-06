//! Closed, typed subcauses for semantic-fragment faults.
//!
//! This module is the sole owner of nested compiler-fault projection. Every
//! serialized operand is required by a concrete fault variant; arbitrary
//! source bytes, names, and nested `Display` strings are never retained.

use super::CompilerFragmentFaultFacts;
use backend_semantic::ir;
use serde::{Deserialize, Serialize};

/// Closed semantic-data lane retained by nested compiler errors.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentDataLane {
    Atoms,
    Products,
    Constructors,
    Lists,
    Children,
    AtomOrder,
    AtomMap,
    ProductOrder,
    ProductMap,
    Colors,
    NextColors,
    Hashes,
    NextHashes,
    Representatives,
    InternSlots,
}

/// Closed resource counter retained by a canonical semantic-data budget fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentDataResource {
    RefinementRounds,
    SortComparisons,
    HashEvaluations,
    InternProbes,
    Work,
}

/// Typed semantic/IR subcause with only the scalar operands needed to explain
/// and reproduce a failure. Variant names are stable wire cause tags.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "family",
    content = "fault",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NestedCompilerFaultFacts {
    EntityRecord(EntityRecordFaultFacts),
    TypeNode(TypeNodeFaultFacts),
    Atom(AtomFaultFacts),
    CanonicalData(CanonicalDataFaultFacts),
    SemanticData(SemanticDataFaultFacts),
    Occurrence(OccurrenceFaultFacts),
    SignatureCarrier(SignatureCarrierFaultFacts),
    Validation(ValidationFaultFacts),
}

/// Stable specific nested-cause tag. The tag is derived from the typed
/// `NestedCompilerFaultFacts` variant and is never independently stored.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentNestedFaultKind {
    EntityTypeReference,
    EntityNameReference,
    EntityKindTag,
    EntityReservedBits,
    TypeNodeReservedBytes,
    TypeNodeTag,
    TypeNodePrimitive,
    TypeNodeEdge,
    AtomRange,
    AtomEmpty,
    CanonicalDataCount,
    CanonicalDataNativeCount,
    CanonicalDataNativeWork,
    CanonicalDataCanonicalCountOverflow,
    CanonicalDataCanonicalCountMismatch,
    CanonicalDataScratch,
    CanonicalDataOutputTooSmall,
    CanonicalDataProductHead,
    CanonicalDataProductList,
    CanonicalDataConstructorCount,
    CanonicalDataConstructorTag,
    CanonicalDataConstructorReservedPayload,
    CanonicalDataConstructorArityOverflow,
    CanonicalDataConstructorArity,
    CanonicalDataProductChildRole,
    CanonicalDataListExtent,
    CanonicalDataNativeExtent,
    CanonicalDataProductChild,
    CanonicalDataOutputLength,
    CanonicalDataCanonicalListExtent,
    CanonicalDataCanonicalAtom,
    CanonicalDataCanonicalProduct,
    CanonicalDataCanonicalList,
    CanonicalDataRefinementBound,
    CanonicalDataInternTableFull,
    CanonicalDataInternEntry,
    CanonicalDataResourceCounterOverflow,
    CanonicalDataBudgetAdmission,
    CanonicalDataBudgetExceeded,
    SemanticDataHeader,
    SemanticDataAtomLength,
    SemanticDataProductHead,
    SemanticDataProductList,
    SemanticDataConstructorCount,
    SemanticDataEntityRootCount,
    SemanticDataEntityRoot,
    SemanticDataConstructorTag,
    SemanticDataConstructorReservedPayload,
    SemanticDataConstructorArityOverflow,
    SemanticDataConstructorArity,
    SemanticDataListExtent,
    SemanticDataChildRoleCode,
    SemanticDataChildRole,
    SemanticDataChildTag,
    SemanticDataLocalChild,
    SemanticDataLocalReserved,
    SemanticDataExternalAuthority,
    SemanticDataTrailing,
    OccurrenceOwner,
    OccurrenceLocalTarget,
    OccurrenceTargetTag,
    OccurrenceOriginTag,
    OccurrenceReferenceKind,
    OccurrenceConfidence,
    OccurrenceSpan,
    OccurrenceKindCell,
    OccurrenceEmptyPath,
    OccurrenceTruncated,
    OccurrenceTrailingBytes,
    OccurrenceLegacyStableTarget,
    OccurrenceAuthorityDomain,
    OccurrenceAuthorityWidth,
    ValidateTruncatedHeader,
    ValidateMagic,
    ValidateSchema,
    ValidateDeclaredLength,
    ValidateExtent,
    ValidateWireWidth,
    SignatureCarrierRoleCount,
    SignatureCarrierRoleKind,
    SignatureCarrierRoleOwnerKind,
    SignatureCarrierBindingOwnerSet,
    SignatureCarrierBindingSignature,
    SignatureCarrierBindingCounts,
    SignatureCarrierBindingEdgeRole,
    SignatureCarrierBindingTargetCount,
    SignatureCarrierBindingTargetKind,
    SignatureCarrierBindingType,
    SignatureCarrierBindingEdgeMismatch,
}

type Lane = CompilerFragmentDataLane;
type Resource = CompilerFragmentDataResource;

macro_rules! nested_fault_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident { $($field:ident : $ty:ty),* $(,)? }),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
        pub enum $name {
            $($variant { $($field: $ty),* }),*
        }
    };
}

nested_fault_enum! {
    /// Entity-record and entity-coordinate validation subcauses.
    EntityRecordFaultFacts {
        TypeReference { ordinal: u32, target: u32, node_count: u32 },
        NameReference { ordinal: u32, target: u32, atom_count: u32 },
        KindTag { ordinal: u32, actual: u16 },
        ReservedBits { ordinal: u32, actual: u16 },
    }
}

const fn nested(fault: NestedCompilerFaultFacts) -> CompilerFragmentFaultFacts {
    CompilerFragmentFaultFacts::Nested { fault }
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

const fn count_lane(value: ir::DataCountLane) -> CompilerFragmentDataLane {
    use ir::DataCountLane as L;
    match value {
        L::Atoms => Lane::Atoms,
        L::Products => Lane::Products,
        L::Constructors => Lane::Constructors,
        L::Lists => Lane::Lists,
        L::Children => Lane::Children,
    }
}

const fn scratch_lane(value: ir::DataScratchLane) -> CompilerFragmentDataLane {
    use ir::DataScratchLane as L;
    match value {
        L::AtomOrder => Lane::AtomOrder,
        L::AtomMap => Lane::AtomMap,
        L::ProductOrder => Lane::ProductOrder,
        L::ProductMap => Lane::ProductMap,
        L::Colors => Lane::Colors,
        L::NextColors => Lane::NextColors,
        L::Hashes => Lane::Hashes,
        L::NextHashes => Lane::NextHashes,
        L::Representatives => Lane::Representatives,
        L::InternSlots => Lane::InternSlots,
    }
}

const fn output_lane(value: ir::DataOutputLane) -> CompilerFragmentDataLane {
    count_lane(match value {
        ir::DataOutputLane::Atoms => ir::DataCountLane::Atoms,
        ir::DataOutputLane::Products => ir::DataCountLane::Products,
        ir::DataOutputLane::Constructors => ir::DataCountLane::Constructors,
        ir::DataOutputLane::Lists => ir::DataCountLane::Lists,
        ir::DataOutputLane::Children => ir::DataCountLane::Children,
    })
}

const fn resource(value: ir::DataResource) -> CompilerFragmentDataResource {
    use ir::DataResource as R;
    match value {
        R::RefinementRounds => Resource::RefinementRounds,
        R::SortComparisons => Resource::SortComparisons,
        R::HashEvaluations => Resource::HashEvaluations,
        R::InternProbes => Resource::InternProbes,
        R::Work => Resource::Work,
    }
}

const fn child_role(value: ir::ProductChildRole) -> u8 {
    use ir::ProductChildRole as R;
    match value {
        R::FunctionParameter => 0,
        R::FunctionResult => 1,
        R::GenericArgument => 2,
        R::TupleElement => 3,
        R::ArrayElement => 4,
        R::UnionMember => 5,
        R::IntersectionMember => 6,
        R::ProductMember => 7,
    }
}

const fn signature_role(value: ir::SignatureCarrierBindingRole) -> u8 {
    match value {
        ir::SignatureCarrierBindingRole::Parameter => 0,
        ir::SignatureCarrierBindingRole::Result => 1,
    }
}

pub(super) fn entity_record(
    ordinal: u32,
    fault: ir::EntityRecordFault,
) -> CompilerFragmentFaultFacts {
    use EntityRecordFaultFacts as E;
    let fault = match fault {
        ir::EntityRecordFault::Type(fault) => E::TypeReference {
            ordinal,
            target: fault.target.raw,
            node_count: fault.node_count,
        },
        ir::EntityRecordFault::Name(fault) => E::NameReference {
            ordinal,
            target: fault.target.raw,
            atom_count: fault.atom_count,
        },
        ir::EntityRecordFault::Kind { actual } => E::KindTag { ordinal, actual },
        ir::EntityRecordFault::Reserved { actual } => E::ReservedBits { ordinal, actual },
    };
    nested(NestedCompilerFaultFacts::EntityRecord(fault))
}

pub(super) fn type_node(ordinal: u32, fault: ir::TypeNodeFault) -> CompilerFragmentFaultFacts {
    use TypeNodeFaultFacts as T;
    let fault = match fault {
        ir::TypeNodeFault::Reserved { actual } => T::ReservedBytes {
            ordinal,
            packed: u32::from(actual[0])
                | (u32::from(actual[1]) << 8)
                | (u32::from(actual[2]) << 16),
        },
        ir::TypeNodeFault::Tag { actual } => T::Tag { ordinal, actual },
        ir::TypeNodeFault::Primitive { actual } => T::Primitive { ordinal, actual },
        ir::TypeNodeFault::Edge { target, node_count } => T::Edge {
            ordinal,
            target: target.raw,
            node_count,
        },
    };
    nested(NestedCompilerFaultFacts::TypeNode(fault))
}

pub(super) fn atom(fault: ir::AtomFault) -> CompilerFragmentFaultFacts {
    use AtomFaultFacts as A;
    let fault = match fault {
        ir::AtomFault::Range {
            ordinal,
            start,
            length,
            byte_count,
        } => A::Range {
            ordinal: ordinal.raw,
            start,
            length,
            byte_count,
        },
        ir::AtomFault::Empty { ordinal } => A::Empty {
            ordinal: ordinal.raw,
        },
    };
    nested(NestedCompilerFaultFacts::Atom(fault))
}

pub(super) fn canonical_data(fault: &ir::CanonicalDataError) -> CompilerFragmentFaultFacts {
    use CanonicalDataFaultFacts as C;
    use ir::CanonicalDataError as E;
    let fault = match fault {
        E::Count { lane, actual, .. } => C::Count {
            lane: count_lane(*lane),
            actual: usize_u64(*actual),
        },
        E::NativeCount { lane, actual, .. } => C::NativeCount {
            lane: count_lane(*lane),
            actual: *actual,
        },
        E::NativeWork { actual, .. } => C::NativeWork {
            actual: usize_u64(*actual),
        },
        E::CanonicalCountOverflow { lane } => C::CanonicalCountOverflow {
            lane: count_lane(*lane),
        },
        E::CanonicalCountMismatch {
            lane,
            expected,
            actual,
        } => C::CanonicalCountMismatch {
            lane: count_lane(*lane),
            expected: *expected,
            actual: *actual,
        },
        E::Scratch {
            lane,
            required,
            actual,
        } => C::Scratch {
            lane: scratch_lane(*lane),
            required: usize_u64(*required),
            available: usize_u64(*actual),
        },
        E::OutputTooSmall {
            lane,
            required,
            available,
        } => C::OutputTooSmall {
            lane: output_lane(*lane),
            required: usize_u64(*required),
            available: usize_u64(*available),
        },
        E::ProductHead {
            product,
            target,
            atom_count,
        } => C::ProductHead {
            product: product.raw,
            target: target.raw,
            atom_count: *atom_count,
        },
        E::ProductList {
            product,
            target,
            list_count,
        } => C::ProductList {
            lane: Lane::Lists,
            product: product.raw,
            target: target.raw,
            list_count: *list_count,
        },
        E::ConstructorCount {
            product_count,
            constructor_count,
        } => C::ConstructorCount {
            lane: Lane::Constructors,
            product_count: *product_count,
            constructor_count: *constructor_count,
        },
        E::ProductConstructor { product, fault } => match fault {
            ir::ProductConstructorFault::Tag { actual } => C::ConstructorTag {
                product: product.raw,
                actual: *actual,
            },
            ir::ProductConstructorFault::ReservedPayload {
                payload0, payload1, ..
            } => C::ConstructorReservedPayload {
                product: product.raw,
                payload0: *payload0,
                payload1: *payload1,
            },
            ir::ProductConstructorFault::ArityOverflow {
                payload0, payload1, ..
            } => C::ConstructorArityOverflow {
                product: product.raw,
                payload0: *payload0,
                payload1: *payload1,
            },
            ir::ProductConstructorFault::Arity {
                expected, actual, ..
            } => C::ConstructorArity {
                product: product.raw,
                expected: *expected,
                actual: *actual,
            },
        },
        E::ProductChildRole {
            product,
            list,
            child_ordinal,
            expected,
            actual,
        } => C::ProductChildRole {
            lane: Lane::Children,
            product: product.raw,
            list: list.raw,
            child_position: usize_u64(*child_ordinal),
            expected: child_role(*expected),
            actual: child_role(*actual),
        },
        E::ListExtent {
            list,
            span,
            pool_length,
        } => C::ListExtent {
            lane: Lane::Children,
            list: list.raw,
            start: span.start,
            length: span.length,
            child_count: usize_u64(*pool_length),
        },
        E::NativeExtent {
            list,
            span,
            pool_length,
            ..
        } => C::NativeExtent {
            lane: Lane::Children,
            list: list.raw,
            start: span.start,
            length: span.length,
            child_count: usize_u64(*pool_length),
        },
        E::ProductChild {
            product,
            list,
            child_ordinal,
            target,
            product_count,
        } => C::ProductChild {
            lane: Lane::Children,
            product: product.raw,
            list: list.raw,
            child_position: usize_u64(*child_ordinal),
            target: target.raw,
            product_count: *product_count,
        },
        E::OutputLength { lane, actual } => C::OutputLength {
            lane: output_lane(*lane),
            actual: usize_u64(*actual),
        },
        E::CanonicalListExtent {
            ordinal,
            span,
            fault,
        } => {
            let ir::PooledListError::OutOfBounds { pool_length, .. } = fault;
            C::CanonicalListExtent {
                lane: Lane::Children,
                ordinal: ordinal.raw,
                start: span.start,
                length: span.length,
                child_count: usize_u64(*pool_length),
            }
        }
        E::CanonicalAtom { ordinal, count } => C::CanonicalAtom {
            lane: Lane::Atoms,
            ordinal: ordinal.raw,
            count: usize_u64(*count),
        },
        E::CanonicalProduct { ordinal, count } => C::CanonicalProduct {
            lane: Lane::Products,
            ordinal: ordinal.raw,
            count: usize_u64(*count),
        },
        E::CanonicalList { ordinal, count } => C::CanonicalList {
            lane: Lane::Lists,
            ordinal: ordinal.raw,
            count: usize_u64(*count),
        },
        E::RefinementBound {
            rounds,
            product_count,
        } => C::RefinementBound {
            lane: Lane::Products,
            rounds: *rounds,
            product_count: *product_count,
        },
        E::InternTableFull { product, capacity } => C::InternTableFull {
            lane: Lane::InternSlots,
            product: product.raw,
            capacity: usize_u64(*capacity),
        },
        E::InternEntry { product, entry, .. } => C::InternEntry {
            lane: Lane::InternSlots,
            product: product.raw,
            entry: *entry,
        },
        E::ResourceCounterOverflow { resource } => C::ResourceCounterOverflow {
            resource: self::resource(*resource),
        },
        E::BudgetAdmission {
            resource,
            required,
            limit,
        } => C::BudgetAdmission {
            resource: self::resource(*resource),
            required: *required,
            limit: *limit,
        },
        E::BudgetExceeded {
            resource,
            observed,
            limit,
        } => C::BudgetExceeded {
            resource: self::resource(*resource),
            observed: *observed,
            limit: *limit,
        },
    };
    nested(NestedCompilerFaultFacts::CanonicalData(fault))
}

pub(super) fn semantic_data(fault: &ir::SemanticDataFault) -> CompilerFragmentFaultFacts {
    use SemanticDataFaultFacts as S;
    use ir::SemanticDataFault as E;
    let fault = match fault {
        E::Header { required, actual } => S::Header {
            required: usize_u64(*required),
            actual: usize_u64(*actual),
        },
        E::AtomLength {
            ordinal,
            length,
            available,
        } => S::AtomLength {
            ordinal: *ordinal,
            length: *length,
            available: usize_u64(*available),
        },
        E::ProductHead {
            product,
            target,
            atom_count,
        } => S::ProductHead {
            product: *product,
            target: *target,
            atom_count: *atom_count,
        },
        E::ProductList {
            product,
            target,
            list_count,
        } => S::ProductList {
            product: *product,
            target: *target,
            list_count: *list_count,
        },
        E::ConstructorCount {
            product_count,
            constructor_count,
        } => S::ConstructorCount {
            product_count: *product_count,
            constructor_count: *constructor_count,
        },
        E::EntityRootCount { expected, actual } => S::EntityRootCount {
            expected: *expected,
            actual: *actual,
        },
        E::EntityRoot {
            entity,
            target,
            product_count,
        } => S::EntityRoot {
            entity: *entity,
            target: *target,
            product_count: *product_count,
        },
        E::Constructor { product, fault } => match fault {
            ir::ProductConstructorFault::Tag { actual } => S::ConstructorTag {
                product: *product,
                actual: *actual,
            },
            ir::ProductConstructorFault::ReservedPayload {
                payload0, payload1, ..
            } => S::ConstructorReservedPayload {
                product: *product,
                payload0: *payload0,
                payload1: *payload1,
            },
            ir::ProductConstructorFault::ArityOverflow {
                payload0, payload1, ..
            } => S::ConstructorArityOverflow {
                product: *product,
                payload0: *payload0,
                payload1: *payload1,
            },
            ir::ProductConstructorFault::Arity {
                expected, actual, ..
            } => S::ConstructorArity {
                product: *product,
                expected: *expected,
                actual: *actual,
            },
        },
        E::ListExtent {
            list,
            start,
            length,
            child_count,
        } => S::ListExtent {
            list: *list,
            start: *start,
            length: *length,
            child_count: *child_count,
        },
        E::ChildRoleCode { child, actual } => S::ChildRoleCode {
            child: *child,
            actual: *actual,
        },
        E::ChildRole {
            child,
            expected,
            actual,
        } => S::ChildRole {
            child: *child,
            expected: child_role(*expected),
            actual: child_role(*actual),
        },
        E::ChildTag { child, actual } => S::ChildTag {
            child: *child,
            actual: *actual,
        },
        E::LocalChild {
            child,
            target,
            product_count,
        } => S::LocalChild {
            child: *child,
            target: *target,
            product_count: *product_count,
        },
        E::LocalReserved { child, .. } => S::LocalReserved { child: *child },
        E::ExternalAuthority {
            child,
            expected,
            observed,
            ..
        } => S::ExternalAuthority {
            child: *child,
            expected: *expected,
            observed: *observed,
        },
        E::Trailing { actual } => S::Trailing {
            actual: usize_u64(*actual),
        },
    };
    nested(NestedCompilerFaultFacts::SemanticData(fault))
}

pub(super) fn occurrence(fault: ir::OccurrenceViewFault) -> CompilerFragmentFaultFacts {
    use OccurrenceFaultFacts as O;
    let fault = match fault {
        ir::OccurrenceViewFault::Owner {
            ordinal,
            owner,
            entity_count,
        } => O::Owner {
            ordinal,
            owner,
            entity_count,
        },
        ir::OccurrenceViewFault::LocalTarget {
            ordinal,
            target,
            entity_count,
        } => O::LocalTarget {
            ordinal,
            target,
            entity_count,
        },
        ir::OccurrenceViewFault::TargetTag { ordinal, actual } => O::TargetTag { ordinal, actual },
        ir::OccurrenceViewFault::OriginTag { ordinal, actual } => O::OriginTag { ordinal, actual },
        ir::OccurrenceViewFault::ReferenceKind { ordinal, actual } => {
            O::ReferenceKind { ordinal, actual }
        }
        ir::OccurrenceViewFault::Confidence { ordinal, actual } => {
            O::Confidence { ordinal, actual }
        }
        ir::OccurrenceViewFault::Span {
            ordinal,
            start,
            end,
        } => O::Span {
            ordinal,
            start,
            end,
        },
        ir::OccurrenceViewFault::KindCell { ordinal, actual } => O::KindCell { ordinal, actual },
        ir::OccurrenceViewFault::EmptyPath { ordinal } => O::EmptyPath { ordinal },
        ir::OccurrenceViewFault::Truncated { ordinal, needed } => O::Truncated {
            ordinal,
            needed: usize_u64(needed),
        },
        ir::OccurrenceViewFault::TrailingBytes { declared } => O::TrailingBytes { declared },
        ir::OccurrenceViewFault::LegacyStableTarget { ordinal, schema } => {
            O::LegacyStableTarget { ordinal, schema }
        }
        ir::OccurrenceViewFault::AuthorityDomain {
            ordinal,
            expected,
            observed,
            ..
        } => O::AuthorityDomain {
            ordinal,
            expected: u8::from(expected),
            observed,
        },
        ir::OccurrenceViewFault::AuthorityWidth { ordinal, actual } => O::AuthorityWidth {
            ordinal,
            actual: usize_u64(actual),
        },
    };
    nested(NestedCompilerFaultFacts::Occurrence(fault))
}

pub(super) fn signature_carrier(error: &ir::BuildError) -> Option<CompilerFragmentFaultFacts> {
    use SignatureCarrierFaultFacts as S;
    use ir::BuildError as E;
    let fault = match error {
        E::SignatureCarrierRoleCount { expected, observed } => S::RoleCount {
            expected: usize_u64(*expected),
            observed: usize_u64(*observed),
        },
        E::SignatureCarrierRoleKind { entity, kind } => S::RoleKind {
            ordinal: entity.raw,
            actual: u16::from(*kind),
        },
        E::SignatureCarrierRoleOwnerKind { owner, kind } => S::RoleOwnerKind {
            owner: owner.raw,
            actual: u16::from(*kind),
        },
        E::SignatureCarrierBindingOwnerSet {
            row,
            expected,
            observed,
        } => S::BindingOwnerSet {
            row: usize_u64(*row),
            expected: (*expected).map(|entity| entity.raw),
            observed: (*observed).map(|entity| entity.raw),
        },
        E::SignatureCarrierBindingSignature { owner } => S::BindingSignature { owner: owner.raw },
        E::SignatureCarrierBindingCounts {
            owner,
            parameters,
            results,
        } => S::BindingCounts {
            owner: owner.raw,
            parameters: *parameters,
            results: *results,
        },
        E::SignatureCarrierBindingEdgeRole {
            owner,
            position,
            expected,
            observed,
        } => S::BindingEdgeRole {
            owner: owner.raw,
            position: *position,
            expected: child_role(*expected),
            observed: child_role(*observed),
        },
        E::SignatureCarrierBindingTargetCount { expected, observed } => S::BindingTargetCount {
            expected: usize_u64(*expected),
            observed: usize_u64(*observed),
        },
        E::SignatureCarrierBindingTargetKind {
            owner,
            carrier,
            kind,
        } => S::BindingTargetKind {
            owner: owner.raw,
            carrier: carrier.raw,
            actual: u16::from(*kind),
        },
        E::SignatureCarrierBindingType {
            owner,
            carrier,
            role,
            position,
        } => S::BindingType {
            owner: owner.raw,
            carrier: carrier.raw,
            role: signature_role(*role),
            position: *position,
        },
        E::SignatureCarrierBindingEdgeMismatch {
            owner,
            position,
            product_target,
            type_target,
        } => S::BindingEdgeMismatch {
            owner: owner.raw,
            position: *position,
            product_target: *product_target,
            type_target: *type_target,
        },
        _ => return None,
    };
    Some(nested(NestedCompilerFaultFacts::SignatureCarrier(fault)))
}

pub(super) fn validation(fault: ValidationFaultFacts) -> CompilerFragmentFaultFacts {
    nested(NestedCompilerFaultFacts::Validation(fault))
}

impl NestedCompilerFaultFacts {
    /// Stable closed subcause tag derived from this variant and its typed operands.
    #[must_use]
    pub const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        match self {
            Self::EntityRecord(fault) => fault.kind(),
            Self::TypeNode(fault) => fault.kind(),
            Self::Atom(fault) => fault.kind(),
            Self::CanonicalData(fault) => fault.kind(),
            Self::SemanticData(fault) => fault.kind(),
            Self::Occurrence(fault) => fault.kind(),
            Self::SignatureCarrier(fault) => fault.kind(),
            Self::Validation(fault) => fault.kind(),
        }
    }
}

impl EntityRecordFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::TypeReference { .. } => K::EntityTypeReference,
            Self::NameReference { .. } => K::EntityNameReference,
            Self::KindTag { .. } => K::EntityKindTag,
            Self::ReservedBits { .. } => K::EntityReservedBits,
        }
    }
}

impl TypeNodeFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::ReservedBytes { .. } => K::TypeNodeReservedBytes,
            Self::Tag { .. } => K::TypeNodeTag,
            Self::Primitive { .. } => K::TypeNodePrimitive,
            Self::Edge { .. } => K::TypeNodeEdge,
        }
    }
}

impl AtomFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::Range { .. } => K::AtomRange,
            Self::Empty { .. } => K::AtomEmpty,
        }
    }
}

impl CanonicalDataFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::Count { .. } => K::CanonicalDataCount,
            Self::NativeCount { .. } => K::CanonicalDataNativeCount,
            Self::NativeWork { .. } => K::CanonicalDataNativeWork,
            Self::CanonicalCountOverflow { .. } => K::CanonicalDataCanonicalCountOverflow,
            Self::CanonicalCountMismatch { .. } => K::CanonicalDataCanonicalCountMismatch,
            Self::Scratch { .. } => K::CanonicalDataScratch,
            Self::OutputTooSmall { .. } => K::CanonicalDataOutputTooSmall,
            Self::ProductHead { .. } => K::CanonicalDataProductHead,
            Self::ProductList { .. } => K::CanonicalDataProductList,
            Self::ConstructorCount { .. } => K::CanonicalDataConstructorCount,
            Self::ConstructorTag { .. } => K::CanonicalDataConstructorTag,
            Self::ConstructorReservedPayload { .. } => K::CanonicalDataConstructorReservedPayload,
            Self::ConstructorArityOverflow { .. } => K::CanonicalDataConstructorArityOverflow,
            Self::ConstructorArity { .. } => K::CanonicalDataConstructorArity,
            Self::ProductChildRole { .. } => K::CanonicalDataProductChildRole,
            Self::ListExtent { .. } => K::CanonicalDataListExtent,
            Self::NativeExtent { .. } => K::CanonicalDataNativeExtent,
            Self::ProductChild { .. } => K::CanonicalDataProductChild,
            Self::OutputLength { .. } => K::CanonicalDataOutputLength,
            Self::CanonicalListExtent { .. } => K::CanonicalDataCanonicalListExtent,
            Self::CanonicalAtom { .. } => K::CanonicalDataCanonicalAtom,
            Self::CanonicalProduct { .. } => K::CanonicalDataCanonicalProduct,
            Self::CanonicalList { .. } => K::CanonicalDataCanonicalList,
            Self::RefinementBound { .. } => K::CanonicalDataRefinementBound,
            Self::InternTableFull { .. } => K::CanonicalDataInternTableFull,
            Self::InternEntry { .. } => K::CanonicalDataInternEntry,
            Self::ResourceCounterOverflow { .. } => K::CanonicalDataResourceCounterOverflow,
            Self::BudgetAdmission { .. } => K::CanonicalDataBudgetAdmission,
            Self::BudgetExceeded { .. } => K::CanonicalDataBudgetExceeded,
        }
    }
}

impl SemanticDataFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::Header { .. } => K::SemanticDataHeader,
            Self::AtomLength { .. } => K::SemanticDataAtomLength,
            Self::ProductHead { .. } => K::SemanticDataProductHead,
            Self::ProductList { .. } => K::SemanticDataProductList,
            Self::ConstructorCount { .. } => K::SemanticDataConstructorCount,
            Self::EntityRootCount { .. } => K::SemanticDataEntityRootCount,
            Self::EntityRoot { .. } => K::SemanticDataEntityRoot,
            Self::ConstructorTag { .. } => K::SemanticDataConstructorTag,
            Self::ConstructorReservedPayload { .. } => K::SemanticDataConstructorReservedPayload,
            Self::ConstructorArityOverflow { .. } => K::SemanticDataConstructorArityOverflow,
            Self::ConstructorArity { .. } => K::SemanticDataConstructorArity,
            Self::ListExtent { .. } => K::SemanticDataListExtent,
            Self::ChildRoleCode { .. } => K::SemanticDataChildRoleCode,
            Self::ChildRole { .. } => K::SemanticDataChildRole,
            Self::ChildTag { .. } => K::SemanticDataChildTag,
            Self::LocalChild { .. } => K::SemanticDataLocalChild,
            Self::LocalReserved { .. } => K::SemanticDataLocalReserved,
            Self::ExternalAuthority { .. } => K::SemanticDataExternalAuthority,
            Self::Trailing { .. } => K::SemanticDataTrailing,
        }
    }
}

impl OccurrenceFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::Owner { .. } => K::OccurrenceOwner,
            Self::LocalTarget { .. } => K::OccurrenceLocalTarget,
            Self::TargetTag { .. } => K::OccurrenceTargetTag,
            Self::OriginTag { .. } => K::OccurrenceOriginTag,
            Self::ReferenceKind { .. } => K::OccurrenceReferenceKind,
            Self::Confidence { .. } => K::OccurrenceConfidence,
            Self::Span { .. } => K::OccurrenceSpan,
            Self::KindCell { .. } => K::OccurrenceKindCell,
            Self::EmptyPath { .. } => K::OccurrenceEmptyPath,
            Self::Truncated { .. } => K::OccurrenceTruncated,
            Self::TrailingBytes { .. } => K::OccurrenceTrailingBytes,
            Self::LegacyStableTarget { .. } => K::OccurrenceLegacyStableTarget,
            Self::AuthorityDomain { .. } => K::OccurrenceAuthorityDomain,
            Self::AuthorityWidth { .. } => K::OccurrenceAuthorityWidth,
        }
    }
}

impl SignatureCarrierFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::RoleCount { .. } => K::SignatureCarrierRoleCount,
            Self::RoleKind { .. } => K::SignatureCarrierRoleKind,
            Self::RoleOwnerKind { .. } => K::SignatureCarrierRoleOwnerKind,
            Self::BindingOwnerSet { .. } => K::SignatureCarrierBindingOwnerSet,
            Self::BindingSignature { .. } => K::SignatureCarrierBindingSignature,
            Self::BindingCounts { .. } => K::SignatureCarrierBindingCounts,
            Self::BindingEdgeRole { .. } => K::SignatureCarrierBindingEdgeRole,
            Self::BindingTargetCount { .. } => K::SignatureCarrierBindingTargetCount,
            Self::BindingTargetKind { .. } => K::SignatureCarrierBindingTargetKind,
            Self::BindingType { .. } => K::SignatureCarrierBindingType,
            Self::BindingEdgeMismatch { .. } => K::SignatureCarrierBindingEdgeMismatch,
        }
    }
}

impl ValidationFaultFacts {
    const fn kind(self) -> super::CompilerFragmentNestedFaultKind {
        use super::CompilerFragmentNestedFaultKind as K;
        match self {
            Self::TruncatedHeader { .. } => K::ValidateTruncatedHeader,
            Self::Magic { .. } => K::ValidateMagic,
            Self::Schema { .. } => K::ValidateSchema,
            Self::DeclaredLength { .. } => K::ValidateDeclaredLength,
            Self::Extent { .. } => K::ValidateExtent,
            Self::WireWidthDeclaredLength { .. }
            | Self::WireWidthSectionItemCount { .. }
            | Self::WireWidthSectionOffset { .. }
            | Self::WireWidthSectionByteLength { .. }
            | Self::WireWidthAtomStart { .. }
            | Self::WireWidthAtomLength { .. } => K::ValidateWireWidth,
        }
    }
}

nested_fault_enum! {
    /// Type-node validation subcauses.
    TypeNodeFaultFacts {
        ReservedBytes { ordinal: u32, packed: u32 },
        Tag { ordinal: u32, actual: u8 },
        Primitive { ordinal: u32, actual: u32 },
        Edge { ordinal: u32, target: u32, node_count: u32 },
    }
}

nested_fault_enum! {
    /// Atom-record validation subcauses.
    AtomFaultFacts {
        Range { ordinal: u32, start: u32, length: u32, byte_count: u32 },
        Empty { ordinal: u32 },
    }
}

nested_fault_enum! {
    /// Canonical semantic graph preparation subcauses.
    CanonicalDataFaultFacts {
        Count { lane: Lane, actual: u64 },
        NativeCount { lane: Lane, actual: u32 },
        NativeWork { actual: u64 },
        CanonicalCountOverflow { lane: Lane },
        CanonicalCountMismatch { lane: Lane, expected: u32, actual: u32 },
        Scratch { lane: Lane, required: u64, available: u64 },
        OutputTooSmall { lane: Lane, required: u64, available: u64 },
        ProductHead { product: u32, target: u32, atom_count: u32 },
        ProductList { lane: Lane, product: u32, target: u32, list_count: u32 },
        ConstructorCount { lane: Lane, product_count: u32, constructor_count: u32 },
        ConstructorTag { product: u32, actual: u32 },
        ConstructorReservedPayload { product: u32, payload0: u32, payload1: u32 },
        ConstructorArityOverflow { product: u32, payload0: u32, payload1: u32 },
        ConstructorArity { product: u32, expected: u32, actual: u32 },
        ProductChildRole { lane: Lane, product: u32, list: u32, child_position: u64, expected: u8, actual: u8 },
        ListExtent { lane: Lane, list: u32, start: u32, length: u32, child_count: u64 },
        NativeExtent { lane: Lane, list: u32, start: u32, length: u32, child_count: u64 },
        ProductChild { lane: Lane, product: u32, list: u32, child_position: u64, target: u32, product_count: u32 },
        OutputLength { lane: Lane, actual: u64 },
        CanonicalListExtent { lane: Lane, ordinal: u32, start: u32, length: u32, child_count: u64 },
        CanonicalAtom { lane: Lane, ordinal: u32, count: u64 },
        CanonicalProduct { lane: Lane, ordinal: u32, count: u64 },
        CanonicalList { lane: Lane, ordinal: u32, count: u64 },
        RefinementBound { lane: Lane, rounds: u32, product_count: u32 },
        InternTableFull { lane: Lane, product: u32, capacity: u64 },
        InternEntry { lane: Lane, product: u32, entry: u64 },
        ResourceCounterOverflow { resource: Resource },
        BudgetAdmission { resource: Resource, required: u64, limit: u64 },
        BudgetExceeded { resource: Resource, observed: u64, limit: u64 },
    }
}

nested_fault_enum! {
    /// Semantic-data reopening subcauses.
    SemanticDataFaultFacts {
        Header { required: u64, actual: u64 },
        AtomLength { ordinal: u32, length: u32, available: u64 },
        ProductHead { product: u32, target: u32, atom_count: u32 },
        ProductList { product: u32, target: u32, list_count: u32 },
        ConstructorCount { product_count: u32, constructor_count: u32 },
        EntityRootCount { expected: u32, actual: u32 },
        EntityRoot { entity: u32, target: u32, product_count: u32 },
        ConstructorTag { product: u32, actual: u32 },
        ConstructorReservedPayload { product: u32, payload0: u32, payload1: u32 },
        ConstructorArityOverflow { product: u32, payload0: u32, payload1: u32 },
        ConstructorArity { product: u32, expected: u32, actual: u32 },
        ListExtent { list: u32, start: u32, length: u32, child_count: u32 },
        ChildRoleCode { child: u32, actual: u8 },
        ChildRole { child: u32, expected: u8, actual: u8 },
        ChildTag { child: u32, actual: u8 },
        LocalChild { child: u32, target: u32, product_count: u32 },
        LocalReserved { child: u32 },
        ExternalAuthority { child: u32, expected: u8, observed: u8 },
        Trailing { actual: u64 },
    }
}

nested_fault_enum! {
    /// Occurrence-plane reopening subcauses.
    OccurrenceFaultFacts {
        Owner { ordinal: u32, owner: u32, entity_count: u32 },
        LocalTarget { ordinal: u32, target: u32, entity_count: u32 },
        TargetTag { ordinal: u32, actual: u8 },
        OriginTag { ordinal: u32, actual: u8 },
        ReferenceKind { ordinal: u32, actual: u8 },
        Confidence { ordinal: u32, actual: u8 },
        Span { ordinal: u32, start: u32, end: u32 },
        KindCell { ordinal: u32, actual: u16 },
        EmptyPath { ordinal: u32 },
        Truncated { ordinal: u32, needed: u64 },
        TrailingBytes { declared: u32 },
        LegacyStableTarget { ordinal: u32, schema: u16 },
        AuthorityDomain { ordinal: u32, expected: u8, observed: u8 },
        AuthorityWidth { ordinal: u32, actual: u64 },
    }
}

nested_fault_enum! {
    /// Signature-carrier binding subcauses.
    SignatureCarrierFaultFacts {
        RoleCount { expected: u64, observed: u64 },
        RoleKind { ordinal: u32, actual: u16 },
        RoleOwnerKind { owner: u32, actual: u16 },
        BindingOwnerSet { row: u64, expected: Option<u32>, observed: Option<u32> },
        BindingSignature { owner: u32 },
        BindingCounts { owner: u32, parameters: u32, results: u32 },
        BindingEdgeRole { owner: u32, position: u32, expected: u8, observed: u8 },
        BindingTargetCount { expected: u64, observed: u64 },
        BindingTargetKind { owner: u32, carrier: u32, actual: u16 },
        BindingType { owner: u32, carrier: u32, role: u8, position: u32 },
        BindingEdgeMismatch { owner: u32, position: u32, product_target: u32, type_target: u32 },
    }
}

nested_fault_enum! {
    /// Fragment header and extent validation subcauses.
    ValidationFaultFacts {
        TruncatedHeader { required: u64, actual: u64 },
        Magic { actual: u32 },
        Schema { actual: u16 },
        DeclaredLength { declared: u64, actual: u64 },
        Extent { required: u64, actual: u64 },
        WireWidthDeclaredLength { actual: u32 },
        WireWidthSectionItemCount { ordinal: u16, actual: u32 },
        WireWidthSectionOffset { ordinal: u16, actual: u32 },
        WireWidthSectionByteLength { ordinal: u16, actual: u32 },
        WireWidthAtomStart { ordinal: u32, actual: u32 },
        WireWidthAtomLength { ordinal: u32, actual: u32 },
    }
}
