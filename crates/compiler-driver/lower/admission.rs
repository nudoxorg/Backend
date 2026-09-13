//! Shared value-only projection of canonical fact-admission faults.
//!
//! Language lowerers use this one converter so every authority retains the
//! same typed admission cause and exact bounded operands.

use crate::types::{FactFault, ParentageState, SourceSpanFact, TypeChildLane};
use backend_semantic::ir::{
    ProductChildRole, ProductConstructorFault, ProductConstructorTag, SemanticTypeFault,
    SemanticTypeTag, TypeCell,
};
use backend_semantic::vocabulary::{
    ProjectionAdmissionFault, ProjectionChildRole, ProjectionConstructorFault,
    ProjectionConstructorTag, ProjectionParentageState, ProjectionSemanticTypeFault,
    ProjectionSemanticTypeTag, ProjectionSpan, ProjectionTypeCell, ProjectionTypeChildLane,
};

// Every supported Rust target has a `usize` no wider than the portable
// terminal's `u64` operands.  Keep this as a compile-time proof so widening
// never silently truncates a fact-admission coordinate and never needs a
// panic/fabricated overflow payload at the terminal boundary.
const _: () = assert!(usize::BITS <= u64::BITS);

#[inline]
pub(crate) fn portable_count(value: usize) -> u64 {
    value as u64
}

fn portable_constructor_tag(tag: ProductConstructorTag) -> ProjectionConstructorTag {
    match tag {
        ProductConstructorTag::Function => ProjectionConstructorTag::Function,
        ProductConstructorTag::Generic => ProjectionConstructorTag::Generic,
        ProductConstructorTag::Tuple => ProjectionConstructorTag::Tuple,
        ProductConstructorTag::Array => ProjectionConstructorTag::Array,
        ProductConstructorTag::Union => ProjectionConstructorTag::Union,
        ProductConstructorTag::Intersection => ProjectionConstructorTag::Intersection,
        ProductConstructorTag::Product => ProjectionConstructorTag::Product,
    }
}

fn portable_constructor_fault(fault: ProductConstructorFault) -> ProjectionConstructorFault {
    match fault {
        ProductConstructorFault::Tag { actual } => ProjectionConstructorFault::Tag { actual },
        ProductConstructorFault::ReservedPayload {
            tag,
            payload0,
            payload1,
        } => ProjectionConstructorFault::ReservedPayload {
            tag: portable_constructor_tag(tag),
            payload0,
            payload1,
        },
        ProductConstructorFault::ArityOverflow {
            tag,
            payload0,
            payload1,
        } => ProjectionConstructorFault::ArityOverflow {
            tag: portable_constructor_tag(tag),
            payload0,
            payload1,
        },
        ProductConstructorFault::Arity {
            tag,
            expected,
            actual,
        } => ProjectionConstructorFault::Arity {
            tag: portable_constructor_tag(tag),
            expected,
            actual,
        },
    }
}

fn portable_child_role(role: ProductChildRole) -> ProjectionChildRole {
    match role {
        ProductChildRole::FunctionParameter => ProjectionChildRole::FunctionParameter,
        ProductChildRole::FunctionResult => ProjectionChildRole::FunctionResult,
        ProductChildRole::GenericArgument => ProjectionChildRole::GenericArgument,
        ProductChildRole::TupleElement => ProjectionChildRole::TupleElement,
        ProductChildRole::ArrayElement => ProjectionChildRole::ArrayElement,
        ProductChildRole::UnionMember => ProjectionChildRole::UnionMember,
        ProductChildRole::IntersectionMember => ProjectionChildRole::IntersectionMember,
        ProductChildRole::ProductMember => ProjectionChildRole::ProductMember,
    }
}

fn portable_type_tag(tag: SemanticTypeTag) -> ProjectionSemanticTypeTag {
    match tag {
        SemanticTypeTag::SelfType => ProjectionSemanticTypeTag::SelfType,
        SemanticTypeTag::Primitive => ProjectionSemanticTypeTag::Primitive,
        SemanticTypeTag::Tuple => ProjectionSemanticTypeTag::Tuple,
        SemanticTypeTag::Slice => ProjectionSemanticTypeTag::Slice,
        SemanticTypeTag::Array => ProjectionSemanticTypeTag::Array,
        SemanticTypeTag::Union => ProjectionSemanticTypeTag::Union,
        SemanticTypeTag::Intersection => ProjectionSemanticTypeTag::Intersection,
        SemanticTypeTag::Never => ProjectionSemanticTypeTag::Never,
        SemanticTypeTag::Any => ProjectionSemanticTypeTag::Any,
        SemanticTypeTag::Unknown => ProjectionSemanticTypeTag::Unknown,
        SemanticTypeTag::Nominal => ProjectionSemanticTypeTag::Nominal,
        SemanticTypeTag::Apply => ProjectionSemanticTypeTag::Apply,
        SemanticTypeTag::TypeVar => ProjectionSemanticTypeTag::TypeVar,
        SemanticTypeTag::Wildcard => ProjectionSemanticTypeTag::Wildcard,
        SemanticTypeTag::FunctionPointer => ProjectionSemanticTypeTag::FunctionPointer,
        SemanticTypeTag::Annotated => ProjectionSemanticTypeTag::Annotated,
        SemanticTypeTag::Conditional => ProjectionSemanticTypeTag::Conditional,
        SemanticTypeTag::Mapped => ProjectionSemanticTypeTag::Mapped,
        SemanticTypeTag::TemplateLiteral => ProjectionSemanticTypeTag::TemplateLiteral,
        SemanticTypeTag::AnonymousRecord => ProjectionSemanticTypeTag::AnonymousRecord,
        SemanticTypeTag::ImplTrait => ProjectionSemanticTypeTag::ImplTrait,
        SemanticTypeTag::DynTrait => ProjectionSemanticTypeTag::DynTrait,
        SemanticTypeTag::Inferred => ProjectionSemanticTypeTag::Inferred,
        SemanticTypeTag::QualifiedPath => ProjectionSemanticTypeTag::QualifiedPath,
        SemanticTypeTag::Map => ProjectionSemanticTypeTag::Map,
        SemanticTypeTag::Channel => ProjectionSemanticTypeTag::Channel,
        SemanticTypeTag::ArraySequence => ProjectionSemanticTypeTag::ArraySequence,
        SemanticTypeTag::ArrayRectangular => ProjectionSemanticTypeTag::ArrayRectangular,
        SemanticTypeTag::ArrayFixed => ProjectionSemanticTypeTag::ArrayFixed,
        SemanticTypeTag::ArrayConstExpression => ProjectionSemanticTypeTag::ArrayConstExpression,
        SemanticTypeTag::ArrayIncomplete => ProjectionSemanticTypeTag::ArrayIncomplete,
        SemanticTypeTag::CQualified => ProjectionSemanticTypeTag::CQualified,
    }
}

fn portable_type_cell(cell: TypeCell) -> ProjectionTypeCell {
    match cell {
        TypeCell::Payload0 => ProjectionTypeCell::Payload0,
        TypeCell::Payload1 => ProjectionTypeCell::Payload1,
        TypeCell::Text => ProjectionTypeCell::Text,
        TypeCell::Text2 => ProjectionTypeCell::Text2,
        TypeCell::Nominal => ProjectionTypeCell::Nominal,
    }
}

fn portable_type_fault(fault: SemanticTypeFault) -> ProjectionSemanticTypeFault {
    match fault {
        SemanticTypeFault::Tag { actual } => ProjectionSemanticTypeFault::Tag { actual },
        SemanticTypeFault::ReservedCell { tag, cell, actual } => {
            ProjectionSemanticTypeFault::ReservedCell {
                tag: portable_type_tag(tag),
                cell: portable_type_cell(cell),
                actual,
            }
        }
        SemanticTypeFault::MissingCell { tag, cell } => ProjectionSemanticTypeFault::MissingCell {
            tag: portable_type_tag(tag),
            cell: portable_type_cell(cell),
        },
        SemanticTypeFault::Reason { actual } => ProjectionSemanticTypeFault::Reason { actual },
        SemanticTypeFault::PrimitiveShape { actual } => {
            ProjectionSemanticTypeFault::PrimitiveShape { actual }
        }
        SemanticTypeFault::CvQualifiers { actual } => {
            ProjectionSemanticTypeFault::CvQualifiers { actual }
        }
        SemanticTypeFault::Width { actual } => ProjectionSemanticTypeFault::Width { actual },
        SemanticTypeFault::ChildCount { tag, law, actual } => {
            ProjectionSemanticTypeFault::ChildCount {
                tag: portable_type_tag(tag),
                min: law.min,
                max: law.max,
                actual,
            }
        }
        SemanticTypeFault::ChildNameForbidden { tag, position } => {
            ProjectionSemanticTypeFault::ChildNameForbidden {
                tag: portable_type_tag(tag),
                position,
            }
        }
        SemanticTypeFault::ChildNameRequired { tag, position } => {
            ProjectionSemanticTypeFault::ChildNameRequired {
                tag: portable_type_tag(tag),
                position,
            }
        }
        SemanticTypeFault::ChildFlagsForbidden {
            tag,
            position,
            actual,
        } => ProjectionSemanticTypeFault::ChildFlagsForbidden {
            tag: portable_type_tag(tag),
            position,
            actual,
        },
        SemanticTypeFault::VariadicParameter { position, actual } => {
            ProjectionSemanticTypeFault::VariadicParameter { position, actual }
        }
        SemanticTypeFault::ChildTextForbidden { tag, position } => {
            ProjectionSemanticTypeFault::ChildTextForbidden {
                tag: portable_type_tag(tag),
                position,
            }
        }
    }
}

fn portable_type_child_lane(lane: TypeChildLane) -> ProjectionTypeChildLane {
    match lane {
        TypeChildLane::Declared => ProjectionTypeChildLane::Declared,
        TypeChildLane::Anonymous => ProjectionTypeChildLane::Anonymous,
        TypeChildLane::Computed => ProjectionTypeChildLane::Computed,
    }
}

fn portable_parentage(state: ParentageState) -> ProjectionParentageState {
    match state {
        ParentageState::Unavailable => ProjectionParentageState::Unavailable,
        ParentageState::Root => ProjectionParentageState::Root,
        ParentageState::Bound { parent } => ProjectionParentageState::Bound { parent: parent.raw },
        ParentageState::UnrepresentedAuthorityOwner { identity } => {
            ProjectionParentageState::UnrepresentedAuthorityOwner { identity }
        }
    }
}

fn portable_span(span: SourceSpanFact) -> ProjectionSpan {
    ProjectionSpan {
        start: span.start,
        end: span.end,
    }
}

/// Projects the driver's full typed admission rejection into the portable
/// value vocabulary used by every language authority terminal.
pub(crate) fn portable_admission(fault: FactFault) -> ProjectionAdmissionFault {
    match fault {
        FactFault::EmptyName => ProjectionAdmissionFault::EmptyName,
        FactFault::Capacity => ProjectionAdmissionFault::Capacity,
        FactFault::ChildCapacity => ProjectionAdmissionFault::ChildCapacity,
        FactFault::ProductChildPoolCapacity {
            used,
            requested,
            capacity,
        } => ProjectionAdmissionFault::ProductChildPoolCapacity {
            used: portable_count(used),
            requested: portable_count(requested),
            capacity: portable_count(capacity),
        },
        FactFault::Constructor(cause) => ProjectionAdmissionFault::Constructor {
            cause: portable_constructor_fault(cause),
        },
        FactFault::ChildRole {
            position,
            expected,
            actual,
        } => ProjectionAdmissionFault::ChildRole {
            position: portable_count(position),
            expected: portable_child_role(expected),
            actual: portable_child_role(actual),
        },
        FactFault::ChildTarget {
            position,
            target,
            fact_count,
        } => ProjectionAdmissionFault::ChildTarget {
            position: portable_count(position),
            target,
            fact_count: portable_count(fact_count),
        },
        FactFault::TypeRecord(cause) => ProjectionAdmissionFault::TypeRecord {
            cause: portable_type_fault(cause),
        },
        FactFault::TypeChild { position, fault } => ProjectionAdmissionFault::TypeChild {
            position: portable_count(position),
            cause: portable_type_fault(fault),
        },
        FactFault::TypeChildTarget {
            position,
            target,
            fact_count,
        } => ProjectionAdmissionFault::TypeChildTarget {
            position: portable_count(position),
            target,
            fact_count: portable_count(fact_count),
        },
        FactFault::TypeChildCapacity => ProjectionAdmissionFault::TypeChildCapacity,
        FactFault::TypeChildPoolCapacity {
            lane,
            used,
            requested,
            capacity,
        } => ProjectionAdmissionFault::TypeChildPoolCapacity {
            lane: portable_type_child_lane(lane),
            used: portable_count(used),
            requested: portable_count(requested),
            capacity: portable_count(capacity),
        },
        FactFault::TypeRowCapacity => ProjectionAdmissionFault::TypeRowCapacity,
        FactFault::ComputedRowCapacity => ProjectionAdmissionFault::ComputedRowCapacity,
        FactFault::OccurrenceOwner { owner, fact_count } => {
            ProjectionAdmissionFault::OccurrenceOwner {
                owner,
                fact_count: portable_count(fact_count),
            }
        }
        FactFault::OccurrenceCapacity => ProjectionAdmissionFault::OccurrenceCapacity,
        FactFault::DocOwner { owner, fact_count } => ProjectionAdmissionFault::DocOwner {
            owner,
            fact_count: portable_count(fact_count),
        },
        FactFault::DocCapacity => ProjectionAdmissionFault::DocCapacity,
        FactFault::ExtensionAtomCapacity => ProjectionAdmissionFault::ExtensionAtomCapacity,
        FactFault::TypeParameterCapacity => ProjectionAdmissionFault::TypeParameterCapacity,
        FactFault::TypeParameterBoundCapacity {
            requested,
            available,
        } => ProjectionAdmissionFault::TypeParameterBoundCapacity {
            requested: portable_count(requested),
            available: portable_count(available),
        },
        FactFault::RefListCapacity => ProjectionAdmissionFault::RefListCapacity,
        FactFault::RefListElements => ProjectionAdmissionFault::RefListElements,
        FactFault::RefTarget {
            lane,
            raw,
            fact_count,
        } => ProjectionAdmissionFault::RefTarget {
            lane,
            raw,
            fact_count: portable_count(fact_count),
        },
        FactFault::SourceSpan {
            entity,
            start,
            end,
            source_len,
        } => ProjectionAdmissionFault::SourceSpan {
            entity,
            start,
            end,
            source_len,
        },
        FactFault::ConflictingSourceSpan {
            entity,
            existing,
            requested,
        } => ProjectionAdmissionFault::ConflictingSourceSpan {
            entity: entity.raw,
            existing: portable_span(existing),
            requested: portable_span(requested),
        },
        FactFault::ConflictingParentage {
            entity,
            existing,
            requested,
        } => ProjectionAdmissionFault::ConflictingParentage {
            entity: entity.raw,
            existing: portable_parentage(existing),
            requested: portable_parentage(requested),
        },
    }
}
