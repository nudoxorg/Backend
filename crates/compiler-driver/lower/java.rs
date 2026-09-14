//! Projects source-bound javac authority facts through the shared canonical fact lane.
//!
//! The projection covers every plane the validated image carries: declaration
//! facts, the recursive type graph, executable signatures with parameter and
//! result carrier facts, resolved call occurrences, Javadoc fragments, and
//! overload sibling lists. Emission is two-pass: every module, package, and
//! type declaration is committed first, so every nominal, type-child, and
//! product target is a strictly backward fact ordinal.
//!
//! Declared types are projected row-by-row from the image's own type plane.
//! Positions the lane cannot carry — foreign nominals, composites without a
//! carrier row — fold to typed `Unknown` rows that retain the image spelling
//! instead of fabricating a shape. Parameter and result carrier facts are named
//! by their javac-provided type spelling because image version 1 carries no
//! parameter-name plane. Modifiers stay out of the lane: visibility is not a
//! lane cell and is never fabricated. Never recovers Java facts by scanning
//! source text or a native parser fallback.

use core::{iter::ExactSizeIterator, str};

use backend_semantic::ir::{
    AtomListId, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, EntityListId, ForeignKey,
    ForeignKeyFault, ForeignOrigin, JavaFacts, NominalRef, Occurrence, OccurrenceConfidence,
    OccurrenceTarget, ProductChildRole, ReferenceKind, RelSpan, SemanticProductConstructor,
    SemanticTypeRecord, SemanticTypeTag, TypeListId, TypeReason, TypeWidth,
};
use backend_frontend_java::legacy::{
    AtomError, BoundImageError, Declaration, DeclarationKind, DocFlavor, HeaderError, ImageError,
    ImagePlane, JavaAuthorityImage, JavaImage, JavaRelease, Reference, SectionError, SymbolRef,
    TypeFact, TypeKind, TypeRef,
};
use backend_semantic::vocabulary::{
    JavaForeignKeyFault, JavaImageAtomFault, JavaImageFault, JavaImageHeaderFault, JavaImagePlane,
    JavaImageSectionFault, JavaProjectionFault, JavaProjectionIndexPhase, JavaProjectionTypeKind,
    JavaRelease as ProfileRelease, JavaSymbolAtom, LoweringUnsupported,
};
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
/// the portable closed Java terminal. Every variant keeps the source/image
/// operand that its projection site can actually prove.
#[derive(Debug)]
enum ProjectionFault {
    /// A validated image plane rejected a coordinate during projection.
    Image(
        /// The exact image-plane rejection.
        ImageError,
    ),
    /// The recursive type graph exceeded the producer's documented depth budget.
    Depth {
        /// The exact authority type row at the recursion boundary.
        type_row: u32,
    },
    /// A fixed row layout promise was violated: a required atom or child was absent.
    Malformed {
        /// The exact authority type row whose layout was incomplete.
        type_row: u32,
        /// The image type row that broke its closed layout.
        kind: TypeKind,
    },
    /// A primitive spelling outside javac's closed kind vocabulary.
    Primitive {
        /// The image type row whose primitive spelling was rejected.
        type_row: u32,
    },
    /// An image atom was not valid UTF-8 although the image validated its planes.
    Utf8 {
        /// The symbol row whose atom failed decoding.
        symbol: u32,
        /// The exact atom role in that symbol row.
        atom: JavaSymbolAtom,
    },
    /// The bound compile source is not UTF-8 text, so javac's UTF-16
    /// coordinates have no byte domain to project into.
    SourceUtf8,
    /// A javac UTF-16 coordinate has no byte offset in the bound source.
    Utf16 {
        /// The requested UTF-16 unit offset.
        units: u32,
        /// The bound source's total UTF-16 length.
        utf16_len: u32,
    },
    /// A javac UTF-16 range could not be represented as an ordered relative
    /// byte span. Both source coordinates remain available to the terminal.
    Utf16Range {
        /// The reported source start coordinate.
        start: u32,
        /// The reported source end coordinate.
        end: u32,
    },
    /// A reference owner names no executable admitted by this image.
    OrphanOwner {
        /// The unresolved owner symbol coordinate.
        owner: SymbolRef,
    },
    /// A foreign key could not be built for a resolved external target.
    ForeignKey(
        /// The exact foreign-key rejection.
        ForeignKeyFault,
    ),
    /// The bounded overload sibling list overflowed its pooled width.
    SiblingCapacity {
        /// The exact authority executable symbol being indexed.
        symbol: u32,
    },
    /// A bounded projection index overflowed its lane width.
    IndexCapacity {
        /// The closed operation whose compact coordinate could not fit.
        phase: JavaProjectionIndexPhase,
    },
}

/// Folds one projection fault into the portable closed Java terminal without
/// substituting an unsupported-declaration result.
fn terminal(fault: ProjectionFault) -> JavaCollectError {
    let fault = match fault {
        ProjectionFault::Image(cause) => match java_image_fault(cause) {
            Ok(cause) => JavaProjectionFault::Image { cause },
            Err(()) => JavaProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::FactOrdinal,
            },
        },
        ProjectionFault::Depth { type_row } => JavaProjectionFault::Depth { type_row },
        ProjectionFault::Malformed { type_row, kind } => JavaProjectionFault::MalformedType {
            type_row,
            kind: java_projection_type_kind(kind),
        },
        ProjectionFault::Primitive { type_row } => JavaProjectionFault::Primitive { type_row },
        ProjectionFault::Utf8 { symbol, atom } => JavaProjectionFault::AtomUtf8 { symbol, atom },
        ProjectionFault::SourceUtf8 => JavaProjectionFault::SourceUtf8,
        ProjectionFault::Utf16 { units, utf16_len } => JavaProjectionFault::Utf16Offset {
            units,
            source_utf16_len: utf16_len,
        },
        ProjectionFault::Utf16Range { start, end } => {
            JavaProjectionFault::Utf16Range { start, end }
        }
        ProjectionFault::OrphanOwner { owner } => JavaProjectionFault::OrphanOwner {
            owner: owner.ordinal,
        },
        ProjectionFault::ForeignKey(cause) => JavaProjectionFault::ForeignKey {
            cause: java_foreign_key_fault(cause),
        },
        ProjectionFault::SiblingCapacity { symbol } => {
            JavaProjectionFault::SiblingCapacity { symbol }
        }
        ProjectionFault::IndexCapacity { phase } => JavaProjectionFault::IndexCapacity { phase },
    };
    JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault })
}

/// Converts one validated Java image plane into the portable plane vocabulary.
fn java_image_plane(plane: ImagePlane) -> JavaImagePlane {
    match plane {
        ImagePlane::Atoms => JavaImagePlane::Atoms,
        ImagePlane::AtomBytes => JavaImagePlane::AtomBytes,
        ImagePlane::Types => JavaImagePlane::Types,
        ImagePlane::TypeChildren => JavaImagePlane::TypeChildren,
        ImagePlane::Symbols => JavaImagePlane::Symbols,
        ImagePlane::SymbolParameters => JavaImagePlane::SymbolParameters,
        ImagePlane::Declarations => JavaImagePlane::Declarations,
        ImagePlane::References => JavaImagePlane::References,
        ImagePlane::DeclarationExtensions => JavaImagePlane::DeclarationExtensions,
        ImagePlane::ExtensionEntries => JavaImagePlane::ExtensionEntries,
    }
}

/// Converts a native image coordinate to its checked portable width.
fn image_u32(value: usize) -> Result<u32, ()> {
    u32::try_from(value).map_err(|_| ())
}

/// Converts one nested Java header rejection without dropping its operands.
fn java_header_fault(cause: HeaderError) -> Result<JavaImageHeaderFault, ()> {
    Ok(match cause {
        HeaderError::Truncated { actual } => JavaImageHeaderFault::Truncated {
            actual: image_u32(actual)?,
        },
        HeaderError::Magic { found } => JavaImageHeaderFault::Magic { found },
        HeaderError::Version { found } => JavaImageHeaderFault::Version { found },
        HeaderError::Length { found } => JavaImageHeaderFault::Length { found },
        HeaderError::Release { found } => JavaImageHeaderFault::Release { found },
        HeaderError::SectionCount { found } => JavaImageHeaderFault::SectionCount { found },
        HeaderError::BodyLength { declared, actual } => JavaImageHeaderFault::BodyLength {
            declared: image_u32(declared)?,
            actual: image_u32(actual)?,
        },
    })
}

/// Converts one nested Java directory rejection without dropping its operands.
fn java_section_fault(cause: SectionError) -> Result<JavaImageSectionFault, ()> {
    Ok(match cause {
        SectionError::Tag { expected, found } => JavaImageSectionFault::Tag { expected, found },
        SectionError::RowBytes { expected, found } => JavaImageSectionFault::RowBytes {
            expected: image_u32(expected)?,
            found: image_u32(found)?,
        },
        SectionError::ByteCount {
            count,
            row_bytes,
            found,
        } => JavaImageSectionFault::ByteCount {
            count: image_u32(count)?,
            row_bytes: image_u32(row_bytes)?,
            found: image_u32(found)?,
        },
        SectionError::Offset { expected, found } => JavaImageSectionFault::Offset {
            expected: image_u32(expected)?,
            found: image_u32(found)?,
        },
        SectionError::Range {
            offset,
            length,
            image_bytes,
        } => JavaImageSectionFault::Range {
            offset: image_u32(offset)?,
            length: image_u32(length)?,
            image_bytes: image_u32(image_bytes)?,
        },
    })
}

/// Converts one nested Java atom-table rejection without dropping its operands.
fn java_atom_fault(cause: AtomError) -> Result<JavaImageAtomFault, ()> {
    Ok(match cause {
        AtomError::NonCanonicalOffset { found } => JavaImageAtomFault::NonCanonicalOffset {
            found: image_u32(found)?,
        },
        AtomError::Range => JavaImageAtomFault::Range,
        AtomError::Utf8 => JavaImageAtomFault::Utf8,
        AtomError::TrailingBytes => JavaImageAtomFault::TrailingBytes,
    })
}

/// Converts every validated Java image rejection into its portable closed form.
/// A `usize` that cannot fit the portable coordinate width is an explicit
/// projection capacity failure at the caller, never a truncated operand.
fn java_image_fault(cause: ImageError) -> Result<JavaImageFault, ()> {
    Ok(match cause {
        ImageError::Header(cause) => JavaImageFault::Header {
            cause: java_header_fault(cause)?,
        },
        ImageError::Section { plane, cause } => JavaImageFault::Section {
            plane: java_image_plane(plane),
            cause: java_section_fault(cause)?,
        },
        ImageError::Digest => JavaImageFault::Digest,
        ImageError::Atom { index, cause } => JavaImageFault::Atom {
            index: image_u32(index)?,
            cause: java_atom_fault(cause)?,
        },
        ImageError::AbsentAtom => JavaImageFault::AbsentAtom,
        ImageError::Coordinate {
            plane,
            index,
            upper_bound,
        } => JavaImageFault::Coordinate {
            plane: java_image_plane(plane),
            index: image_u32(index)?,
            upper_bound: image_u32(upper_bound)?,
        },
        ImageError::Tag { plane, found } => JavaImageFault::Tag {
            plane: java_image_plane(plane),
            found,
        },
        ImageError::ChildRange {
            plane,
            start,
            count,
            upper_bound,
        } => JavaImageFault::ChildRange {
            plane: java_image_plane(plane),
            start: image_u32(start)?,
            count: image_u32(count)?,
            upper_bound: image_u32(upper_bound)?,
        },
        ImageError::ReferenceRange { start, end } => JavaImageFault::ReferenceRange { start, end },
        ImageError::DocumentationPresence => JavaImageFault::DocumentationPresence,
        ImageError::ModifierBits { found } => JavaImageFault::ModifierBits { found },
        ImageError::RecordComponentKind { index } => JavaImageFault::RecordComponentKind {
            index: image_u32(index)?,
        },
        ImageError::ExtensionReserved => JavaImageFault::ExtensionReserved,
    })
}

/// Retains one bounded-lane rejection with the exact candidate ordinal and
/// declaration-name length supplied by its caller.
fn lane_rejection(fact: usize, name_len: usize, cause: FactFault) -> JavaCollectError {
    JavaCollectError::Rejected(FactRejection {
        fact,
        name_len,
        cause,
    })
}

/// Maps the image's closed Java type tag into the portable vocabulary without
/// retaining a language-crate value in the driver terminal.
fn java_projection_type_kind(kind: TypeKind) -> JavaProjectionTypeKind {
    match kind {
        TypeKind::Primitive => JavaProjectionTypeKind::Primitive,
        TypeKind::Void => JavaProjectionTypeKind::Void,
        TypeKind::Declared => JavaProjectionTypeKind::Declared,
        TypeKind::Array => JavaProjectionTypeKind::Array,
        TypeKind::Variable => JavaProjectionTypeKind::Variable,
        TypeKind::Wildcard => JavaProjectionTypeKind::Wildcard,
        TypeKind::Intersection => JavaProjectionTypeKind::Intersection,
        TypeKind::Union => JavaProjectionTypeKind::Union,
        TypeKind::Error => JavaProjectionTypeKind::Error,
        TypeKind::None => JavaProjectionTypeKind::None,
        TypeKind::Null => JavaProjectionTypeKind::Null,
    }
}

/// Maps the closed IR foreign-key fault into the portable Java terminal.
fn java_foreign_key_fault(cause: ForeignKeyFault) -> JavaForeignKeyFault {
    match cause {
        ForeignKeyFault::EmptyPath => JavaForeignKeyFault::EmptyPath,
        ForeignKeyFault::BackslashInPath => JavaForeignKeyFault::BackslashInPath,
    }
}

/// Admits one fact and returns its proven backward ordinal.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<u32, JavaCollectError> {
    let ordinal = push_fact(facts, fact).map_err(JavaCollectError::Rejected)?;
    u32::try_from(ordinal).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::FactOrdinal,
        })
    })
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
/// `PrimitiveShape::Utf16CodeUnit` wire cell.
const SHAPE_UTF16_CODE_UNIT: u32 = 18;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// `void` builtin spelling; the void row is a primitive builtin leaf.
const VOID_SPELLING: &[u8] = b"void";

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
    let utf16_len = utf16_length(source_text).map_err(terminal)?;

    let mut names = NameIndex::new();
    // Pass one: module, package, and type declarations. Type declarations
    // carry the recursive terminal — a nominal row naming their own ordinal.
    for declared in image.declarations() {
        let declared =
            declared.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        match declared.kind {
            DeclarationKind::Class
            | DeclarationKind::Interface
            | DeclarationKind::Record
            | DeclarationKind::Enum
            | DeclarationKind::Annotation => {
                let ordinal = push_type_root(facts, &declared)?;
                names.record(declared.name.bytes, ordinal).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: JavaProjectionIndexPhase::NameIndex,
                    })
                })?;
                push_docs(facts, &names, ordinal, &declared)?;
            }
            DeclarationKind::Module | DeclarationKind::Package => {
                push_root(facts, &names, &declared)?
            }
            DeclarationKind::Field
            | DeclarationKind::EnumConstant
            | DeclarationKind::Constructor
            | DeclarationKind::Method => {}
        }
    }

    // Pass two: members with projected rows, signature facts, and overloads.
    let mut executables = Executables::new();
    let mut symbols = SymbolIndex::new();
    for declared in image.declarations() {
        let declared =
            declared.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        match declared.kind {
            DeclarationKind::Field | DeclarationKind::EnumConstant => {
                push_member(facts, image, &names, &declared)?;
            }
            DeclarationKind::Constructor | DeclarationKind::Method => {
                push_executable(
                    facts,
                    image,
                    &names,
                    &mut executables,
                    &mut symbols,
                    &declared,
                )?;
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

    // Pass three: compiler-resolved call occurrences in image order.
    for reference in image.references() {
        let reference =
            reference.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        push_occurrence(facts, image, &symbols, source_text, utf16_len, &reference)
            .map_err(terminal)?;
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
        DeclarationKind::Module => EntityKind::Module,
        DeclarationKind::Package => EntityKind::Namespace,
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
        EntityKind::Record | EntityKind::Module | EntityKind::Namespace => {
            SemanticProductConstructor::PRODUCT
        }
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Constant
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter
        | EntityKind::Macro => LEAF_PRODUCT,
    }
}

/// One projected javac type row prepared for a fact: the lattice record, its
/// ordered backward type-row children, the nearest image atom spelling, and
/// whether the row is `void`.
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
fn project<'image>(
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

    fn record(&mut self, name: &'image [u8], ordinal: u32) -> Result<(), ProjectionFault> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::NameIndex,
            });
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

    fn record(&mut self, symbol: SymbolRef, ordinal: u32) -> Result<(), ProjectionFault> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::SymbolIndex,
            });
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
        symbol: u32,
        constructor: bool,
        owner: Option<&'image [u8]>,
        name: &[u8],
        out: &mut [u32; MAX_REF_LIST_ELEMENTS],
    ) -> Result<usize, ProjectionFault> {
        let mut count = 0usize;
        for (entry_constructor, entry_owner, entry_name, ordinal) in
            self.entries.iter().take(self.len)
        {
            if *entry_constructor == constructor && *entry_owner == owner && *entry_name == name {
                if count == MAX_REF_LIST_ELEMENTS {
                    return Err(ProjectionFault::SiblingCapacity { symbol });
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
    ) -> Result<(), ProjectionFault> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::ExecutableIndex,
            });
        }
        self.entries[self.len] = (constructor, owner, name, ordinal);
        self.len += 1;
        Ok(())
    }
}

/// Pushes one module or package root with its honestly unknown declared type.
fn push_root<'source>(
    facts: &mut FactSet<'source>,
    names: &NameIndex<'source>,
    declared: &Declaration<'source>,
) -> Result<(), JavaCollectError> {
    let kind = entity_kind(declared.kind);
    let fact = SemanticFact::new(kind, declared.name.bytes, constructor(kind));
    let ordinal = push(facts, fact)?;
    push_docs(facts, names, ordinal, declared)
}

/// Pushes one type declaration with the recursive terminal: a nominal row
/// naming the declaration's own ordinal. Documentation is pushed by the caller
/// after the qualified name joins the link index.
fn push_type_root<'source>(
    facts: &mut FactSet<'source>,
    declared: &Declaration<'source>,
) -> Result<u32, JavaCollectError> {
    let kind = entity_kind(declared.kind);
    let self_ordinal = u32::try_from(facts.len()).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::FactOrdinal,
        })
    })?;
    let fact = SemanticFact::new(kind, declared.name.bytes, constructor(kind))
        .typed(nominal_record(self_ordinal));
    push(facts, fact)
}

/// Finds the already-admitted declaration that owns any anonymous structural
/// type rows for this Java declaration.  Source-bound member ownership wins;
/// a module/package root is an honest fallback for a flat authority fixture.
fn type_anchor(
    facts: &FactSet<'_>,
    names: &NameIndex<'_>,
    declared: &Declaration<'_>,
) -> Result<u32, ProjectionFault> {
    declared
        .owner
        .and_then(|owner| names.lookup(owner.bytes))
        .or_else(|| (facts.len() != 0).then_some(0))
        .ok_or(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::NameIndex,
        })
}

/// Pushes one field or enum constant with its projected declared type.
fn push_member<'source>(
    facts: &mut FactSet<'source>,
    image: JavaImage<'source>,
    names: &NameIndex<'source>,
    declared: &Declaration<'source>,
) -> Result<(), JavaCollectError> {
    let kind = entity_kind(declared.kind);
    let anchor = type_anchor(facts, names, declared).map_err(terminal)?;
    let projected = match declared.semantic_type {
        Some(reference) => {
            project(facts, image, names, anchor, reference, DEPTH_LIMIT).map_err(terminal)?
        }
        None => ProjectedType::leaf(unknown_record(TypeReason::Unannotated, None), None),
    };
    let fact = projected.attach(SemanticFact::new(
        kind,
        declared.name.bytes,
        constructor(kind),
    ));
    let ordinal = push(facts, fact)?;
    push_docs(facts, names, ordinal, declared)
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
) -> Result<(), JavaCollectError> {
    let symbol_reference = match declared.symbol {
        Some(reference) => reference,
        None => {
            let Some(type_reference) = declared.semantic_type else {
                return Err(terminal(ProjectionFault::IndexCapacity {
                    phase: JavaProjectionIndexPhase::Signature,
                }));
            };
            return Err(terminal(ProjectionFault::Malformed {
                type_row: type_reference.ordinal,
                kind: TypeKind::None,
            }));
        }
    };
    let symbol = image
        .symbol(symbol_reference)
        .map_err(|cause| terminal(ProjectionFault::Image(cause)))?;
    let is_constructor = declared.kind == DeclarationKind::Constructor;
    let anchor = type_anchor(facts, names, declared).map_err(terminal)?;

    // Parameter carriers first so every executable target stays backward.
    let mut parameter_ordinals = [0_u32; MAX_FACT_CHILDREN];
    let mut signature_children = [0_u32; MAX_TYPE_CHILDREN];
    let mut parameter_count = 0usize;
    let mut signature_count = 0usize;
    for parameter in symbol.parameters {
        let projected =
            project(facts, image, names, anchor, parameter, DEPTH_LIMIT).map_err(terminal)?;
        let name = projected.spelling.ok_or_else(|| {
            terminal(ProjectionFault::Malformed {
                type_row: parameter.ordinal,
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
            project(facts, image, names, anchor, return_type, DEPTH_LIMIT).map_err(terminal)?;
        if !projected.void {
            let name = projected.spelling.ok_or_else(|| {
                terminal(ProjectionFault::Malformed {
                    type_row: return_type.ordinal,
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

    // Function-pointer row: the result-presence flag commits only with a
    // result carrier, matching the javac return type.
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
    if result_ordinal.is_some() {
        record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
    }

    // Overload siblings: distinct javac signatures already differ in their
    // constructor and type cells, so identities never need ordinal folding.
    let mut siblings = [0_u32; MAX_REF_LIST_ELEMENTS];
    let sibling_count = executables
        .siblings(
            symbol_reference.ordinal,
            is_constructor,
            declared.owner.map(|atom| atom.bytes),
            declared.name.bytes,
            &mut siblings,
        )
        .map_err(terminal)?;
    let Some(sibling_ordinals) = siblings.get(..sibling_count) else {
        return Err(terminal(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::ExecutableIndex,
        }));
    };
    let extension = java_facts(facts, sibling_ordinals)
        .map_err(|cause| lane_rejection(facts.len(), declared.name.bytes.len(), cause))?;

    let parameter_total = u32::try_from(parameter_count).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::Signature,
        })
    })?;
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
    symbols.record(symbol_reference, ordinal).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::SymbolIndex,
        })
    })?;
    executables
        .record(
            is_constructor,
            declared.owner.map(|atom| atom.bytes),
            declared.name.bytes,
            ordinal,
        )
        .map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::ExecutableIndex,
            })
        })?;
    push_docs(facts, names, ordinal, declared)
}

/// Builds one Java extension row. Throws, annotations, and record components
/// stay on the empty pooled lists because image version 1 carries no such
/// planes; interning the empty lists keeps every pooled coordinate valid
/// against the reopened pools lane.
fn java_facts<'source>(
    facts: &mut FactSet<'source>,
    siblings: &[u32],
) -> Result<JavaFacts, FactFault> {
    let overloads = facts.intern_entity_list(siblings)?;
    let _ = facts.intern_atom_list(&[])?;
    let _ = facts.intern_type_list(&[])?;
    Ok(JavaFacts {
        throws: TypeListId::new(0),
        annotations: AtomListId::new(0),
        overloads,
        record_components: EntityListId::new(0),
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
    source_text: &'source str,
    utf16_len: u32,
    reference: &Reference<'source>,
) -> Result<(), ProjectionFault> {
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
            let namespace = symbol.owner.utf8().map_err(|_| ProjectionFault::Utf8 {
                symbol: reference.target.ordinal,
                atom: JavaSymbolAtom::Owner,
            })?;
            let name = symbol.name.utf8().map_err(|_| ProjectionFault::Utf8 {
                symbol: reference.target.ordinal,
                atom: JavaSymbolAtom::Name,
            })?;
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
    let start = utf16_byte_offset(source_text, reference.start, utf16_len)?;
    let end = utf16_byte_offset(source_text, reference.end, utf16_len)?;
    let span = RelSpan::new(start, end).map_err(|_| ProjectionFault::Utf16Range {
        start: reference.start,
        end: reference.end,
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
        .map_err(|_| ProjectionFault::IndexCapacity {
            phase: JavaProjectionIndexPhase::FactOrdinal,
        })?;
    Ok(())
}

/// Projects one javac UTF-16 coordinate onto the bound source's byte domain.
fn utf16_byte_offset(source: &str, units: u32, utf16_len: u32) -> Result<u32, ProjectionFault> {
    let mut seen = 0_u32;
    for (offset, character) in source.char_indices() {
        if seen == units {
            return u32::try_from(offset).map_err(|_| ProjectionFault::Utf16 { units, utf16_len });
        }
        let Ok(width) = u32::try_from(character.len_utf16()) else {
            return Err(ProjectionFault::Utf16 { units, utf16_len });
        };
        seen = match seen.checked_add(width) {
            Some(seen) => seen,
            None => return Err(ProjectionFault::Utf16 { units, utf16_len }),
        };
    }
    if seen == units {
        return u32::try_from(source.len())
            .map_err(|_| ProjectionFault::Utf16 { units, utf16_len });
    }
    Err(ProjectionFault::Utf16 { units, utf16_len })
}

/// The bound source's total UTF-16 length, or an explicit capacity fault when
/// it cannot fit the compact coordinate width.
fn utf16_length(source: &str) -> Result<u32, ProjectionFault> {
    let mut total = 0_u32;
    for character in source.chars() {
        let width =
            u32::try_from(character.len_utf16()).map_err(|_| ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::Utf16,
            })?;
        total = total
            .checked_add(width)
            .ok_or(ProjectionFault::IndexCapacity {
                phase: JavaProjectionIndexPhase::Utf16,
            })?;
    }
    Ok(total)
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
    // The validated javac declaration row owns its documentation field even
    // when it is absent, so `docs: []` is not an authority gap.
    facts
        .mark_documentation_captured(owner)
        .map_err(|cause| lane_rejection(owner as usize, declared.name.bytes.len(), cause))?;
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
                push_text(facts, owner, line).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: JavaProjectionIndexPhase::Documentation,
                    })
                })?;
            }
        }
        if line_end == doc.len() {
            break;
        }
        facts
            .push_doc(owner, DocFragmentInput::SoftBreak)
            .map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: JavaProjectionIndexPhase::Documentation,
                })
            })?;
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
) -> Result<(), ProjectionFault> {
    let mut cursor = 0usize;
    while cursor < line.len() {
        let rest = line.get(cursor..).unwrap_or(&[]);
        match inline_tag(rest) {
            None => {
                push_text(facts, owner, rest).map_err(|_| ProjectionFault::IndexCapacity {
                    phase: JavaProjectionIndexPhase::Documentation,
                })?;
                break;
            }
            Some(tag) => {
                let prose = rest.get(..tag.at).unwrap_or(&[]);
                push_text(facts, owner, prose).map_err(|_| ProjectionFault::IndexCapacity {
                    phase: JavaProjectionIndexPhase::Documentation,
                })?;
                if tag.link {
                    push_link(facts, names, owner, tag.interior).map_err(|_| {
                        ProjectionFault::IndexCapacity {
                            phase: JavaProjectionIndexPhase::Documentation,
                        }
                    })?;
                } else if !tag.interior.is_empty() {
                    facts
                        .push_doc(owner, DocFragmentInput::Code(tag.interior))
                        .map_err(|_| ProjectionFault::IndexCapacity {
                            phase: JavaProjectionIndexPhase::Documentation,
                        })?;
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
    use backend_semantic::ir::{DocFactFault, OccurrenceFault};
    use backend_semantic::ir::{FragmentView, SourceIdentity, TypeFactFault};
    use backend_semantic::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use sha2::Sha256;

    const HEADER_BYTES: usize = 176;
    const DIRECTORY_OFFSET: usize = 48;
    const DIRECTORY_ENTRY_BYTES: usize = 16;
    const IMAGE_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v1\0";
    const BOUND_DOMAIN: &[u8] = b"nudox.java.bound.authority.image.sha256.v1\0";
    const ABSENT: u32 = u32::MAX;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("collection rejected the authority image: {0:?}")]
        Collect(JavaCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(crate::lower::AdmissionFault),
        #[error("fragment validation rejected the bytes: {0:?}")]
        Validate(backend_semantic::ir::FragmentError),
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

    impl From<backend_semantic::ir::FragmentError> for TestError {
        fn from(error: backend_semantic::ir::FragmentError) -> Self {
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
            let sections = [
                atoms,
                atom_bytes,
                types,
                type_children,
                symbols,
                symbol_parameters,
                declarations,
                references,
            ];
            let row_bytes = [8_u16, 1, 16, 4, 16, 4, 32, 20];
            let mut image = vec![0_u8; HEADER_BYTES];
            image[..4].copy_from_slice(b"NJAI");
            image[4..6].copy_from_slice(&1_u16.to_le_bytes());
            image[6..8].copy_from_slice(&u16::try_from(HEADER_BYTES)?.to_le_bytes());
            image[8..10].copy_from_slice(&21_u16.to_le_bytes());
            image[10..12].copy_from_slice(&8_u16.to_le_bytes());
            let body = sections.iter().map(Vec::len).sum::<usize>();
            image[12..16].copy_from_slice(&u32::try_from(body)?.to_le_bytes());
            let mut offset = HEADER_BYTES;
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
            digest.update(IMAGE_DOMAIN);
            digest.update(&image[..16]);
            digest.update(&image[DIRECTORY_OFFSET..HEADER_BYTES]);
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
            LanguageProfile::Java(backend_semantic::vocabulary::JavaRelease::Java21),
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
    ) -> Result<backend_semantic::ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        cursor
            .nth(ordinal)
            .ok_or(TestError::Missing("type row"))?
            .map_err(TestError::from)
    }

    #[test]
    fn empty_image_admits_the_current_schema_fragment_without_semantic_sections()
    -> Result<(), TestError> {
        let fix = Fixture::default();
        let bytes = lower(&fix, b"")?;
        let view = FragmentView::validate(&bytes)?;
        if view.type_facts().is_some() || view.occurrences().is_some() || view.docs().is_some() {
            return Err(TestError::Missing("absent semantic sections"));
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
            || int_method.record.payload1 != SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
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
            || second.record.payload1 != SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
        {
            return Err(TestError::Missing("second overload row"));
        }
        // The overload sibling lists travel in the pooled lanes: the first
        // brew carries the empty list and the second lists the first's fact
        // ordinal.
        if !entity_list(&view, 0)?.is_empty() {
            return Err(TestError::Missing("first overload has no siblings"));
        }
        let listed = entity_list(&view, 1)?;
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
        // The renamed method joins no sibling list: every list is empty and
        // only the deduplicated empty row remains in the pool.
        if !entity_list(&view, 0)?.is_empty() || entity_list(&view, 1).is_ok() {
            return Err(TestError::Missing("renamed method has no siblings"));
        }
        Ok(())
    }

    /// Lends one pooled entity list through the schema-aware pool view.
    fn entity_list<'a>(view: &'a FragmentView<'a>, ordinal: u32) -> Result<Vec<u32>, TestError> {
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Tail)?
            .ok_or(TestError::Missing("pools"))?;
        Ok(pools
            .entity_list(ordinal)
            .map_err(|_| TestError::Missing("entity list"))?
            .iter()
            .collect())
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
        // Facts: 0 Cafe, 1 Node, 2 pair, 3 many, 4 named.
        let pair_row = row(&view, 2)?;
        if pair_row.record.tag != SemanticTypeTag::Apply
            || pair_row.record.children.length != 2
            || pair_row.record.children.start != 0
        {
            return Err(TestError::Missing("generic application row"));
        }
        let array = row(&view, 3)?;
        if array.record.tag != SemanticTypeTag::ArraySequence || array.record.children.length != 1 {
            return Err(TestError::Missing("array arity row"));
        }
        let foreign_row = row(&view, 4)?;
        if foreign_row.record.tag != SemanticTypeTag::Unknown
            || foreign_row.record.text != Some(b"java.lang.String".as_slice())
        {
            return Err(TestError::Missing("foreign declared unknown"));
        }
        Ok(())
    }

    #[test]
    fn nested_sequence_arrays_keep_every_dimension_without_a_spelling_ceiling()
    -> Result<(), TestError> {
        let source = b"class Matrix { Node[][][][][][][][][] deep; }";
        let mut fix = Fixture::default();
        let matrix = fix.class(b"demo.Matrix");
        let node = fix.class(b"demo.Node");
        let mut nested = node;
        for _ in 0..9 {
            let row = u32::try_from(fix.types.len())?;
            fix.types.push(TypeRow {
                kind: 4,
                flags: 0,
                atom: None,
                children: vec![nested],
            });
            nested = row;
        }
        let deep = fix.atom(b"deep");
        fix.declarations.push(DeclarationRow {
            kind: 8,
            name: deep,
            owner: Some(usize::try_from(matrix)?),
            documentation: None,
            semantic_type: Some(nested),
            symbol: None,
        });

        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Eight inner anonymous sequence rows precede the three declared
        // rows; the ninth, outer sequence is the `deep` field's own row.
        let deep = row(&view, 10)?;
        if deep.record.tag != SemanticTypeTag::ArraySequence || deep.record.children.length != 1 {
            return Err(TestError::Missing("outer nested sequence"));
        }
        for ordinal in 0..8 {
            let inner = row(&view, ordinal)?;
            if inner.record.tag != SemanticTypeTag::ArraySequence
                || inner.record.children.length != 1
            {
                return Err(TestError::Missing("inner nested sequence"));
            }
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
            Err(JavaCollectError::Rejected(rejection))
                if rejection.fact == crate::lower::MAX_EMISSION_FACTS
                    && rejection.name_len == 6
                    && rejection.cause == FactFault::Capacity => {}
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("capacity rejection")),
        }
        Ok(())
    }

    #[test]
    fn projection_terminal_retains_class_and_utf16_operands() -> Result<(), TestError> {
        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Utf16 {
                units: 7,
                utf16_len: 12,
            })
        else {
            return Err(TestError::Missing("Java projection terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::Utf16Offset {
                units: 7,
                source_utf16_len: 12,
            })
        {
            return Err(TestError::Missing("exact UTF-16 projection operands"));
        }
        Ok(())
    }

    #[test]
    fn projection_terminal_retains_type_owner_and_atom_coordinates() -> Result<(), TestError> {
        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Primitive { type_row: 17 })
        else {
            return Err(TestError::Missing("primitive projection terminal"));
        };
        if fault != (backend_semantic::vocabulary::JavaProjectionFault::Primitive { type_row: 17 }) {
            return Err(TestError::Missing("primitive type row"));
        }

        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Depth { type_row: 23 })
        else {
            return Err(TestError::Missing("depth projection terminal"));
        };
        if fault != (backend_semantic::vocabulary::JavaProjectionFault::Depth { type_row: 23 }) {
            return Err(TestError::Missing("depth type row"));
        }

        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Malformed {
                type_row: 24,
                kind: TypeKind::Array,
            })
        else {
            return Err(TestError::Missing("malformed-type projection terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::MalformedType {
                type_row: 24,
                kind: backend_semantic::vocabulary::JavaProjectionTypeKind::Array,
            })
        {
            return Err(TestError::Missing("malformed type coordinates"));
        }

        let mut fix = Fixture::default();
        let owner_atom = fix.atom(b"demo.Owner");
        let name_atom = fix.atom(b"call");
        fix.symbols.push(SymbolRow {
            owner: owner_atom,
            name: name_atom,
            parameters: Vec::new(),
        });
        fix.declarations.push(DeclarationRow {
            kind: 11,
            name: name_atom,
            owner: None,
            documentation: None,
            semantic_type: None,
            symbol: Some(0),
        });
        let bytes = fix.bind(b"")?;
        let owner = backend_frontend_java::legacy::JavaAuthorityImage::open(&bytes)
            .map_err(|_| TestError::Missing("validated symbol fixture"))?
            .image
            .declarations()
            .next()
            .ok_or(TestError::Missing("symbol declaration"))?
            .map_err(|_| TestError::Missing("symbol declaration image"))?
            .symbol
            .ok_or(TestError::Missing("symbol coordinate"))?;
        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::OrphanOwner { owner })
        else {
            return Err(TestError::Missing("orphan-owner projection terminal"));
        };
        if fault != (backend_semantic::vocabulary::JavaProjectionFault::OrphanOwner { owner: 0 }) {
            return Err(TestError::Missing("orphan owner coordinate"));
        }

        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Utf8 {
                symbol: 31,
                atom: backend_semantic::vocabulary::JavaSymbolAtom::Name,
            })
        else {
            return Err(TestError::Missing("atom UTF-8 projection terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::AtomUtf8 {
                symbol: 31,
                atom: backend_semantic::vocabulary::JavaSymbolAtom::Name,
            })
        {
            return Err(TestError::Missing("atom UTF-8 coordinates"));
        }
        Ok(())
    }

    #[test]
    fn projection_terminal_retains_nested_image_coordinates() -> Result<(), TestError> {
        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Image(ImageError::ChildRange {
                plane: ImagePlane::TypeChildren,
                start: 4,
                count: 3,
                upper_bound: 9,
            }))
        else {
            return Err(TestError::Missing("image projection terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::Image {
                cause: backend_semantic::vocabulary::JavaImageFault::ChildRange {
                    plane: backend_semantic::vocabulary::JavaImagePlane::TypeChildren,
                    start: 4,
                    count: 3,
                    upper_bound: 9,
                },
            })
        {
            return Err(TestError::Missing("nested image coordinates"));
        }

        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) = terminal(
            ProjectionFault::Image(ImageError::Header(HeaderError::BodyLength {
                declared: 11,
                actual: 7,
            })),
        ) else {
            return Err(TestError::Missing("header projection terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::Image {
                cause: backend_semantic::vocabulary::JavaImageFault::Header {
                    cause: backend_semantic::vocabulary::JavaImageHeaderFault::BodyLength {
                        declared: 11,
                        actual: 7,
                    },
                },
            })
        {
            return Err(TestError::Missing("nested header operands"));
        }
        Ok(())
    }

    #[test]
    fn image_coordinate_overflow_is_capacity_not_truncation() -> Result<(), TestError> {
        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) =
            terminal(ProjectionFault::Image(ImageError::Header(
                HeaderError::Truncated { actual: usize::MAX },
            )))
        else {
            return Err(TestError::Missing("image overflow terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::IndexCapacity {
                phase: backend_semantic::vocabulary::JavaProjectionIndexPhase::FactOrdinal,
            })
        {
            return Err(TestError::Missing("checked image coordinate overflow"));
        }
        Ok(())
    }

    #[test]
    fn projection_terminal_does_not_fold_foreign_key_to_unsupported_declaration()
    -> Result<(), TestError> {
        let JavaCollectError::Lowering(LoweringUnsupported::JavaProjection { fault }) = terminal(
            ProjectionFault::ForeignKey(ForeignKeyFault::BackslashInPath),
        ) else {
            return Err(TestError::Missing("foreign-key projection terminal"));
        };
        if fault
            != (backend_semantic::vocabulary::JavaProjectionFault::ForeignKey {
                cause: backend_semantic::vocabulary::JavaForeignKeyFault::BackslashInPath,
            })
        {
            return Err(TestError::Missing("foreign-key projection operands"));
        }
        Ok(())
    }

    #[test]
    fn lane_rejection_keeps_full_fact_cause_and_candidate_context() -> Result<(), TestError> {
        let JavaCollectError::Rejected(rejection) = lane_rejection(13, 5, FactFault::Capacity)
        else {
            return Err(TestError::Missing("fact rejection terminal"));
        };
        if rejection.fact != 13 || rejection.name_len != 5 || rejection.cause != FactFault::Capacity
        {
            return Err(TestError::Missing("full fact rejection operands"));
        }
        Ok(())
    }
}
