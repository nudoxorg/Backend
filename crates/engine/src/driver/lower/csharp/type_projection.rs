//! Projects one Roslyn type graph into lattice rows.
//!
//! Namespaces and type declarations are already pushed. This lane walks the
//! image type plane from each declared root, interning compound children as
//! anonymous rows so every target is strictly backward. Positions the lane
//! cannot carry fold to typed unknown rows that keep the image spelling.

use super::{
    CSharpCollectError, DECIMAL_SPELLING, INTEGER_SIGNED_FLAG, INTEGER_WIDTH_SHIFT, Names,
    Ordinals, ProjectedType, ProjectionFault, SHAPE_BOOL, SHAPE_BUILTIN, SHAPE_FLOAT,
    SHAPE_INTEGER, SHAPE_MUT_POINTER, SHAPE_NATIVE_SIGNED_INTEGER, SHAPE_POINTER_ADDRESS_INTEGER,
    SHAPE_STR, SHAPE_UTF16_CODE_UNIT, VOID_SPELLING, lane_terminal, nominal_record, terminal,
    unknown_record,
};
use crate::driver::lower::{FactSet, MAX_TYPE_CHILDREN};
use backend_frontend_csharp::legacy::{
    CSharpImage, NullabilityCell, TypeNode, TypeNodeKind, TypeRef,
};
use backend_semantic::ir::{
    AnnotationKind, SemanticTypeRecord, SemanticTypeTag, TypeReason, TypeWidth,
};
use backend_semantic::vocabulary::CSharpProjectionIndexPhase;

/// Projects one image type coordinate into the lane's lattice at fact level.
pub(super) fn project_fact_type<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    anchor: u32,
    reference: TypeRef,
    depth: usize,
) -> Result<ProjectedType<'source>, CSharpCollectError> {
    if depth == 0 {
        return Err(terminal(ProjectionFault::Depth {
            type_row: reference.ordinal(),
        }));
    }
    let node = image.type_node(reference).map_err(ProjectionFault::Image)?;
    let projection = owned_node(
        facts,
        image,
        names,
        ordinals,
        anchor,
        reference.ordinal(),
        &node,
        depth,
    )?;
    if let Some(kind) = reference_nullability(&projection) {
        let spelling = projection.spelling;
        let nullable = projection.nullable;
        let void = projection.void;
        let target = projection_target(facts, anchor, projection)?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Annotated);
        record.payload0 = kind as u32;
        return Ok(ProjectedType {
            record,
            children: vec![(target, None)],
            spelling,
            nullable,
            void,
        });
    }
    match projection.fact {
        // The bare in-file nominal terminal: the fact's record names the
        // declaration's own backward ordinal and no rows are interned.
        Some(ordinal) => Ok(ProjectedType {
            record: nominal_record(ordinal),
            children: Vec::new(),
            spelling: projection.spelling,
            nullable: projection.nullable,
            void: projection.void,
        }),
        // The node owns a compound record: the fact's own record repeats the
        // projection's record over its already-interned child rows. The root
        // never becomes a child target, so it needs no anonymous row.
        None => Ok(ProjectedType {
            record: projection.record,
            children: projection.children,
            spelling: projection.spelling,
            nullable: projection.nullable,
            void: projection.void,
        }),
    }
}

/// Projects one type coordinate to a child-usable coordinate: a bare in-file
/// nominal names its fact ordinal; every other shape is interned as an
/// anonymous row owned by `anchor`.
pub(super) fn child_target<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    anchor: u32,
    reference: TypeRef,
    depth: usize,
) -> Result<u32, CSharpCollectError> {
    if depth == 0 {
        return Err(terminal(ProjectionFault::Depth {
            type_row: reference.ordinal(),
        }));
    }
    let node = image.type_node(reference).map_err(ProjectionFault::Image)?;
    let projection = owned_node(
        facts,
        image,
        names,
        ordinals,
        anchor,
        reference.ordinal(),
        &node,
        depth,
    )?;
    let annotation = reference_nullability(&projection);
    let target = projection_target(facts, anchor, projection)?;
    match annotation {
        Some(kind) => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Annotated);
            record.payload0 = kind as u32;
            intern_row(facts, anchor, record, &[(target, None)])
        }
        None => Ok(target),
    }
}

/// Turns one already-projected C# node into a backward type coordinate.
/// Bare in-image declarations retain their declaration coordinate; every
/// other node is materialized once under the caller's reserved owner.
fn projection_target<'source>(
    facts: &mut FactSet<'source>,
    anchor: u32,
    projection: OwnedNode<'source>,
) -> Result<u32, CSharpCollectError> {
    match projection.fact {
        Some(ordinal) => Ok(ordinal),
        None => intern_row(facts, anchor, projection.record, &projection.children),
    }
}

/// Maps a meaningful Roslyn reference-nullability cell into a structural
/// annotation. Oblivious (`None`) is intentionally absence, not a fabricated
/// assertion about the reference type. Roslyn also marks a nullable value
/// type (`int?`, `(int, string)?`) `Annotated`; that node already is the
/// `NullableValue` annotation, so wrapping it again would claim `T??`.
fn reference_nullability(projection: &OwnedNode<'_>) -> Option<AnnotationKind> {
    if projection.record.tag == SemanticTypeTag::Annotated
        && projection.record.payload0 == AnnotationKind::NullableValue as u32
    {
        return None;
    }
    match projection.nullable {
        NullabilityCell::None => None,
        NullabilityCell::Annotated => Some(AnnotationKind::NullableReference),
        NullabilityCell::NotAnnotated => Some(AnnotationKind::NonNullableReference),
    }
}

/// The interning of one compound node is one staging transaction: append its
/// already-resolved children first, then admit the row that validates exactly
/// that pending range. `FactSet` deliberately rejects an empty pending range
/// for unary/tuple rows, so reversing this order loses every compound C# type.
fn intern_row<'source>(
    facts: &mut FactSet<'source>,
    anchor: u32,
    record: SemanticTypeRecord<'source>,
    children: &[(u32, Option<&'source [u8]>)],
) -> Result<u32, CSharpCollectError> {
    for (target, name) in children {
        facts
            .anonymous_type_child(*target, *name, 0)
            .map_err(|fault| lane_terminal(facts.len(), 0, fault))?;
    }
    // `FactSet` is bounded by `MAX_EMISSION_FACTS`, which is const-checked
    // against the wire coordinate width in the shared lower module.
    let fact_count = u32::try_from(facts.len()).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::FactOrdinal,
            observed: crate::driver::lower::portable_count(facts.len()),
        })
    })?;
    if anchor == fact_count {
        facts
            .intern_reserved_anchor_type_row(anchor, record)
            .map_err(|fault| lane_terminal(facts.len(), 0, fault))
    } else {
        facts
            .intern_anonymous_type_row(anchor, record)
            .map_err(|fault| lane_terminal(facts.len(), 0, fault))
    }
}

/// One owned compound node built bottom-up: its children are already
/// interned, its record is complete, and `fact` is set only for the bare
/// in-file nominal terminal.
struct OwnedNode<'source> {
    record: SemanticTypeRecord<'source>,
    children: Vec<(u32, Option<&'source [u8]>)>,
    spelling: Option<&'source [u8]>,
    nullable: NullabilityCell,
    void: bool,
    fact: Option<u32>,
}

/// Projects one type node into its owned record, interning every child row.
fn owned_node<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    anchor: u32,
    type_row: u32,
    node: &TypeNode<'source>,
    depth: usize,
) -> Result<OwnedNode<'source>, CSharpCollectError> {
    match node.kind {
        TypeNodeKind::Named => {
            named_node(facts, image, names, ordinals, anchor, type_row, node, depth)
        }
        TypeNodeKind::Array => {
            let rank = node.children.len();
            let mut elements = node.children.clone();
            let Some(element) = elements.next() else {
                return Ok(spelled_unknown(node));
            };
            for sibling in elements {
                if sibling.ty != element.ty {
                    return Err(terminal(ProjectionFault::HeterogeneousArrayRank {
                        first: element.ty.ordinal(),
                        observed: sibling.ty.ordinal(),
                    }));
                }
            }
            // Jagged chains project as nested array rows. A rectangular rank
            // is a numeric authority fact, not a bounded comma spelling.
            let Some(rank) = u16::try_from(rank).ok().filter(|rank| *rank != 0) else {
                return Ok(spelled_unknown(node));
            };
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayRectangular);
            record.payload0 = u32::from(rank);
            let target =
                child_target(facts, image, names, ordinals, anchor, element.ty, depth - 1)?;
            Ok(OwnedNode {
                record,
                children: vec![(target, None)],
                spelling: node.spelling.map(|atom| atom.bytes),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        TypeNodeKind::Pointer => {
            let Some(pointee) = node.children.clone().next() else {
                return Ok(spelled_unknown(node));
            };
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_MUT_POINTER;
            let target =
                child_target(facts, image, names, ordinals, anchor, pointee.ty, depth - 1)?;
            Ok(OwnedNode {
                record,
                children: vec![(target, None)],
                spelling: node.spelling.map(|atom| atom.bytes),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        TypeNodeKind::NullableValue => {
            // `T?` is a closed nullable-value annotation over its inner row.
            let Some(inner) = node.children.clone().next() else {
                return Ok(spelled_unknown(node));
            };
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Annotated);
            record.payload0 = AnnotationKind::NullableValue as u32;
            let target = child_target(facts, image, names, ordinals, anchor, inner.ty, depth - 1)?;
            Ok(OwnedNode {
                record,
                children: vec![(target, None)],
                spelling: node.spelling.map(|atom| atom.bytes),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        TypeNodeKind::Tuple => {
            let record = SemanticTypeRecord::leaf(SemanticTypeTag::Tuple);
            let mut children = Vec::new();
            for element in node.children {
                let target =
                    child_target(facts, image, names, ordinals, anchor, element.ty, depth - 1)?;
                children.push((target, element.name.map(|atom| atom.bytes)));
            }
            Ok(OwnedNode {
                record,
                children,
                spelling: node.spelling.map(|atom| atom.bytes),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        TypeNodeKind::FunctionPointer => {
            // The producer writes [parameters…, result?] and flags the row
            // when a result child exists, so void signatures stay
            // distinguishable from one-parameter signatures.
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
            let mut children = Vec::new();
            for child in node.children.clone() {
                let target =
                    child_target(facts, image, names, ordinals, anchor, child.ty, depth - 1)?;
                children.push((target, None));
            }
            if node.has_return {
                record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
            }
            Ok(OwnedNode {
                record,
                children,
                spelling: node.spelling.map(|atom| atom.bytes),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        TypeNodeKind::TypeParameter => {
            let name = node
                .spelling
                .map(|atom| atom.bytes)
                .ok_or_else(|| terminal(ProjectionFault::Depth { type_row }))?;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(name);
            Ok(OwnedNode {
                record,
                children: Vec::new(),
                spelling: Some(name),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        TypeNodeKind::Dynamic => Ok(OwnedNode {
            record: unknown_record(TypeReason::DynamicallyTyped, None),
            children: Vec::new(),
            spelling: node.spelling.map(|atom| atom.bytes),
            nullable: node.nullable,
            void: false,
            fact: None,
        }),
        TypeNodeKind::Error => Ok(spelled_unknown(node)),
    }
}

/// Projects one named use: an exact-width primitive spelling becomes its
/// primitive row, an in-file nominal the backward fact terminal, a generic
/// application an `Apply` row, and every foreign `System/*` use a typed
/// unknown retaining the qualified spelling.
fn named_node<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    anchor: u32,
    type_row: u32,
    node: &TypeNode<'source>,
    depth: usize,
) -> Result<OwnedNode<'source>, CSharpCollectError> {
    let spelling = node
        .spelling
        .map(|atom| atom.bytes)
        .ok_or_else(|| terminal(ProjectionFault::Depth { type_row }))?;
    if let Some(record) = primitive_record(spelling) {
        let void = matches!(spelling, b"System.Void" | b"void");
        return Ok(OwnedNode {
            record,
            children: Vec::new(),
            spelling: Some(spelling),
            nullable: node.nullable,
            void,
            fact: None,
        });
    }
    let arguments: Vec<_> = node.children.clone().collect();
    let local_ordinal = match names.lookup(spelling) {
        Some(coordinate) => ordinals.lookup(coordinate.index().map_err(terminal)?),
        None => None,
    };
    match local_ordinal {
        Some(ordinal) if arguments.is_empty() => Ok(OwnedNode {
            record: nominal_record(ordinal),
            children: Vec::new(),
            spelling: Some(spelling),
            nullable: node.nullable,
            void: false,
            fact: Some(ordinal),
        }),
        Some(ordinal) => {
            // Generic application: children are [base, arguments…] and every
            // argument projects to its own backward row.
            if arguments.len() + 1 > MAX_TYPE_CHILDREN {
                return Ok(unknown_owned(spelling, node.nullable));
            }
            let mut children = vec![(ordinal, None)];
            for argument in arguments {
                let target = child_target(
                    facts,
                    image,
                    names,
                    ordinals,
                    anchor,
                    argument.ty,
                    depth - 1,
                )?;
                children.push((target, argument.name.map(|atom| atom.bytes)));
            }
            Ok(OwnedNode {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children,
                spelling: Some(spelling),
                nullable: node.nullable,
                void: false,
                fact: None,
            })
        }
        None => Ok(unknown_owned(spelling, node.nullable)),
    }
}

/// The typed unknown projection for one foreign qualified spelling.
fn unknown_owned(spelling: &[u8], nullable: NullabilityCell) -> OwnedNode<'_> {
    OwnedNode {
        record: unknown_record(TypeReason::UnresolvedExternal, Some(spelling)),
        children: Vec::new(),
        spelling: Some(spelling),
        nullable,
        void: false,
        fact: None,
    }
}

/// The typed unknown projection retaining the node's written spelling.
fn spelled_unknown<'source>(node: &TypeNode<'source>) -> OwnedNode<'source> {
    let spelling = node.spelling.map(|atom| atom.bytes);
    let record = match spelling {
        Some(spelling) => unknown_record(TypeReason::NoIrRepresentation, Some(spelling)),
        None => unknown_record(TypeReason::OracleGap, None),
    };
    OwnedNode {
        record,
        children: Vec::new(),
        spelling,
        nullable: node.nullable,
        void: false,
        fact: None,
    }
}

/// Maps one closed primitive spelling onto its exact width, signedness, and
/// shape cells: `int` is I32, `nint` is a native signed word, `nuint` a
/// pointer-address word, `decimal` and `void` stay builtins, and every other
/// spelling is no primitive at all.
fn primitive_record(spelling: &[u8]) -> Option<SemanticTypeRecord<'static>> {
    let integer = |width: u32, signed: bool| -> SemanticTypeRecord<'static> {
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_INTEGER;
        record.payload1 =
            (width << INTEGER_WIDTH_SHIFT) | if signed { INTEGER_SIGNED_FLAG } else { 0 };
        record
    };
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
    match spelling {
        b"System.Boolean" | b"bool" => {
            record.payload0 = SHAPE_BOOL;
            Some(record)
        }
        b"System.Byte" | b"byte" => Some(integer(8, false)),
        b"System.SByte" | b"sbyte" => Some(integer(8, true)),
        b"System.Int16" | b"short" => Some(integer(16, true)),
        b"System.UInt16" | b"ushort" => Some(integer(16, false)),
        b"System.Int32" | b"int" => Some(integer(32, true)),
        b"System.UInt32" | b"uint" => Some(integer(32, false)),
        b"System.Int64" | b"long" => Some(integer(64, true)),
        b"System.UInt64" | b"ulong" => Some(integer(64, false)),
        b"System.IntPtr" | b"nint" => {
            record.payload0 = SHAPE_NATIVE_SIGNED_INTEGER;
            Some(record)
        }
        b"System.UIntPtr" | b"nuint" => {
            record.payload0 = SHAPE_POINTER_ADDRESS_INTEGER;
            Some(record)
        }
        b"System.Single" | b"float" => {
            record.payload0 = SHAPE_FLOAT;
            record.payload1 = TypeWidth::Fixed(32).to_cell();
            Some(record)
        }
        b"System.Double" | b"double" => {
            record.payload0 = SHAPE_FLOAT;
            record.payload1 = TypeWidth::Fixed(64).to_cell();
            Some(record)
        }
        b"System.Char" | b"char" => {
            record.payload0 = SHAPE_UTF16_CODE_UNIT;
            record.payload1 = TypeWidth::Fixed(16).to_cell();
            Some(record)
        }
        b"System.String" | b"string" => {
            record.payload0 = SHAPE_STR;
            Some(record)
        }
        b"System.Decimal" | b"decimal" => {
            record.payload0 = SHAPE_BUILTIN;
            record.text = Some(DECIMAL_SPELLING);
            Some(record)
        }
        b"System.Void" | b"void" => {
            record.payload0 = SHAPE_BUILTIN;
            record.text = Some(VOID_SPELLING);
            Some(record)
        }
        _ => None,
    }
}
