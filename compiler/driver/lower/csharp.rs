//! Projects validated Roslyn authority facts through the shared canonical lane.
//!
//! The projection covers every plane the version-3 image carries: declaration
//! facts, the recursive type graph (generic applications, arrays, `T?`,
//! tuples, pointers, delegates), executable signatures with parameter and
//! result facts, compiler-resolved occurrences — including explicit
//! interface implementations' bindings to the interface members they
//! implement — XML-documentation provenance with summary fragments, applied
//! attributes, nullability cells, reference kinds, member-effect flags,
//! partial roles, and generic constraints.
//!
//! The `params` modifier has no canonical callable-element cell yet (its
//! explicit array type still lands in full). Declaration-site generic
//! variance and closed special constraints travel through the shared generic
//! parameter algebra.
//! Pattern-based locals are absent from the producer's fact surface, so the
//! projection records none rather than inventing any.
//!
//! Emission is two-pass over declarations: namespaces and type declarations
//! are committed first, so every nominal, type-child, and constraint target
//! is a strictly backward fact ordinal. Delegates and members follow, then
//! language extensions, occurrences, and documentation.
//!
//! Declared types project row-by-row: an in-file named use becomes the
//! backward `Nominal` terminal, a generic application an `Apply` row over its
//! base and argument rows, `T[]` an `Array` row over its element, `T?` (the
//! collapsed `Nullable<T>` value form) an `Annotated` row over its inner row,
//! tuples labelled `Tuple` rows, pointers `Primitive(MutPointer)` rows, and
//! delegates `FunctionPointer` rows over their invocation signatures.
//! Compound positions that are children of other compounds are interned as
//! anonymous type rows owned by the declaration's enclosing fact, because
//! the pooled row lane only admits already-pushed owners. Positions the lane
//! cannot carry — foreign `System/*` nominals, `dynamic`, unbound error
//! types — fold to typed `Unknown` rows that retain the image spelling.
//!
//! Nullability, reference kinds, member effects, partial roles, attribute
//! spellings, and XML provenance live only in the C# extension plane; they
//! never fabricate lane-visible type cells. Never recovers C# facts by
//! scanning source text or a native parser fallback.

use compiler_ir::{
    AnnotationKind, AtomId, AtomListId, CSharpFacts, CSharpMemberEffects, CSharpNullability,
    CSharpPartialRole, CSharpReferenceKind, DocFragmentInput, DocLinkTarget, EntityId, EntityKind,
    ForeignKey, ForeignOrigin, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget,
    ProductChildRole, ReferenceKind, RelSpan, SemanticProductConstructor, SemanticTypeRecord,
    SemanticTypeTag, SourceSpan, TypeParameterListId, TypeReason, TypeWidth,
};
use compiler_languages_csharp::{
    CSharpImage, Declaration, DeclarationKind, HeaderError, ImageError, NullabilityCell,
    Parameter, PartialRole, RefKind, ReferenceTag, ResolvedReference, Section, TypeNode,
    TypeNodeKind, TypeRef, VarianceTag,
};
use compiler_vocabulary::{
    CSharpImageFault, CSharpImageHeaderFault, CSharpImageSection, CSharpImageTypeKind,
    CSharpProjectionFault as PortableCSharpProjectionFault, CSharpProjectionIndexPhase,
    LoweringUnsupported,
};
use sha2::{Digest, Sha256};

use crate::lower::{
    EmissionExtension, FactSet, LEAF_PRODUCT, MAX_EMISSION_FACTS, MAX_REF_LIST_ELEMENTS,
    MAX_TYPE_CHILDREN, SemanticFact, StagedSourceSpan, push_fact,
};
use crate::types::{CSharpProjectionFault, FactFault, FactRejection};

/// Exact rejection while lending source-bound Roslyn declaration facts.
#[derive(Debug)]
pub(crate) enum CSharpCollectError {
    /// The fixed binary authority image failed validation.
    Image(ImageError),
    /// The image's Roslyn source digest differs from the compile request.
    SourceBinding {
        /// SHA-256 digest of exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the authority image.
        observed: [u8; 32],
    },
    /// A validated declaration name span cannot name the exact source bytes.
    Span { start: u32, end: u32 },
    /// A bounded canonical lane rejected an authority declaration.
    Lowering(LoweringUnsupported),
    /// Canonical admission rejected one exact fact; operands retained.
    Rejected(FactRejection),
    /// Projection rejected one exact validated authority fact; operands retained.
    Projection(CSharpProjectionFault),
}

/// One C# authority projection fault crossing the collector boundary.
///
/// The terminal deliberately retains the fault rather than collapsing it to
/// a declaration-support claim; support and malformed authority coordinates
/// are different public outcomes.
#[derive(Debug)]
enum ProjectionFault {
    Image(ImageError),
    Depth { type_row: u32 },
    NameSpan { start: u32, end: u32 },
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    Foreign { reference: u32 },
    AttributeCapacity { spellings: u32 },
    IndexCapacity {
        phase: CSharpProjectionIndexPhase,
        observed: u64,
    },
    HeterogeneousArrayRank { first: u32, observed: u32 },
    Fact(FactFault),
}

fn terminal(fault: ProjectionFault) -> CSharpCollectError {
    match fault {
        ProjectionFault::Image(image) => match portable_image_fault(image) {
            Ok(cause) => CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::Image { cause },
            }),
            Err((phase, observed)) => {
                CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                    fault: PortableCSharpProjectionFault::IndexCapacity { phase, observed },
                })
            }
        },
        ProjectionFault::Depth { type_row } => {
            CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::Depth { type_row },
            })
        }
        ProjectionFault::NameSpan { start, end } => {
            CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::NameSpan { start, end },
            })
        }
        ProjectionFault::OwnerOrder {
            owner_start,
            reference_start,
        } => CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
            fault: PortableCSharpProjectionFault::OwnerOrder {
                owner_start,
                reference_start,
            },
        }),
        ProjectionFault::Foreign { reference } => {
            CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::Foreign { reference },
            })
        }
        ProjectionFault::AttributeCapacity { spellings } => {
            CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::AttributeCapacity { spellings },
            })
        }
        ProjectionFault::IndexCapacity { phase, observed } => {
            CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::IndexCapacity { phase, observed },
            })
        }
        ProjectionFault::HeterogeneousArrayRank { first, observed } => {
            CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::HeterogeneousArrayRank { first, observed },
            })
        }
        ProjectionFault::Fact(fault) => CSharpCollectError::Projection(CSharpProjectionFault::Fact(fault)),
    }
}

fn image_u32(
    value: usize,
    phase: CSharpProjectionIndexPhase,
) -> Result<u32, (CSharpProjectionIndexPhase, u64)> {
    u32::try_from(value).map_err(|_| (phase, value as u64))
}

const fn image_section(section: Section) -> CSharpImageSection {
    match section {
        Section::Atoms => CSharpImageSection::Atoms,
        Section::AtomBytes => CSharpImageSection::AtomBytes,
        Section::Declarations => CSharpImageSection::Declarations,
        Section::Parameters => CSharpImageSection::Parameters,
        Section::TypeParameters => CSharpImageSection::TypeParameters,
        Section::TypeConstraints => CSharpImageSection::TypeConstraints,
        Section::Types => CSharpImageSection::Types,
        Section::TypeChildren => CSharpImageSection::TypeChildren,
        Section::Attributes => CSharpImageSection::Attributes,
        Section::Docs => CSharpImageSection::Docs,
        Section::References => CSharpImageSection::References,
    }
}

const fn image_type_kind(kind: TypeNodeKind) -> CSharpImageTypeKind {
    match kind {
        TypeNodeKind::Named => CSharpImageTypeKind::Named,
        TypeNodeKind::Array => CSharpImageTypeKind::Array,
        TypeNodeKind::Pointer => CSharpImageTypeKind::Pointer,
        TypeNodeKind::NullableValue => CSharpImageTypeKind::NullableValue,
        TypeNodeKind::Tuple => CSharpImageTypeKind::Tuple,
        TypeNodeKind::FunctionPointer => CSharpImageTypeKind::FunctionPointer,
        TypeNodeKind::TypeParameter => CSharpImageTypeKind::TypeParameter,
        TypeNodeKind::Dynamic => CSharpImageTypeKind::Dynamic,
        TypeNodeKind::Error => CSharpImageTypeKind::Error,
    }
}

fn portable_header_fault(
    error: HeaderError,
) -> Result<CSharpImageHeaderFault, (CSharpProjectionIndexPhase, u64)> {
    Ok(match error {
        HeaderError::Truncated { actual } => CSharpImageHeaderFault::Truncated {
            actual: image_u32(actual, CSharpProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::Magic { found } => CSharpImageHeaderFault::Magic { found },
        HeaderError::Version { found } => CSharpImageHeaderFault::Version { found },
        HeaderError::Length { found } => CSharpImageHeaderFault::Length {
            found: image_u32(found, CSharpProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::SectionCount { found } => CSharpImageHeaderFault::SectionCount {
            found: image_u32(found, CSharpProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::BodyLength { declared, actual } => CSharpImageHeaderFault::BodyLength {
            declared: image_u32(declared, CSharpProjectionIndexPhase::ImageHeader)?,
            actual: image_u32(actual, CSharpProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::Reserved => CSharpImageHeaderFault::Reserved,
        HeaderError::DirectoryTag { expected, found } => {
            CSharpImageHeaderFault::DirectoryTag { expected, found }
        }
        HeaderError::DirectoryRowBytes { expected, found } => {
            CSharpImageHeaderFault::DirectoryRowBytes {
                expected: image_u32(expected, CSharpProjectionIndexPhase::ImageHeader)?,
                found: image_u32(found, CSharpProjectionIndexPhase::ImageHeader)?,
            }
        }
        HeaderError::DirectoryByteCount {
            count,
            row_bytes,
            found,
        } => CSharpImageHeaderFault::DirectoryByteCount {
            count: image_u32(count, CSharpProjectionIndexPhase::ImageHeader)?,
            row_bytes: image_u32(row_bytes, CSharpProjectionIndexPhase::ImageHeader)?,
            found: image_u32(found, CSharpProjectionIndexPhase::ImageHeader)?,
        },
        HeaderError::DirectoryOffset { expected, found } => {
            CSharpImageHeaderFault::DirectoryOffset {
                expected: image_u32(expected, CSharpProjectionIndexPhase::ImageHeader)?,
                found: image_u32(found, CSharpProjectionIndexPhase::ImageHeader)?,
            }
        }
        HeaderError::DirectoryRange {
            offset,
            length,
            image_bytes,
        } => CSharpImageHeaderFault::DirectoryRange {
            offset: image_u32(offset, CSharpProjectionIndexPhase::ImageHeader)?,
            length: image_u32(length, CSharpProjectionIndexPhase::ImageHeader)?,
            image_bytes: image_u32(image_bytes, CSharpProjectionIndexPhase::ImageHeader)?,
        },
    })
}

fn portable_image_fault(
    error: ImageError,
) -> Result<CSharpImageFault, (CSharpProjectionIndexPhase, u64)> {
    Ok(match error {
        ImageError::Header(error) => CSharpImageFault::Header {
            cause: portable_header_fault(error)?,
        },
        ImageError::Digest => CSharpImageFault::Digest,
        ImageError::DeclarationKind {
            index,
            found,
            plane,
        } => CSharpImageFault::DeclarationKind {
            index: image_u32(index, CSharpProjectionIndexPhase::Declaration)?,
            found,
            plane: image_section(plane),
        },
        ImageError::DeclarationReserved { index, plane } => {
            CSharpImageFault::DeclarationReserved {
                index: image_u32(index, CSharpProjectionIndexPhase::Declaration)?,
                plane: image_section(plane),
            }
        }
        ImageError::NameRange {
            index,
            offset,
            length,
            atom_bytes,
        } => CSharpImageFault::NameRange {
            index: image_u32(index, CSharpProjectionIndexPhase::Name)?,
            offset,
            length,
            atom_bytes: image_u32(atom_bytes, CSharpProjectionIndexPhase::Name)?,
        },
        ImageError::NameUtf8 { index } => CSharpImageFault::NameUtf8 {
            index: image_u32(index, CSharpProjectionIndexPhase::Name)?,
        },
        ImageError::Span { index, start, end } => CSharpImageFault::Span {
            index: image_u32(index, CSharpProjectionIndexPhase::SourceSpan)?,
            start,
            end,
        },
        ImageError::TypeChildCount {
            index,
            kind,
            min,
            max,
            actual,
        } => CSharpImageFault::TypeChildCount {
            index: image_u32(index, CSharpProjectionIndexPhase::TypeRow)?,
            kind: image_type_kind(kind),
            min: image_u32(min, CSharpProjectionIndexPhase::TypeChild)?,
            max: image_u32(max, CSharpProjectionIndexPhase::TypeChild)?,
            actual: image_u32(actual, CSharpProjectionIndexPhase::TypeChild)?,
        },
    })
}

/// Retains a non-declaration lane rejection at the projection boundary.
fn lane_terminal(fault: FactFault) -> CSharpCollectError {
    terminal(ProjectionFault::Fact(fault))
}

impl From<ProjectionFault> for CSharpCollectError {
    fn from(fault: ProjectionFault) -> Self {
        terminal(fault)
    }
}

/// Admits one fact and returns its proven backward ordinal.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<u32, CSharpCollectError> {
    let ordinal = push_fact(facts, fact).map_err(CSharpCollectError::Rejected)?;
    u32::try_from(ordinal).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::FactOrdinal,
            observed: ordinal as u64,
        })
    })
}

/// Producer depth budget of the recursive type graph, documented by the image
/// writer. The projection enforces the same budget so a hostile image cannot
/// drive unbounded recursion.
const DEPTH_LIMIT: usize = 64;

/// `PrimitiveShape::Integer` wire cell.
const SHAPE_INTEGER: u32 = 0;
/// `PrimitiveShape::Float` wire cell.
const SHAPE_FLOAT: u32 = 1;
/// `PrimitiveShape::Bool` wire cell.
const SHAPE_BOOL: u32 = 2;
/// `PrimitiveShape::Utf16CodeUnit` wire cell.
const SHAPE_UTF16_CODE_UNIT: u32 = 18;
/// `PrimitiveShape::Str` wire cell.
const SHAPE_STR: u32 = 4;
/// `PrimitiveShape::MutPointer` wire cell.
const SHAPE_MUT_POINTER: u32 = 5;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;
/// `PrimitiveShape::NativeSignedInteger` wire cell.
const SHAPE_NATIVE_SIGNED_INTEGER: u32 = 14;
/// `PrimitiveShape::PointerAddressInteger` wire cell.
const SHAPE_POINTER_ADDRESS_INTEGER: u32 = 16;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// Multidimensional array text spellings by rank, ranks one through four.
/// Rank one spells `[]`; higher ranks keep their comma spelling; deeper
/// ranks fold to typed unknowns with their written spelling.

/// The void builtin spelling; the void row is a primitive builtin leaf.
const VOID_SPELLING: &[u8] = b"void";

/// The decimal builtin spelling; the lane keeps the exact-width cell a
/// builtin because no closed primitive shape owns a 128-bit float.
const DECIMAL_SPELLING: &[u8] = b"decimal";

/// Foreign-key ecosystem of every unresolved C# target.
const ECOSYSTEM_STR: &str = "nuget";

/// Documentation-link ecosystem of every unresolved `cref`.
const ECOSYSTEM: &[u8] = b"nuget";

/// Streams all typed Roslyn facts — declarations, recursive types,
/// signatures, extensions, occurrences, and docs — into the shared fact lane.
pub(crate) fn collect<'source>(
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), CSharpCollectError> {
    let image = CSharpImage::open(image_bytes).map_err(CSharpCollectError::Image)?;
    let expected: [u8; 32] = Sha256::digest(source).into();
    let observed = image.source_digest();
    if observed != expected {
        return Err(CSharpCollectError::SourceBinding { expected, observed });
    }
    let total = image.declarations().len();

    // Pass one: namespaces and type declarations. Type declarations carry
    // the recursive terminal — a nominal row naming their own ordinal — and
    // seed the qualified-name index every later type use resolves against.
    let mut names = Names::new();
    let mut ordinals = Ordinals::new();
    for coordinate in 0..total {
        let declared = declaration(&image, coordinate).map_err(terminal)?;
        match declared.kind {
            DeclarationKind::Class
            | DeclarationKind::Struct
            | DeclarationKind::Interface
            | DeclarationKind::Enum
            | DeclarationKind::Record
            | DeclarationKind::RecordStruct => {
                // A partial pair commits one type fact: the Definition part
                // owns it, and the producer folds every part's members under
                // that one row, so an Implementation part is never pushed.
                if declared.partial == PartialRole::Implementation {
                    continue;
                }
                let ordinal = push_type_root(facts, source, &declared)?;
                ordinals.record(coordinate, ordinal).map_err(terminal)?;
                if let Some(qualified) = declared.qualified {
                    names
                        .record(
                            qualified.bytes,
                            ImageDeclarationId::from_index(coordinate).map_err(terminal)?,
                        )
                        .map_err(terminal)?;
                }
            }
            DeclarationKind::Namespace => {
                let ordinal = push_namespace(facts, source, &declared)?;
                ordinals.record(coordinate, ordinal).map_err(terminal)?;
            }
            DeclarationKind::Delegate
            | DeclarationKind::Field
            | DeclarationKind::EnumMember
            | DeclarationKind::Property
            | DeclarationKind::Indexer
            | DeclarationKind::Event
            | DeclarationKind::Constructor
            | DeclarationKind::Method
            | DeclarationKind::Operator
            | DeclarationKind::Conversion => {}
        }
    }

    // Pass two: delegates first — their invocation signatures anchor the
    // type plane — then every member in declaration order.
    for coordinate in 0..total {
        let declared = declaration(&image, coordinate).map_err(terminal)?;
        if declared.kind == DeclarationKind::Delegate {
            let ordinal = push_delegate(facts, &image, &names, &ordinals, source, &declared)?;
            ordinals.record(coordinate, ordinal).map_err(terminal)?;
        }
    }
    for coordinate in 0..total {
        let declared = declaration(&image, coordinate).map_err(terminal)?;
        match declared.kind {
            DeclarationKind::Delegate
            | DeclarationKind::Namespace
            | DeclarationKind::Class
            | DeclarationKind::Struct
            | DeclarationKind::Interface
            | DeclarationKind::Enum
            | DeclarationKind::Record
            | DeclarationKind::RecordStruct => {}
            DeclarationKind::Field
            | DeclarationKind::EnumMember
            | DeclarationKind::Property
            | DeclarationKind::Indexer
            | DeclarationKind::Event => {
                let ordinal = push_member(facts, &image, &names, &ordinals, source, &declared)?;
                ordinals.record(coordinate, ordinal).map_err(terminal)?;
            }
            DeclarationKind::Constructor
            | DeclarationKind::Method
            | DeclarationKind::Operator
            | DeclarationKind::Conversion => {
                let ordinal = push_executable(facts, &image, &names, &ordinals, source, &declared)?;
                ordinals.record(coordinate, ordinal).map_err(terminal)?;
            }
        }
    }

    // The Roslyn image owns both containment and identifier coordinates.
    // Bind only relationships whose owner survived emission: partial
    // implementation rows deliberately have no local ordinal and therefore
    // remain explicit `Unavailable`, never fabricated roots. The image does
    // not retain a declaration-end coordinate, so capture the exact named
    // identifier span rather than pretending its start/name-end interval is
    // the whole declaration.
    // One image-aligned lane records the only positive conclusion this pass
    // needs: a retained declaration has some authority child. We record it
    // before filtering the child itself, so an unsupported or filtered child
    // prevents an empty-set claim for its retained owner.
    let mut owner_has_child = vec![false; total];
    for coordinate in 0..total {
        let declared = declaration(&image, coordinate).map_err(terminal)?;
        let owner = declared
            .owner
            .map(|owner| {
                usize::try_from(owner).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: CSharpProjectionIndexPhase::Declaration,
                        observed: owner as u64,
                    })
                })
            })
            .transpose()?;
        if let Some(owner) = owner
            && let Some(has_child) = owner_has_child.get_mut(owner)
        {
            *has_child = true;
        }
        let Some(ordinal) = ordinals.lookup(coordinate) else {
            continue;
        };
        let span =
            StagedSourceSpan::new(declared.name_start, declared.name_end).ok_or_else(|| {
                terminal(ProjectionFault::NameSpan {
                    start: declared.name_start,
                    end: declared.name_end,
                })
            })?;
        facts
            .attach_source_span(ordinal, span)
            .map_err(lane_terminal)?;
        // The Roslyn image owns this declaration's XML-doc plane even when
        // the corresponding summary is absent or empty.
        facts
            .mark_documentation_captured(ordinal)
            .map_err(lane_terminal)?;
        match owner {
            None => facts.mark_parentage_root(ordinal).map_err(lane_terminal)?,
            Some(owner) => {
                if let Some(parent) = ordinals.lookup(owner) {
                    facts
                        .attach_parent(ordinal, parent)
                        .map_err(lane_terminal)?;
                }
            }
        }
    }

    // This completed relation pass can prove an exact empty member set only
    // when no authority declaration names this row as owner. An owner with
    // even one child remains unavailable until every child is represented or
    // explicitly canonicalized under the retained row; a completed pass
    // alone is not proof. Signature carriers have no image coordinate and
    // are never marked. A filtered C# owner still has no stable representable
    // identity, so its child intentionally remains parentage-unavailable
    // rather than being fabricated as a root.
    for coordinate in 0..total {
        let Some(ordinal) = ordinals.lookup(coordinate) else {
            continue;
        };
        if !owner_has_child[coordinate] {
            facts
                .mark_members_captured(ordinal)
                .map_err(lane_terminal)?;
        }
    }

    // Pass three: language extensions, after every constraint target exists.
    let attributes = attribute_index(&image).map_err(terminal)?;
    for coordinate in 0..total {
        let Some(ordinal) = ordinals.lookup(coordinate) else {
            continue;
        };
        let declared = declaration(&image, coordinate).map_err(terminal)?;
        let extension = csharp_facts(
            facts,
            &image,
            &names,
            &ordinals,
            &declared,
            &attributes,
            coordinate,
        )?;
        facts
            .attach_extension(
                usize::try_from(ordinal).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: CSharpProjectionIndexPhase::FactOrdinal,
                        observed: ordinal as u64,
                    })
                })?,
                EmissionExtension::CSharp(extension),
            )
            .map_err(lane_terminal)?;
    }

    // Pass four: compiler-resolved occurrences in image order.
    for (reference_index, reference) in image.references().enumerate() {
        let reference = reference.map_err(ProjectionFault::Image)?;
        let reference_index = u32::try_from(reference_index).map_err(|_| {
            terminal(ProjectionFault::IndexCapacity {
                phase: CSharpProjectionIndexPhase::Reference,
                observed: reference_index as u64,
            })
        })?;
        push_occurrence(facts, &image, &ordinals, reference_index, &reference).map_err(terminal)?;
    }

    // Pass five: XML documentation provenance and summary fragments.
    for doc in image.docs() {
        let doc = doc.map_err(ProjectionFault::Image)?;
        push_doc(facts, &names, &ordinals, &doc).map_err(terminal)?;
    }
    Ok(())
}

/// Borrows one validated declaration row.
fn declaration<'image>(
    image: &CSharpImage<'image>,
    coordinate: usize,
) -> Result<Declaration<'image>, ProjectionFault> {
    let raw = u32::try_from(coordinate).map_err(|_| ProjectionFault::IndexCapacity {
        phase: CSharpProjectionIndexPhase::Declaration,
        observed: coordinate as u64,
    })?;
    image.declaration(raw).map_err(ProjectionFault::Image)
}

/// Verifies one declared name span names the exact bound source bytes and
/// returns the borrowed identifier.
fn checked_name<'source>(
    source: &'source [u8],
    declared: &Declaration<'source>,
) -> Result<&'source [u8], CSharpCollectError> {
    let fault = |start: u32, end: u32| CSharpCollectError::Span { start, end };
    let start = usize::try_from(declared.name_start)
        .map_err(|_| fault(declared.name_start, declared.name_end))?;
    let end = usize::try_from(declared.name_end)
        .map_err(|_| fault(declared.name_start, declared.name_end))?;
    let name = source
        .get(start..end)
        .ok_or(fault(declared.name_start, declared.name_end))?;
    if name != declared.name.bytes {
        return Err(fault(declared.name_start, declared.name_end));
    }
    Ok(name)
}

const fn entity_kind(kind: DeclarationKind, is_const: bool) -> EntityKind {
    match kind {
        DeclarationKind::Class
        | DeclarationKind::Struct
        | DeclarationKind::Record
        | DeclarationKind::RecordStruct => EntityKind::Record,
        DeclarationKind::Interface => EntityKind::Trait,
        DeclarationKind::Enum => EntityKind::Enum,
        DeclarationKind::Namespace => EntityKind::Namespace,
        DeclarationKind::Delegate
        | DeclarationKind::Constructor
        | DeclarationKind::Method
        | DeclarationKind::Operator
        | DeclarationKind::Conversion => EntityKind::Function,
        DeclarationKind::Field if is_const => EntityKind::Constant,
        DeclarationKind::Field => EntityKind::Field,
        DeclarationKind::EnumMember => EntityKind::Variant,
        DeclarationKind::Property | DeclarationKind::Indexer | DeclarationKind::Event => {
            EntityKind::Field
        }
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

/// The recursive terminal: a nominal row naming the fact's own ordinal.
const fn nominal_record(ordinal: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
    record
}

/// One honest unknown row whose reason carries an optional spelling.
fn unknown_record(reason: TypeReason, spelling: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Unknown);
    record.payload0 = u32::from(reason);
    record.text = spelling;
    record
}

/// Bounded qualified-name index of the image's named type declarations.
struct Names<'image> {
    entries: [(&'image [u8], ImageDeclarationId); MAX_EMISSION_FACTS],
    len: usize,
}

impl<'image> Names<'image> {
    const fn new() -> Self {
        Self {
            entries: [(&[], ImageDeclarationId(0)); MAX_EMISSION_FACTS],
            len: 0,
        }
    }

    fn record(
        &mut self,
        name: &'image [u8],
        coordinate: ImageDeclarationId,
    ) -> Result<(), ProjectionFault> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity {
                phase: CSharpProjectionIndexPhase::Name,
                observed: self.len as u64,
            });
        }
        self.entries[self.len] = (name, coordinate);
        self.len += 1;
        Ok(())
    }

    fn lookup(&self, name: &[u8]) -> Option<ImageDeclarationId> {
        self.entries
            .iter()
            .take(self.len)
            .find(|(known, _)| *known == name)
            .map(|(_, coordinate)| *coordinate)
    }

    /// Resolves one `cref` body: qualified names match exactly and simple
    /// names match as the last qualified segment.
    fn lookup_link(&self, spelling: &[u8]) -> Option<ImageDeclarationId> {
        for (name, coordinate) in self.entries.iter().take(self.len) {
            if *name == spelling {
                return Some(*coordinate);
            }
            if let Some(boundary) = name.len().checked_sub(spelling.len() + 1)
                && name.get(boundary) == Some(&b'.')
                && name.get(boundary + 1..) == Some(spelling)
            {
                return Some(*coordinate);
            }
        }
        None
    }
}

/// Bounded index from declaration coordinates to pushed fact ordinals.
struct Ordinals {
    entries: [(usize, u32); MAX_EMISSION_FACTS],
    len: usize,
}

/// A declaration coordinate in the immutable Roslyn image. It is deliberately
/// not a fragment coordinate: image declaration order and canonical emission
/// order diverge for partial types, members, and synthetic signature rows.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageDeclarationId(u32);

/// A type-fact coordinate in the staging image. This is the only coordinate
/// C# generic constraints may lend to the shared type-parameter pool.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StagedTypeFactId(u32);

impl ImageDeclarationId {
    fn from_index(index: usize) -> Result<Self, ProjectionFault> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| ProjectionFault::IndexCapacity {
                phase: CSharpProjectionIndexPhase::Declaration,
                observed: index as u64,
            })
    }

    fn index(self) -> Result<usize, ProjectionFault> {
        usize::try_from(self.0).map_err(|_| ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::Declaration,
            observed: self.0 as u64,
        })
    }
}

impl StagedTypeFactId {
    const fn raw(self) -> u32 {
        self.0
    }
}

/// Resolves a named type through the two coordinate domains. Keeping this
/// bridge here prevents an image-row index from ever being passed as a fact
/// type reference (the former C# constraint corruption seam).
fn staged_type_for_named_image_declaration(
    names: &Names<'_>,
    ordinals: &Ordinals,
    spelling: &[u8],
) -> Option<StagedTypeFactId> {
    let image = names.lookup(spelling)?;
    let staged = ordinals.lookup(usize::try_from(image.0).ok()?)?;
    Some(StagedTypeFactId(staged))
}

impl Ordinals {
    const fn new() -> Self {
        Self {
            entries: [(usize::MAX, 0); MAX_EMISSION_FACTS],
            len: 0,
        }
    }

    fn record(&mut self, coordinate: usize, ordinal: u32) -> Result<(), ProjectionFault> {
        if self.len == self.entries.len() {
            return Err(ProjectionFault::IndexCapacity {
                phase: CSharpProjectionIndexPhase::FactOrdinal,
                observed: self.len as u64,
            });
        }
        self.entries[self.len] = (coordinate, ordinal);
        self.len += 1;
        Ok(())
    }

    fn lookup(&self, coordinate: usize) -> Option<u32> {
        self.entries
            .iter()
            .take(self.len)
            .find(|(known, _)| *known == coordinate)
            .map(|(_, ordinal)| *ordinal)
    }
}

/// Pushes one namespace fact with its honestly unknown declared type.
fn push_namespace<'source>(
    facts: &mut FactSet<'source>,
    source: &'source [u8],
    declared: &Declaration<'source>,
) -> Result<u32, CSharpCollectError> {
    let name = checked_name(source, declared)?;
    let kind = entity_kind(declared.kind, declared.flags.is_const);
    push(facts, SemanticFact::new(kind, name, constructor(kind)))
}

/// Pushes one type declaration with the recursive terminal: a nominal row
/// naming the declaration's own ordinal.
fn push_type_root<'source>(
    facts: &mut FactSet<'source>,
    source: &'source [u8],
    declared: &Declaration<'source>,
) -> Result<u32, CSharpCollectError> {
    let name = checked_name(source, declared)?;
    let kind = entity_kind(declared.kind, declared.flags.is_const);
    let self_ordinal = u32::try_from(facts.len()).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::FactOrdinal,
            observed: facts.len() as u64,
        })
    })?;
    push(
        facts,
        SemanticFact::new(kind, name, constructor(kind)).typed(nominal_record(self_ordinal)),
    )
}

/// Resolves the enclosing fact anchor for anonymous rows of one declaration:
/// the owner declaration's fact when one exists, otherwise the first pushed
/// fact. The pooled row lane only admits already-pushed owners, and owner
/// declarations are always committed in pass one.
fn anchor_for(ordinals: &Ordinals, declared: &Declaration<'_>) -> Result<u32, CSharpCollectError> {
    let owner = declared
        .owner
        .map(|owner| {
            usize::try_from(owner).map_err(|_| {
                terminal(ProjectionFault::IndexCapacity {
                    phase: CSharpProjectionIndexPhase::Declaration,
                    observed: owner as u64,
                })
            })
        })
        .transpose()?;
    Ok(owner
        .and_then(|owner| ordinals.lookup(owner))
        .or_else(|| ordinals.lookup(0))
        .unwrap_or(0))
}

/// Pushes one delegate: its parameter facts first, then the delegate fact
/// whose function product and function-pointer row target those carriers.
fn push_delegate<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    source: &'source [u8],
    declared: &Declaration<'source>,
) -> Result<u32, CSharpCollectError> {
    let name = checked_name(source, declared)?;
    let anchor = anchor_for(ordinals, declared)?;
    let mut signature = Signature::new();
    for parameter in declared.parameters.iter() {
        let parameter = parameter
            .map_err(ProjectionFault::Image)
            .map_err(terminal)?;
        signature.push_parameter(facts, image, names, ordinals, anchor, parameter)?;
    }
    if let Some(return_type) = declared.declared_type {
        signature.push_result(facts, image, names, ordinals, anchor, return_type)?;
    }
    let fact = signature.finish(name);
    push(facts, fact)
}

/// Pushes one member declaration: field, enum member, property, indexer, or
/// event, with its projected declared type.
fn push_member<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    source: &'source [u8],
    declared: &Declaration<'source>,
) -> Result<u32, CSharpCollectError> {
    let name = checked_name(source, declared)?;
    let kind = entity_kind(declared.kind, declared.flags.is_const);
    let anchor = anchor_for(ordinals, declared)?;

    // Indexer parameters become ordered product members of the member fact.
    let mut parameter_ordinals = Vec::new();
    if declared.kind == DeclarationKind::Indexer {
        for parameter in declared.parameters.iter() {
            let parameter = parameter
                .map_err(ProjectionFault::Image)
                .map_err(terminal)?;
            let ordinal = push_parameter_fact(facts, image, names, ordinals, anchor, parameter)?;
            parameter_ordinals.push(ordinal);
        }
    }

    let projection = match declared.declared_type {
        Some(reference) => project_fact_type(
            facts,
            image,
            names,
            ordinals,
            anchor,
            reference,
            DEPTH_LIMIT,
        )?,
        None => ProjectedType {
            record: unknown_record(TypeReason::OracleGap, None),
            children: Vec::new(),
            spelling: None,
            nullable: NullabilityCell::None,
            void: false,
        },
    };
    let mut fact = SemanticFact::new(
        kind,
        name,
        if parameter_ordinals.is_empty() {
            constructor(kind)
        } else {
            SemanticProductConstructor::PRODUCT
        },
    )
    .typed(projection.record);
    for (target, label) in projection.children {
        fact = fact.type_child(target, label, 0);
    }
    for ordinal in parameter_ordinals {
        fact = fact.child(ProductChildRole::ProductMember, ordinal);
    }
    push(facts, fact)
}

/// Pushes one constructor, method, operator, or conversion: its parameter
/// facts first, then the executable fact whose function product and
/// function-pointer row target those carriers. Overloads already differ in
/// their constructor and signature cells, so identities stay distinct.
fn push_executable<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    source: &'source [u8],
    declared: &Declaration<'source>,
) -> Result<u32, CSharpCollectError> {
    let name = checked_name(source, declared)?;
    let anchor = anchor_for(ordinals, declared)?;
    let mut signature = Signature::new();
    for parameter in declared.parameters.iter() {
        let parameter = parameter
            .map_err(ProjectionFault::Image)
            .map_err(terminal)?;
        signature.push_parameter(facts, image, names, ordinals, anchor, parameter)?;
    }
    if let Some(return_type) = declared.declared_type {
        signature.push_result(facts, image, names, ordinals, anchor, return_type)?;
    }
    let fact = signature.finish(name);
    push(facts, fact)
}

/// One executable signature under construction: ordered parameter carriers
/// and the optional result carrier.
struct Signature {
    parameter_ordinals: Vec<u32>,
    result_ordinal: Option<u32>,
}

impl Signature {
    const fn new() -> Self {
        Self {
            parameter_ordinals: Vec::new(),
            result_ordinal: None,
        }
    }

    fn push_parameter<'source>(
        &mut self,
        facts: &mut FactSet<'source>,
        image: &CSharpImage<'source>,
        names: &Names<'source>,
        ordinals: &Ordinals,
        anchor: u32,
        parameter: Parameter<'source>,
    ) -> Result<(), CSharpCollectError> {
        let ordinal = push_parameter_fact(facts, image, names, ordinals, anchor, parameter)?;
        self.parameter_ordinals.push(ordinal);
        Ok(())
    }

    fn push_result<'source>(
        &mut self,
        facts: &mut FactSet<'source>,
        image: &CSharpImage<'source>,
        names: &Names<'source>,
        ordinals: &Ordinals,
        anchor: u32,
        return_type: TypeRef,
    ) -> Result<(), CSharpCollectError> {
        let projection = project_fact_type(
            facts,
            image,
            names,
            ordinals,
            anchor,
            return_type,
            DEPTH_LIMIT,
        )?;
        if projection.void {
            return Ok(());
        }
        // The result carrier belongs to the executable's key family: a
        // `Parameter` fact named by the return type's written spelling.
        let name = projection.spelling.unwrap_or(b"result");
        let mut fact =
            SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT).typed(projection.record);
        for (target, label) in projection.children {
            fact = fact.type_child(target, label, 0);
        }
        let ordinal = push(facts, fact)?;
        self.result_ordinal = Some(ordinal);
        Ok(())
    }

    /// Finishes the executable fact: function constructor over the carriers
    /// and the function-pointer record over the same ordered rows.
    fn finish<'source>(self, name: &'source [u8]) -> SemanticFact<'source> {
        let arity = u32::try_from(self.parameter_ordinals.len()).unwrap_or(u32::MAX);
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if self.result_ordinal.is_some() {
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name,
            SemanticProductConstructor::function(arity, u32::from(self.result_ordinal.is_some())),
        )
        .typed(record);
        for ordinal in &self.parameter_ordinals {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        if let Some(ordinal) = self.result_ordinal {
            fact = fact.child(ProductChildRole::FunctionResult, ordinal);
        }
        for ordinal in self
            .parameter_ordinals
            .iter()
            .copied()
            .chain(self.result_ordinal)
        {
            fact = fact.type_child(ordinal, None, 0);
        }
        fact
    }
}

/// Pushes one parameter carrier fact with its projected declared type and
/// the parameter's reference convention.
fn push_parameter_fact<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    anchor: u32,
    parameter: Parameter<'source>,
) -> Result<u32, CSharpCollectError> {
    let projection = project_fact_type(
        facts,
        image,
        names,
        ordinals,
        anchor,
        parameter.ty,
        DEPTH_LIMIT,
    )?;
    let mut fact = SemanticFact::new(EntityKind::Parameter, parameter.name.bytes, LEAF_PRODUCT)
        .typed(projection.record);
    for (target, label) in projection.children {
        fact = fact.type_child(target, label, 0);
    }
    let mut extension = empty_extension();
    extension.nullability = lane_nullability(projection.nullable);
    extension.reference_kind = lane_reference_kind(parameter.ref_kind);
    push(
        facts,
        fact.with_extension(EmissionExtension::CSharp(extension)),
    )
}

/// One projected declaration-site type: the lattice record its fact attaches,
/// its ordered backward children, the written root spelling for carrier
/// names, the meaningful nullability cell, and the void leaf flag.
struct ProjectedType<'source> {
    record: SemanticTypeRecord<'source>,
    children: Vec<(u32, Option<&'source [u8]>)>,
    spelling: Option<&'source [u8]>,
    nullable: NullabilityCell,
    void: bool,
}

/// Projects one image type coordinate into the lane's lattice at fact level.
fn project_fact_type<'source>(
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
    if let Some(kind) = reference_nullability(projection.nullable) {
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
fn child_target<'source>(
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
    let nullable = projection.nullable;
    let target = projection_target(facts, anchor, projection)?;
    match reference_nullability(nullable) {
        Some(kind) => {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Annotated);
            record.payload0 = kind as u32;
            intern_row(facts, anchor, record, &[(target, None)]).map_err(lane_terminal)
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
        None => intern_row(facts, anchor, projection.record, &projection.children)
            .map_err(lane_terminal),
    }
}

/// Maps a meaningful Roslyn reference-nullability cell into a structural
/// annotation. Oblivious (`None`) is intentionally absence, not a fabricated
/// assertion about the reference type.
const fn reference_nullability(cell: NullabilityCell) -> Option<AnnotationKind> {
    match cell {
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
) -> Result<u32, FactFault> {
    for (target, name) in children {
        facts.anonymous_type_child(*target, *name, 0)?;
    }
    facts.intern_anonymous_type_row(anchor, record)
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

/// The empty extension row used before its cells are known.
const fn empty_extension() -> CSharpFacts {
    CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints: TypeParameterListId::new(0),
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes: AtomListId::new(0),
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    }
}

/// The lane's nullability cell for one image nullability cell.
const fn lane_nullability(cell: NullabilityCell) -> CSharpNullability {
    match cell {
        NullabilityCell::None => CSharpNullability::Oblivious,
        NullabilityCell::Annotated => CSharpNullability::Nullable,
        NullabilityCell::NotAnnotated => CSharpNullability::NonNullable,
    }
}

/// The lane's reference-kind cell for one image convention. `ref readonly`
/// collapses onto `In`: the lane's closed convention lattice has no fourth
/// by-reference cell, and the collapse is documented, never silent.
const fn lane_reference_kind(kind: RefKind) -> CSharpReferenceKind {
    match kind {
        RefKind::Value => CSharpReferenceKind::Value,
        RefKind::In | RefKind::RefReadonly => CSharpReferenceKind::In,
        RefKind::Ref => CSharpReferenceKind::Ref,
        RefKind::Out => CSharpReferenceKind::Out,
    }
}

/// The lane's partial-role cell for one image role.
const fn lane_partial(role: PartialRole) -> CSharpPartialRole {
    match role {
        PartialRole::None => CSharpPartialRole::None,
        PartialRole::Definition => CSharpPartialRole::Definition,
        PartialRole::Implementation => CSharpPartialRole::Implementation,
    }
}

/// Builds the per-declaration extension row: interned attribute spellings,
/// the pooled constraint rows, effect flags, partial role, nullability, and
/// the XML provenance atom with its comment span.
fn csharp_facts<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    declared: &Declaration<'source>,
    attributes: &[(u32, &'source [u8])],
    coordinate: usize,
) -> Result<CSharpFacts, CSharpCollectError> {
    let mut extension = empty_extension();
    extension.partial = lane_partial(declared.partial);
    extension.effects = CSharpMemberEffects {
        is_async: declared.flags.is_async,
        is_iterator: declared.flags.is_iterator,
        is_extension: declared.flags.is_extension,
    };

    // Nullability: executables, delegates, and members carry their return
    // or declared type's cell; namespaces and type declarations have none.
    let nullability_source = declared.declared_type;
    if let Some(return_type) = nullability_source
        && let Ok(node) = image.type_node(return_type)
    {
        extension.nullability = lane_nullability(node.nullable);
    }

    // Constraints: one pooled row per declaration-site parameter preserving
    // every resolvable constraint in source order. A generic contract is not
    // a first-match slot.
    let start = facts.type_parameter_len;
    let start = u32::try_from(start).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::FactOrdinal,
            observed: start as u64,
        })
    })?;
    let anchor = ordinals
        .lookup(coordinate)
        .ok_or_else(|| {
            terminal(ProjectionFault::IndexCapacity {
                phase: CSharpProjectionIndexPhase::FactOrdinal,
                observed: coordinate as u64,
            })
        })?;
    for generic in declared.type_parameters.iter() {
        let generic = generic.map_err(ProjectionFault::Image).map_err(terminal)?;
        let mut bounds = Vec::with_capacity(generic.constraints.len());
        for child in generic.constraints {
            bounds.push(compiler_ir::ExtensionTypeParameterBound::Type(
                child_target(
                    facts,
                    image,
                    names,
                    ordinals,
                    anchor,
                    child,
                    MAX_TYPE_CHILDREN,
                )?,
            ));
        }
        // The authority's current `reference_type` bit distinguishes `class`
        // but not `class?`; we retain only the proved non-nullable form here
        // and must extend the producer before claiming nullable-reference
        // constraint fidelity.
        let primary = if generic.unmanaged {
            compiler_ir::TypeParameterPrimaryRequirement::Unmanaged
        } else if generic.reference_type {
            compiler_ir::TypeParameterPrimaryRequirement::Reference { nullable: false }
        } else if generic.value_type {
            compiler_ir::TypeParameterPrimaryRequirement::Value
        } else if generic.not_null {
            compiler_ir::TypeParameterPrimaryRequirement::NotNull
        } else {
            compiler_ir::TypeParameterPrimaryRequirement::None
        };
        let variance = match generic.variance {
            VarianceTag::Invariant => compiler_ir::Variance::Invariant,
            VarianceTag::Out => compiler_ir::Variance::Covariant,
            VarianceTag::In => compiler_ir::Variance::Contravariant,
        };
        facts
            .push_type_parameter_with_bounds(
                generic.name.bytes,
                &bounds,
                None,
                compiler_ir::ExtensionTypeParameterKind::Type {
                    inference: compiler_ir::TypeParameterInference::Ordinary,
                },
                variance,
                compiler_ir::TypeParameterRequirements {
                    primary,
                    constructor: generic.constructor,
                    allows_ref_like: generic.allows_ref_like,
                },
            )
            .map_err(lane_terminal)?;
    }
    extension.constraints = TypeParameterListId::new(start);

    // Attributes: one interned spelling per applied attribute row.
    let mut atoms = Vec::new();
    let raw_coordinate = u32::try_from(coordinate).map_err(|_| {
        terminal(ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::Declaration,
            observed: coordinate as u64,
        })
    })?;
    for (_declaration, spelling) in attributes
        .iter()
        .filter(|(declaration, _)| *declaration == raw_coordinate)
    {
        if atoms.len() >= MAX_REF_LIST_ELEMENTS {
            return Err(terminal(ProjectionFault::AttributeCapacity {
                spellings: u32::try_from(atoms.len() + 1).map_err(|_| {
                    terminal(ProjectionFault::IndexCapacity {
                        phase: CSharpProjectionIndexPhase::Attribute,
                        observed: (atoms.len() + 1) as u64,
                    })
                })?,
            }));
        }
        let atom = facts.intern_atom(spelling).map_err(lane_terminal)?;
        atoms.push(atom);
    }
    extension.attributes = facts.intern_atom_list(&atoms).map_err(lane_terminal)?;

    // XML provenance: the documented file travels as a provisional atom with
    // the comment's byte span; admission rewrites the coordinate.
    if let Some(doc) = declared
        .doc
        .and_then(|doc| usize::try_from(doc).ok())
        .and_then(|index| image.docs().nth(index))
        .and_then(|row| row.ok())
    {
        let provisional = facts.intern_atom(doc.file.bytes).map_err(lane_terminal)?;
        extension.xml_provenance = SourceSpan::new(AtomId::new(provisional), doc.start, doc.end);
    }
    Ok(extension)
}

/// Bounded attribute index grouped by declaration coordinate.
fn attribute_index<'source>(
    image: &CSharpImage<'source>,
) -> Result<Vec<(u32, &'source [u8])>, ProjectionFault> {
    let mut rows = Vec::new();
    for attribute in image.attributes() {
        let attribute = attribute.map_err(ProjectionFault::Image)?;
        rows.push((attribute.declaration, attribute.spelling.bytes));
    }
    Ok(rows)
}

/// Pushes one compiler-resolved occurrence: an in-image target folds to a
/// local ordinal, every other Roslyn-resolved target to a nuget-namespaced
/// foreign key, both at oracle confidence and owner-relative spans.
fn push_occurrence<'source>(
    facts: &mut FactSet<'source>,
    image: &CSharpImage<'source>,
    ordinals: &Ordinals,
    reference_index: u32,
    reference: &ResolvedReference<'source>,
) -> Result<(), ProjectionFault> {
    let owner_coordinate = usize::try_from(reference.owner).map_err(|_| {
        ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::Reference,
            observed: reference.owner as u64,
        }
    })?;
    let owner = ordinals
        .lookup(owner_coordinate)
        .ok_or(ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::Reference,
            observed: reference.owner as u64,
        })?;
    let owner_start = image
        .declaration(reference.owner)
        .map_err(ProjectionFault::Image)?
        .decl_start;
    if reference.start < owner_start {
        // The reference precedes its owner's declaration start; the lane's
        // owner-relative span cannot host it and none is synthesized.
        return Err(ProjectionFault::OwnerOrder {
            owner_start,
            reference_start: reference.start,
        });
    }
    let start = reference.start - owner_start;
    let end = reference.end - owner_start;
    let span = RelSpan::new(start, end).map_err(|_| ProjectionFault::OwnerOrder {
        owner_start,
        reference_start: reference.start,
    })?;
    let target = match reference.target {
        Some(coordinate) => {
            let ordinal = ordinals
                .lookup(usize::try_from(coordinate).map_err(|_| {
                    ProjectionFault::IndexCapacity {
                        phase: CSharpProjectionIndexPhase::Reference,
                        observed: coordinate as u64,
                    }
                })?)
                .ok_or(ProjectionFault::IndexCapacity {
                    phase: CSharpProjectionIndexPhase::Reference,
                    observed: coordinate as u64,
                })?;
            OccurrenceTarget::Local(EntityId::new(ordinal))
        }
        None => {
            // Image atoms are UTF-8-validated at open, so the spelling has a
            // string domain; the key keeps the written spelling as both path
            // and display.
            let spelling =
                str::from_utf8(reference.spelling.bytes)
                    .map_err(|_| ProjectionFault::Foreign { reference: reference_index })?;
            let key = ForeignKey::new(
                ForeignOrigin::Universe {
                    ecosystem: ECOSYSTEM_STR,
                },
                spelling,
                spelling,
                foreign_kind(reference.kind),
            )
            .map_err(|_| ProjectionFault::Foreign {
                reference: reference_index,
            })?;
            OccurrenceTarget::Foreign(key)
        }
    };
    facts
        .push_occurrence(
            owner,
            Occurrence {
                target,
                kind: reference_kind(reference.kind),
                confidence: OccurrenceConfidence::Oracle,
                span,
            },
        )
        .map_err(|_| ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::Reference,
            observed: owner as u64,
        })?;
    Ok(())
}

/// The lane's reference-kind cell for one image reference class. The lane's
/// closed lattice and the image's closed vocabulary share the four expression
/// classes, so no two of those collapse and none is unreachable. An explicit
/// interface implementation's binding has no implementation class in the
/// lane's closed lattice; it lands as `MethodCall` — the class an invocation
/// through the implemented interface member carries — at oracle confidence,
/// so consumers can still resolve every implementation of one interface
/// member from the occurrence plane alone.
const fn reference_kind(kind: ReferenceTag) -> ReferenceKind {
    match kind {
        ReferenceTag::Invocation | ReferenceTag::InterfaceImplementation => {
            ReferenceKind::MethodCall
        }
        ReferenceTag::ObjectCreation => ReferenceKind::TypeReference,
        ReferenceTag::MemberAccess => ReferenceKind::FieldAccess,
        ReferenceTag::UsingDirective => ReferenceKind::Import,
    }
}

/// The foreign declaration kind hinted by one image reference class. An
/// implementation binding resolves to a method-shaped interface member.
const fn foreign_kind(kind: ReferenceTag) -> Option<EntityKind> {
    match kind {
        ReferenceTag::Invocation | ReferenceTag::InterfaceImplementation => {
            Some(EntityKind::Function)
        }
        ReferenceTag::ObjectCreation => Some(EntityKind::Record),
        ReferenceTag::MemberAccess => Some(EntityKind::Field),
        ReferenceTag::UsingDirective => Some(EntityKind::Module),
    }
}

/// Streams one declaration's XML documentation into the documentation lane as
/// text runs, inline code, and links to local or foreign declarations.
fn push_doc<'source>(
    facts: &mut FactSet<'source>,
    names: &Names<'source>,
    ordinals: &Ordinals,
    doc: &compiler_languages_csharp::Doc<'source>,
) -> Result<(), ProjectionFault> {
    let coordinate = usize::try_from(doc.declaration).map_err(|_| {
        ProjectionFault::IndexCapacity {
            phase: CSharpProjectionIndexPhase::Documentation,
            observed: doc.declaration as u64,
        }
    })?;
    let Some(owner) = ordinals.lookup(coordinate) else {
        return Ok(());
    };
    let Some(inner) = summary_inner(doc.xml.bytes) else {
        return Ok(());
    };
    for fragment in summary_fragments(names, ordinals, inner) {
        facts
            .push_doc(owner, fragment)
            .map_err(|_| ProjectionFault::IndexCapacity {
                phase: CSharpProjectionIndexPhase::Documentation,
                observed: owner as u64,
            })?;
    }
    Ok(())
}

/// Locates the first `<summary>` element's borrowed inner bytes.
fn summary_inner(xml: &[u8]) -> Option<&[u8]> {
    let opener = find(xml, b"<summary")?;
    let after_open = opener + b"<summary".len();
    let close_angle = xml
        .get(after_open..)?
        .iter()
        .position(|byte| *byte == b'>')?
        + after_open;
    let inner_start = close_angle.checked_add(1)?;
    let closer = find(xml.get(inner_start..)?, b"</summary>")? + inner_start;
    xml.get(inner_start..closer)
}

/// Splits one `<summary>` inner body into borrowed fragments: prose text,
/// `<c>` inline code, `<see cref>` links, and `<paramref>` names, with soft
/// breaks at `<para>` boundaries.
fn summary_fragments<'source>(
    names: &Names<'source>,
    ordinals: &Ordinals,
    inner: &'source [u8],
) -> Vec<DocFragmentInput<'source>> {
    let mut fragments = Vec::new();
    let mut cursor = 0usize;
    while cursor < inner.len() {
        let rest = inner.get(cursor..).unwrap_or(&[]);
        let Some(at) = rest.iter().position(|byte| *byte == b'<') else {
            push_text(&mut fragments, rest);
            break;
        };
        push_text(&mut fragments, rest.get(..at).unwrap_or(&[]));
        let tag = rest.get(at..).unwrap_or(&[]);
        cursor += at + tag_fragments(names, ordinals, tag, &mut fragments);
    }
    fragments
}

/// Emits one borrowed text fragment when the trimmed bytes are non-empty.
fn push_text<'source>(fragments: &mut Vec<DocFragmentInput<'source>>, bytes: &'source [u8]) {
    let trimmed = trim(bytes);
    if !trimmed.is_empty() {
        fragments.push(DocFragmentInput::Text(trimmed));
    }
}

/// Consumes one XML tag at the front of `tag`, emitting the fragment it
/// denotes, and returns the byte length consumed. Unknown tags stay
/// transparent: only their markup is skipped, so their inner text still
/// reaches the prose fragments.
fn tag_fragments<'source>(
    names: &Names<'source>,
    ordinals: &Ordinals,
    tag: &'source [u8],
    fragments: &mut Vec<DocFragmentInput<'source>>,
) -> usize {
    if tag.starts_with(b"<!--") {
        return find(tag, b"-->").map_or(tag.len(), |at| at + 3);
    }
    if !tag.starts_with(b"<") {
        return 1;
    }
    let rest = tag.get(1..).unwrap_or(&[]);
    let name_end = rest
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/')
        .unwrap_or(rest.len());
    let name = rest.get(..name_end).unwrap_or(&[]);
    let body_end = find(rest, b">").map_or(rest.len(), |at| at + 1);
    if rest.starts_with(b"/") {
        // A close tag: paragraph closers break, every other stays invisible.
        if name == b"para" {
            fragments.push(DocFragmentInput::SoftBreak);
        }
        return body_end + 1;
    }
    let self_closing = rest
        .get(..body_end.saturating_sub(1))
        .is_some_and(|body| body.ends_with(b"/"));
    let open_tag = rest.get(..body_end).unwrap_or(&[]);
    match name {
        b"para" => fragments.push(DocFragmentInput::SoftBreak),
        b"c" => {
            let after_body = rest.get(body_end..).unwrap_or(&[]);
            let inner_end = find(after_body, b"</c>").map_or(after_body.len(), |at| at);
            let inner = after_body.get(..inner_end).unwrap_or(&[]);
            if !inner.is_empty() {
                fragments.push(DocFragmentInput::Code(inner));
            }
            return 1 + body_end + inner_end + usize::from(!inner.is_empty()) * b"</c>".len();
        }
        b"see" | b"seealso" => {
            let label = if self_closing {
                None
            } else {
                let after_body = rest.get(body_end..).unwrap_or(&[]);
                let close = find(after_body, &close_of(name)).map_or(after_body.len(), |at| at);
                let inner = trim(after_body.get(..close).unwrap_or(&[]));
                Some((inner, close + close_of(name).len()))
            };
            if let Some(link) = see_link(
                names,
                ordinals,
                open_tag,
                label.and_then(
                    |(inner, _)| {
                        if inner.is_empty() { None } else { Some(inner) }
                    },
                ),
            ) {
                fragments.push(link);
            }
            return match label {
                Some((_, consumed)) => 1 + body_end + consumed,
                None => 1 + body_end,
            };
        }
        b"paramref" => {
            if let Some(name) = attribute_value(open_tag, b"name") {
                fragments.push(DocFragmentInput::Text(name));
            }
        }
        _ => {}
    }
    body_end + 1
}

/// The close tag of one element name.
const fn close_of(name: &[u8]) -> &[u8] {
    match name {
        b"c" => b"</c>",
        b"see" => b"</see>",
        b"seealso" => b"</seealso>",
        b"para" => b"</para>",
        _ => b">",
    }
}

/// Parses one `<see cref="T:X" />` open tag into a link fragment whose target
/// resolves locally when the cref names an in-image declaration. A labelled
/// form uses its inner text as the link label.
fn see_link<'source>(
    names: &Names<'source>,
    ordinals: &Ordinals,
    tag: &'source [u8],
    label: Option<&'source [u8]>,
) -> Option<DocFragmentInput<'source>> {
    let cref = attribute_value(tag, b"cref")?;
    let body = cref_body(cref);
    let target = match names
        .lookup_link(body)
        .and_then(|coordinate| usize::try_from(coordinate.0).ok())
        .and_then(|coordinate| ordinals.lookup(coordinate))
    {
        Some(ordinal) => DocLinkTarget::Local(EntityId::new(ordinal)),
        None => DocLinkTarget::Foreign {
            ecosystem: ECOSYSTEM,
            path: body,
        },
    };
    Some(DocFragmentInput::Link {
        label: label.unwrap_or(body),
        target,
    })
}

/// Strips the documentation-id prefix and argument list from one cref.
fn cref_body(cref: &[u8]) -> &[u8] {
    let without_prefix = match cref.iter().position(|byte| *byte == b':') {
        Some(at) if at <= 2 => cref.get(at + 1..).unwrap_or(cref),
        _ => cref,
    };
    match without_prefix.iter().position(|byte| *byte == b'(') {
        Some(at) => without_prefix.get(..at).unwrap_or(without_prefix),
        None => without_prefix,
    }
}

/// Borrows one XML attribute value from a complete open tag.
fn attribute_value<'tag>(tag: &'tag [u8], name: &[u8]) -> Option<&'tag [u8]> {
    let mut needle = name.to_vec();
    needle.extend_from_slice(b"=\"");
    let at = find(tag, &needle)?;
    let value_start = at + needle.len();
    let rest = tag.get(value_start..)?;
    let close = rest.iter().position(|byte| *byte == b'"')?;
    rest.get(..close)
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
    use compiler_ir::{
        AnnotationKind, CSharpFacts, DocFragmentInput, DocLinkTarget, EntityKind, ForeignOrigin,
        FragmentView, LanguageExtensionWireFact, NominalRef, OccurrenceTarget, PrimitiveShape,
        SemanticTypeTag, SourceIdentity,
    };
    use compiler_languages_csharp::{ImageError, VarianceTag};
    use compiler_vocabulary::{
        CSharpImageFault, CSharpProjectionFault as PortableCSharpProjectionFault, CSharpVersion,
        CompileRecipeFact, LanguageProfile, LoweringUnsupported, NativeTool, Stage,
    };
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
    use sha2::{Digest, Sha256};

    use super::{CSharpCollectError, ProjectionFault, collect, terminal};
    use crate::lower::{FactSet, MAX_EMISSION_FACTS, admit};

    const HEADER_BYTES: usize = 256;
    const DIRECTORY_OFFSET: usize = 48;
    const DIRECTORY_ENTRY_BYTES: usize = 16;
    const IMAGE_DIGEST_OFFSET: usize = 224;
    const ABSENT: u32 = u32::MAX;
    const DIGEST_DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v3\0";

    /// Wire kind cells reused by fixture rows.
    const KIND_NAMED: u8 = 1;
    const KIND_TUPLE: u8 = 5;
    const KIND_FIELD: u8 = 9;
    const KIND_METHOD: u8 = 15;
    const KIND_OPERATOR: u8 = 16;
    const NULL_NONE: u8 = 0;
    const NULL_ANNOTATED: u8 = 1;
    const NULL_NOT_ANNOTATED: u8 = 2;
    const PARTIAL_DEFINITION: u8 = 1;
    const PARTIAL_IMPLEMENTATION: u8 = 2;
    const REF_VALUE: u8 = 0;
    const REF_REF: u8 = 2;
    const REF_INVOCATION: u8 = 1;
    const REF_IMPL_BINDING: u8 = 5;
    const FLAG_EXPLICIT_INTERFACE: u8 = 0x10;
    const GENERIC_REFERENCE_TYPE: u8 = 0x1;
    const GENERIC_VALUE_TYPE: u8 = 0x2;
    const GENERIC_UNMANAGED: u8 = 0x8;
    const GENERIC_CONSTRUCTOR: u8 = 0x10;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("collection rejected the authority image: {0:?}")]
        Collect(CSharpCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(crate::lower::AdmissionFault),
        #[error("fragment validation rejected the bytes: {0:?}")]
        Validate(compiler_ir::FragmentError),
        #[error("expected {0}")]
        Missing(&'static str),
        #[error("committed bytes changed")]
        Tail,
        #[error("fixture scalar conversion failed")]
        Num,
    }

    impl From<CSharpCollectError> for TestError {
        fn from(error: CSharpCollectError) -> Self {
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

    #[test]
    fn image_projection_retains_exact_name_range_operands() {
        let error = terminal(ProjectionFault::Image(ImageError::NameRange {
            index: 7,
            offset: 11,
            length: 13,
            atom_bytes: 17,
        }));
        let CSharpCollectError::Lowering(LoweringUnsupported::CSharpProjection {
            fault: PortableCSharpProjectionFault::Image {
                cause: CSharpImageFault::NameRange {
                    index,
                    offset,
                    length,
                    atom_bytes,
                },
            },
        }) = error
        else {
            panic!("image fault lost its closed C# projection operands");
        };
        assert_eq!((index, offset, length, atom_bytes), (7, 11, 13, 17));
    }

    #[derive(Clone)]
    struct ParamRow {
        ty: u32,
        name: usize,
        ref_kind: u8,
    }

    #[derive(Clone)]
    struct GenericRow {
        name: usize,
        constraints: Vec<u32>,
        variance: u8,
        requirements: u8,
    }

    #[derive(Clone)]
    struct Decl {
        kind: u8,
        flags: u8,
        partial: u8,
        name: usize,
        qualified: Option<usize>,
        owner: Option<u32>,
        declared_type: Option<u32>,
        decl_start: u32,
        name_start: u32,
        name_end: u32,
        params: Vec<ParamRow>,
        generics: Vec<GenericRow>,
        doc: Option<usize>,
    }

    impl Decl {
        fn new(kind: u8, name: usize, qualified: Option<usize>) -> Self {
            Self {
                kind,
                flags: 0,
                partial: 0,
                name,
                qualified,
                owner: None,
                declared_type: None,
                decl_start: 0,
                name_start: 0,
                name_end: 0,
                params: Vec::new(),
                generics: Vec::new(),
                doc: None,
            }
        }

        fn at(mut self, decl_start: u32, name_start: u32, name_end: u32) -> Self {
            self.decl_start = decl_start;
            self.name_start = name_start;
            self.name_end = name_end;
            self
        }

        fn of_kind(mut self, kind: u8) -> Self {
            self.kind = kind;
            self
        }

        fn typed(mut self, ty: u32) -> Self {
            self.declared_type = Some(ty);
            self
        }

        fn owned_by(mut self, owner: u32) -> Self {
            self.owner = Some(owner);
            self
        }

        fn flagged(mut self, flags: u8) -> Self {
            self.flags = flags;
            self
        }

        fn with_param(mut self, param: ParamRow) -> Self {
            self.params.push(param);
            self
        }

        fn generic_with_requirements(
            mut self,
            name: usize,
            constraints: Vec<u32>,
            variance: VarianceTag,
            requirements: u8,
        ) -> Self {
            self.generics.push(GenericRow {
                name,
                constraints,
                variance: variance as u8,
                requirements,
            });
            self
        }
    }

    #[derive(Clone)]
    struct TypeRow {
        kind: u8,
        nullable: u8,
        has_return: u8,
        spelling: Option<usize>,
        children: Vec<(Option<usize>, u32)>,
    }

    #[derive(Clone)]
    struct DocRow {
        declaration: u32,
        file: usize,
        start: u32,
        end: u32,
        xml: usize,
    }

    #[derive(Clone)]
    struct RefRow {
        owner: u32,
        target: Option<u32>,
        spelling: usize,
        file: usize,
        start: u32,
        end: u32,
        kind: u8,
    }

    /// One Roslyn authority image under construction, encoded exactly like
    /// the vendored producer: canonical sections, fixed directory, and the
    /// domain-separated checksum over header and body.
    #[derive(Clone, Default)]
    struct Fixture {
        atoms: Vec<Vec<u8>>,
        declarations: Vec<Decl>,
        types: Vec<TypeRow>,
        attributes: Vec<(u32, usize)>,
        docs: Vec<DocRow>,
        references: Vec<RefRow>,
    }

    impl Fixture {
        fn atom(&mut self, text: &[u8]) -> usize {
            self.atoms.push(text.to_vec());
            self.atoms.len() - 1
        }

        /// Locates one name inside the bound source, proving the span.
        fn span_of<'text>(source: &'text [u8], name: &[u8]) -> (u32, u32) {
            let at = source
                .windows(name.len())
                .position(|window| window == name)
                .unwrap_or(0);
            (
                u32::try_from(at).unwrap_or(u32::MAX),
                u32::try_from(at + name.len()).unwrap_or(u32::MAX),
            )
        }

        fn named(&mut self, spelling: &[u8]) -> u32 {
            let atom = self.atom(spelling);
            self.types.push(TypeRow {
                kind: KIND_NAMED,
                nullable: NULL_NONE,
                has_return: 0,
                spelling: Some(atom),
                children: Vec::new(),
            });
            u32::try_from(self.types.len() - 1).unwrap_or(u32::MAX)
        }

        /// One namespace-scoped class whose identifier occupies its source
        /// range, with its recursive nominal type row, already pushed.
        fn class(&mut self, qualified: &[u8], source: &[u8]) -> u32 {
            let simple = match qualified.iter().rposition(|byte| *byte == b'.') {
                Some(at) => &qualified[at + 1..],
                None => qualified,
            };
            let qualified_atom = self.atom(qualified);
            let name_atom = self.atom(simple);
            let ty = self.named(qualified);
            let (name_start, name_end) = Self::span_of(source, simple);
            let declaration = Decl::new(1, name_atom, Some(qualified_atom))
                .typed(ty)
                .at(name_start, name_start, name_end);
            self.declarations.push(declaration);
            u32::try_from(self.declarations.len() - 1).unwrap_or(u32::MAX)
        }

        /// One field declaration row, not yet pushed.
        fn field(&mut self, owner: u32, name: &[u8], ty: u32, source: &[u8]) -> Decl {
            let name_atom = self.atom(name);
            let (name_start, name_end) = Self::span_of(source, name);
            Decl::new(KIND_FIELD, name_atom, None)
                .typed(ty)
                .owned_by(owner)
                .at(name_start, name_start, name_end)
        }

        /// One method declaration row, not yet pushed. The declared type is
        /// the method's return type coordinate, when one exists.
        fn method(
            &mut self,
            owner: u32,
            name: &[u8],
            declared_type: Option<u32>,
            source: &[u8],
        ) -> Decl {
            let name_atom = self.atom(name);
            let (name_start, name_end) = Self::span_of(source, name);
            let decl_start = name_start;
            let declared = Decl::new(KIND_METHOD, name_atom, None)
                .owned_by(owner)
                .at(decl_start, name_start, name_end);
            match declared_type {
                Some(ty) => declared.typed(ty),
                None => declared,
            }
        }

        fn encode(&self, source: &[u8]) -> Result<Vec<u8>, TestError> {
            let cell = |value: usize| u32::try_from(value).map_err(|_| TestError::Num);
            let mut atoms = Vec::new();
            let mut atom_bytes = Vec::new();
            for text in &self.atoms {
                atoms.extend_from_slice(&cell(atom_bytes.len())?.to_le_bytes());
                atoms.extend_from_slice(&cell(text.len())?.to_le_bytes());
                atom_bytes.extend_from_slice(text);
            }
            let mut params = Vec::new();
            let mut tparam_rows = Vec::new();
            let mut tconstraint_rows = Vec::new();
            let mut declarations = Vec::new();
            for row in &self.declarations {
                let param_start = cell(params.len() / 24)?;
                for param in &row.params {
                    params.extend_from_slice(&param.ty.to_le_bytes());
                    params.extend_from_slice(&cell(param.name)?.to_le_bytes());
                    params.push(param.ref_kind);
                    params.push(0);
                    params.extend_from_slice(&0_u16.to_le_bytes());
                    params.extend_from_slice(&ABSENT.to_le_bytes());
                    params.extend_from_slice(&cell(param.name)?.to_le_bytes());
                    params.extend_from_slice(&cell(param.name)?.to_le_bytes());
                }
                let generic_start = cell(tparam_rows.len() / 12)?;
                for generic in &row.generics {
                    let constraint_start = cell(tconstraint_rows.len() / 4)?;
                    for constraint in &generic.constraints {
                        tconstraint_rows.extend_from_slice(&constraint.to_le_bytes());
                    }
                    tparam_rows.extend_from_slice(&cell(generic.name)?.to_le_bytes());
                    tparam_rows.extend_from_slice(&constraint_start.to_le_bytes());
                    tparam_rows.extend_from_slice(
                        &u16::try_from(generic.constraints.len())
                            .map_err(|_| TestError::Num)?
                            .to_le_bytes(),
                    );
                    tparam_rows.push(generic.variance);
                    tparam_rows.push(generic.requirements);
                }
                declarations.push(row.kind);
                declarations.push(row.flags);
                declarations.push(row.partial);
                declarations.push(0);
                declarations.extend_from_slice(&cell(row.name)?.to_le_bytes());
                declarations.extend_from_slice(
                    &row.qualified
                        .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                        .to_le_bytes(),
                );
                declarations.extend_from_slice(&row.owner.unwrap_or(ABSENT).to_le_bytes());
                declarations.extend_from_slice(&row.declared_type.unwrap_or(ABSENT).to_le_bytes());
                declarations.extend_from_slice(&row.decl_start.to_le_bytes());
                declarations.extend_from_slice(&row.name_start.to_le_bytes());
                declarations.extend_from_slice(&row.name_end.to_le_bytes());
                declarations.extend_from_slice(&param_start.to_le_bytes());
                declarations.extend_from_slice(
                    &u16::try_from(row.params.len())
                        .map_err(|_| TestError::Num)?
                        .to_le_bytes(),
                );
                declarations.extend_from_slice(&generic_start.to_le_bytes());
                declarations.extend_from_slice(
                    &u16::try_from(row.generics.len())
                        .map_err(|_| TestError::Num)?
                        .to_le_bytes(),
                );
                declarations.extend_from_slice(
                    &row.doc
                        .map_or(ABSENT, |d| cell(d).unwrap_or(ABSENT))
                        .to_le_bytes(),
                );
            }
            let mut type_rows = Vec::new();
            let mut type_children = Vec::new();
            for row in &self.types {
                let child_start = cell(type_children.len() / 8)?;
                for (name, ty) in &row.children {
                    type_children.extend_from_slice(
                        &name
                            .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                            .to_le_bytes(),
                    );
                    type_children.extend_from_slice(&ty.to_le_bytes());
                }
                type_rows.push(row.kind);
                type_rows.push(row.nullable);
                type_rows.push(0);
                type_rows.push(row.has_return);
                type_rows.extend_from_slice(
                    &row.spelling
                        .map_or(ABSENT, |a| cell(a).unwrap_or(ABSENT))
                        .to_le_bytes(),
                );
                type_rows.extend_from_slice(&child_start.to_le_bytes());
                type_rows.extend_from_slice(&cell(row.children.len())?.to_le_bytes());
            }
            let mut attribute_rows = Vec::new();
            for (declaration, spelling) in &self.attributes {
                attribute_rows.extend_from_slice(&declaration.to_le_bytes());
                attribute_rows.extend_from_slice(&cell(*spelling)?.to_le_bytes());
            }
            let mut doc_rows = Vec::new();
            for row in &self.docs {
                doc_rows.extend_from_slice(&row.declaration.to_le_bytes());
                doc_rows.extend_from_slice(&cell(row.file)?.to_le_bytes());
                doc_rows.extend_from_slice(&row.start.to_le_bytes());
                doc_rows.extend_from_slice(&row.end.to_le_bytes());
                doc_rows.extend_from_slice(&cell(row.xml)?.to_le_bytes());
            }
            let mut reference_rows = Vec::new();
            for row in &self.references {
                reference_rows.extend_from_slice(&row.owner.to_le_bytes());
                reference_rows.extend_from_slice(&row.target.unwrap_or(ABSENT).to_le_bytes());
                reference_rows.extend_from_slice(&cell(row.spelling)?.to_le_bytes());
                reference_rows.extend_from_slice(&cell(row.file)?.to_le_bytes());
                reference_rows.extend_from_slice(&row.start.to_le_bytes());
                reference_rows.extend_from_slice(&row.end.to_le_bytes());
                reference_rows.push(row.kind);
                reference_rows.extend_from_slice(&[0; 3]);
            }
            let sections = [
                atoms,
                atom_bytes,
                declarations,
                params,
                tparam_rows,
                tconstraint_rows,
                type_rows,
                type_children,
                attribute_rows,
                doc_rows,
                reference_rows,
            ];
            let row_bytes = [8_u16, 1, 48, 24, 12, 4, 16, 8, 8, 20, 28];
            let mut image = vec![0_u8; HEADER_BYTES];
            image[..4].copy_from_slice(b"NCAI");
            image[4..6].copy_from_slice(&3_u16.to_le_bytes());
            image[6..8].copy_from_slice(
                &u16::try_from(HEADER_BYTES)
                    .map_err(|_| TestError::Num)?
                    .to_le_bytes(),
            );
            let body = sections.iter().map(Vec::len).sum::<usize>();
            image[8..12].copy_from_slice(
                &u32::try_from(body)
                    .map_err(|_| TestError::Num)?
                    .to_le_bytes(),
            );
            image[12..44].copy_from_slice(Sha256::digest(source).as_slice());
            image[44..46].copy_from_slice(
                &u16::try_from(sections.len())
                    .map_err(|_| TestError::Num)?
                    .to_le_bytes(),
            );
            let mut offset = HEADER_BYTES;
            for (index, section) in sections.iter().enumerate() {
                let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
                image[entry..entry + 2].copy_from_slice(
                    &u16::try_from(index + 1)
                        .map_err(|_| TestError::Num)?
                        .to_le_bytes(),
                );
                image[entry + 2..entry + 4].copy_from_slice(&row_bytes[index].to_le_bytes());
                image[entry + 4..entry + 8].copy_from_slice(
                    &u32::try_from(section.len() / usize::from(row_bytes[index]))
                        .map_err(|_| TestError::Num)?
                        .to_le_bytes(),
                );
                image[entry + 8..entry + 12].copy_from_slice(
                    &u32::try_from(offset)
                        .map_err(|_| TestError::Num)?
                        .to_le_bytes(),
                );
                image[entry + 12..entry + 16].copy_from_slice(
                    &u32::try_from(section.len())
                        .map_err(|_| TestError::Num)?
                        .to_le_bytes(),
                );
                offset += section.len();
            }
            let mut digest = Sha256::new();
            digest.update(DIGEST_DOMAIN);
            digest.update(&image[..IMAGE_DIGEST_OFFSET]);
            for section in &sections {
                digest.update(section);
            }
            image.resize(HEADER_BYTES, 0);
            image[IMAGE_DIGEST_OFFSET..HEADER_BYTES].copy_from_slice(&digest.finalize());
            let mut complete = image;
            for section in &sections {
                complete.extend_from_slice(section);
            }
            Ok(complete)
        }
    }

    /// Lowers one fixture image against its bound source and writes the
    /// committed fragment, proving the untouched output tail stayed unchanged.
    fn lower(fix: &Fixture, source: &[u8]) -> Result<Vec<u8>, TestError> {
        let image = fix.encode(source)?;
        let mut facts = FactSet::new();
        collect(source, &image, &mut facts)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).map_err(|_| TestError::Num)?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::CSharp(CSharpVersion::CSharp14),
            Stage::LowerIr,
            NativeTool::CSharpCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"csharp-authority-toolchain"),
        );
        let mut output = vec![0xa5_u8; 4 * 1024 * 1024];
        let length = admit(&facts, identity, recipe, recipe.profile, &mut output)?.len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    /// Decodes one entity row as its name bytes and kind.
    fn entity_rows<'fragment>(
        view: &FragmentView<'fragment>,
    ) -> Vec<(&'fragment [u8], EntityKind)> {
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        view.entities()
            .map(|entity| {
                (
                    atoms.get(entity.name.raw as usize).copied().unwrap_or(&[]),
                    entity.kind,
                )
            })
            .collect()
    }

    /// Decodes one type-fact row by its lane ordinal.
    fn row<'fragment>(
        view: &FragmentView<'fragment>,
        ordinal: usize,
    ) -> Result<compiler_ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        cursor
            .nth(ordinal)
            .ok_or(TestError::Missing("type row"))?
            .map_err(|_| TestError::Missing("type row decode"))
    }

    /// Decodes the C# extension row of one entity from the raw
    /// language-extension section: a 16-byte header, seven 20-byte plane
    /// directories (C# is the second), then the row table and fact pool.
    fn csharp_extension(view: &FragmentView<'_>, ordinal: usize) -> Result<CSharpFacts, TestError> {
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
        let directory = 16 + 20;
        let rows = usize::try_from(word(directory + 4)?).map_err(|_| TestError::Num)?;
        let facts_count = word(directory + 8)?;
        let offset = usize::try_from(word(directory + 12)?).map_err(|_| TestError::Num)?;
        if facts_count == 0 {
            return Err(TestError::Missing("extension facts"));
        }
        let fact_ordinal = word(offset + ordinal * 4)?;
        if fact_ordinal == u32::MAX {
            return Err(TestError::Missing("extension row"));
        }
        let at =
            offset + rows * 4 + usize::try_from(fact_ordinal).map_err(|_| TestError::Num)? * 36;
        CSharpFacts::decode(payload, at).ok_or(TestError::Missing("extension decode"))
    }

    #[test]
    fn empty_image_admits_the_current_schema_fragment_without_semantic_sections()
    -> Result<(), TestError> {
        let fix = Fixture::default();
        let bytes = lower(&fix, b"")?;
        let view = FragmentView::validate(&bytes)?;
        if view.type_facts().is_some()
            || view.occurrences().is_some()
            || view.docs().is_some()
            || view.language_extension_payload().is_some()
        {
            return Err(TestError::Missing("absent semantic sections"));
        }
        Ok(())
    }

    #[test]
    fn one_class_commits_its_recursive_self_nominal_and_extension_row() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        let _ = fix.class(b"Demo.Widget", b"class Widget {}");
        let source = b"class Widget {}";
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 1 || entities[0] != (&b"Widget"[..], EntityKind::Record) {
            return Err(TestError::Missing("single record fact"));
        }
        let only = row(&view, 0)?;
        if only.owner.raw != 0
            || only.record.tag != SemanticTypeTag::Nominal
            || only.record.nominal != Some(NominalRef::Local(compiler_ir::EntityId::new(0)))
        {
            return Err(TestError::Missing("recursive self nominal"));
        }
        if row(&view, 1).is_ok() {
            return Err(TestError::Missing("exact fact count"));
        }
        let extension = csharp_extension(&view, 0)?;
        if extension.xml_provenance.is_some()
            || extension.partial != compiler_ir::CSharpPartialRole::None
        {
            return Err(TestError::Missing("empty extension cells"));
        }
        Ok(())
    }

    #[test]
    fn recursive_field_targets_the_strictly_backward_declaration_ordinal() -> Result<(), TestError>
    {
        let source = b"class Node { Node next; }";
        let mut fix = Fixture::default();
        let node = fix.class(b"demo.Node", source);
        let node_type = fix.named(b"demo.Node");
        let field = fix.field(node, b"next", node_type, source);
        fix.declarations.push(field);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 class Node, 1 field next.
        let field = row(&view, 1)?;
        if field.record.nominal != Some(NominalRef::Local(compiler_ir::EntityId::new(0))) {
            return Err(TestError::Missing("backward field nominal"));
        }
        // Falsifier: retyping the field away from the declaration changes the
        // committed bytes into a typed unknown row.
        let mut mutated = fix.clone();
        mutated.types[node_type as usize].spelling = Some(mutated.atom(b"demo.Other"));
        let other = lower(&mutated, source)?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let field = row(&view, 1)?;
        if field.record.tag != SemanticTypeTag::Unknown
            || field.record.payload0 != u32::from(compiler_ir::TypeReason::UnresolvedExternal)
            || field.record.text != Some(b"demo.Other".as_slice())
        {
            return Err(TestError::Missing("typed unknown for foreign nominal"));
        }
        Ok(())
    }

    #[test]
    fn exact_widths_map_the_closed_primitive_spellings() -> Result<(), TestError> {
        let source = b"class P { int a0; byte a1; nint a2; double a3; bool a4; string a5; }";
        let mut fix = Fixture::default();
        let widget = fix.class(b"demo.P", source);
        let spellings: [(&[u8], &[u8], u32, u32); 6] = [
            (
                b"System.Int32",
                b"a0",
                PrimitiveShape::Integer as u32,
                (32 << 1) | 1,
            ),
            (
                b"System.Byte",
                b"a1",
                PrimitiveShape::Integer as u32,
                8 << 1,
            ),
            (
                b"System.IntPtr",
                b"a2",
                PrimitiveShape::NativeSignedInteger as u32,
                0,
            ),
            (b"System.Double", b"a3", PrimitiveShape::Float as u32, 64),
            (b"System.Boolean", b"a4", PrimitiveShape::Bool as u32, 0),
            (b"System.String", b"a5", PrimitiveShape::Str as u32, 0),
        ];
        for (spelling, name, _shape, _payload1) in spellings {
            let ty = fix.named(spelling);
            fix.types[ty as usize].nullable = NULL_NONE;
            let field = fix.field(widget, name, ty, source);
            fix.declarations.push(field);
        }
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 class, then one carrier fact per field. Primitive field
        // types attach directly to their facts, so the lane holds exactly
        // seven records in fact order with no anonymous rows.
        for (index, (spelling, name, shape, payload1)) in spellings.iter().enumerate() {
            let field = row(&view, 1 + index)?;
            if field.record.tag != SemanticTypeTag::Primitive
                || field.record.payload0 != *shape
                || field.record.payload1 != *payload1
            {
                return Err(TestError::Missing("exact width cells"));
            }
            let _ = (spelling, name);
        }
        Ok(())
    }

    const fn tuple_row_placeholder() -> u32 {
        0
    }

    const fn nullable_row_placeholder() -> u32 {
        1
    }

    #[test]
    fn tuple_and_annotated_value_forms_project_their_children() -> Result<(), TestError> {
        let source = b"class P { (int, string Name) pair; int? maybe; }";
        let mut fix = Fixture::default();
        let widget = fix.class(b"demo.P", source);
        let int_row = fix.named(b"System.Int32");
        let string_row = fix.named(b"System.String");
        let label = fix.atom(b"Name");
        let tuple_spelling = fix.atom(b"(int, string Name)");
        let nullable_spelling = fix.atom(b"int?");
        let pair_field = fix.field(widget, b"pair", tuple_row_placeholder(), source);
        let maybe_field = fix.field(widget, b"maybe", nullable_row_placeholder(), source);
        let tuple_row = u32::try_from(fix.types.len()).map_err(|_| TestError::Num)?;
        fix.types.push(TypeRow {
            kind: KIND_TUPLE,
            nullable: NULL_NONE,
            has_return: 0,
            spelling: Some(tuple_spelling),
            children: vec![(None, int_row), (Some(label), string_row)],
        });
        let nullable_row = u32::try_from(fix.types.len()).map_err(|_| TestError::Num)?;
        fix.types.push(TypeRow {
            kind: 4,
            nullable: NULL_NONE,
            has_return: 0,
            spelling: Some(nullable_spelling),
            children: vec![(None, int_row)],
        });
        let pair_field = pair_field.typed(tuple_row);
        let maybe_field = maybe_field.typed(nullable_row);
        fix.declarations.push(pair_field);
        fix.declarations.push(maybe_field);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 class, 1 pair, 2 maybe. Compound children intern one
        // anonymous row per projection — int, string, and the second int —
        // so the lane is [int, string, int, class, pair, maybe].
        let pair = row(&view, 4)?;
        if pair.record.tag != SemanticTypeTag::Tuple || pair.record.children.length != 2 {
            return Err(TestError::Missing("tuple row"));
        }
        let maybe = row(&view, 5)?;
        if maybe.record.tag != SemanticTypeTag::Annotated
            || maybe.record.payload0 != AnnotationKind::NullableValue as u32
            || maybe.record.children.length != 1
        {
            return Err(TestError::Missing("annotated value row"));
        }
        Ok(())
    }

    #[test]
    fn nested_reference_nullability_stays_structural_at_each_type_node() -> Result<(), TestError> {
        let source = b"class Box { string? maybe; string present; }";
        let mut fix = Fixture::default();
        let box_type = fix.class(b"demo.Box", source);
        let nullable = fix.named(b"System.String");
        let nonnullable = fix.named(b"System.String");
        fix.types[nullable as usize].nullable = NULL_ANNOTATED;
        fix.types[nonnullable as usize].nullable = NULL_NOT_ANNOTATED;
        let maybe = fix.field(box_type, b"maybe", nullable, source);
        let present = fix.field(box_type, b"present", nonnullable, source);
        fix.declarations.push(maybe);
        fix.declarations.push(present);

        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // The two primitive child rows are anonymous; class then fields are
        // dense entity rows. Both reference annotations must survive rather
        // than being relegated to the outer C# extension cell.
        let maybe = row(&view, 3)?;
        if maybe.record.tag != SemanticTypeTag::Annotated
            || maybe.record.payload0 != AnnotationKind::NullableReference as u32
        {
            return Err(TestError::Missing("nullable reference annotation"));
        }
        let present = row(&view, 4)?;
        if present.record.tag != SemanticTypeTag::Annotated
            || present.record.payload0 != AnnotationKind::NonNullableReference as u32
        {
            return Err(TestError::Missing("nonnullable reference annotation"));
        }
        Ok(())
    }

    #[test]
    fn generic_application_and_constraint_pool_carry_backward_ordinals() -> Result<(), TestError> {
        let source = b"interface IPart {} interface IOther {} interface Widget<out T, U> {}";
        let mut fix = Fixture::default();
        let part = fix.class(b"demo.IPart", source);
        let other = fix.class(b"demo.IOther", source);
        let widget = fix.class(b"demo.Widget", source);
        let generic_name = fix.atom(b"T");
        let unmanaged_name = fix.atom(b"U");
        let part_type = fix.named(b"demo.IPart");
        let other_type = fix.named(b"demo.IOther");
        let declaration = fix.declarations[widget as usize]
            .clone()
            .generic_with_requirements(
                generic_name,
                vec![part_type, other_type],
                VarianceTag::Out,
                GENERIC_REFERENCE_TYPE | GENERIC_CONSTRUCTOR,
            )
            // Roslyn reports `unmanaged` alongside its implied value-type
            // bit. The shared primary requirement must retain `unmanaged`,
            // not let the weaker implied bit win.
            .generic_with_requirements(
                unmanaged_name,
                Vec::new(),
                VarianceTag::Invariant,
                GENERIC_VALUE_TYPE | GENERIC_UNMANAGED,
            );
        fix.declarations[widget as usize] = declaration;
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 IPart, 1 IOther, 2 Widget.
        let extension = csharp_extension(&view, 2)?;
        // The reopened schema-5 pool owns both the parameter row and its
        // source-ordered bound range; raw byte offsets never stand in for a
        // generic contract.
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Missing("pools"))?
            .ok_or(TestError::Missing("pools"))?;
        let compiler_ir::ReopenedTypeParameterList::Exact(parameters) = pools
            .type_parameter_list(extension.constraints)
            .map_err(|_| TestError::Missing("parameter list"))?
        else {
            return Err(TestError::Missing("exact parameter list"));
        };
        if parameters.length != 2 {
            return Err(TestError::Missing("two generic parameters"));
        }
        let parameter = parameters
            .get(0)
            .map_err(|_| TestError::Missing("parameter"))?;
        if parameter.name != b"T" {
            return Err(TestError::Missing("pooled parameter name"));
        }
        let bounds = pools
            .type_parameter_bounds(parameter)
            .map_err(|_| TestError::Missing("bounds"))?
            .ok_or(TestError::Missing("exact bounds"))?;
        if bounds.length != 2
            || bounds
                .get(0)
                .map_err(|_| TestError::Missing("constraint"))?
                != compiler_ir::DecodedTypeParameterBound::Type(part)
            || bounds
                .get(1)
                .map_err(|_| TestError::Missing("second constraint"))?
                != compiler_ir::DecodedTypeParameterBound::Type(other)
        {
            return Err(TestError::Missing("constraint ordinals"));
        }
        if parameter.semantics
            != (compiler_ir::DecodedTypeParameterSemantics::Exact {
                bounds: compiler_ir::ExtensionTypeParameterBoundRange {
                    start: 0,
                    length: 2,
                },
                variance: compiler_ir::Variance::Covariant,
                kind: compiler_ir::DecodedTypeParameterKind::Type {
                    inference: compiler_ir::TypeParameterInference::Ordinary,
                },
                requirements: compiler_ir::TypeParameterRequirements {
                    primary: compiler_ir::TypeParameterPrimaryRequirement::Reference {
                        nullable: false,
                    },
                    constructor: true,
                    allows_ref_like: false,
                },
            })
        {
            return Err(TestError::Missing(
                "covariant class constructor requirements",
            ));
        }
        let unmanaged = parameters
            .get(1)
            .map_err(|_| TestError::Missing("unmanaged parameter"))?;
        if unmanaged.name != b"U"
            || unmanaged.semantics
                != (compiler_ir::DecodedTypeParameterSemantics::Exact {
                    bounds: compiler_ir::ExtensionTypeParameterBoundRange {
                        start: 2,
                        length: 0,
                    },
                    variance: compiler_ir::Variance::Invariant,
                    kind: compiler_ir::DecodedTypeParameterKind::Type {
                        inference: compiler_ir::TypeParameterInference::Ordinary,
                    },
                    requirements: compiler_ir::TypeParameterRequirements {
                        primary: compiler_ir::TypeParameterPrimaryRequirement::Unmanaged,
                        constructor: false,
                        allows_ref_like: false,
                    },
                })
        {
            return Err(TestError::Missing("unmanaged requirement precedence"));
        }
        Ok(())
    }

    #[test]
    fn foreign_system_types_fold_to_unknowns_and_nuget_occurrences() -> Result<(), TestError> {
        let source = b"class Widget { System.DateTime stamp; }";
        let mut fix = Fixture::default();
        let widget = fix.class(b"demo.Widget", source);
        let stamp = fix.named(b"System.DateTime");
        let field = fix.field(widget, b"stamp", stamp, source);
        fix.declarations.push(field);
        let console = fix.atom(b"System.Console.Beep");
        let file = fix.atom(b"Widget.cs");
        let (start, end) = (10_u32, 29_u32);
        fix.references.push(RefRow {
            owner: widget,
            target: None,
            spelling: console,
            file,
            start,
            end,
            kind: REF_INVOCATION,
        });
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let stamp_row = row(&view, 1)?;
        if stamp_row.record.tag != SemanticTypeTag::Unknown
            || stamp_row.record.payload0 != u32::from(compiler_ir::TypeReason::UnresolvedExternal)
            || stamp_row.record.text != Some(b"System.DateTime".as_slice())
        {
            return Err(TestError::Missing("foreign unknown row"));
        }
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let occurrence = occurrences
            .next()
            .ok_or(TestError::Missing("occurrence"))?
            .map_err(|_| TestError::Missing("occurrence decode"))?;
        let OccurrenceTarget::Foreign(key) = occurrence.occurrence.target else {
            return Err(TestError::Missing("foreign target"));
        };
        if key.path != "System.Console.Beep" {
            return Err(TestError::Missing("foreign path"));
        }
        let ForeignOrigin::Universe { ecosystem } = key.origin else {
            return Err(TestError::Missing("universe origin"));
        };
        if ecosystem != "nuget"
            || occurrence.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle
            || occurrence.occurrence.kind != compiler_ir::ReferenceKind::MethodCall
        {
            return Err(TestError::Missing("nuget method call"));
        }
        // The span is owner-relative: the reference is written after the
        // owning declaration starts.
        if occurrence.occurrence.span.start != start - 6
            || occurrence.occurrence.span.end != end - 6
        {
            return Err(TestError::Missing("owner relative span"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("single occurrence"));
        }
        Ok(())
    }

    #[test]
    fn signatures_lower_parameter_and_result_carriers_with_reference_kinds() -> Result<(), TestError>
    {
        let source =
            b"class Widget { int brew(string count) { return 0; } int brew() { return 1; } }";
        let mut fix = Fixture::default();
        let widget = fix.class(b"demo.Widget", source);
        let int_row = fix.named(b"System.Int32");
        let string_row = fix.named(b"System.String");
        let count = fix.atom(b"count");
        let first = fix
            .method(widget, b"brew", Some(int_row), source)
            .with_param(ParamRow {
                ty: string_row,
                name: count,
                ref_kind: REF_REF,
            });
        fix.declarations.push(first);
        let second = fix.method(widget, b"brew", Some(int_row), source);
        fix.declarations.push(second);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 class, 1 param carrier, 2 result carrier, 3 first brew,
        // 4 result carrier, 5 second brew. Both overloads keep distinct
        // constructor cells, so neither folds into the other's identity.
        let entities = entity_rows(&view);
        if entities.len() != 6
            || entities[1].1 != EntityKind::Parameter
            || entities[2].1 != EntityKind::Parameter
            || entities[3].1 != EntityKind::Function
            || entities[5].1 != EntityKind::Function
        {
            return Err(TestError::Missing("overload fact set"));
        }
        // Every parameter and result type is primitive, so each carrier's
        // own record rides on the fact; the lane holds the six records in
        // fact order with no anonymous rows.
        let param = row(&view, 1)?;
        if param.record.payload0 != u32::from(PrimitiveShape::Str) {
            return Err(TestError::Missing("string parameter row"));
        }
        let parameter_extension = csharp_extension(&view, 1)?;
        if parameter_extension.reference_kind != compiler_ir::CSharpReferenceKind::Ref {
            return Err(TestError::Missing("ref convention"));
        }
        let method_row = row(&view, 3)?;
        if method_row.record.tag != SemanticTypeTag::FunctionPointer
            || method_row.record.payload1
                != compiler_ir::SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || method_row.record.children.length != 2
        {
            return Err(TestError::Missing("function pointer over carriers"));
        }
        let zero_arity = row(&view, 5)?;
        if zero_arity.record.children.length != 1 {
            return Err(TestError::Missing("zero-arity overload row"));
        }
        // Falsifier: retyping the parameter changes the committed bytes.
        let mut mutated = fix.clone();
        mutated.declarations[1].params[0].ty = int_row;
        let other = lower(&mutated, source)?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    #[test]
    fn async_and_iterator_effects_live_only_in_the_extension_plane() -> Result<(), TestError> {
        let source = b"class Widget { async run() { yield 1; } }";
        let mut fix = Fixture::default();
        let widget = fix.class(b"demo.Widget", source);
        let run = fix.method(widget, b"run", None, source).flagged(0x2 | 0x4);
        fix.declarations.push(run);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let extension = csharp_extension(&view, 1)?;
        if !extension.effects.is_async || !extension.effects.is_iterator {
            return Err(TestError::Missing("async iterator effects"));
        }
        let run = row(&view, 1)?;
        if run.record.tag != SemanticTypeTag::FunctionPointer
            || run.record.payload1 != 0
            || run.record.children.length != 0
        {
            return Err(TestError::Missing("void function pointer"));
        }
        Ok(())
    }

    #[test]
    fn partial_pair_commits_one_type_fact_owned_by_the_definition() -> Result<(), TestError> {
        let source = b"partial class Widget {} partial class Widget {}";
        let mut fix = Fixture::default();
        let first = fix.class(b"demo.Widget", source);
        fix.declarations[first as usize].partial = PARTIAL_DEFINITION;
        let second = fix.class(b"demo.Widget", source);
        fix.declarations[second as usize].partial = PARTIAL_IMPLEMENTATION;
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 1 {
            return Err(TestError::Missing("one owned type fact"));
        }
        let extension = csharp_extension(&view, 0)?;
        if extension.partial != compiler_ir::CSharpPartialRole::Definition {
            return Err(TestError::Missing("definition ownership"));
        }
        Ok(())
    }

    #[test]
    fn xml_provenance_and_summary_fragments_link_local_and_foreign_crefs() -> Result<(), TestError>
    {
        let source = b"interface IPart {} partial class Widget {}";
        let mut fix = Fixture::default();
        let part = fix.class(b"demo.IPart", source);
        let widget = fix.class(b"demo.Widget", source);
        let file = fix.atom(b"Widget.cs");
        let xml_text = b"<member name=\"T:demo.Widget\"><summary>Brews <see cref=\"T:demo.IPart\"/> like <see cref=\"T:System.String\"/> uses <c>brew</c>.</summary></member>";
        let xml = fix.atom(xml_text);
        fix.docs.push(DocRow {
            declaration: widget,
            file,
            start: 12,
            end: 22,
            xml,
        });
        fix.declarations[widget as usize].doc = Some(0);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let extension = csharp_extension(&view, 1)?;
        let provenance = extension
            .xml_provenance
            .ok_or(TestError::Missing("xml provenance"))?;
        if provenance.start() != 12 || provenance.end() != 22 {
            return Err(TestError::Missing("provenance span"));
        }
        // The provenance file atom was rewritten past the fact-name atoms.
        let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
        if atoms.get(provenance.file().raw as usize).copied() != Some(&b"Widget.cs"[..]) {
            return Err(TestError::Missing("provenance file atom"));
        }
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let expected: [DocFragmentInput<'_>; 7] = [
            DocFragmentInput::Text(b"Brews"),
            DocFragmentInput::Link {
                label: b"demo.IPart",
                target: DocLinkTarget::Local(compiler_ir::EntityId::new(part)),
            },
            DocFragmentInput::Text(b"like"),
            DocFragmentInput::Link {
                label: b"System.String",
                target: DocLinkTarget::Foreign {
                    ecosystem: b"nuget",
                    path: b"System.String",
                },
            },
            DocFragmentInput::Text(b"uses"),
            DocFragmentInput::Code(b"brew"),
            DocFragmentInput::Text(b"."),
        ];
        for fragment in expected {
            let fact = docs
                .next()
                .ok_or(TestError::Missing("doc fact"))?
                .map_err(|_| TestError::Missing("doc fact decode"))?;
            if fact.owner.raw != widget || fact.fragment != fragment {
                return Err(TestError::Missing("summary fragment"));
            }
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact doc facts"));
        }
        Ok(())
    }

    #[test]
    fn attribute_spellings_travel_in_the_pooled_atom_list() -> Result<(), TestError> {
        let source = b"class Widget {}";
        let mut fix = Fixture::default();
        let widget = fix.class(b"demo.Widget", source);
        let obsolete = fix.atom(b"Obsolete(\"use New\")");
        fix.attributes.push((widget, obsolete));
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let extension = csharp_extension(&view, 0)?;
        let pools = view
            .discover()
            .extension_pools()
            .map_err(|_| TestError::Missing("pools"))?
            .ok_or(TestError::Missing("pools"))?;
        let attributes = pools
            .atom_list(extension.attributes.raw)
            .map_err(|_| TestError::Missing("attribute list ordinal"))?;
        if attributes.len() != 1 {
            return Err(TestError::Missing("attribute list ordinal"));
        }
        let Some(attribute) = attributes
            .iter()
            .next()
            .and_then(|ordinal| view.atoms().nth(ordinal as usize))
        else {
            return Err(TestError::Missing("one attribute spelling"));
        };
        if attribute.bytes != b"Obsolete(\"use New\")" {
            return Err(TestError::Missing("one attribute spelling"));
        }
        // Falsifier: removing the attribute changes the committed bytes and
        // empties the committed attribute list.
        let mut mutated = fix.clone();
        mutated.attributes.clear();
        let other = lower(&mutated, source)?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let _ = csharp_extension(&view, 0)?;
        Ok(())
    }

    #[test]
    fn namespace_facts_anchor_owner_relative_import_occurrences() -> Result<(), TestError> {
        let source = b"namespace Demo { using System; class Widget {} }";
        let mut fix = Fixture::default();
        let namespace_atom = fix.atom(b"Demo");
        let (start, end) = Fixture::span_of(source, b"Demo");
        fix.declarations
            .push(Decl::new(8, namespace_atom, Some(namespace_atom)).at(start, start, end));
        let _widget = fix.class(b"Demo.Widget", source);
        let using_spelling = fix.atom(b"System");
        let file = fix.atom(b"Widget.cs");
        let (using_start, using_end) = Fixture::span_of(source, b"System");
        fix.references.push(RefRow {
            owner: 0,
            target: None,
            spelling: using_spelling,
            file,
            start: using_start,
            end: using_end,
            kind: 4,
        });
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        let entities = entity_rows(&view);
        if entities.len() != 2
            || !entities.contains(&(&b"Demo"[..], EntityKind::Namespace))
            || !entities.contains(&(&b"Widget"[..], EntityKind::Record))
        {
            return Err(TestError::Missing("namespace module fact"));
        }
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let occurrence = occurrences
            .next()
            .ok_or(TestError::Missing("occurrence"))?
            .map_err(|_| TestError::Missing("occurrence decode"))?;
        if occurrence.occurrence.kind != compiler_ir::ReferenceKind::Import {
            return Err(TestError::Missing("import reference kind"));
        }
        Ok(())
    }

    #[test]
    fn explicit_interface_implementation_binds_to_its_interface_member_occurrence()
    -> Result<(), TestError> {
        let source =
            b"interface IPart { void Brew(); } class Widget: IPart { void IPart.Brew() {} }";
        let mut fix = Fixture::default();
        let part_ty = fix.named(b"demo.IPart");
        let part_qualified = fix.atom(b"demo.IPart");
        let part_name = fix.atom(b"IPart");
        let (part_at, part_end) = Fixture::span_of(source, b"IPart");
        fix.declarations.push(
            Decl::new(3, part_name, Some(part_qualified))
                .typed(part_ty)
                .at(part_at, part_at, part_end),
        );
        let member = fix.method(0, b"Brew", None, source);
        fix.declarations.push(member);
        let widget = fix.class(b"demo.Widget", source);
        // The explicit implementation names `IPart.Brew` at byte 66; the
        // declaring span must name those exact source bytes.
        let implementation = fix
            .method(widget, b"Brew", None, source)
            .at(66, 66, 70)
            .flagged(FLAG_EXPLICIT_INTERFACE);
        fix.declarations.push(implementation);
        let binding_spelling = fix.atom(b"demo.IPart.Brew");
        let binding_file = fix.atom(b"Widget.cs");
        fix.references.push(RefRow {
            owner: 3,
            target: Some(1),
            spelling: binding_spelling,
            file: binding_file,
            start: 66,
            end: 70,
            kind: REF_IMPL_BINDING,
        });
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 IPart, 1 Widget, 2 the interface member, 3 the explicit
        // implementation. The binding occurrence is owned by the
        // implementation and targets the implemented member.
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let occurrence = occurrences
            .next()
            .ok_or(TestError::Missing("occurrence"))?
            .map_err(|_| TestError::Missing("occurrence decode"))?;
        let OccurrenceTarget::Local(target) = occurrence.occurrence.target else {
            return Err(TestError::Missing("local binding target"));
        };
        if occurrence.owner.raw != 3 || target.raw != 2 {
            return Err(TestError::Missing("binding ordinals"));
        }
        if occurrence.occurrence.kind != compiler_ir::ReferenceKind::MethodCall
            || occurrence.occurrence.confidence != compiler_ir::OccurrenceConfidence::Oracle
        {
            return Err(TestError::Missing("oracle method-call binding"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("single occurrence"));
        }
        // Falsifier: pointing the binding at the interface declaration
        // instead of its member retargets the committed occurrence.
        let mut mutated = fix.clone();
        mutated.references[0].target = Some(0);
        let other = lower(&mutated, source)?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let occurrence = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?
            .next()
            .ok_or(TestError::Missing("occurrence"))?
            .map_err(|_| TestError::Missing("occurrence decode"))?;
        let OccurrenceTarget::Local(target) = occurrence.occurrence.target else {
            return Err(TestError::Missing("local binding target"));
        };
        if target.raw != 0 {
            return Err(TestError::Missing("mutated binding target"));
        }
        Ok(())
    }

    #[test]
    fn operator_overloads_and_conversions_land_as_signature_bearing_functions()
    -> Result<(), TestError> {
        let source = b"class Money { static Money operator +(Money a, Money b) { return a; } static implicit operator Money(int value) { return new Money(); } }";
        let mut fix = Fixture::default();
        let money = fix.class(b"demo.Money", source);
        let money_ty = fix.named(b"demo.Money");
        let int_ty = fix.named(b"System.Int32");
        let a = fix.atom(b"a");
        let b = fix.atom(b"b");
        let value = fix.atom(b"value");
        let addition = fix
            .method(money, b"+", Some(money_ty), source)
            .of_kind(KIND_OPERATOR)
            .with_param(ParamRow {
                ty: money_ty,
                name: a,
                ref_kind: REF_VALUE,
            })
            .with_param(ParamRow {
                ty: money_ty,
                name: b,
                ref_kind: REF_VALUE,
            });
        fix.declarations.push(addition);
        let conversion = fix
            .method(money, b"Money", Some(money_ty), source)
            .of_kind(17)
            .at(95, 95, 100)
            .with_param(ParamRow {
                ty: int_ty,
                name: value,
                ref_kind: REF_VALUE,
            });
        fix.declarations.push(conversion);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Money, 1 a, 2 b, 3 the addition's result, 4 the operator,
        // 5 value, 6 the conversion's result, 7 the conversion.
        let entities = entity_rows(&view);
        if entities.len() != 8
            || entities[4].1 != EntityKind::Function
            || entities[7].1 != EntityKind::Function
        {
            return Err(TestError::Missing("operator and conversion functions"));
        }
        let operator_row = row(&view, 4)?;
        if operator_row.record.tag != SemanticTypeTag::FunctionPointer
            || operator_row.record.payload1
                != compiler_ir::SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || operator_row.record.children.length != 3
        {
            return Err(TestError::Missing("operator function row"));
        }
        let conversion_row = row(&view, 7)?;
        if conversion_row.record.tag != SemanticTypeTag::FunctionPointer
            || conversion_row.record.payload1
                != compiler_ir::SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE
            || conversion_row.record.children.length != 2
        {
            return Err(TestError::Missing("conversion function row"));
        }
        // Falsifier: retyping the conversion's result changes the committed
        // bytes into a nominal-of-int signature.
        let mut mutated = fix.clone();
        mutated.declarations[2].declared_type = Some(int_ty);
        let other = lower(&mutated, source)?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        Ok(())
    }

    #[test]
    fn params_parameter_lands_its_explicit_array_type_row() -> Result<(), TestError> {
        let source = b"class Bag { void Rest(params string[] rest) {} }";
        let mut fix = Fixture::default();
        let bag = fix.class(b"demo.Bag", source);
        let string_ty = fix.named(b"System.String");
        let rest_name = fix.atom(b"rest");
        let array_spelling = fix.atom(b"string[]");
        let array_row = u32::try_from(fix.types.len()).map_err(|_| TestError::Num)?;
        fix.types.push(TypeRow {
            kind: 2,
            nullable: NULL_NONE,
            has_return: 0,
            spelling: Some(array_spelling),
            children: vec![(None, string_ty)],
        });
        let rest = fix.method(bag, b"Rest", None, source).with_param(ParamRow {
            ty: array_row,
            name: rest_name,
            ref_kind: REF_VALUE,
        });
        fix.declarations.push(rest);
        let bytes = lower(&fix, source)?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Bag; the interned element row precedes the carriers, so
        // 1 is the anonymous string element, 2 the `rest` carrier, 3 Rest.
        let parameter = row(&view, 2)?;
        if parameter.record.tag != SemanticTypeTag::ArrayRectangular
            || parameter.record.payload0 != 1
            || parameter.record.children.length != 1
        {
            return Err(TestError::Missing("params array row"));
        }
        // Falsifier: rank repeats one element coordinate; it may not select
        // the first of heterogeneous children and silently call that a
        // rectangular array.
        let mut mutated = fix.clone();
        let int_ty = mutated.named(b"System.Int32");
        mutated.types[array_row as usize]
            .children
            .push((None, int_ty));
        let Err(TestError::Collect(CSharpCollectError::Lowering(
            LoweringUnsupported::CSharpProjection {
                fault: PortableCSharpProjectionFault::HeterogeneousArrayRank { .. },
            },
        ))) = lower(&mutated, source)
        else {
            return Err(TestError::Missing("heterogeneous array rank rejection"));
        };
        Ok(())
    }

    #[test]
    fn the_exact_lane_bound_admits_and_one_more_declaration_rejects() -> Result<(), TestError> {
        let mut exact = Fixture::default();
        let mut source = Vec::new();
        for index in 0..MAX_EMISSION_FACTS {
            let name = format!("T{index:03}");
            source.extend_from_slice(name.as_bytes());
            source.push(b' ');
            let mut spelling = b"demo.T".to_vec();
            spelling.extend_from_slice(name.as_bytes());
            let qualified = spelling.clone();
            let simple_atom = exact.atom(name.as_bytes());
            let qualified_atom = exact.atom(&qualified);
            let ty = exact.named(&qualified);
            let (name_start, name_end) = Fixture::span_of(&source, name.as_bytes());
            exact.declarations.push(
                Decl::new(1, simple_atom, Some(qualified_atom))
                    .typed(ty)
                    .at(name_start, name_start, name_end),
            );
            let _ = &mut spelling;
        }
        let bytes = lower(&exact, &source)?;
        let view = FragmentView::validate(&bytes)?;
        if view.entities().count() != MAX_EMISSION_FACTS {
            return Err(TestError::Missing("exact bound"));
        }
        // The over-bound declaration still names honest source bytes, so the
        // rejection is the lane's capacity terminal, not a span fault.
        source.extend_from_slice(b"T999 ");
        let mut over = exact.clone();
        let name = over.atom(b"T999");
        let qualified = over.atom(b"demo.T999");
        let ty = over.named(b"demo.T999");
        let (name_start, name_end) = Fixture::span_of(&source, b"T999");
        over.declarations.push(
            Decl::new(1, name, Some(qualified))
                .typed(ty)
                .at(name_start, name_start, name_end),
        );
        let image = over.encode(&source)?;
        let mut facts = FactSet::new();
        match collect(&source, &image, &mut facts) {
            Err(CSharpCollectError::Rejected(rejection))
                if rejection.fact == crate::lower::MAX_EMISSION_FACTS
                    && rejection.name_len == 4
                    && rejection.cause == crate::types::FactFault::Capacity => {}
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("capacity rejection")),
        }
        Ok(())
    }

    #[test]
    fn source_binding_and_name_spans_are_proven_before_admission() -> Result<(), TestError> {
        let mut fix = Fixture::default();
        let _ = fix.class(b"demo.Widget", b"class Widget {}");
        let source = b"class Widget {}";
        let image = fix.encode(source)?;

        // A digest bound to other source bytes is the typed rejection with
        // both operands retained.
        let mut facts = FactSet::new();
        match collect(b"other source entirely", &image, &mut facts) {
            Err(CSharpCollectError::SourceBinding { expected, observed }) => {
                if expected != Sha256::digest(b"other source entirely").as_slice()
                    || observed != Sha256::digest(source).as_slice()
                {
                    return Err(TestError::Missing("binding operands"));
                }
            }
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("binding rejection")),
        }

        // A name span that names other source bytes is the typed rejection.
        let mut mutated = fix.clone();
        mutated.declarations[0].decl_start = 0;
        mutated.declarations[0].name_start = 0;
        mutated.declarations[0].name_end = 5;
        let image = mutated.encode(source)?;
        let mut facts = FactSet::new();
        match collect(source, &image, &mut facts) {
            Err(CSharpCollectError::Span { start: 0, end: 5 }) => {}
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("span rejection")),
        }
        Ok(())
    }

    const _: () = {
        // The fixture keeps every wire constant in lockstep with the reader.
        assert!(REF_VALUE == 0 && REF_REF == 2 && REF_INVOCATION == 1);
    };
}
