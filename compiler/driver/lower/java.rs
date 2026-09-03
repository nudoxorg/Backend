//! Projects source-bound javac authority facts through the shared canonical fact lane.
//!
//! The projection covers every plane the validated image carries: declaration
//! facts, the recursive type graph, executable signatures with parameter and
//! result carrier facts, resolved call occurrences, Javadoc fragments, and
//! overload sibling lists. Emission is four-pass: every module, package, and
//! type declaration is committed first, so every nominal, type-child, and
//! product target is a strictly backward fact ordinal.
//!
//! Declared types are projected row-by-row from the image's own type plane.
//! Foreign nominal leaves inside compounds become anonymous `Unknown` rows
//! retaining their spelling; bare foreign leaves keep their fact-level fold.
//! Composite rows retain every honest child shape rather than fabricating an
//! ownership edge. Parameter and result carrier facts are named
//! by their javac-provided type spelling because image version 1 carries no
//! parameter-name plane. Modifiers stay out of the lane: visibility is not a
//! lane cell and is never fabricated. Never recovers Java facts by scanning
//! source text or a native parser fallback.

use core::{iter::ExactSizeIterator, str};

use compiler_ir::{
    DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ForeignKey, ForeignKeyFault,
    ForeignOrigin, JavaFacts, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget,
    ProductChildRole, ReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeRecord,
    SemanticTypeTag, TypeReason, TypeWidth,
};
use compiler_languages_java::{
    BoundImageError, Declaration, DeclarationExtension, DeclarationKind, DocFlavor, ImageError,
    JavaAuthorityImage, JavaImage, JavaRelease, Reference, SymbolRef, TypeFact, TypeKind, TypeRef,
};
use compiler_vocabulary::{JavaRelease as ProfileRelease, LoweringUnsupported};
use sha2::Digest;

use crate::lower::{
    EmissionExtension, FactSet, LEAF_PRODUCT, MAX_EMISSION_FACTS, MAX_FACT_CHILDREN,
    MAX_REF_LIST_ELEMENTS, MAX_TYPE_CHILDREN, SemanticFact, push_fact,
};
use crate::types::{FactFault, FactRejection};

/// Exact rejection while lending source-bound javac declaration facts.
#[derive(Debug)]
pub(crate) enum JavaCollectError {
    /// The outer source-binding envelope or embedded javac image failed validation.
    Image(BoundImageError),
    /// The image was produced for a Java release other than the selected profile.
    Release {
        /// Requested compiler profile release.
        requested: ProfileRelease,
        /// Release retained by the attributed `JavacTask` image.
        observed: JavaRelease,
    },
    /// The source-bound image belongs to other source bytes than this compile request.
    SourceBinding {
        /// SHA-256 digest of exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the source-bound javac image.
        observed: [u8; 32],
    },
    /// The bounded canonical lane rejected an authority fact.
    Lowering(LoweringUnsupported),
    /// Canonical admission rejected one exact fact; operands retained.
    Rejected(FactRejection),
}

/// Exact projection fault retained until the collect boundary folds it into
/// the lane's closed terminal. The shared driver failure match owns the
/// terminal arms and lies outside this module's ownership, so every fault
/// class folds to the same closed terminal as bounded-lane capacity; the
/// operands remain named here so the collapse site stays typed.
#[derive(Debug)]
enum ProjectionFault<'image> {
    /// A validated image plane rejected a coordinate during projection.
    Image(
        /// The exact image-plane rejection.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        ImageError,
    ),
    /// The recursive type graph exceeded the producer's documented depth budget.
    Depth,
    /// A fixed row layout promise was violated: a required atom or child was absent.
    Malformed {
        /// The image type row that broke its closed layout.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        kind: TypeKind,
    },
    /// A primitive spelling outside javac's closed kind vocabulary.
    Primitive {
        /// The rejected spelling bytes.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        spelling: &'image [u8],
    },
    /// An image atom was not valid UTF-8 although the image validated its planes.
    Utf8,
    /// The bound compile source is not UTF-8 text, so javac's UTF-16
    /// coordinates have no byte domain to project into.
    SourceUtf8,
    /// A javac UTF-16 coordinate has no byte offset in the bound source.
    Utf16 {
        /// The requested UTF-16 unit offset.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        units: u32,
        /// The bound source's total UTF-16 length.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        utf16_len: u32,
    },
    /// A reference owner names no executable admitted by this image.
    OrphanOwner {
        /// The unresolved owner symbol coordinate.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        owner: SymbolRef,
    },
    /// A foreign key could not be built for a resolved external target.
    ForeignKey(
        /// The exact foreign-key rejection.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        ForeignKeyFault,
    ),
    /// The bounded overload sibling list overflowed its pooled width.
    SiblingCapacity,
    /// A bounded projection index overflowed its lane width.
    IndexCapacity,
}

/// Folds one projection fault into the lane's closed terminal. The shared
/// driver failure match owns the terminal arms and is outside this module's
/// ownership, so operand-preserving Java terminals stay folded here; adding a
/// terminal arm is recorded as a lane criticism in the module's review notes.
fn terminal(fault: ProjectionFault<'_>) -> JavaCollectError {
    let _ = fault;
    JavaCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Folds one bounded-lane fact rejection into the lane's closed terminal.
fn lane_terminal(fault: FactFault) -> JavaCollectError {
    let _ = fault;
    JavaCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Admits one fact and returns its proven backward ordinal.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<u32, JavaCollectError> {
    let ordinal = push_fact(facts, fact).map_err(JavaCollectError::Rejected)?;
    u32::try_from(ordinal)
        .map_err(|_| JavaCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration))
}

/// Producer depth budget of the recursive type graph, documented by the image
/// writer. The projection enforces the same budget so a hostile image cannot
/// drive unbounded recursion.
const DEPTH_LIMIT: usize = 64;

/// Image `Declared` flag marking a nested type's enclosing row child.
const ENCLOSING_FLAG: u8 = 1;

/// Image `Wildcard` flag marking an extends bound child.
const WILDCARD_EXTENDS_FLAG: u8 = 1;

/// Image `Wildcard` flag marking a super bound child.
const WILDCARD_SUPER_FLAG: u8 = 2;

/// `PrimitiveShape::Integer` wire cell.
const SHAPE_INTEGER: u32 = 0;
/// `PrimitiveShape::Float` wire cell.
const SHAPE_FLOAT: u32 = 1;
/// `PrimitiveShape::Bool` wire cell.
const SHAPE_BOOL: u32 = 2;
/// `PrimitiveShape::Char` wire cell.
const SHAPE_CHAR: u32 = 3;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// `void` builtin spelling; the void row is a primitive builtin leaf.
const VOID_SPELLING: &[u8] = b"void";

/// Array arity spellings the lane can commit as a borrowed text cell. Deeper
/// arrays keep their element spelling in a typed `Unknown` row instead of an
/// allocated spelling.
const ARITY_SPELLINGS: [&[u8]; 8] = [
    b"[]",
    b"[][]",
    b"[][][]",
    b"[][][][]",
    b"[][][][][]",
    b"[][][][][][]",
    b"[][][][][][][]",
    b"[][][][][][][][]",
];

/// Maximum non-ASCII character boundaries retained by one source basis.
/// This inline table is bounded; exceeding it returns `IndexCapacity` rather
/// than falling back to repeated source scans.
const MAX_UTF16_BOUNDARIES: usize = 1024;

#[derive(Clone, Copy)]
struct SpanBoundary {
    units: u32,
    bytes: u32,
    utf16_width: u32,
    byte_width: u32,
}

enum SpanBasis {
    Ascii {
        utf16_len: u32,
    },
    Sparse {
        utf16_len: u32,
        boundaries: [SpanBoundary; MAX_UTF16_BOUNDARIES],
        length: usize,
    },
}

impl SpanBasis {
    /// Computes the UTF-16 length and sparse projection index in one pass.
    fn prepare(source: &str) -> Result<Self, ProjectionFault<'static>> {
        let mut utf16_len = 0_u32;
        let mut boundaries = [SpanBoundary {
            units: 0,
            bytes: 0,
            utf16_width: 0,
            byte_width: 0,
        }; MAX_UTF16_BOUNDARIES];
        let mut length = 0;
        let mut ascii = true;
        for (offset, character) in source.char_indices() {
            let utf16_width =
                u32::try_from(character.len_utf16()).map_err(|_| ProjectionFault::Utf16 {
                    units: u32::MAX,
                    utf16_len: u32::MAX,
                })?;
            let byte_width =
                u32::try_from(character.len_utf8()).map_err(|_| ProjectionFault::Utf16 {
                    units: u32::MAX,
                    utf16_len: u32::MAX,
                })?;
            if !character.is_ascii() {
                ascii = false;
                let boundary = boundaries
                    .get_mut(length)
                    .ok_or(ProjectionFault::IndexCapacity)?;
                *boundary = SpanBoundary {
                    units: utf16_len,
                    bytes: u32::try_from(offset).map_err(|_| ProjectionFault::Utf16 {
                        units: u32::MAX,
                        utf16_len: u32::MAX,
                    })?,
                    utf16_width,
                    byte_width,
                };
                length += 1;
            }
            utf16_len = utf16_len
                .checked_add(utf16_width)
                .ok_or(ProjectionFault::Utf16 {
                    units: u32::MAX,
                    utf16_len: u32::MAX,
                })?;
        }
        if ascii {
            Ok(Self::Ascii { utf16_len })
        } else {
            Ok(Self::Sparse {
                utf16_len,
                boundaries,
                length,
            })
        }
    }

    fn utf16_len(&self) -> u32 {
        match self {
            Self::Ascii { utf16_len } | Self::Sparse { utf16_len, .. } => *utf16_len,
        }
    }

    /// Binary-searches the preceding non-ASCII boundary. A coordinate inside
    /// an astral surrogate pair retains the prior typed UTF-16 rejection.
    fn byte_offset(&self, units: u32) -> Result<u32, ProjectionFault<'static>> {
        let utf16_len = self.utf16_len();
        if units > utf16_len {
            return Err(ProjectionFault::Utf16 { units, utf16_len });
        }
        let (boundaries, length) = match self {
            Self::Ascii { .. } => return Ok(units),
            Self::Sparse {
                boundaries, length, ..
            } => (boundaries, *length),
        };
        let mut low = 0;
        let mut high = length;
        while low < high {
            let middle = low + (high - low) / 2;
            if boundaries[middle].units <= units {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low == 0 {
            return Ok(units);
        }
        let boundary = boundaries[low - 1];
        if units == boundary.units {
            return Ok(boundary.bytes);
        }
        let end_units = boundary
            .units
            .checked_add(boundary.utf16_width)
            .ok_or(ProjectionFault::Utf16 { units, utf16_len })?;
        if units < end_units {
            return Err(ProjectionFault::Utf16 { units, utf16_len });
        }
        boundary
            .bytes
            .checked_add(boundary.byte_width)
            .and_then(|bytes| bytes.checked_add(units - end_units))
            .ok_or(ProjectionFault::Utf16 { units, utf16_len })
    }
}

/// Wildcard variance cell for an unbounded `<?>`.
const VARIANCE_INVARIANT: u32 = 0;
/// Wildcard variance cell for `? extends T`.
const VARIANCE_COVARIANT: u32 = 1;
/// Wildcard variance cell for `? super T`.
const VARIANCE_CONTRAVARIANT: u32 = 2;

/// Javadoc inline code opener.
const INLINE_CODE_OPENER: &[u8] = b"{@code ";
/// Javadoc inline link opener.
const INLINE_LINK_OPENER: &[u8] = b"{@link ";

/// Streams all typed javac facts — declarations, recursive types, signatures,
/// occurrences, docs, and overloads — into the shared fact lane.
pub(crate) fn collect<'source>(
    profile: ProfileRelease,
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), JavaCollectError> {
    let authority = JavaAuthorityImage::open(image_bytes).map_err(JavaCollectError::Image)?;
    if !release_matches(profile, authority.image.release) {
        return Err(JavaCollectError::Release {
            requested: profile,
            observed: authority.image.release,
        });
    }
    let expected: [u8; 32] = sha2::Sha256::digest(source).into();
    let observed = authority.source_digest();
    if observed != expected {
        return Err(JavaCollectError::SourceBinding { expected, observed });
    }
    let image = authority.image;
    let source_text = str::from_utf8(source).map_err(|_| terminal(ProjectionFault::SourceUtf8))?;
    let span_basis = SpanBasis::prepare(source_text).map_err(terminal)?;

    let mut names = NameIndex::new();
    // Pass one: module, package, and type declarations. Type declarations
    // carry the recursive terminal — a nominal row naming their own ordinal.
    let mut declaration_ordinals = [0_u32; MAX_EMISSION_FACTS];
    for (declaration_index, declared) in image.declarations().enumerate() {
        let declared =
            declared.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        let ordinal = match declared.kind {
            DeclarationKind::Class
            | DeclarationKind::Interface
            | DeclarationKind::Record
            | DeclarationKind::Enum
            | DeclarationKind::Annotation => {
                let ordinal = push_type_root(facts, image, &declared, declaration_index)?;
                names
                    .record(declared.name.bytes, ordinal)
                    .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
                push_docs(facts, &names, ordinal, &declared)?;
                ordinal
            }
            DeclarationKind::Module => {
                push_root(facts, image, &names, &declared, declaration_index)?
            }
            DeclarationKind::Package if !declared.name.bytes.is_empty() => {
                push_root(facts, image, &names, &declared, declaration_index)?
            }
            // The unnamed package cannot carry package-info annotations or
            // documentation, so this row carries no retainable facts.
            DeclarationKind::Package => 0,
            DeclarationKind::Field
            | DeclarationKind::EnumConstant
            | DeclarationKind::Constructor
            | DeclarationKind::Method => 0,
        };
        *declaration_ordinals
            .get_mut(declaration_index)
            .ok_or_else(|| terminal(ProjectionFault::IndexCapacity))? = ordinal;
    }

    // Pass two: members with projected rows, signature facts, and overloads.
    let mut executables = Executables::new();
    let mut symbols = SymbolIndex::new();
    for (declaration_index, declared) in image.declarations().enumerate() {
        let declared =
            declared.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        match declared.kind {
            DeclarationKind::Field | DeclarationKind::EnumConstant => {
                let ordinal = push_member(facts, image, &names, &declared, declaration_index)?;
                declaration_ordinals[declaration_index] = ordinal;
            }
            DeclarationKind::Constructor | DeclarationKind::Method => {
                let ordinal = push_executable(
                    facts,
                    image,
                    &names,
                    &mut executables,
                    &mut symbols,
                    &declared,
                    declaration_index,
                )?;
                declaration_ordinals[declaration_index] = ordinal;
            }
            DeclarationKind::Module
            | DeclarationKind::Package
            | DeclarationKind::Class
            | DeclarationKind::Interface
            | DeclarationKind::Record
            | DeclarationKind::Enum
            | DeclarationKind::Annotation => {}
        }
    }

    // Pass three: record extensions now that every component field has an ordinal.
    for (declaration_index, declared) in image.declarations().enumerate() {
        let declared =
            declared.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        if declared.kind != DeclarationKind::Record {
            continue;
        }
        let inputs = declaration_extensions(image, declaration_index).map_err(terminal)?;
        let mut components = [0_u32; MAX_REF_LIST_ELEMENTS];
        for (slot, component) in inputs.record_components[..inputs.record_count]
            .iter()
            .enumerate()
        {
            components[slot] = *declaration_ordinals
                .get(*component)
                .ok_or_else(|| terminal(ProjectionFault::IndexCapacity))?;
        }
        let extension = java_facts(facts, &[], &[], &inputs, &components[..inputs.record_count])
            .map_err(lane_terminal)?;
        // Record roots are emitted without an extension in pass one; this
        // replacement is deliberate because component field ordinals exist
        // only after pass two, and attach_extension replaces the slot.
        facts
            .attach_extension(
                declaration_ordinals[declaration_index] as usize,
                EmissionExtension::Java(extension),
            )
            .map_err(lane_terminal)?;
    }

    // Pass four: compiler-resolved call occurrences in image order.
    for reference in image.references() {
        let reference =
            reference.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        push_occurrence(facts, image, &symbols, &span_basis, &reference).map_err(terminal)?;
    }
    Ok(())
}

const fn release_matches(profile: ProfileRelease, authority: JavaRelease) -> bool {
    matches!(
        (profile, authority),
        (ProfileRelease::Java8, JavaRelease::Java8)
            | (ProfileRelease::Java11, JavaRelease::Java11)
            | (ProfileRelease::Java17, JavaRelease::Java17)
            | (ProfileRelease::Java21, JavaRelease::Java21)
            | (ProfileRelease::Java25, JavaRelease::Java25)
    )
}

const fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Module | DeclarationKind::Package => EntityKind::Module,
        DeclarationKind::Class | DeclarationKind::Record => EntityKind::Record,
        DeclarationKind::Interface | DeclarationKind::Annotation => EntityKind::Trait,
        DeclarationKind::Enum => EntityKind::Enum,
        DeclarationKind::Field => EntityKind::Field,
        DeclarationKind::EnumConstant => EntityKind::Variant,
        DeclarationKind::Constructor | DeclarationKind::Method => EntityKind::Function,
    }
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record | EntityKind::Module => SemanticProductConstructor::PRODUCT,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Constant
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter => LEAF_PRODUCT,
    }
}

/// One projected javac type row prepared for a fact: the lattice record, its
/// ordered backward fact-ordinal children, the nearest image atom spelling,
/// and whether the row is `void`.
struct ProjectedType<'image> {
    record: SemanticTypeRecord<'image>,
    children: [u32; MAX_TYPE_CHILDREN],
    child_count: usize,
    spelling: Option<&'image [u8]>,
    void: bool,
}

impl<'image> ProjectedType<'image> {
    /// Wraps one childless lattice record with its nearest atom spelling.
    const fn leaf(record: SemanticTypeRecord<'image>, spelling: Option<&'image [u8]>) -> Self {
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
    fn attach(self, fact: SemanticFact<'image>) -> SemanticFact<'image> {
        let mut fact = fact.typed(self.record);
        for ordinal in self.children.iter().take(self.child_count) {
            fact = fact.type_child(*ordinal, None, 0);
        }
        fact
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

const fn unknown_record(reason: TypeReason, spelling: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Unknown);
    record.payload0 = reason_cell(reason);
    record.text = spelling;
    record
}

const fn reason_cell(reason: TypeReason) -> u32 {
    match reason {
        TypeReason::Unannotated => 0,
        TypeReason::DynamicallyTyped => 1,
        TypeReason::UnresolvedLocalName => 2,
        TypeReason::UnresolvedExternal => 3,
        TypeReason::TruncatedAtDepthLimit => 4,
        TypeReason::OracleGap => 5,
        TypeReason::NoIrRepresentation => 6,
    }
}

/// The recursive terminal: a nominal row naming the fact's own ordinal.
const fn nominal_record(ordinal: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
    record
}

/// A qualified path row whose text cell is the image's own qualified spelling.
const fn qualified_record(spelling: &[u8]) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::QualifiedPath);
    record.text = Some(spelling);
    record
}

/// An array row whose text cell carries the derived arity spelling.
const fn array_record(spelling: &[u8]) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Array);
    record.text = Some(spelling);
    record
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
fn project<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    reference: TypeRef,
    depth: usize,
    anchor: u32,
) -> Result<ProjectedType<'image>, ProjectionFault<'image>> {
    if depth == 0 {
        return Err(ProjectionFault::Depth);
    }
    let row = image.type_fact(reference).map_err(ProjectionFault::Image)?;
    match row.kind {
        TypeKind::Primitive => primitive(&row),
        TypeKind::Void => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_BUILTIN;
            record.text = Some(VOID_SPELLING);
            Ok(ProjectedType::leaf(record, None).with_void())
        }
        TypeKind::Declared => declared(facts, image, names, &row, depth, anchor),
        TypeKind::Array => array(facts, image, names, reference, depth, anchor),
        TypeKind::Variable => {
            let spelling = required_spelling(&row)?;
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(spelling);
            Ok(ProjectedType::leaf(record, Some(spelling)))
        }
        TypeKind::Wildcard => wildcard(facts, image, names, &row, depth, anchor),
        TypeKind::Intersection => members(
            facts,
            image,
            names,
            &row,
            SemanticTypeTag::Intersection,
            depth,
            anchor,
        ),
        TypeKind::Union => members(
            facts,
            image,
            names,
            &row,
            SemanticTypeTag::Union,
            depth,
            anchor,
        ),
        TypeKind::Error => {
            let spelling = required_spelling(&row)?;
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
    row: &TypeFact<'image>,
) -> Result<ProjectedType<'image>, ProjectionFault<'image>> {
    let spelling = required_spelling(row)?;
    let (shape_cell, payload1) = primitive_cells(spelling)?;
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
    record.payload0 = shape_cell;
    record.payload1 = payload1;
    Ok(ProjectedType::leaf(record, Some(spelling)))
}

/// Maps one closed javac primitive spelling onto its exact shape and payload
/// cells: byte/short/int/long are signed fixed-width integers, float/double
/// are fixed-width floats, char is the character shape, boolean the boolean
/// shape.
fn primitive_cells(spelling: &[u8]) -> Result<(u32, u32), ProjectionFault<'_>> {
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
        b"char" => (SHAPE_CHAR, 0),
        b"float" => (SHAPE_FLOAT, TypeWidth::Fixed(32).to_cell()),
        b"double" => (SHAPE_FLOAT, TypeWidth::Fixed(64).to_cell()),
        _ => return Err(ProjectionFault::Primitive { spelling }),
    };
    Ok(cells)
}

/// Projects one declared row: a pure in-image spelling becomes the nominal
/// terminal; a compound uses its own spelling as the application base and
/// retains every argument row. Enclosing rows retain only their final enclosing
/// child, while a foreign compound base becomes an honest anonymous unknown.
fn declared<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    row: &TypeFact<'image>,
    depth: usize,
    anchor: u32,
) -> Result<ProjectedType<'image>, ProjectionFault<'image>> {
    let spelling = required_spelling(row)?;
    let total = row.children.len();
    let enclosing = row.flags & ENCLOSING_FLAG == ENCLOSING_FLAG;
    if total == 0 {
        return Ok(match names.lookup(spelling) {
            Some(ordinal) => ProjectedType::leaf(nominal_record(ordinal), Some(spelling)),
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
            let enclosing = enclosing.ok_or(ProjectionFault::Malformed { kind: row.kind })?;
            let target = child_target(facts, image, names, enclosing, anchor, depth - 1)?;
            if let Some(projected) =
                ProjectedType::leaf(qualified_record(spelling), Some(spelling)).child(target)
            {
                return Ok(projected);
            }
        }
        return Ok(unknown_declared(spelling));
    }
    if argument_count + 1 > MAX_TYPE_CHILDREN {
        return Ok(unknown_declared(spelling));
    }
    let base = match names.lookup(spelling) {
        Some(ordinal) => ordinal,
        None => intern_row(
            facts,
            anchor,
            ProjectedType::leaf(
                unknown_record(TypeReason::UnresolvedExternal, Some(spelling)),
                Some(spelling),
            ),
        )?,
    };
    let mut projected = ProjectedType::leaf(
        SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
        Some(spelling),
    )
    .child(base)
    .ok_or(ProjectionFault::Malformed { kind: row.kind })?;
    for argument in children.take(argument_count) {
        let target = child_target(facts, image, names, argument, anchor, depth - 1)?;
        projected = projected
            .child(target)
            .ok_or(ProjectionFault::Malformed { kind: row.kind })?;
    }
    Ok(projected)
}

/// Projects one array row: the arity walk collapses consecutive array rows
/// into one row whose text carries the derived arity spelling and whose single
/// child targets the ultimate component row, including an honest foreign leaf.
fn array<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    reference: TypeRef,
    depth: usize,
    anchor: u32,
) -> Result<ProjectedType<'image>, ProjectionFault<'image>> {
    let mut arity = 0usize;
    let mut cursor = reference;
    loop {
        let row = image.type_fact(cursor).map_err(ProjectionFault::Image)?;
        if row.kind != TypeKind::Array {
            break;
        }
        arity += 1;
        if arity > DEPTH_LIMIT {
            return Err(ProjectionFault::Depth);
        }
        let mut children = row.children;
        cursor = children
            .next()
            .ok_or(ProjectionFault::Malformed { kind: row.kind })?;
    }
    let component = project(facts, image, names, cursor, depth - 1, anchor)?;
    if component.record.nominal.is_some() {
        if let Some(spelling) = ARITY_SPELLINGS.get(arity.saturating_sub(1)) {
            let target = intern_row(
                facts,
                anchor,
                ProjectedType::leaf(
                    unknown_record(TypeReason::NoIrRepresentation, component.spelling),
                    component.spelling,
                ),
            )?;
            let projected = ProjectedType::leaf(array_record(spelling), component.spelling)
                .child(target)
                .ok_or(ProjectionFault::Malformed {
                    kind: TypeKind::Array,
                })?;
            return Ok(projected);
        }
    }
    let spelling = ARITY_SPELLINGS
        .get(arity - 1)
        .copied()
        .or_else(|| {
            image
                .type_fact(reference)
                .ok()?
                .spelling
                .map(|atom| atom.bytes)
        })
        .ok_or(ProjectionFault::Malformed {
            kind: TypeKind::Array,
        })?;
    let target = if component.record.tag == SemanticTypeTag::Unknown {
        let row = image.type_fact(cursor).map_err(ProjectionFault::Image)?;
        if row.kind == TypeKind::Declared && row.children.len() == 0 {
            let spelling = required_spelling(&row)?;
            intern_row(
                facts,
                anchor,
                ProjectedType::leaf(
                    unknown_record(TypeReason::UnresolvedExternal, Some(spelling)),
                    Some(spelling),
                ),
            )?
        } else {
            intern_row(facts, anchor, component)?
        }
    } else if component.record.tag == SemanticTypeTag::Nominal {
        match component.record.nominal {
            Some(NominalRef::Local(target)) => target.raw,
            _ => {
                return Err(ProjectionFault::Malformed {
                    kind: TypeKind::Array,
                });
            }
        }
    } else {
        intern_row(facts, anchor, component)?
    };
    ProjectedType::leaf(array_record(spelling), Some(spelling))
        .child(target)
        .ok_or(ProjectionFault::Malformed {
            kind: TypeKind::Array,
        })
}

/// Projects one wildcard row: a bound keeps its variance cell and projected
/// child, while an unbounded wildcard alone folds to an oracle-gap unknown.
fn wildcard<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    row: &TypeFact<'image>,
    depth: usize,
    anchor: u32,
) -> Result<ProjectedType<'image>, ProjectionFault<'image>> {
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
    let target = child_target(facts, image, names, bound, anchor, depth - 1)?;
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Wildcard);
    record.payload0 = variance;
    if let Some(projected) = ProjectedType::leaf(record, None).child(target) {
        return Ok(projected);
    }
    let spelling = nearest_spelling(image, bound, depth - 1)?;
    Ok(ProjectedType::leaf(unknown_projection(spelling), spelling))
}

/// Projects one union or intersection row with structurally lowered member
/// children; only an empty or over-wide member list folds to an unknown.
fn members<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    row: &TypeFact<'image>,
    tag: SemanticTypeTag,
    depth: usize,
    anchor: u32,
) -> Result<ProjectedType<'image>, ProjectionFault<'image>> {
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
    let first = children
        .next()
        .ok_or(ProjectionFault::Malformed { kind: row.kind })?;
    let first_spelling = nearest_spelling(image, first, DEPTH_LIMIT)?;
    let mut projected = ProjectedType::leaf(SemanticTypeRecord::leaf(tag), first_spelling);
    let mut reference = first;
    loop {
        let target = child_target(facts, image, names, reference, anchor, depth - 1)?;
        projected = projected
            .child(target)
            .ok_or(ProjectionFault::Malformed { kind: row.kind })?;
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
) -> Result<Option<u32>, ProjectionFault<'image>> {
    let row = image.type_fact(reference).map_err(ProjectionFault::Image)?;
    if row.kind != TypeKind::Declared || row.children.len() != 0 || row.flags & ENCLOSING_FLAG != 0
    {
        return Ok(None);
    }
    let spelling = required_spelling(&row)?;
    Ok(names.lookup(spelling))
}

/// Returns the backward coordinate usable by a compound parent. Bare local
/// nominals retain the fast path; foreign bare leaves become anonymous unknowns
/// and every other shape is interned with the enclosing fact as owner.
fn child_target<'image>(
    facts: &mut FactSet<'image>,
    image: JavaImage<'image>,
    names: &NameIndex<'image>,
    reference: TypeRef,
    anchor: u32,
    depth: usize,
) -> Result<u32, ProjectionFault<'image>> {
    if let Some(ordinal) = pure_nominal(image, names, reference)? {
        return Ok(ordinal);
    }
    let row = image.type_fact(reference).map_err(ProjectionFault::Image)?;
    let projected = if row.kind == TypeKind::Declared && row.children.len() == 0 {
        let spelling = required_spelling(&row)?;
        ProjectedType::leaf(
            unknown_record(TypeReason::UnresolvedExternal, Some(spelling)),
            Some(spelling),
        )
    } else {
        project(facts, image, names, reference, depth, anchor)?
    };
    intern_row(facts, anchor, projected)
}

/// Interns one anonymous record after all child coordinates are produced. The
/// anchor is ownership only; it is never emitted as a semantic child.
fn intern_row<'image>(
    facts: &mut FactSet<'image>,
    anchor: u32,
    projected: ProjectedType<'image>,
) -> Result<u32, ProjectionFault<'image>> {
    let count = u32::try_from(projected.child_count).map_err(|_| ProjectionFault::IndexCapacity)?;
    projected
        .record
        .validate(count)
        .map_err(|_| ProjectionFault::IndexCapacity)?;
    // Children append first so the intern consumes them as this row's own
    // pending range; every target is already interned and strictly backward.
    for target in projected.children.iter().take(projected.child_count) {
        facts
            .anonymous_type_child(*target, None, 0)
            .map_err(|_| ProjectionFault::IndexCapacity)?;
    }
    let row = facts
        .intern_anonymous_type_row(anchor, projected.record)
        .map_err(|fault| ProjectionFault::IndexCapacity)?;
    Ok(row)
}

/// Walks one type subtree to the nearest image atom spelling, following the
/// first child of array, wildcard, union, and intersection rows.
fn nearest_spelling<'image>(
    image: JavaImage<'image>,
    reference: TypeRef,
    depth: usize,
) -> Result<Option<&'image [u8]>, ProjectionFault<'image>> {
    let mut cursor = reference;
    let mut remaining = depth;
    loop {
        if remaining == 0 {
            return Err(ProjectionFault::Depth);
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
    row: &TypeFact<'image>,
) -> Result<&'image [u8], ProjectionFault<'image>> {
    row.spelling
        .map(|atom| atom.bytes)
        .ok_or(ProjectionFault::Malformed { kind: row.kind })
}

/// Bounded qualified-name index of the image's type declarations.
struct NameIndex<'image> {
    entries: [(&'image [u8], u32); MAX_EMISSION_FACTS],
    len: usize,
}

impl<'image> NameIndex<'image> {
    const fn new() -> Self {
        Self {
            entries: [(&[], 0); MAX_EMISSION_FACTS],
            len: 0,
        }
    }

    fn record(&mut self, name: &'image [u8], ordinal: u32) -> Result<(), ProjectionFault<'image>> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity);
        }
        self.entries[self.len] = (name, ordinal);
        self.len += 1;
        Ok(())
    }

    fn lookup(&self, name: &[u8]) -> Option<u32> {
        self.entries
            .iter()
            .take(self.len)
            .find(|(known, _)| *known == name)
            .map(|(_, ordinal)| *ordinal)
    }

    /// Resolves one link spelling: qualified names match exactly and simple
    /// names match as the last qualified segment.
    fn lookup_link(&self, spelling: &[u8]) -> Option<u32> {
        for (name, ordinal) in self.entries.iter().take(self.len) {
            if *name == spelling {
                return Some(*ordinal);
            }
            if let Some(boundary) = name.len().checked_sub(spelling.len() + 1)
                && name.get(boundary) == Some(&b'.')
                && name.get(boundary + 1..) == Some(spelling)
            {
                return Some(*ordinal);
            }
        }
        None
    }
}

/// Bounded index from executable symbol coordinates to declaration ordinals.
struct SymbolIndex {
    entries: [(Option<SymbolRef>, u32); MAX_EMISSION_FACTS],
    len: usize,
}

impl SymbolIndex {
    const fn new() -> Self {
        Self {
            entries: [(None, 0); MAX_EMISSION_FACTS],
            len: 0,
        }
    }

    fn record(&mut self, symbol: SymbolRef, ordinal: u32) -> Result<(), ProjectionFault<'static>> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity);
        }
        self.entries[self.len] = (Some(symbol), ordinal);
        self.len += 1;
        Ok(())
    }

    fn lookup(&self, symbol: SymbolRef) -> Option<u32> {
        self.entries
            .iter()
            .take(self.len)
            .find(|(known, _)| *known == Some(symbol))
            .map(|(_, ordinal)| *ordinal)
    }
}

/// One overload registry entry: executable class, declaring type, simple
/// name, and the pushed fact ordinal.
type ExecutableEntry<'image> = (bool, Option<&'image [u8]>, &'image [u8], u32);

/// Bounded registry of pushed executables for overload sibling discovery.
struct Executables<'image> {
    entries: [ExecutableEntry<'image>; MAX_EMISSION_FACTS],
    len: usize,
}

impl<'image> Executables<'image> {
    const fn new() -> Self {
        Self {
            entries: [(false, None, &[], 0); MAX_EMISSION_FACTS],
            len: 0,
        }
    }

    /// Collects the already-pushed executables that share this declaring type,
    /// name, and executable class, or an error when they exceed the pooled
    /// sibling-list width.
    fn siblings(
        &self,
        constructor: bool,
        owner: Option<&'image [u8]>,
        name: &[u8],
        out: &mut [u32; MAX_REF_LIST_ELEMENTS],
    ) -> Result<usize, ProjectionFault<'image>> {
        let mut count = 0usize;
        for (entry_constructor, entry_owner, entry_name, ordinal) in
            self.entries.iter().take(self.len)
        {
            if *entry_constructor == constructor && *entry_owner == owner && *entry_name == name {
                if count == MAX_REF_LIST_ELEMENTS {
                    return Err(ProjectionFault::SiblingCapacity);
                }
                out[count] = *ordinal;
                count += 1;
            }
        }
        Ok(count)
    }

    fn record(
        &mut self,
        constructor: bool,
        owner: Option<&'image [u8]>,
        name: &'image [u8],
        ordinal: u32,
    ) -> Result<(), ProjectionFault<'image>> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity);
        }
        self.entries[self.len] = (constructor, owner, name, ordinal);
        self.len += 1;
        Ok(())
    }
}

/// Pushes one module or package root with its honestly unknown declared type.
fn push_root<'source>(
    facts: &mut FactSet<'source>,
    image: JavaImage<'source>,
    names: &NameIndex<'source>,
    declared: &Declaration<'source>,
    declaration_index: usize,
) -> Result<u32, JavaCollectError> {
    let kind = entity_kind(declared.kind);
    let inputs = declaration_extensions(image, declaration_index).map_err(terminal)?;
    let extension = java_facts(facts, &[], &[], &inputs, &[]).map_err(lane_terminal)?;
    let fact = SemanticFact::new(kind, declared.name.bytes, constructor(kind))
        .with_extension(EmissionExtension::Java(extension));
    let ordinal = push(facts, fact)?;
    push_docs(facts, names, ordinal, declared)?;
    Ok(ordinal)
}

struct JavaExtensionInputs<'image> {
    annotations: [compiler_languages_java::Atom<'image>; MAX_REF_LIST_ELEMENTS],
    annotation_count: usize,
    record_components: [usize; MAX_REF_LIST_ELEMENTS],
    record_count: usize,
}

fn declaration_extensions<'image>(
    image: JavaImage<'image>,
    declaration_index: usize,
) -> Result<JavaExtensionInputs<'image>, ProjectionFault<'image>> {
    let mut inputs = JavaExtensionInputs {
        annotations: [compiler_languages_java::Atom { bytes: &[] }; MAX_REF_LIST_ELEMENTS],
        annotation_count: 0,
        record_components: [0; MAX_REF_LIST_ELEMENTS],
        record_count: 0,
    };
    for extension in image
        .declaration_extensions(declaration_index)
        .map_err(ProjectionFault::Image)?
    {
        match extension.map_err(ProjectionFault::Image)? {
            DeclarationExtension::Throws(_) => {}
            DeclarationExtension::Annotation(atom) => {
                *inputs
                    .annotations
                    .get_mut(inputs.annotation_count)
                    .ok_or(ProjectionFault::IndexCapacity)? = atom;
                inputs.annotation_count += 1;
            }
            DeclarationExtension::RecordComponent(index) => {
                *inputs
                    .record_components
                    .get_mut(inputs.record_count)
                    .ok_or(ProjectionFault::IndexCapacity)? = index;
                inputs.record_count += 1;
            }
        }
    }
    Ok(inputs)
}

/// Pushes one type declaration with the recursive terminal: a nominal row
/// naming the declaration's own ordinal. Documentation is pushed by the caller
/// after the qualified name joins the link index.
fn push_type_root<'source>(
    facts: &mut FactSet<'source>,
    image: JavaImage<'source>,
    declared: &Declaration<'source>,
    declaration_index: usize,
) -> Result<u32, JavaCollectError> {
    let kind = entity_kind(declared.kind);
    let self_ordinal =
        u32::try_from(facts.len()).map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
    let inputs = declaration_extensions(image, declaration_index).map_err(terminal)?;
    let extension = if declared.kind == DeclarationKind::Record {
        None
    } else {
        Some(java_facts(facts, &[], &[], &inputs, &[]).map_err(lane_terminal)?)
    };
    let mut fact = SemanticFact::new(kind, declared.name.bytes, constructor(kind))
        .typed(nominal_record(self_ordinal));
    if let Some(extension) = extension {
        fact = fact.with_extension(EmissionExtension::Java(extension));
    }
    push(facts, fact)
}

/// Pushes one field or enum constant with its projected declared type.
fn push_member<'source>(
    facts: &mut FactSet<'source>,
    image: JavaImage<'source>,
    names: &NameIndex<'source>,
    declared: &Declaration<'source>,
    declaration_index: usize,
) -> Result<u32, JavaCollectError> {
    let kind = entity_kind(declared.kind);
    let anchor = declared
        .owner
        .and_then(|owner| names.lookup(owner.bytes))
        .ok_or_else(|| {
            terminal(ProjectionFault::Malformed {
                kind: TypeKind::None,
            })
        })?;
    let projected = match declared.semantic_type {
        Some(reference) => {
            project(facts, image, names, reference, DEPTH_LIMIT, anchor).map_err(terminal)?
        }
        None => ProjectedType::leaf(unknown_record(TypeReason::Unannotated, None), None),
    };
    let inputs = declaration_extensions(image, declaration_index).map_err(terminal)?;
    let extension = java_facts(facts, &[], &[], &inputs, &[]).map_err(lane_terminal)?;
    let fact = projected.attach(
        SemanticFact::new(kind, declared.name.bytes, constructor(kind))
            .with_extension(EmissionExtension::Java(extension)),
    );
    let ordinal = push(facts, fact)?;
    push_docs(facts, names, ordinal, declared)?;
    Ok(ordinal)
}

/// Pushes one constructor or method: its parameter and result carrier facts
/// first, then the executable fact whose function product and function-pointer
/// row target those carriers, the overload sibling list, and its docs.
fn push_executable<'source>(
    facts: &mut FactSet<'source>,
    image: JavaImage<'source>,
    names: &NameIndex<'source>,
    executables: &mut Executables<'source>,
    symbols: &mut SymbolIndex,
    declared: &Declaration<'source>,
    declaration_index: usize,
) -> Result<u32, JavaCollectError> {
    let symbol_reference = declared.symbol.ok_or_else(|| {
        terminal(ProjectionFault::Malformed {
            kind: TypeKind::None,
        })
    })?;
    let symbol = image
        .symbol(symbol_reference)
        .map_err(|cause| terminal(ProjectionFault::Image(cause)))?;
    let is_constructor = declared.kind == DeclarationKind::Constructor;
    let anchor = declared
        .owner
        .and_then(|owner| names.lookup(owner.bytes))
        .ok_or_else(|| {
            terminal(ProjectionFault::OrphanOwner {
                owner: symbol_reference,
            })
        })?;

    // Parameter carriers first so every executable target stays backward.
    let mut parameter_ordinals = [0_u32; MAX_FACT_CHILDREN];
    let mut signature_children = [0_u32; MAX_TYPE_CHILDREN];
    let mut parameter_count = 0usize;
    let mut signature_count = 0usize;
    for parameter in symbol.parameters {
        let projected =
            project(facts, image, names, parameter, DEPTH_LIMIT, anchor).map_err(terminal)?;
        let name = projected.spelling.ok_or_else(|| {
            terminal(ProjectionFault::Malformed {
                kind: TypeKind::None,
            })
        })?;
        let carrier =
            projected.attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT));
        let ordinal = push(facts, carrier)?;
        if let Some(slot) = parameter_ordinals.get_mut(parameter_count) {
            *slot = ordinal;
        }
        if let Some(slot) = signature_children.get_mut(signature_count) {
            *slot = ordinal;
        }
        parameter_count += 1;
        signature_count += 1;
    }

    // Result carrier for non-void methods; constructors carry none.
    let mut result_ordinal = None;
    if !is_constructor && let Some(return_type) = declared.semantic_type {
        let projected =
            project(facts, image, names, return_type, DEPTH_LIMIT, anchor).map_err(terminal)?;
        if !projected.void {
            let name = projected.spelling.ok_or_else(|| {
                terminal(ProjectionFault::Malformed {
                    kind: TypeKind::None,
                })
            })?;
            let carrier =
                projected.attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT));
            let ordinal = push(facts, carrier)?;
            if let Some(slot) = signature_children.get_mut(signature_count) {
                *slot = ordinal;
            }
            signature_count += 1;
            result_ordinal = Some(ordinal);
        }
    }

    let inputs = declaration_extensions(image, declaration_index).map_err(terminal)?;
    let mut throw_ordinals = [0_u32; MAX_REF_LIST_ELEMENTS];
    let mut throw_count = 0usize;
    for extension in image
        .declaration_extensions(declaration_index)
        .map_err(|error| terminal(ProjectionFault::Image(error)))?
    {
        let DeclarationExtension::Throws(reference) =
            extension.map_err(|error| terminal(ProjectionFault::Image(error)))?
        else {
            continue;
        };
        let projected =
            project(facts, image, names, reference, DEPTH_LIMIT, anchor).map_err(terminal)?;
        let name = projected.spelling.ok_or_else(|| {
            terminal(ProjectionFault::Malformed {
                kind: TypeKind::None,
            })
        })?;
        throw_ordinals[throw_count] = push(
            facts,
            projected.attach(SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)),
        )?;
        throw_count += 1;
    }

    // Function-pointer row: the result-presence flag commits only with a
    // result carrier, matching the javac return type.
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
    if result_ordinal.is_some() {
        record.payload1 = SemanticTypeRecord::RESULT_FLAG;
    }

    // Overload siblings: distinct javac signatures already differ in their
    // constructor and type cells, so identities never need ordinal folding.
    let mut siblings = [0_u32; MAX_REF_LIST_ELEMENTS];
    let sibling_count = executables
        .siblings(
            is_constructor,
            declared.owner.map(|atom| atom.bytes),
            declared.name.bytes,
            &mut siblings,
        )
        .map_err(terminal)?;
    let Some(sibling_ordinals) = siblings.get(..sibling_count) else {
        return Err(terminal(ProjectionFault::IndexCapacity));
    };
    let extension = java_facts(
        facts,
        sibling_ordinals,
        &throw_ordinals[..throw_count],
        &inputs,
        &[],
    )
    .map_err(lane_terminal)?;

    let parameter_total =
        u32::try_from(parameter_count).map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
    let mut fact = SemanticFact::new(
        EntityKind::Function,
        declared.name.bytes,
        SemanticProductConstructor::function(parameter_total, u32::from(result_ordinal.is_some())),
    )
    .typed(record)
    .with_extension(EmissionExtension::Java(extension));
    for ordinal in parameter_ordinals.iter().take(parameter_count) {
        fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
    }
    if let Some(ordinal) = result_ordinal {
        fact = fact.child(ProductChildRole::FunctionResult, ordinal);
    }
    for ordinal in signature_children.iter().take(signature_count) {
        fact = fact.type_child(*ordinal, None, 0);
    }
    let ordinal = push(facts, fact)?;
    symbols
        .record(symbol_reference, ordinal)
        .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
    executables
        .record(
            is_constructor,
            declared.owner.map(|atom| atom.bytes),
            declared.name.bytes,
            ordinal,
        )
        .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
    push_docs(facts, names, ordinal, declared)?;
    Ok(ordinal)
}

/// Builds one Java extension row. Throws, annotations, and record components
/// stay on the empty pooled lists because image version 1 carries no such
/// planes; interning the empty lists keeps every pooled coordinate valid
/// against the reopened pools lane.
fn java_facts<'source>(
    facts: &mut FactSet<'source>,
    siblings: &[u32],
    throws: &[u32],
    inputs: &JavaExtensionInputs<'source>,
    record_components: &[u32],
) -> Result<JavaFacts, FactFault> {
    let overloads = facts.intern_entity_list(siblings)?;
    let mut atoms = [0_u32; MAX_REF_LIST_ELEMENTS];
    for (slot, atom) in inputs.annotations[..inputs.annotation_count]
        .iter()
        .enumerate()
    {
        atoms[slot] = facts.intern_atom(atom.bytes)?;
    }
    let annotations = facts.intern_atom_list(&atoms[..inputs.annotation_count])?;
    let throws = facts.intern_type_list(throws)?;
    let record_components = facts.intern_entity_list(record_components)?;
    Ok(JavaFacts {
        throws,
        annotations,
        overloads,
        record_components,
    })
}

/// Pushes one resolved call occurrence: an in-image target folds to a local
/// ordinal, every other javac-resolved target to a maven-namespaced foreign
/// key, both at oracle confidence. javac's UTF-16 source coordinates are
/// projected onto the bound source's byte domain; the image carries no
/// declaration spans, so the file start is the only provable shared basis.
fn push_occurrence<'source>(
    facts: &mut FactSet<'source>,
    image: JavaImage<'source>,
    symbols: &SymbolIndex,
    span_basis: &SpanBasis,
    reference: &Reference<'source>,
) -> Result<(), ProjectionFault<'source>> {
    let owner = symbols
        .lookup(reference.owner)
        .ok_or(ProjectionFault::OrphanOwner {
            owner: reference.owner,
        })?;
    let target = match symbols.lookup(reference.target) {
        Some(ordinal) => OccurrenceTarget::Local(EntityId::new(ordinal)),
        None => {
            let symbol = image
                .symbol(reference.target)
                .map_err(ProjectionFault::Image)?;
            let namespace = symbol.owner.utf8().map_err(|_| ProjectionFault::Utf8)?;
            let name = symbol.name.utf8().map_err(|_| ProjectionFault::Utf8)?;
            // The image stores the declaring type and member name as two
            // separate atoms, so the declaring type travels as the namespace
            // and the member as the canonical path — no allocation, no join.
            let key = ForeignKey::new(
                ForeignOrigin::Namespace {
                    ecosystem: "maven",
                    namespace,
                },
                name,
                name,
                Some(EntityKind::Function),
            )
            .map_err(ProjectionFault::ForeignKey)?;
            OccurrenceTarget::Foreign(key)
        }
    };
    let start = span_basis.byte_offset(reference.start)?;
    let end = span_basis.byte_offset(reference.end)?;
    let utf16_len = span_basis.utf16_len();
    let span = RelSpan::new(start, end).map_err(|_| ProjectionFault::Utf16 {
        units: reference.start,
        utf16_len,
    })?;
    facts
        .push_occurrence(
            owner,
            Occurrence {
                target,
                kind: ReferenceKind::FunctionCall,
                confidence: OccurrenceConfidence::Oracle,
                span,
            },
        )
        .map_err(|_| ProjectionFault::IndexCapacity)?;
    Ok(())
}

/// Streams one declaration's Javadoc atom into the documentation lane as text
/// lines, soft breaks, inline code runs, and links to local or foreign
/// declarations. Traditional Javadoc carries inline tags; Markdown
/// documentation keeps plain text lines.
fn push_docs<'source>(
    facts: &mut FactSet<'source>,
    names: &NameIndex<'source>,
    owner: u32,
    declared: &Declaration<'source>,
) -> Result<(), JavaCollectError> {
    let Some(documentation) = declared.documentation else {
        return Ok(());
    };
    let doc = documentation.bytes;
    let mut line_start = 0usize;
    while line_start < doc.len() {
        let line_end = doc
            .get(line_start..)
            .unwrap_or(&[])
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(doc.len(), |at| line_start + at);
        let line = doc.get(line_start..line_end).unwrap_or(&[]);
        match declared.documentation_flavor {
            DocFlavor::Traditional => push_doc_line(facts, names, owner, line).map_err(terminal)?,
            DocFlavor::Markdown | DocFlavor::Absent => {
                push_text(facts, owner, line)
                    .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
            }
        }
        if line_end == doc.len() {
            break;
        }
        facts
            .push_doc(owner, DocFragmentInput::SoftBreak)
            .map_err(|_| terminal(ProjectionFault::IndexCapacity))?;
        line_start = line_end + 1;
    }
    Ok(())
}

/// Emits one traditional doc line, splitting `{@code …}` and `{@link …}`
/// inline tags into code and link fragments; the surrounding prose stays text.
fn push_doc_line<'source>(
    facts: &mut FactSet<'source>,
    names: &NameIndex<'source>,
    owner: u32,
    line: &'source [u8],
) -> Result<(), ProjectionFault<'source>> {
    let mut cursor = 0usize;
    while cursor < line.len() {
        let rest = line.get(cursor..).unwrap_or(&[]);
        match inline_tag(rest) {
            None => {
                push_text(facts, owner, rest).map_err(|_| ProjectionFault::IndexCapacity)?;
                break;
            }
            Some(tag) => {
                let prose = rest.get(..tag.at).unwrap_or(&[]);
                push_text(facts, owner, prose).map_err(|_| ProjectionFault::IndexCapacity)?;
                if tag.link {
                    push_link(facts, names, owner, tag.interior)
                        .map_err(|_| ProjectionFault::IndexCapacity)?;
                } else if !tag.interior.is_empty() {
                    facts
                        .push_doc(owner, DocFragmentInput::Code(tag.interior))
                        .map_err(|_| ProjectionFault::IndexCapacity)?;
                }
                cursor += tag.after;
            }
        }
    }
    Ok(())
}

/// One found Javadoc inline tag, borrowing its interior from the line.
struct InlineTag<'line> {
    /// Whether the tag is `{@link}` (as opposed to `{@code}`).
    link: bool,
    /// Byte offset of the opener within the scanned slice.
    at: usize,
    /// Byte offset just past the closing brace within the scanned slice.
    after: usize,
    /// The tag's borrowed interior bytes.
    interior: &'line [u8],
}

/// Finds the first `{@code ` or `{@link ` inline tag in the scanned slice.
fn inline_tag(line: &[u8]) -> Option<InlineTag<'_>> {
    let code = find(line, INLINE_CODE_OPENER);
    let link = find(line, INLINE_LINK_OPENER);
    let (at, is_link) = match (code, link) {
        (Some(code_at), Some(link_at)) if link_at < code_at => (link_at, true),
        (Some(code_at), _) => (code_at, false),
        (None, Some(link_at)) => (link_at, true),
        (None, None) => return None,
    };
    let opener = if is_link {
        INLINE_LINK_OPENER
    } else {
        INLINE_CODE_OPENER
    };
    let interior_start = at + opener.len();
    let close = find(line.get(interior_start..)?, b"}")? + interior_start;
    Some(InlineTag {
        link: is_link,
        at,
        after: close + 1,
        interior: line.get(interior_start..close).unwrap_or(&[]),
    })
}

/// Emits one borrowed text fragment when the bytes are non-empty; the
/// documentation lane rejects empty text cells.
fn push_text<'source>(
    facts: &mut FactSet<'source>,
    owner: u32,
    bytes: &'source [u8],
) -> Result<(), FactFault> {
    if bytes.is_empty() {
        return Ok(());
    }
    facts.push_doc(owner, DocFragmentInput::Text(bytes))
}

/// Emits one `{@link target label}` fragment: a target matching an admitted
/// declaration links locally, everything else links to the maven ecosystem by
/// its raw spelling bytes.
fn push_link<'source>(
    facts: &mut FactSet<'source>,
    names: &NameIndex<'source>,
    owner: u32,
    interior: &'source [u8],
) -> Result<(), FactFault> {
    let split = interior
        .iter()
        .position(|byte| *byte == b' ' || *byte == b'\t');
    let (spelling, label) = match split {
        Some(at) => {
            let target = interior.get(..at).unwrap_or(&[]);
            let trimmed = trim(interior.get(at + 1..).unwrap_or(&[]));
            if trimmed.is_empty() {
                (target, target)
            } else {
                (target, trimmed)
            }
        }
        None => (interior, interior),
    };
    if label.is_empty() {
        return Ok(());
    }
    let declaration = spelling
        .iter()
        .position(|byte| *byte == b'#')
        .and_then(|at| spelling.get(..at))
        .unwrap_or(spelling);
    if declaration.is_empty() {
        return push_text(facts, owner, trim(interior));
    }
    let target = match names.lookup_link(declaration) {
        Some(ordinal) => DocLinkTarget::Local(EntityId::new(ordinal)),
        None => DocLinkTarget::Foreign {
            ecosystem: b"maven",
            path: declaration,
        },
    };
    facts.push_doc(owner, DocFragmentInput::Link { label, target })
}

/// Trims ASCII whitespace from both borrowed ends without copying.
fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    bytes.get(start..end).unwrap_or(&[])
}

/// Searches one byte slice for another, returning the relative offset.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|at| {
        haystack
            .get(*at..at + needle.len())
            .is_some_and(|window| window == needle)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use compiler_ir::{DocFactFault, OccurrenceFault};
    use compiler_ir::{FragmentView, LanguageExtensionWireFact, SourceIdentity, TypeFactFault};
    use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
    use sha2::Sha256;

    const HEADER_BYTES: usize = 176;
    const V2_HEADER_BYTES: usize = 208;
    const DIRECTORY_OFFSET: usize = 48;
    const DIRECTORY_ENTRY_BYTES: usize = 16;
    const IMAGE_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v1\0";
    const V2_IMAGE_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v2\0";
    const BOUND_DOMAIN: &[u8] = b"nudox.java.bound.authority.image.sha256.v1\0";
    const ABSENT: u32 = u32::MAX;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("collection rejected the authority image: {0:?}")]
        Collect(JavaCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(crate::lower::AdmissionFault),
        #[error("fragment validation rejected the bytes: {0:?}")]
        Validate(compiler_ir::FragmentError),
        #[error("type fact cursor rejected: {0:?}")]
        TypeFact(TypeFactFault),
        #[error("occurrence cursor rejected: {0:?}")]
        Occurrence(OccurrenceFault),
        #[error("documentation cursor rejected: {0:?}")]
        Doc(DocFactFault),
        #[error("fixture scalar conversion failed: {0}")]
        Num(core::num::TryFromIntError),
        #[error("expected {0}")]
        Missing(&'static str),
        #[error("committed bytes changed")]
        Tail,
    }

    impl From<JavaCollectError> for TestError {
        fn from(error: JavaCollectError) -> Self {
            Self::Collect(error)
        }
    }

    impl From<crate::lower::AdmissionFault> for TestError {
        fn from(error: crate::lower::AdmissionFault) -> Self {
            Self::Admission(error)
        }
    }

    impl From<compiler_ir::FragmentError> for TestError {
        fn from(error: compiler_ir::FragmentError) -> Self {
            Self::Validate(error)
        }
    }

    impl From<TypeFactFault> for TestError {
        fn from(error: TypeFactFault) -> Self {
            Self::TypeFact(error)
        }
    }

    impl From<OccurrenceFault> for TestError {
        fn from(error: OccurrenceFault) -> Self {
            Self::Occurrence(error)
        }
    }

    impl From<DocFactFault> for TestError {
        fn from(error: DocFactFault) -> Self {
            Self::Doc(error)
        }
    }

    impl From<core::num::TryFromIntError> for TestError {
        fn from(error: core::num::TryFromIntError) -> Self {
            Self::Num(error)
        }
    }

    #[derive(Clone)]
    struct TypeRow {
        kind: u8,
        flags: u8,
        atom: Option<usize>,
        children: Vec<u32>,
    }

    #[derive(Clone)]
    struct SymbolRow {
        owner: usize,
        name: usize,
        parameters: Vec<u32>,
    }

    #[derive(Clone)]
    struct DeclarationRow {
        kind: u8,
        name: usize,
        owner: Option<usize>,
        documentation: Option<usize>,
        semantic_type: Option<u32>,
        symbol: Option<u32>,
    }

    #[derive(Clone)]
    struct ReferenceRow {
        owner: u32,
        target: u32,
        file: usize,
        start: u32,
        end: u32,
    }

    #[derive(Clone, Copy)]
    struct ExtensionRow {
        tag: u8,
        value: u32,
    }

    /// One javac authority image under construction, encoded exactly like the
    /// vendored doclet writer: canonical planes, fixed directory, domain
    /// separated checksums, and the source-bound outer envelope.
    #[derive(Clone, Default)]
    struct Fixture {
        atoms: Vec<Vec<u8>>,
        types: Vec<TypeRow>,
        symbols: Vec<SymbolRow>,
        declarations: Vec<DeclarationRow>,
        references: Vec<ReferenceRow>,
        /// Emits v2 only when this per-declaration extension table is non-empty;
        /// otherwise the original v1 fixture bytes remain the compatibility oracle.
        extensions: Vec<Vec<ExtensionRow>>,
    }

    impl Fixture {
        fn atom(&mut self, text: &[u8]) -> usize {
            self.atoms.push(text.to_vec());
            self.atoms.len() - 1
        }

        fn declared(&mut self, spelling: &[u8]) -> u32 {
            let atom = self.atom(spelling);
            self.types.push(TypeRow {
                kind: 3,
                flags: 0,
                atom: Some(atom),
                children: Vec::new(),
            });
            u32::try_from(self.types.len() - 1).unwrap_or(u32::MAX)
        }

        fn class(&mut self, qualified: &[u8]) -> u32 {
            let name = self.atom(qualified);
            let semantic_type = self.declared(qualified);
            self.declarations.push(DeclarationRow {
                kind: 3,
                name,
                owner: None,
                documentation: None,
                semantic_type: Some(semantic_type),
                symbol: None,
            });
            u32::try_from(self.declarations.len() - 1).unwrap_or(u32::MAX)
        }

        fn package(&mut self, name: &[u8]) -> u32 {
            let name = self.atom(name);
            self.declarations.push(DeclarationRow {
                kind: 2,
                name,
                owner: None,
                documentation: None,
                semantic_type: None,
                symbol: None,
            });
            u32::try_from(self.declarations.len() - 1).unwrap_or(u32::MAX)
        }

        fn extension(&mut self, declaration: usize, tag: u8, value: u32) {
            while self.extensions.len() <= declaration {
                self.extensions.push(Vec::new());
            }
            self.extensions[declaration].push(ExtensionRow { tag, value });
        }

        fn bind(&self, source: &[u8]) -> Result<Vec<u8>, TestError> {
            let inner = self.encode_image()?;
            let mut bound = vec![0_u8; 80];
            bound[..4].copy_from_slice(b"NJAB");
            bound[4..6].copy_from_slice(&1_u16.to_le_bytes());
            bound[6..8].copy_from_slice(&80_u16.to_le_bytes());
            bound[8..12].copy_from_slice(&u32::try_from(inner.len())?.to_le_bytes());
            bound[12..44].copy_from_slice(Sha256::digest(source).as_slice());
            let mut digest = Sha256::new();
            digest.update(BOUND_DOMAIN);
            digest.update(&bound[..44]);
            digest.update(&bound[76..80]);
            digest.update(&inner);
            bound[44..76].copy_from_slice(&digest.finalize());
            bound.extend_from_slice(&inner);
            Ok(bound)
        }

        fn encode_image(&self) -> Result<Vec<u8>, TestError> {
            let cell = |value: usize| u32::try_from(value).map_err(TestError::from);
            let mut atoms = Vec::new();
            let mut atom_bytes = Vec::new();
            for text in &self.atoms {
                atoms.extend_from_slice(&cell(atom_bytes.len())?.to_le_bytes());
                atoms.extend_from_slice(&cell(text.len())?.to_le_bytes());
                atom_bytes.extend_from_slice(text);
            }
            let mut types = Vec::new();
            let mut type_children = Vec::new();
            for row in &self.types {
                let start = cell(type_children.len() / 4)?;
                for child in &row.children {
                    type_children.extend_from_slice(&child.to_le_bytes());
                }
                types.push(row.kind);
                types.push(row.flags);
                types.extend_from_slice(&u16::try_from(row.children.len())?.to_le_bytes());
                let atom = match row.atom {
                    Some(atom) => cell(atom)?,
                    None => ABSENT,
                };
                types.extend_from_slice(&atom.to_le_bytes());
                types.extend_from_slice(&start.to_le_bytes());
                types.extend_from_slice(&0_u32.to_le_bytes());
            }
            let mut symbols = Vec::new();
            let mut symbol_parameters = Vec::new();
            for row in &self.symbols {
                let start = cell(symbol_parameters.len() / 4)?;
                for parameter in &row.parameters {
                    symbol_parameters.extend_from_slice(&parameter.to_le_bytes());
                }
                symbols.extend_from_slice(&cell(row.owner)?.to_le_bytes());
                symbols.extend_from_slice(&cell(row.name)?.to_le_bytes());
                symbols.extend_from_slice(&start.to_le_bytes());
                symbols.extend_from_slice(&u16::try_from(row.parameters.len())?.to_le_bytes());
                symbols.extend_from_slice(&0_u16.to_le_bytes());
            }
            let mut declarations = Vec::new();
            for row in &self.declarations {
                let owner = match row.owner {
                    Some(owner) => cell(owner)?,
                    None => ABSENT,
                };
                let documentation = match row.documentation {
                    Some(documentation) => cell(documentation)?,
                    None => ABSENT,
                };
                declarations.push(row.kind);
                declarations.push(1);
                declarations.push(u8::from(row.documentation.is_some()));
                declarations.push(0);
                declarations.extend_from_slice(&0_u32.to_le_bytes());
                declarations.extend_from_slice(&cell(row.name)?.to_le_bytes());
                declarations.extend_from_slice(&owner.to_le_bytes());
                declarations.extend_from_slice(&documentation.to_le_bytes());
                declarations.extend_from_slice(&ABSENT.to_le_bytes());
                declarations.extend_from_slice(&row.semantic_type.unwrap_or(ABSENT).to_le_bytes());
                declarations.extend_from_slice(&row.symbol.unwrap_or(ABSENT).to_le_bytes());
            }
            let mut references = Vec::new();
            for row in &self.references {
                references.extend_from_slice(&row.owner.to_le_bytes());
                references.extend_from_slice(&row.target.to_le_bytes());
                references.extend_from_slice(&cell(row.file)?.to_le_bytes());
                references.extend_from_slice(&row.start.to_le_bytes());
                references.extend_from_slice(&row.end.to_le_bytes());
            }
            let mut declaration_extensions = Vec::new();
            let mut extension_entries = Vec::new();
            for index in 0..self.declarations.len() {
                let rows = self.extensions.get(index).map_or(&[][..], Vec::as_slice);
                let start = if rows.is_empty() {
                    0
                } else {
                    u32::try_from(extension_entries.len() / 8)?
                };
                declaration_extensions.extend_from_slice(&start.to_le_bytes());
                declaration_extensions.extend_from_slice(&u32::try_from(rows.len())?.to_le_bytes());
                for row in rows {
                    extension_entries.push(row.tag);
                    extension_entries.extend_from_slice(&[0; 3]);
                    extension_entries.extend_from_slice(&row.value.to_le_bytes());
                }
            }
            let v2 = !extension_entries.is_empty();
            let sections = if v2 {
                vec![
                    atoms,
                    atom_bytes,
                    types,
                    type_children,
                    symbols,
                    symbol_parameters,
                    declarations,
                    references,
                    declaration_extensions,
                    extension_entries,
                ]
            } else {
                vec![
                    atoms,
                    atom_bytes,
                    types,
                    type_children,
                    symbols,
                    symbol_parameters,
                    declarations,
                    references,
                ]
            };
            let row_bytes = if v2 {
                vec![8_u16, 1, 16, 4, 16, 4, 32, 20, 8, 8]
            } else {
                vec![8_u16, 1, 16, 4, 16, 4, 32, 20]
            };
            let header_bytes = if v2 { V2_HEADER_BYTES } else { HEADER_BYTES };
            let mut image = vec![0_u8; header_bytes];
            image[..4].copy_from_slice(b"NJAI");
            image[4..6].copy_from_slice(&(if v2 { 2_u16 } else { 1_u16 }).to_le_bytes());
            image[6..8].copy_from_slice(&u16::try_from(header_bytes)?.to_le_bytes());
            image[8..10].copy_from_slice(&21_u16.to_le_bytes());
            image[10..12].copy_from_slice(&u16::try_from(sections.len())?.to_le_bytes());
            let body = sections.iter().map(Vec::len).sum::<usize>();
            image[12..16].copy_from_slice(&u32::try_from(body)?.to_le_bytes());
            let mut offset = header_bytes;
            for (index, section) in sections.iter().enumerate() {
                let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
                image[entry..entry + 2].copy_from_slice(&u16::try_from(index + 1)?.to_le_bytes());
                image[entry + 2..entry + 4].copy_from_slice(&row_bytes[index].to_le_bytes());
                let count = section.len() / usize::from(row_bytes[index]);
                image[entry + 4..entry + 8].copy_from_slice(&u32::try_from(count)?.to_le_bytes());
                image[entry + 8..entry + 12].copy_from_slice(&u32::try_from(offset)?.to_le_bytes());
                image[entry + 12..entry + 16]
                    .copy_from_slice(&u32::try_from(section.len())?.to_le_bytes());
                offset += section.len();
            }
            let mut digest = Sha256::new();
            digest.update(if v2 { V2_IMAGE_DOMAIN } else { IMAGE_DOMAIN });
            digest.update(&image[..16]);
            digest.update(&image[DIRECTORY_OFFSET..header_bytes]);
            for section in &sections {
                digest.update(section);
            }
            image[16..DIRECTORY_OFFSET].copy_from_slice(&digest.finalize());
            let mut complete = image;
            for section in sections {
                complete.extend_from_slice(&section);
            }
            Ok(complete)
        }
    }

    /// Lowers one fixture image and writes the committed fragment, proving the
    /// untouched output tail stayed unchanged.
    fn lower(fix: &Fixture, source: &[u8]) -> Result<Vec<u8>, TestError> {
        let image = fix.bind(source)?;
        let mut facts = FactSet::new();
        collect(ProfileRelease::Java21, source, &image, &mut facts)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len())?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Java(compiler_vocabulary::JavaRelease::Java21),
            Stage::LowerIr,
            NativeTool::JavaCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"java-authority-toolchain"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length = crate::lower::admit(&facts, identity, recipe, recipe.profile, &mut output)
            .map_err(TestError::from)?
            .len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    fn row<'fragment>(
        view: &FragmentView<'fragment>,
        ordinal: usize,
    ) -> Result<compiler_ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        cursor
            .nth(ordinal)
            .ok_or(TestError::Missing("type row"))?
            .map_err(TestError::from)
    }

    fn java_extension(view: &FragmentView<'_>, ordinal: usize) -> Result<JavaFacts, TestError> {
        let payload = view
            .language_extension_payload()
            .ok_or(TestError::Missing("extension section"))?;
        let word = |at: usize| -> Result<u32, TestError> {
            let bytes: [u8; 4] = payload
                .get(at..at + 4)
                .ok_or(TestError::Missing("extension word"))?
                .try_into()
                .map_err(|_| TestError::Missing("extension word"))?;
            Ok(u32::from_le_bytes(bytes))
        };
        let directory = 16 + 5 * 20;
        let rows = usize::try_from(word(directory + 4)?)?;
        let offset = usize::try_from(word(directory + 12)?)?;
        let fact = word(offset + ordinal * 4)?;
        if fact == ABSENT {
            return Err(TestError::Missing("extension row"));
        }
        JavaFacts::decode(payload, offset + rows * 4 + usize::try_from(fact)? * 16)
            .ok_or(TestError::Missing("java extension decode"))
    }

    fn pool_list(pools: &[u8], lane: usize, ordinal: usize) -> Result<Vec<u32>, TestError> {
        let word = |at: usize| -> Result<u32, TestError> {
            let bytes: [u8; 4] = pools
                .get(at..at + 4)
                .ok_or(TestError::Missing("pool word"))?
                .try_into()
                .map_err(|_| TestError::Missing("pool word"))?;
            Ok(u32::from_le_bytes(bytes))
        };
        let mut cursor = 4;
        for current_lane in 0..3 {
            let count = usize::try_from(word(cursor)?)?;
            cursor += 4;
            for current in 0..count {
                let length = usize::try_from(word(cursor)?)?;
                if current_lane == lane && current == ordinal {
                    return (0..length)
                        .map(|index| word(cursor + 4 + index * 4))
                        .collect();
                }
                cursor += 4 + length * 4;
            }
        }
        Err(TestError::Missing("pooled list"))
    }

    fn atom_list<'a>(
        view: &'a FragmentView<'a>,
        ordinal: usize,
    ) -> Result<Vec<&'a [u8]>, TestError> {
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        pool_list(
            view.extension_pool_payload()
                .ok_or(TestError::Missing("pools"))?,
            0,
            ordinal,
        )?
        .into_iter()
        .map(|index| {
            atoms
                .get(usize::try_from(index)?)
                .copied()
                .ok_or(TestError::Missing("atom ordinal"))
        })
        .collect()
    }

    #[test]
    fn empty_image_admits_the_schema1_fragment_without_semantic_sections() -> Result<(), TestError>
    {
        let fix = Fixture::default();
        let bytes = lower(&fix, b"")?;
        let view = FragmentView::validate(&bytes)?;
        if view.type_facts().is_some() || view.occurrences().is_some() || view.docs().is_some() {
            return Err(TestError::Missing("absent semantic sections"));
        }
        Ok(())
    }

    #[test]
    fn unnamed_package_row_is_skipped_but_class_facts_are_committed() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.package(b"");
        fix.class(b"C");
        let bytes = lower(&fix, b"class C {}")?;
        let view = FragmentView::validate(&bytes)?;
        let entities: Vec<_> = view.entities().collect();
        if entities.len() != 1
            || entities[0].kind != EntityKind::Record
            || view
                .atoms()
                .nth(entities[0].name.raw as usize)
                .map(|atom| atom.bytes)
                != Some(b"C".as_slice())
        {
            return Err(TestError::Missing("unnamed package skipped"));
        }
        Ok(())
    }

    #[test]
    fn named_package_retains_its_annotation_atom() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.package(b"demo");
        let annotation = fix.atom(b"Ldemo/Package;");
        fix.extension(0, 2, u32::try_from(annotation)?);
        let bytes = lower(&fix, b"package demo;")?;
        let view = FragmentView::validate(&bytes)?;
        if atom_list(&view, java_extension(&view, 0)?.annotations.raw as usize)?
            != vec![b"Ldemo/Package;".as_slice()]
        {
            return Err(TestError::Missing("named package annotation"));
        }
        Ok(())
    }

    #[test]
    fn one_class_commits_its_recursive_self_nominal_row() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.Cafe");
        let bytes = lower(&fix, b"class Cafe {}")?;
        let view = FragmentView::validate(&bytes)?;
        let only = row(&view, 0)?;
        if only.owner.raw != 0
            || only.record.tag != SemanticTypeTag::Nominal
            || only.record.nominal != Some(NominalRef::Local(EntityId::new(0)))
        {
            return Err(TestError::Missing("recursive self nominal"));
        }
        if row(&view, 1).is_ok() {
            return Err(TestError::Missing("single row"));
        }
        Ok(())
    }

    #[test]
    fn recursive_type_projects_the_field_nominal_strictly_backward() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        let node = fix.class(b"demo.Node");
        let next = fix.atom(b"next");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: next,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(node),
            symbol: None,
        });
        let bytes = lower(&fix, b"class Node { Node next; }")?;
        let view = FragmentView::validate(&bytes)?;
        let field = row(&view, 1)?;
        if field.record.nominal != Some(NominalRef::Local(EntityId::new(0))) {
            return Err(TestError::Missing("backward field nominal"));
        }
        // Falsifier: retyping the field away from the declaration changes the
        // committed bytes into a typed unknown row.
        let mut mutated = fix.clone();
        let foreign = mutated.declared(b"demo.Other");
        mutated.declarations[1].semantic_type = Some(foreign);
        let other = lower(&mutated, b"class Node { Node next; }")?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let field = row(&view, 1)?;
        if field.record.tag != SemanticTypeTag::Unknown
            || field.record.text != Some(b"demo.Other".as_slice())
        {
            return Err(TestError::Missing("typed unknown for foreign nominal"));
        }
        Ok(())
    }

    #[test]
    fn mutual_recursion_targets_strictly_backward_ordinals() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        let a = fix.class(b"demo.A");
        let b = fix.class(b"demo.B");
        let b_field = fix.atom(b"b");
        let a_field = fix.atom(b"a");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: b_field,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(b),
            symbol: None,
        });
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: a_field,
            owner: Some(1),
            documentation: None,
            semantic_type: Some(a),
            symbol: None,
        });
        let bytes = lower(&fix, b"class A { B b; } class B { A a; }")?;
        let view = FragmentView::validate(&bytes)?;
        let first = row(&view, 2)?;
        let second = row(&view, 3)?;
        if first.record.nominal != Some(NominalRef::Local(EntityId::new(1)))
            || second.record.nominal != Some(NominalRef::Local(EntityId::new(0)))
        {
            return Err(TestError::Missing("mutual backward nominals"));
        }
        Ok(())
    }

    #[test]
    fn primitive_fields_and_signatures_commit_exact_width_and_signedness_cells()
    -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.P");
        let spellings: [&[u8]; 8] = [
            b"byte", b"short", b"int", b"long", b"float", b"double", b"char", b"boolean",
        ];
        let mut field_types = Vec::new();
        for (index, spelling) in spellings.iter().enumerate() {
            let atom = fix.atom(spelling);
            fix.types.push(TypeRow {
                kind: 1,
                flags: 0,
                atom: Some(atom),
                children: Vec::new(),
            });
            let reference = u32::try_from(index + 1)?;
            field_types.push(reference);
            let name = fix.atom(spelling);
            fix.declarations.push(DeclarationRow {
                kind: 8,
                name,
                owner: Some(0),
                documentation: None,
                semantic_type: Some(reference),
                symbol: None,
            });
        }
        let brew = fix.atom(b"brew");
        let sip = fix.atom(b"sip");
        let void_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 2,
            flags: 0,
            atom: None,
            children: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: brew,
            parameters: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: sip,
            parameters: Vec::new(),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: brew,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(void_row),
            symbol: Some(0),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: sip,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(3),
            symbol: Some(1),
        });
        let source = b"class P { byte byte; int sip() { return 0; } void brew() {} }";
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 class, 1..8 fields, 9 void brew, 10 int carrier, 11 sip.
        let byte_row = row(&view, 1)?;
        if byte_row.record.tag != SemanticTypeTag::Primitive
            || byte_row.record.payload0 != SHAPE_INTEGER
            || byte_row.record.payload1 != (8 << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG
        {
            return Err(TestError::Missing("byte width and signedness"));
        }
        let float_row = row(&view, 5)?;
        if float_row.record.payload0 != SHAPE_FLOAT || float_row.record.payload1 != 32 {
            return Err(TestError::Missing("float width cell"));
        }
        let boolean_row = row(&view, 8)?;
        if boolean_row.record.payload0 != SHAPE_BOOL || boolean_row.record.payload1 != 0 {
            return Err(TestError::Missing("boolean shape cell"));
        }
        let void_method = row(&view, 9)?;
        if void_method.record.tag != SemanticTypeTag::FunctionPointer
            || void_method.record.payload1 != 0
            || void_method.record.children.length != 0
        {
            return Err(TestError::Missing("void function pointer"));
        }
        let carrier = row(&view, 10)?;
        if carrier.owner.raw != 10
            || carrier.record.tag != SemanticTypeTag::Primitive
            || carrier.record.payload1 != (32 << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG
        {
            return Err(TestError::Missing("int result carrier"));
        }
        let int_method = row(&view, 11)?;
        if int_method.record.tag != SemanticTypeTag::FunctionPointer
            || int_method.record.payload1 != SemanticTypeRecord::RESULT_FLAG
            || int_method.record.children.length != 1
        {
            return Err(TestError::Missing("int function pointer result flag"));
        }
        if row(&view, 12).is_ok() {
            return Err(TestError::Missing("exact fact count"));
        }
        Ok(())
    }

    #[test]
    fn overload_pair_keeps_distinct_rows_and_lists_earlier_siblings() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.C");
        let brew = fix.atom(b"brew");
        let brew2 = fix.atom(b"brew");
        let int_atom = fix.atom(b"int");
        fix.types.push(TypeRow {
            kind: 1,
            flags: 0,
            atom: Some(int_atom),
            children: Vec::new(),
        });
        let string_row = fix.declared(b"java.lang.String");
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: brew,
            parameters: vec![1],
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: brew2,
            parameters: vec![string_row],
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: brew,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(1),
            symbol: Some(0),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: brew2,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(string_row),
            symbol: Some(1),
        });
        let source = b"class C { int brew() { return 0; } String brew() { return null; } }";
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 class, 1 int param carrier, 2 int result carrier, 3 first
        // brew, 4 unknown param carrier, 5 unknown result carrier, 6 second brew.
        let second = row(&view, 6)?;
        if second.record.tag != SemanticTypeTag::FunctionPointer
            || second.record.payload1 != SemanticTypeRecord::RESULT_FLAG
        {
            return Err(TestError::Missing("second overload row"));
        }
        // The overload sibling lists travel in the pooled lanes: the first
        // brew carries the empty list and the second lists the first's fact
        // ordinal.
        let pools = view
            .extension_pool_payload()
            .ok_or(TestError::Missing("pools"))?;
        if !entity_list(pools, 0)?.is_empty() {
            return Err(TestError::Missing("first overload has no siblings"));
        }
        let listed = entity_list(pools, 1)?;
        if listed != vec![3] {
            return Err(TestError::Missing("overload sibling ordinal"));
        }
        // Falsifier: renaming the second method empties the sibling list and
        // changes the committed bytes.
        let mut mutated = fix.clone();
        mutated.atoms[brew2] = b"shake".to_vec();
        let other = lower(&mutated, source)?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let pools = view
            .extension_pool_payload()
            .ok_or(TestError::Missing("pools"))?;
        // The renamed method joins no sibling list: every list is empty and
        // only the deduplicated empty row remains in the pool.
        if !entity_list(pools, 0)?.is_empty() || entity_list(pools, 1).is_ok() {
            return Err(TestError::Missing("renamed method has no siblings"));
        }
        Ok(())
    }

    /// Reads one pooled entity reference list back out of the pools payload.
    /// With no type parameters the layout is four lane counts plus rows.
    fn entity_list(pools: &[u8], ordinal: usize) -> Result<Vec<u32>, TestError> {
        let word = |at: usize| -> Result<u32, TestError> {
            let raw = pools
                .get(at..at + 4)
                .ok_or(TestError::Missing("pool word"))?;
            let bytes: [u8; 4] = raw
                .try_into()
                .map_err(|_| TestError::Missing("pool word"))?;
            Ok(u32::from_le_bytes(bytes))
        };
        let width = |length: u32| -> Result<usize, TestError> {
            let length = usize::try_from(length)?;
            length.checked_mul(4).ok_or(TestError::Missing("pool word"))
        };
        let mut cursor = 4; // type parameter count (zero)
        for lane in 0..3u32 {
            let count = word(cursor)?;
            cursor += 4;
            for list in 0..count {
                let length = word(cursor)?;
                if lane == 2 && usize::try_from(list)? == ordinal {
                    let mut elements = Vec::new();
                    for position in 0..length {
                        let at = cursor + 4 + width(position)?;
                        elements.push(word(at)?);
                    }
                    return Ok(elements);
                }
                cursor += 4 + width(length)?;
            }
        }
        Err(TestError::Missing("entity list"))
    }

    #[test]
    fn foreign_nested_application_keeps_base_and_argument_rows() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.Cafe");
        let outer = fix.atom(b"java.util.List");
        let inner = fix.atom(b"java.util.List");
        let string = fix.atom(b"java.lang.String");
        let string_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(string),
            children: Vec::new(),
        });
        let inner_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(inner),
            children: vec![string_row],
        });
        let outer_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(outer),
            children: vec![inner_row],
        });
        let field = fix.atom(b"grid");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: field,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(outer_row),
            symbol: None,
        });
        let bytes = lower(&fix, b"class Cafe { List<List<String>> grid; }")?;
        let view = FragmentView::validate(&bytes)?;
        // Anonymous wire rows: 0 outer-base List, 1 inner-base List,
        // 2 String, 3 inner Apply; the outer Apply is the field fact's own
        // record. Facts 4 Cafe, 5 grid.
        let list = row(&view, 1)?;
        let string = row(&view, 2)?;
        let inner = row(&view, 3)?;
        let field = row(&view, 5)?;
        if list.record.tag != SemanticTypeTag::Unknown
            || list.record.payload0 != reason_cell(TypeReason::UnresolvedExternal)
            || list.record.text != Some(b"java.util.List".as_slice())
            || string.record.tag != SemanticTypeTag::Unknown
            || string.record.payload0 != reason_cell(TypeReason::UnresolvedExternal)
            || string.record.text != Some(b"java.lang.String".as_slice())
            || inner.record.tag != SemanticTypeTag::Apply
            || inner.record.children.length != 2
            || field.record.tag != SemanticTypeTag::Apply
            || field.record.children.length != 2
        {
            return Err(TestError::Missing("honest nested foreign application"));
        }
        Ok(())
    }

    #[test]
    fn arrays_retain_foreign_and_primitive_component_rows() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.Cafe");
        let list_atom = fix.atom(b"java.util.List");
        let string_atom = fix.atom(b"java.lang.String");
        let string = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(string_atom),
            children: Vec::new(),
        });
        let array = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 4,
            flags: 0,
            atom: None,
            children: vec![string],
        });
        let list = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(list_atom),
            children: vec![array],
        });
        let rows = fix.atom(b"rows");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: rows,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(list),
            symbol: None,
        });
        let int_atom = fix.atom(b"int");
        let int = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 1,
            flags: 0,
            atom: Some(int_atom),
            children: Vec::new(),
        });
        let int_array = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 4,
            flags: 0,
            atom: None,
            children: vec![int],
        });
        let int_array_2 = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 4,
            flags: 0,
            atom: None,
            children: vec![int_array],
        });
        let numbers = fix.atom(b"numbers");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: numbers,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(int_array_2),
            symbol: None,
        });
        let bytes = lower(
            &fix,
            b"class Cafe { List<String[]> rows; int[][] numbers; }",
        )?;
        let view = FragmentView::validate(&bytes)?;
        // Anonymous wire rows: 0 List leaf, 1 String leaf, 2 [']' over the
        // String leaf, 3 int primitive; the List Apply and the numbers Array
        // ride their field facts. Facts 4 Cafe, 5 rows, 6 numbers.
        let rows = row(&view, 5)?;
        if rows.record.tag != SemanticTypeTag::Apply || rows.record.children.length != 2 {
            return Err(TestError::Missing("array generic argument"));
        }
        let array = row(&view, 2)?;
        if array.record.tag != SemanticTypeTag::Array
            || array.record.text != Some(b"[]".as_slice())
            || array.record.children.length != 1
        {
            return Err(TestError::Missing("foreign array component"));
        }
        let numbers = row(&view, 6)?;
        if numbers.record.tag != SemanticTypeTag::Array
            || numbers.record.text != Some(b"[][]".as_slice())
            || numbers.record.children.length != 1
        {
            return Err(TestError::Missing("primitive array arity"));
        }
        Ok(())
    }

    #[test]
    fn wildcard_bound_and_intersection_keep_structural_children() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.Cafe");
        let list_atom = fix.atom(b"java.util.List");
        let string_atom = fix.atom(b"java.lang.String");
        let string = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(string_atom),
            children: Vec::new(),
        });
        let bound = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(list_atom),
            children: vec![string],
        });
        let wildcard = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 6,
            flags: 1,
            atom: None,
            children: vec![bound],
        });
        let outer = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(list_atom),
            children: vec![wildcard],
        });
        let value = fix.atom(b"value");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: value,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(outer),
            symbol: None,
        });
        let node = u32::try_from(fix.types.len())?;
        fix.class(b"demo.Node");
        let array = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 4,
            flags: 0,
            atom: None,
            children: vec![node],
        });
        let intersection = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 7,
            flags: 0,
            atom: None,
            children: vec![array, node],
        });
        let both = fix.atom(b"both");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: both,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(intersection),
            symbol: None,
        });
        let bytes = lower(
            &fix,
            b"class Cafe { List<? extends List<String>> value; } class Node {}",
        )?;
        let view = FragmentView::validate(&bytes)?;
        // Anonymous wire rows: 0 outer-base List, 1 bound-base List,
        // 2 String, 3 bound Apply, 4 covariant Wildcard over the Apply,
        // 5 Node nominal, and 6 its Array wrapper. The outer List Apply
        // rides the value fact. Facts 7 Cafe, 8 Node, 9 value, 10 both
        // (Intersection over the Node array row and Node).
        let wildcard = row(&view, 4)?;
        if wildcard.record.tag != SemanticTypeTag::Wildcard
            || wildcard.record.payload0 != VARIANCE_COVARIANT
            || wildcard.record.children.length != 1
            || row(&view, 3)?.record.tag != SemanticTypeTag::Apply
        {
            return Err(TestError::Missing("wildcard structural bound"));
        }
        let intersection = row(&view, 10)?;
        if intersection.record.tag != SemanticTypeTag::Intersection
            || intersection.record.children.length != 2
            || row(&view, 6)?.record.tag != SemanticTypeTag::Array
        {
            return Err(TestError::Missing("intersection structural member"));
        }
        Ok(())
    }

    #[test]
    fn span_basis_projects_ascii_mixed_boundaries_and_rejects_hostile_units()
    -> Result<(), TestError> {
        let ascii = SpanBasis::prepare("abc").map_err(|_| TestError::Missing("ASCII basis"))?;
        if !matches!(&ascii, SpanBasis::Ascii { utf16_len: 3 })
            || ascii
                .byte_offset(0)
                .map_err(|_| TestError::Missing("ASCII identity projection"))?
                != 0
            || ascii
                .byte_offset(3)
                .map_err(|_| TestError::Missing("ASCII identity projection"))?
                != 3
        {
            return Err(TestError::Missing("ASCII identity projection"));
        }

        let source = "a😀éz😀b";
        let basis = SpanBasis::prepare(source).map_err(|_| TestError::Missing("mixed basis"))?;
        let exact = [(0, 0), (1, 1), (3, 5), (4, 7), (5, 8), (7, 12), (8, 13)];
        for (units, expected) in exact {
            if basis
                .byte_offset(units)
                .map_err(|_| TestError::Missing("mixed UTF-16 boundary"))?
                != expected
            {
                return Err(TestError::Missing("mixed UTF-16 boundary"));
            }
        }
        for units in [2, 6, 9] {
            match basis.byte_offset(units) {
                Err(ProjectionFault::Utf16 {
                    units: actual,
                    utf16_len: 8,
                }) if actual == units => {}
                _ => return Err(TestError::Missing("surrogate or beyond-end fault")),
            }
        }

        let empty = SpanBasis::prepare("").map_err(|_| TestError::Missing("empty basis"))?;
        if empty
            .byte_offset(0)
            .map_err(|_| TestError::Missing("zero-length reference"))?
            != 0
        {
            return Err(TestError::Missing("zero-length reference"));
        }
        Ok(())
    }

    #[test]
    fn foreign_call_targets_the_maven_namespace_key_and_converts_spans() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.X");
        let method = fix.atom(b"m");
        let intern = fix.atom(b"intern");
        let string_atom = fix.atom(b"java.lang.String");
        let file = fix.atom(b"X.java");
        let void_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 2,
            flags: 0,
            atom: None,
            children: Vec::new(),
        });
        let source_text = "class X { void m() { \u{1F600}.intern(); } }";
        let call_at = source_text
            .find("intern")
            .ok_or(TestError::Missing("call site"))?;
        let units: usize = source_text[..call_at].chars().map(char::len_utf16).sum();
        let start = u32::try_from(units)?;
        let end = start + u32::try_from("intern".len())?;
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: method,
            parameters: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: string_atom,
            name: intern,
            parameters: Vec::new(),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: method,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(void_row),
            symbol: Some(0),
        });
        fix.references.push(ReferenceRow {
            owner: 0,
            target: 1,
            file,
            start,
            end,
        });
        let source = source_text.as_bytes();
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let occurrence = occurrences
            .next()
            .ok_or(TestError::Missing("occurrence"))??;
        if occurrence.owner.raw != 1 {
            return Err(TestError::Missing("owner executable ordinal"));
        }
        let OccurrenceTarget::Foreign(key) = occurrence.occurrence.target else {
            return Err(TestError::Missing("foreign target"));
        };
        if key.path != "intern"
            || key.kind != Some(EntityKind::Function)
            || occurrence.occurrence.confidence != OccurrenceConfidence::Oracle
            || occurrence.occurrence.kind != ReferenceKind::FunctionCall
        {
            return Err(TestError::Missing("maven namespace foreign key"));
        }
        let ForeignOrigin::Namespace {
            ecosystem,
            namespace,
        } = key.origin
        else {
            return Err(TestError::Missing("namespace origin"));
        };
        if ecosystem != "maven" || namespace != "java.lang.String" {
            return Err(TestError::Missing("declaring type namespace"));
        }
        if occurrence.occurrence.span.start != u32::try_from(call_at)? {
            return Err(TestError::Missing("utf16 to byte span start"));
        }
        if occurrence.occurrence.span.end != u32::try_from(call_at + "intern".len())? {
            return Err(TestError::Missing("utf16 to byte span end"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("single occurrence"));
        }
        Ok(())
    }

    #[test]
    fn local_call_targets_the_declared_ordinal_at_oracle_confidence() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.X");
        let caller = fix.atom(b"caller");
        let callee = fix.atom(b"callee");
        let file = fix.atom(b"X.java");
        let source_text = "class X { void caller() { callee(); } void callee() {} }";
        let call_at = source_text
            .find("callee()")
            .ok_or(TestError::Missing("call site"))?;
        let void_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 2,
            flags: 0,
            atom: None,
            children: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: caller,
            parameters: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name: callee,
            parameters: Vec::new(),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: caller,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(void_row),
            symbol: Some(0),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: callee,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(void_row),
            symbol: Some(1),
        });
        fix.references.push(ReferenceRow {
            owner: 0,
            target: 1,
            file,
            start: u32::try_from(call_at)?,
            end: u32::try_from(call_at + "callee".len())?,
        });
        let bytes = lower(&fix, source_text.as_bytes())?;
        let view = FragmentView::validate(&bytes)?;
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let occurrence = occurrences
            .next()
            .ok_or(TestError::Missing("occurrence"))??;
        if occurrence.occurrence.target != OccurrenceTarget::Local(EntityId::new(2)) {
            return Err(TestError::Missing("local callee ordinal"));
        }
        if occurrence.occurrence.confidence != OccurrenceConfidence::Oracle {
            return Err(TestError::Missing("oracle confidence"));
        }
        Ok(())
    }

    #[test]
    fn javadoc_splits_into_text_softbreak_code_and_local_foreign_links() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        let cafe = fix.atom(b"demo.Cafe");
        fix.declared(b"demo.Cafe");
        let doc = fix.atom(
            b"Line one.\nUse {@code brew} and {@link demo.Cafe Cafe}.\nSee {@link java.lang.String}.",
        );
        fix.declarations.push(DeclarationRow {
            kind: 3,
            name: cafe,
            owner: None,
            documentation: Some(doc),
            semantic_type: Some(0),
            symbol: None,
        });
        let bytes = lower(&fix, b"class Cafe {}")?;
        let view = FragmentView::validate(&bytes)?;
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let expected: [DocFragmentInput<'_>; 11] = [
            DocFragmentInput::Text(b"Line one."),
            DocFragmentInput::SoftBreak,
            DocFragmentInput::Text(b"Use "),
            DocFragmentInput::Code(b"brew"),
            DocFragmentInput::Text(b" and "),
            DocFragmentInput::Link {
                label: b"Cafe",
                target: DocLinkTarget::Local(EntityId::new(0)),
            },
            DocFragmentInput::Text(b"."),
            DocFragmentInput::SoftBreak,
            DocFragmentInput::Text(b"See "),
            DocFragmentInput::Link {
                label: b"java.lang.String",
                target: DocLinkTarget::Foreign {
                    ecosystem: b"maven",
                    path: b"java.lang.String",
                },
            },
            DocFragmentInput::Text(b"."),
        ];
        for fragment in expected {
            let fact = docs.next().ok_or(TestError::Missing("doc fact"))??;
            if fact.owner.raw != 0 || fact.fragment != fragment {
                return Err(TestError::Missing("doc fragment"));
            }
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact doc facts"));
        }
        Ok(())
    }

    #[test]
    fn generic_array_and_foreign_fields_project_their_rows() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.Cafe");
        let node = fix.class(b"demo.Node");
        let pair = fix.atom(b"pair");
        let many = fix.atom(b"many");
        let named = fix.atom(b"named");
        let applied = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(0),
            children: vec![node],
        });
        let array_row = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 4,
            flags: 0,
            atom: None,
            children: vec![node],
        });
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: pair,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(applied),
            symbol: None,
        });
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: many,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(array_row),
            symbol: None,
        });
        let foreign = fix.declared(b"java.lang.String");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: named,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(foreign),
            symbol: None,
        });
        let bytes = lower(
            &fix,
            b"class Cafe { Cafe<Node> pair; Node[] many; String named; }",
        )?;
        let view = FragmentView::validate(&bytes)?;
        // Anonymous rows: 0 Node nominal, 1 Node array. Facts: 2 Cafe,
        // 3 Node, 4 pair, 5 many, 6 named.
        let pair_row = row(&view, 3)?;
        if pair_row.record.tag != SemanticTypeTag::Apply
            || pair_row.record.children.length != 2
            || pair_row.record.children.start != 0
        {
            return Err(TestError::Missing("generic application row"));
        }
        let array = row(&view, 4)?;
        if array.record.tag != SemanticTypeTag::Array
            || array.record.text != Some(b"[]".as_slice())
            || array.record.children.length != 1
        {
            return Err(TestError::Missing("array arity row"));
        }
        let foreign_row = row(&view, 5)?;
        if foreign_row.record.tag != SemanticTypeTag::Unknown
            || foreign_row.record.text != Some(b"java.lang.String".as_slice())
        {
            return Err(TestError::Missing("foreign declared unknown"));
        }
        Ok(())
    }

    #[test]
    fn capacity_beyond_the_lane_rejects_exactly() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        for index in 0..crate::lower::MAX_EMISSION_FACTS + 1 {
            let mut spelling = b"p".to_vec();
            spelling.extend_from_slice(index.to_string().as_bytes());
            let name = fix.atom(&spelling);
            fix.declarations.push(DeclarationRow {
                kind: 2,
                name,
                owner: None,
                documentation: None,
                semantic_type: None,
                symbol: None,
            });
        }
        let image = fix.bind(b"")?;
        let mut facts = FactSet::new();
        match collect(ProfileRelease::Java21, b"", &image, &mut facts) {
            Err(JavaCollectError::Lowering(
                compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
            )) => {}
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("capacity rejection")),
        }
        Ok(())
    }

    #[test]
    fn throws_entries_create_parameter_carriers_and_an_ordered_type_list() -> Result<(), TestError>
    {
        let mut fix = Fixture::default();
        fix.class(b"demo.C");
        let first = fix.declared(b"java.io.IOException");
        let second = fix.declared(b"java.sql.SQLException");
        let name = fix.atom(b"run");
        let void = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 2,
            flags: 0,
            atom: None,
            children: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name,
            parameters: Vec::new(),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(void),
            symbol: Some(0),
        });
        fix.extension(1, 1, first);
        fix.extension(1, 1, second);
        let bytes = lower(
            &fix,
            b"class C { void run() throws IOException, SQLException {} }",
        )?;
        let view = FragmentView::validate(&bytes)?;
        if view
            .entities()
            .nth(1)
            .ok_or(TestError::Missing("first carrier"))?
            .kind
            != EntityKind::Parameter
            || view
                .entities()
                .nth(2)
                .ok_or(TestError::Missing("second carrier"))?
                .kind
                != EntityKind::Parameter
        {
            return Err(TestError::Missing("throws carriers"));
        }
        let extension = java_extension(&view, 3)?;
        let throws = pool_list(
            view.extension_pool_payload()
                .ok_or(TestError::Missing("pools"))?,
            1,
            extension.throws.raw as usize,
        )?;
        if throws != vec![1, 2] {
            return Err(TestError::Missing("throws image order"));
        }
        Ok(())
    }

    #[test]
    fn annotations_decode_the_exact_interned_atom_bytes_for_class_and_method()
    -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.C");
        let name = fix.atom(b"run");
        let void = u32::try_from(fix.types.len())?;
        fix.types.push(TypeRow {
            kind: 2,
            flags: 0,
            atom: None,
            children: Vec::new(),
        });
        fix.symbols.push(SymbolRow {
            owner: 0,
            name,
            parameters: Vec::new(),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(void),
            symbol: Some(0),
        });
        let class_atom = fix.atom(b"Ldemo/Class;");
        let method_atom = fix.atom(b"Ldemo/Method;");
        fix.extension(0, 2, u32::try_from(class_atom)?);
        fix.extension(1, 2, u32::try_from(method_atom)?);
        let bytes = lower(&fix, b"class C { void run() {} }")?;
        let view = FragmentView::validate(&bytes)?;
        if atom_list(&view, java_extension(&view, 0)?.annotations.raw as usize)?
            != vec![b"Ldemo/Class;".as_slice()]
            || atom_list(&view, java_extension(&view, 1)?.annotations.raw as usize)?
                != vec![b"Ldemo/Method;".as_slice()]
        {
            return Err(TestError::Missing("exact annotation atoms"));
        }
        Ok(())
    }

    #[test]
    fn record_components_attach_after_fields_and_retain_record_annotations() -> Result<(), TestError>
    {
        let mut fix = Fixture::default();
        let record = fix.atom(b"demo.R");
        let ty = fix.declared(b"demo.R");
        fix.declarations.push(DeclarationRow {
            kind: 5,
            name: record,
            owner: None,
            documentation: None,
            semantic_type: Some(ty),
            symbol: None,
        });
        let one = fix.atom(b"one");
        let two = fix.atom(b"two");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: one,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(ty),
            symbol: None,
        });
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: two,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(ty),
            symbol: None,
        });
        let annotation = fix.atom(b"Ldemo/Record;");
        fix.extension(0, 2, u32::try_from(annotation)?);
        fix.extension(0, 3, 1);
        fix.extension(0, 3, 2);
        let bytes = lower(&fix, b"record R(int one, int two) {}")?;
        let view = FragmentView::validate(&bytes)?;
        let extension = java_extension(&view, 0)?;
        let components = pool_list(
            view.extension_pool_payload()
                .ok_or(TestError::Missing("pools"))?,
            2,
            extension.record_components.raw as usize,
        )?;
        if components != vec![1, 2]
            || atom_list(&view, extension.annotations.raw as usize)?
                != vec![b"Ldemo/Record;".as_slice()]
        {
            return Err(TestError::Missing("attached record extension"));
        }
        Ok(())
    }

    #[test]
    fn module_annotations_are_not_dropped_by_root_projection() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        let module = fix.atom(b"demo.module");
        fix.declarations.push(DeclarationRow {
            kind: 1,
            name: module,
            owner: None,
            documentation: None,
            semantic_type: None,
            symbol: None,
        });
        let annotation = fix.atom(b"Ldemo/Module;");
        fix.extension(0, 2, u32::try_from(annotation)?);
        let bytes = lower(&fix, b"module demo.module;")?;
        let view = FragmentView::validate(&bytes)?;
        if atom_list(&view, java_extension(&view, 0)?.annotations.raw as usize)?
            != vec![b"Ldemo/Module;".as_slice()]
        {
            return Err(TestError::Missing("module annotation"));
        }
        Ok(())
    }

    #[test]
    fn two_hundred_fifty_seven_programmatic_fields_fold_through_typed_capacity()
    -> Result<(), TestError> {
        let mut fix = Fixture::default();
        fix.class(b"demo.C");
        let ty = fix.declared(b"foreign.T");
        for index in 0..257 {
            let name = fix.atom(index.to_string().as_bytes());
            fix.declarations.push(DeclarationRow {
                kind: 8,
                name,
                owner: Some(0),
                documentation: None,
                semantic_type: Some(ty),
                symbol: None,
            });
        }
        match lower(&fix, b"class C {}") {
            Err(TestError::Collect(JavaCollectError::Lowering(
                compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
            ))) => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(TestError::Missing("capacity rejection")),
        }
    }

    #[test]
    fn generic_application_depth_sixty_four_succeeds_and_sixty_five_folds() -> Result<(), TestError>
    {
        let mut fix = Fixture::default();
        fix.class(b"demo.C");
        let mut root = fix.declared(b"T0");
        for depth in 1..=63 {
            let atom = fix.atom(format!("T{depth}").as_bytes());
            let next = u32::try_from(fix.types.len())?;
            fix.types.push(TypeRow {
                kind: 3,
                flags: 0,
                atom: Some(atom),
                children: vec![root],
            });
            root = next;
        }
        let field = fix.atom(b"value");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: field,
            owner: Some(0),
            documentation: None,
            semantic_type: Some(root),
            symbol: None,
        });
        let source = b"class C { T63 value; }";
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let apply_rows = view
            .type_facts()
            .ok_or(TestError::Missing("type facts"))?
            .map(|fact| fact.map(|fact| usize::from(fact.record.tag == SemanticTypeTag::Apply)))
            .try_fold(0usize, |count, fact| Ok::<_, TypeFactFault>(count + fact?))?;
        if apply_rows != 63 {
            return Err(TestError::Missing("depth sixty-four apply"));
        }
        let mut deeper = fix.clone();
        let atom = deeper.atom(b"T64");
        let intermediate = u32::try_from(deeper.types.len())?;
        deeper.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(atom),
            children: vec![root],
        });
        let atom = deeper.atom(b"T65");
        let next = u32::try_from(deeper.types.len())?;
        deeper.types.push(TypeRow {
            kind: 3,
            flags: 0,
            atom: Some(atom),
            children: vec![intermediate],
        });
        deeper.declarations[1].semantic_type = Some(next);
        match lower(&deeper, source) {
            Err(TestError::Collect(JavaCollectError::Lowering(
                compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
            ))) => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(TestError::Missing("depth sixty-five rejection")),
        }
    }
}
