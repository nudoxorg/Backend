//! Projects one javac type graph into lattice rows.
//!
//! Named declarations are already pushed. This lane walks the image type
//! plane from each declared root and folds positions the lane cannot carry
//! to typed unknown rows that keep the image spelling.

use super::{
    DEPTH_LIMIT, ENCLOSING_FLAG, INTEGER_SIGNED_FLAG, INTEGER_WIDTH_SHIFT, NameIndex,
    ProjectionFault, SHAPE_BOOL, SHAPE_BUILTIN, SHAPE_FLOAT, SHAPE_INTEGER, SHAPE_UTF16_CODE_UNIT,
    VARIANCE_CONTRAVARIANT, VARIANCE_COVARIANT, VARIANCE_INVARIANT, VOID_SPELLING,
    WILDCARD_EXTENDS_FLAG, WILDCARD_SUPER_FLAG, nominal_record, unknown_record,
};
use crate::driver::lower::{FactSet, MAX_TYPE_CHILDREN, SemanticFact};
use backend_frontend_java::legacy::{JavaImage, TypeFact, TypeKind, TypeRef};
use backend_semantic::ir::{
    NominalRef, SemanticTypeRecord, SemanticTypeTag, TypeReason, TypeWidth,
};
use backend_semantic::vocabulary::JavaProjectionIndexPhase;

/// One projected javac type row prepared for a fact: the lattice record, its
/// ordered backward type-row children, the nearest image atom spelling, and
/// whether the row is `void`.
pub(super) struct ProjectedType<'image> {
    record: SemanticTypeRecord<'image>,
    children: [u32; MAX_TYPE_CHILDREN],
    child_count: usize,
    pub(super) spelling: Option<&'image [u8]>,
    pub(super) void: bool,
}

impl<'image> ProjectedType<'image> {
    /// Wraps one childless lattice record with its nearest atom spelling.
    pub(super) const fn leaf(
        record: SemanticTypeRecord<'image>,
        spelling: Option<&'image [u8]>,
    ) -> Self {
        Self {
            record,
            children: [0; MAX_TYPE_CHILDREN],
            child_count: 0,
            spelling,
            void: false,
        }
    }

    /// Marks the projection as `void`.
    const fn with_void(mut self) -> Self {
        self.void = true;
        self
    }

    /// Appends one backward fact-ordinal child, or `None` when the bounded
    /// type-child lane is full.
    fn child(mut self, ordinal: u32) -> Option<Self> {
        let slot = self.children.get_mut(self.child_count)?;
        *slot = ordinal;
        self.child_count += 1;
        Some(self)
    }

    /// Commits the projection onto one fact under construction.
    pub(super) fn attach(self, fact: SemanticFact<'image>) -> SemanticFact<'image> {
        let mut fact = fact.typed(self.record);
        for ordinal in self.children.iter().take(self.child_count) {
            fact = fact.type_child(*ordinal, None, 0);
        }
        fact
    }

    /// Materializes this projected row as a child of an already-admitted
    /// declaration when it is not the direct local-nominal terminal. This is
    /// the bridge that lets nested Java arrays retain one structural sequence
    /// node per written `[]` without fabricating carrier declarations.
    fn into_child(self, facts: &mut FactSet<'image>, anchor: u32) -> Result<u32, ProjectionFault> {
        if self.child_count == 0
            && let Some(NominalRef::Local(target)) = self.record.nominal
        {
            return Ok(target.raw);
        }
        for ordinal in self.children.iter().take(self.child_count) {
            facts.anonymous_type_child(*ordinal, None, 0).map_err(|_| {
                ProjectionFault::IndexCapacity {
                    phase: JavaProjectionIndexPhase::TypeChild,
                }
            })?;
        }
        facts
            .intern_anonymous_type_row(anchor, self.record)
            .map_err(|_| ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::TypeRow,
            })
    }
}

/// The typed unknown record for one optional spelling: the text cell is kept
/// exactly when a spelling exists, because `NoIrRepresentation` requires one.
fn unknown_projection(spelling: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    match spelling {
        Some(spelling) => unknown_record(TypeReason::NoIrRepresentation, Some(spelling)),
        None => unknown_record(TypeReason::OracleGap, None),
    }
}

/// A qualified path row whose text cell is the image's own qualified spelling.
const fn qualified_record(spelling: &[u8]) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::QualifiedPath);
    record.text = Some(spelling);
    record
}

/// A Java sequence array has no numeric extent: each source `[]` is one
/// nested `ArraySequence` row, and the dialect owns token placement.
const fn array_record() -> SemanticTypeRecord<'static> {
    SemanticTypeRecord::leaf(SemanticTypeTag::ArraySequence)
}

/// The typed unknown projection fold for one optional spelling.
fn unknown_fold(spelling: Option<&[u8]>) -> ProjectedType<'_> {
    ProjectedType::leaf(unknown_projection(spelling), spelling)
}

/// The typed unknown fold for one declared spelling.
fn unknown_declared(spelling: &[u8]) -> ProjectedType<'_> {
    ProjectedType::leaf(
        unknown_record(TypeReason::NoIrRepresentation, Some(spelling)),
        Some(spelling),
    )
}

/// Projects one image type coordinate into the lane's lattice.
pub(super) fn project<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    anchor: u32,
    reference: TypeRef,
    depth: usize,
) -> Result<ProjectedType<'image>, ProjectionFault> {
    if depth == 0 {
        return Err(ProjectionFault::Depth {
            type_row: reference.ordinal,
        });
    }
    let row = image.type_fact(reference).map_err(ProjectionFault::Image)?;
    match row.kind {
        TypeKind::Primitive => primitive(reference, &row),
        TypeKind::Void => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_BUILTIN;
            record.text = Some(VOID_SPELLING);
            Ok(ProjectedType::leaf(record, None).with_void())
        }
        TypeKind::Declared => declared(image, names, &row, reference.ordinal),
        TypeKind::Array => array(facts, image, names, anchor, reference, depth),
        TypeKind::Variable => {
            let spelling = required_spelling(reference.ordinal, &row)?;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(spelling);
            Ok(ProjectedType::leaf(record, Some(spelling)))
        }
        TypeKind::Wildcard => wildcard(image, names, &row, depth),
        TypeKind::Intersection => members(
            image,
            names,
            &row,
            reference.ordinal,
            SemanticTypeTag::Intersection,
        ),
        TypeKind::Union => members(
            image,
            names,
            &row,
            reference.ordinal,
            SemanticTypeTag::Union,
        ),
        TypeKind::Error => {
            let spelling = required_spelling(reference.ordinal, &row)?;
            Ok(ProjectedType::leaf(
                unknown_record(TypeReason::NoIrRepresentation, Some(spelling)),
                Some(spelling),
            ))
        }
        TypeKind::None | TypeKind::Null => Ok(ProjectedType::leaf(
            unknown_record(TypeReason::OracleGap, None),
            None,
        )),
    }
}

/// Projects one primitive row onto its exact width and signedness cells.
fn primitive<'image>(
    reference: TypeRef,
    row: &TypeFact<'image>,
) -> Result<ProjectedType<'image>, ProjectionFault> {
    let spelling = required_spelling(reference.ordinal, row)?;
    let (shape_cell, payload1) = primitive_cells(reference.ordinal, spelling)?;
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
    record.payload0 = shape_cell;
    record.payload1 = payload1;
    Ok(ProjectedType::leaf(record, Some(spelling)))
}

/// Maps one closed javac primitive spelling onto its exact shape and payload
/// cells: byte/short/int/long are signed fixed-width integers, float/double
/// are fixed-width floats, char is the character shape, boolean the boolean
/// shape.
fn primitive_cells(type_row: u32, spelling: &[u8]) -> Result<(u32, u32), ProjectionFault> {
    let cells = match spelling {
        b"boolean" => (SHAPE_BOOL, 0),
        b"byte" => (
            SHAPE_INTEGER,
            (8 << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG,
        ),
        b"short" => (
            SHAPE_INTEGER,
            (16 << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG,
        ),
        b"int" => (
            SHAPE_INTEGER,
            (32 << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG,
        ),
        b"long" => (
            SHAPE_INTEGER,
            (64 << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG,
        ),
        b"char" => (SHAPE_UTF16_CODE_UNIT, TypeWidth::Fixed(16).to_cell()),
        b"float" => (SHAPE_FLOAT, TypeWidth::Fixed(32).to_cell()),
        b"double" => (SHAPE_FLOAT, TypeWidth::Fixed(64).to_cell()),
        _ => return Err(ProjectionFault::Primitive { type_row }),
    };
    Ok(cells)
}

/// Projects one declared row: a pure in-image spelling becomes the nominal
/// terminal, a nested spelling becomes a qualified path over its enclosing
/// row, and a generic application commits only when the base and every
/// argument resolve to in-image nominal rows.
fn declared<'image>(
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    row: &TypeFact<'image>,
    type_row: u32,
) -> Result<ProjectedType<'image>, ProjectionFault> {
    let spelling = required_spelling(type_row, row)?;
    let total = row.children.len();
    let enclosing = row.flags & ENCLOSING_FLAG == ENCLOSING_FLAG;
    if total == 0 {
        return Ok(match names.lookup(spelling) {
            Some(ordinal) => ProjectedType::leaf(nominal_record(ordinal), Some(spelling)),
            None if is_language_builtin(spelling) => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(spelling);
                ProjectedType::leaf(record, Some(spelling))
            }
            None => unknown_declared(spelling),
        });
    }
    let argument_count = if enclosing { total - 1 } else { total };
    let mut children = row.children;
    if enclosing {
        // Children are [arguments…, enclosing]; a qualified path carries
        // the enclosing row when it resolves to an in-image declaration.
        if argument_count == 0 {
            let enclosing = children.nth(total - 1);
            let ordinal = match enclosing.map(|reference| pure_nominal(image, names, reference)) {
                Some(Ok(Some(ordinal))) => Some(ordinal),
                Some(Err(fault)) => return Err(fault),
                Some(Ok(None)) | None => None,
            };
            if let Some(projected) = ordinal.and_then(|ordinal| {
                ProjectedType::leaf(qualified_record(spelling), Some(spelling)).child(ordinal)
            }) {
                return Ok(projected);
            }
        }
        return Ok(unknown_declared(spelling));
    }
    // Generic application: children are [base, arguments…] and every argument
    // must resolve to an in-image nominal row.
    let Some(base) = names.lookup(spelling) else {
        return Ok(unknown_declared(spelling));
    };
    if argument_count + 1 > MAX_TYPE_CHILDREN {
        return Ok(unknown_declared(spelling));
    }
    let Some(mut projected) = ProjectedType::leaf(
        SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
        Some(spelling),
    )
    .child(base) else {
        return Ok(unknown_declared(spelling));
    };
    for argument in children.take(argument_count) {
        match pure_nominal(image, names, argument)? {
            Some(ordinal) => match projected.child(ordinal) {
                Some(next) => projected = next,
                None => return Ok(unknown_declared(spelling)),
            },
            None => return Ok(unknown_declared(spelling)),
        }
    }
    Ok(projected)
}

/// `java.lang.String` and `java.lang.Object` are the language's own string
/// and root types, so they lower to the shared `String`/`Object` builtins
/// (C# lowers `System.String` the same way) instead of an unrepresented
/// foreign spelling. The builtin spelling table already names both.
fn is_language_builtin(spelling: &[u8]) -> bool {
    matches!(spelling, b"java.lang.String" | b"java.lang.Object")
}

/// Projects one array row into one structural sequence node per written `[]`.
fn array<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    anchor: u32,
    reference: TypeRef,
    depth: usize,
) -> Result<ProjectedType<'image>, ProjectionFault> {
    let mut arity = 0usize;
    let mut cursor = reference;
    loop {
        let row = image.type_fact(cursor).map_err(ProjectionFault::Image)?;
        if row.kind != TypeKind::Array {
            break;
        }
        arity += 1;
        if arity > DEPTH_LIMIT {
            return Err(ProjectionFault::Depth {
                type_row: cursor.ordinal,
            });
        }
        let mut children = row.children;
        cursor = children.next().ok_or(ProjectionFault::Malformed {
            type_row: cursor.ordinal,
            kind: row.kind,
        })?;
    }
    let component = project(facts, image, names, anchor, cursor, depth - 1)?;
    let spelling = component.spelling;
    let mut target = component.into_child(facts, anchor)?;
    // The outer-most sequence remains this fact's record; every preceding
    // written `[]` is an anonymous sequence owned by the same declaration.
    // There is deliberately no source-spelling arity table or arbitrary
    // fidelity ceiling here.
    for _ in 1..arity {
        facts.anonymous_type_child(target, None, 0).map_err(|_| {
            ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::TypeChild,
            }
        })?;
        target = facts
            .intern_anonymous_type_row(anchor, array_record())
            .map_err(|_| ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::TypeRow,
            })?;
    }
    ProjectedType::leaf(array_record(), spelling)
        .child(target)
        .ok_or(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::TypeChild,
        })
}

/// Projects one wildcard row: an in-image bound commits the variance cell with
/// its bound row; every other wildcard folds to a typed unknown.
fn wildcard<'image>(
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    row: &TypeFact<'image>,
    depth: usize,
) -> Result<ProjectedType<'image>, ProjectionFault> {
    let variance = if row.flags & WILDCARD_EXTENDS_FLAG != 0 {
        VARIANCE_COVARIANT
    } else if row.flags & WILDCARD_SUPER_FLAG != 0 {
        VARIANCE_CONTRAVARIANT
    } else {
        VARIANCE_INVARIANT
    };
    let mut children = row.children;
    let Some(bound) = children.next() else {
        return Ok(ProjectedType::leaf(
            unknown_record(TypeReason::OracleGap, None),
            None,
        ));
    };
    if let Some(ordinal) = pure_nominal(image, names, bound)? {
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Wildcard);
        record.payload0 = variance;
        if let Some(projected) = ProjectedType::leaf(record, None).child(ordinal) {
            return Ok(projected);
        }
    }
    let spelling = nearest_spelling(image, bound, depth - 1)?;
    Ok(ProjectedType::leaf(unknown_projection(spelling), spelling))
}

/// Projects one union or intersection row: every member must resolve to an
/// in-image nominal row, otherwise the whole row folds to a typed unknown that
/// retains the first member's spelling.
fn members<'image>(
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    row: &TypeFact<'image>,
    type_row: u32,
    tag: SemanticTypeTag,
) -> Result<ProjectedType<'image>, ProjectionFault> {
    let total = row.children.len();
    if total == 0 || total > MAX_TYPE_CHILDREN {
        let mut children = row.children;
        let spelling = match children.next() {
            Some(first) => nearest_spelling(image, first, DEPTH_LIMIT)?,
            None => None,
        };
        return Ok(ProjectedType::leaf(unknown_projection(spelling), spelling));
    }
    let mut children = row.children;
    let first = children.next().ok_or(ProjectionFault::Malformed {
        type_row,
        kind: row.kind,
    })?;
    let first_spelling = nearest_spelling(image, first, DEPTH_LIMIT)?;
    let mut projected = ProjectedType::leaf(SemanticTypeRecord::leaf(tag), first_spelling);
    let mut reference = first;
    loop {
        match pure_nominal(image, names, reference)? {
            Some(ordinal) => match projected.child(ordinal) {
                Some(next) => projected = next,
                None => return Ok(unknown_fold(first_spelling)),
            },
            None => return Ok(unknown_fold(first_spelling)),
        }
        let Some(next) = children.next() else {
            break;
        };
        reference = next;
    }
    Ok(projected)
}

/// Resolves one image type coordinate to an in-image declaration ordinal when
/// it is a pure declared row: declared kind, no children, no enclosing row.
fn pure_nominal<'image>(
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    reference: TypeRef,
) -> Result<Option<u32>, ProjectionFault> {
    let row = image.type_fact(reference).map_err(ProjectionFault::Image)?;
    if row.kind != TypeKind::Declared || row.children.len() != 0 || row.flags & ENCLOSING_FLAG != 0
    {
        return Ok(None);
    }
    let spelling = required_spelling(reference.ordinal, &row)?;
    Ok(names.lookup(spelling))
}

/// Walks one type subtree to the nearest image atom spelling, following the
/// first child of array, wildcard, union, and intersection rows.
fn nearest_spelling<'image>(
    image: JavaImage<'image>,
    reference: TypeRef,
    depth: usize,
) -> Result<Option<&'image [u8]>, ProjectionFault> {
    let mut cursor = reference;
    let mut remaining = depth;
    loop {
        if remaining == 0 {
            return Err(ProjectionFault::Depth {
                type_row: cursor.ordinal,
            });
        }
        remaining -= 1;
        let row = image.type_fact(cursor).map_err(ProjectionFault::Image)?;
        match row.kind {
            TypeKind::Primitive | TypeKind::Declared | TypeKind::Variable | TypeKind::Error => {
                return Ok(row.spelling.map(|atom| atom.bytes));
            }
            TypeKind::Array | TypeKind::Wildcard | TypeKind::Intersection | TypeKind::Union => {
                let mut children = row.children;
                let Some(next) = children.next() else {
                    return Ok(None);
                };
                cursor = next;
            }
            TypeKind::Void | TypeKind::None | TypeKind::Null => return Ok(None),
        }
    }
}

/// Borrowed atom bytes of one image row whose closed layout requires them.
fn required_spelling<'image>(
    type_row: u32,
    row: &TypeFact<'image>,
) -> Result<&'image [u8], ProjectionFault> {
    row.spelling
        .map(|atom| atom.bytes)
        .ok_or(ProjectionFault::Malformed {
            type_row,
            kind: row.kind,
        })
}
