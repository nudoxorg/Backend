//! Validates the fixed Roslyn authority image used by direct compiler admission.
//! Lends source-bound declaration names, byte spans, recursive type nodes,
//! signatures, references, XML-documentation provenance, attributes,
//! nullability cells, reference kinds, member-effect flags, partial roles,
//! and generic constraints without JSON reconstruction.
//! Rejects malformed image coordinates before they can enter canonical facts.
//!
//! Wire layout (version 3): a fixed 256-byte header — magic `NCAI`, version,
//! header length, body length, the SHA-256 of the bound source, a section
//! count, and a fixed eleven-entry section directory — followed by the
//! canonical body sections in directory order. A domain-separated SHA-256
//! covers every byte the header declares except the digest cell itself.
//! Version 3 extends the version-2 closed vocabularies with the explicit
//! interface-implementation flag on declaration rows and the
//! implementation-binding class on reference rows; the row layouts are
//! unchanged, and every earlier rejection stays typed.
//!
//! Error laws: the crate's image-fault lattice is frozen at seven variants,
//! so every version-3 fault reuses a variant whose operands name the
//! offending section, the row ordinal, and the observed cells:
//! - envelope and directory violations are [`HeaderError`] arms;
//! - a closed-tag violation on any section is [`ImageError::DeclarationKind`]
//!   with the section and the observed byte;
//! - a reserved-cell violation on any section is
//!   [`ImageError::DeclarationReserved`] with the section;
//! - an atom range outside the atom plane is [`ImageError::NameRange`];
//! - an inverted range or a coordinate outside its target section is
//!   [`ImageError::Span`], carrying the observed cells and the bound.

use core::str;

use sha2::{Digest, Sha256};
use thiserror::Error;

const MAGIC: [u8; 4] = *b"NCAI";
const VERSION: u16 = 3;
const SOURCE_DIGEST_OFFSET: usize = 12;
const DIRECTORY_OFFSET: usize = 48;
const DIRECTORY_ENTRY_BYTES: usize = 16;
const SECTION_COUNT: usize = 11;
const DIRECTORY_BYTES: usize = DIRECTORY_ENTRY_BYTES * SECTION_COUNT;
const IMAGE_DIGEST_OFFSET: usize = DIRECTORY_OFFSET + DIRECTORY_BYTES;
const HEADER_BYTES: usize = IMAGE_DIGEST_OFFSET + 32;
const ABSENT: u32 = u32::MAX;
const DIGEST_DOMAIN: &[u8] = b"nudox.csharp.authority.image.sha256.v3\0";

/// A fixed image section in canonical directory order.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    /// Fixed-width atom offsets and lengths.
    Atoms = 1,
    /// Concatenated UTF-8 atom bytes.
    AtomBytes = 2,
    /// Namespace, type, and member declaration records.
    Declarations = 3,
    /// Signature parameter records.
    Parameters = 4,
    /// Declaration-site generic parameter records.
    TypeParameters = 5,
    /// Generic constraint type coordinates.
    TypeConstraints = 6,
    /// Fixed-width recursive type records.
    Types = 7,
    /// Type-child coordinates with optional tuple labels.
    TypeChildren = 8,
    /// Applied-attribute spelling rows.
    Attributes = 9,
    /// XML documentation provenance rows.
    Docs = 10,
    /// Compiler-resolved reference records.
    References = 11,
}

impl Section {
    const fn from_directory_index(index: usize) -> Self {
        match index {
            0 => Self::Atoms,
            1 => Self::AtomBytes,
            2 => Self::Declarations,
            3 => Self::Parameters,
            4 => Self::TypeParameters,
            5 => Self::TypeConstraints,
            6 => Self::Types,
            7 => Self::TypeChildren,
            8 => Self::Attributes,
            9 => Self::Docs,
            _ => Self::References,
        }
    }

    const fn row_bytes(self) -> usize {
        match self {
            Self::Atoms => 8,
            Self::AtomBytes => 1,
            Self::Declarations => 48,
            Self::Parameters => 24,
            Self::TypeParameters => 12,
            Self::TypeConstraints => 4,
            Self::Types => 16,
            Self::TypeChildren => 8,
            Self::Attributes => 8,
            Self::Docs => 20,
            Self::References => 28,
        }
    }
}

/// A closed Roslyn declaration kind: namespaces, type declarations, and
/// member declarations. The type-declaration discriminants are the frozen
/// version-1 cells.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// A class declaration.
    Class = 1,
    /// A struct declaration.
    Struct = 2,
    /// An interface declaration.
    Interface = 3,
    /// An enum declaration.
    Enum = 4,
    /// A delegate declaration.
    Delegate = 5,
    /// A record class declaration.
    Record = 6,
    /// A record struct declaration.
    RecordStruct = 7,
    /// A namespace declaration (file-scoped or block-scoped).
    Namespace = 8,
    /// A field declaration.
    Field = 9,
    /// An enum member.
    EnumMember = 10,
    /// A non-indexer property declaration.
    Property = 11,
    /// An indexer declaration.
    Indexer = 12,
    /// An event declaration.
    Event = 13,
    /// A constructor declaration.
    Constructor = 14,
    /// An ordinary method declaration.
    Method = 15,
    /// A user-defined operator declaration.
    Operator = 16,
    /// An implicit or explicit conversion declaration.
    Conversion = 17,
}

impl DeclarationKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Class),
            2 => Some(Self::Struct),
            3 => Some(Self::Interface),
            4 => Some(Self::Enum),
            5 => Some(Self::Delegate),
            6 => Some(Self::Record),
            7 => Some(Self::RecordStruct),
            8 => Some(Self::Namespace),
            9 => Some(Self::Field),
            10 => Some(Self::EnumMember),
            11 => Some(Self::Property),
            12 => Some(Self::Indexer),
            13 => Some(Self::Event),
            14 => Some(Self::Constructor),
            15 => Some(Self::Method),
            16 => Some(Self::Operator),
            17 => Some(Self::Conversion),
            _ => None,
        }
    }
}

/// The closed per-declaration effect flags the canonical lane consumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationFlags {
    /// The declaration carries an extension receiver (C# 14 extension block).
    pub is_extension: bool,
    /// The method body is `async`.
    pub is_async: bool,
    /// The method body contains a `yield`.
    pub is_iterator: bool,
    /// The field is `const`.
    pub is_const: bool,
    /// The method or property names its implemented interface member
    /// explicitly (`void IPart.Brew() {}`).
    pub is_explicit_interface: bool,
}

impl DeclarationFlags {
    const ALL: u8 = 0x1f;

    const fn decode(raw: u8) -> Option<Self> {
        if raw & !Self::ALL != 0 {
            return None;
        }
        Some(Self {
            is_extension: raw & 0x1 != 0,
            is_async: raw & 0x2 != 0,
            is_iterator: raw & 0x4 != 0,
            is_const: raw & 0x8 != 0,
            is_explicit_interface: raw & 0x10 != 0,
        })
    }
}

/// A closed partial-declaration role.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PartialRole {
    /// The declaration is not partial.
    None = 0,
    /// The defining half of a partial declaration; it owns the type.
    Definition = 1,
    /// The implementing half of a partial method or property.
    Implementation = 2,
}

impl PartialRole {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::Definition),
            2 => Some(Self::Implementation),
            _ => None,
        }
    }
}

/// A closed parameter and return reference convention. `ref readonly`
/// parameters keep their own cell so the producer never collapses them.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefKind {
    /// Passed by value.
    Value = 0,
    /// Passed `in`.
    In = 1,
    /// Passed `ref`.
    Ref = 2,
    /// Passed `out`.
    Out = 3,
    /// Passed `ref readonly`.
    RefReadonly = 4,
}

impl RefKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Value),
            1 => Some(Self::In),
            2 => Some(Self::Ref),
            3 => Some(Self::Out),
            4 => Some(Self::RefReadonly),
            _ => None,
        }
    }
}

/// The meaningful nullability cell carried by reference-typed nodes.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NullabilityCell {
    /// No nullable annotation was meaningful at this position.
    None = 0,
    /// Explicit `?` annotation.
    Annotated = 1,
    /// Explicitly non-null in an enabled nullable context.
    NotAnnotated = 2,
}

impl NullabilityCell {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::Annotated),
            2 => Some(Self::NotAnnotated),
            _ => None,
        }
    }
}

/// A closed Roslyn type-node form.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeNodeKind {
    /// A (possibly generic) named type use.
    Named = 1,
    /// An array type of any rank.
    Array = 2,
    /// A raw pointer's pointee.
    Pointer = 3,
    /// `Nullable<T>` collapsed to its own node.
    NullableValue = 4,
    /// A tuple type with labelled or positional elements.
    Tuple = 5,
    /// A function pointer signature.
    FunctionPointer = 6,
    /// A generic parameter use.
    TypeParameter = 7,
    /// A C# `dynamic` use.
    Dynamic = 8,
    /// A type Roslyn could not bind.
    Error = 9,
}

impl TypeNodeKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Named),
            2 => Some(Self::Array),
            3 => Some(Self::Pointer),
            4 => Some(Self::NullableValue),
            5 => Some(Self::Tuple),
            6 => Some(Self::FunctionPointer),
            7 => Some(Self::TypeParameter),
            8 => Some(Self::Dynamic),
            9 => Some(Self::Error),
            _ => None,
        }
    }

    /// Closed authority child cardinality. Array rank is represented by the
    /// number of element children; callable rows may be empty for `void ()`.
    const fn child_law(self) -> (usize, usize) {
        match self {
            Self::Array => (1, usize::MAX),
            Self::Pointer | Self::NullableValue => (1, 1),
            Self::TypeParameter | Self::Dynamic | Self::Error => (0, 0),
            Self::Named | Self::Tuple | Self::FunctionPointer => (0, usize::MAX),
        }
    }
}

/// Generic-parameter variance, closed to C#'s two written directions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VarianceTag {
    /// Invariant use.
    Invariant = 0,
    /// Covariant (`out T`).
    Out = 1,
    /// Contravariant (`in T`).
    In = 2,
}

impl VarianceTag {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Invariant),
            1 => Some(Self::Out),
            2 => Some(Self::In),
            _ => None,
        }
    }
}

/// A closed reference class emitted by the compiler-resolved reference plane.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceTag {
    /// A method invocation.
    Invocation = 1,
    /// An object-creation expression.
    ObjectCreation = 2,
    /// A member access.
    MemberAccess = 3,
    /// A using directive (import).
    UsingDirective = 4,
    /// An explicit interface implementation's binding to the interface
    /// member it implements. The owner is the implementing member row and
    /// the target is the implemented interface member row when that member
    /// is inside the assembly.
    InterfaceImplementation = 5,
}

impl ReferenceTag {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Invocation),
            2 => Some(Self::ObjectCreation),
            3 => Some(Self::MemberAccess),
            4 => Some(Self::UsingDirective),
            5 => Some(Self::InterfaceImplementation),
            _ => None,
        }
    }
}

/// One atom borrowed directly from the image's UTF-8 atom plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Atom<'image> {
    /// The canonical UTF-8 atom bytes.
    pub bytes: &'image [u8],
}

/// An opaque validated coordinate in the type section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeRef(u32);

impl TypeRef {
    /// Returns the exact zero-based authority type-row coordinate.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.0
    }
}

/// An exact-size iterator over validated constraint type coordinates of one
/// generic parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConstraintIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
    end: usize,
}

impl Iterator for ConstraintIter<'_> {
    type Item = TypeRef;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let row = self.image.row(Section::TypeConstraints, self.next);
        self.next += 1;
        Some(TypeRef(u32_at(row, 0)))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ConstraintIter<'_> {
    fn len(&self) -> usize {
        self.end - self.next
    }
}

/// One ordered type child: an optional tuple label plus a type coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeChild<'image> {
    /// The labelled tuple element name, when the author wrote one.
    pub name: Option<Atom<'image>>,
    /// The child type coordinate.
    pub ty: TypeRef,
}

/// An exact-size iterator over validated type children of one type node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeChildIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
    end: usize,
}

impl<'image> Iterator for TypeChildIter<'image> {
    type Item = TypeChild<'image>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let row = self.image.row(Section::TypeChildren, self.next);
        self.next += 1;
        Some(TypeChild {
            name: self.image.optional_atom(u32_at(row, 0)),
            ty: TypeRef(u32_at(row, 4)),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for TypeChildIter<'_> {
    fn len(&self) -> usize {
        self.end - self.next
    }
}

/// One borrowed recursive type node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeNode<'image> {
    /// The closed type form.
    pub kind: TypeNodeKind,
    /// The meaningful nullability cell.
    pub nullable: NullabilityCell,
    /// Whether a function-pointer row carries a trailing result child.
    pub has_return: bool,
    /// The qualified spelling atom, when the form carries a name.
    pub spelling: Option<Atom<'image>>,
    /// Ordered labelled children.
    pub children: TypeChildIter<'image>,
}

/// One declared signature parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Parameter<'image> {
    /// The parameter name.
    pub name: Atom<'image>,
    /// The declared type coordinate.
    pub ty: TypeRef,
    /// The closed reference convention.
    pub ref_kind: RefKind,
    /// Whether the parameter is `params`.
    pub is_params: bool,
    /// Whether the parameter declares a default value.
    pub has_default: bool,
    /// The default value's source spelling, when one exists.
    pub default: Option<Atom<'image>>,
    /// Inclusive source byte coordinate of the declared name.
    pub name_start: u32,
    /// Exclusive source byte coordinate of the declared name.
    pub name_end: u32,
}

impl<'image> Parameter<'image> {
    fn decode(image: CSharpImage<'image>, index: usize) -> Result<Self, ImageError> {
        let row = image.row(Section::Parameters, index);
        let ref_kind = RefKind::decode(row[8]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[8],
            plane: Section::Parameters,
        })?;
        Ok(Self {
            name: Atom {
                bytes: image.atom_bytes(u32_at(row, 4)),
            },
            ty: TypeRef(u32_at(row, 0)),
            ref_kind,
            is_params: row[9] & 0x1 != 0,
            has_default: row[9] & 0x2 != 0,
            default: image.optional_atom(u32_at(row, 12)),
            name_start: u32_at(row, 16),
            name_end: u32_at(row, 20),
        })
    }
}

/// One declaration-site generic parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenericParameter<'image> {
    /// The parameter's simple name.
    pub name: Atom<'image>,
    /// The closed variance cell.
    pub variance: VarianceTag,
    /// Structural constraint type coordinates in written order.
    pub constraints: ConstraintIter<'image>,
    /// Whether a `class` constraint is present.
    pub reference_type: bool,
    /// Whether a `struct` constraint is present.
    pub value_type: bool,
    /// Whether a `notnull` constraint is present.
    pub not_null: bool,
    /// Whether an `unmanaged` constraint is present.
    pub unmanaged: bool,
    /// Whether a `new()` constraint is present.
    pub constructor: bool,
    /// Whether C# 13's `allows ref struct` is present.
    pub allows_ref_like: bool,
}

impl<'image> GenericParameter<'image> {
    fn decode(image: CSharpImage<'image>, index: usize) -> Result<Self, ImageError> {
        let row = image.row(Section::TypeParameters, index);
        let constraint_start = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::Span {
            index,
            start: u32_at(row, 4),
            end: u32::try_from(image.section(Section::TypeConstraints).count).unwrap_or(u32::MAX),
        })?;
        let constraint_count = usize::from(u16_at(row, 8));
        let variance = VarianceTag::decode(row[10]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[10],
            plane: Section::TypeParameters,
        })?;
        Ok(Self {
            name: Atom {
                bytes: image.atom_bytes(u32_at(row, 0)),
            },
            variance,
            constraints: ConstraintIter {
                image,
                next: constraint_start,
                end: constraint_start + constraint_count,
            },
            reference_type: row[11] & 0x1 != 0,
            value_type: row[11] & 0x2 != 0,
            not_null: row[11] & 0x4 != 0,
            unmanaged: row[11] & 0x8 != 0,
            constructor: row[11] & 0x10 != 0,
            allows_ref_like: row[11] & 0x20 != 0,
        })
    }
}

/// One applied attribute: the declaring row plus one atom spelling the whole
/// application, constructor arguments and named arguments included.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Attribute<'image> {
    /// The declaring row whose application this is.
    pub declaration: u32,
    /// One atom spelling the whole application, arguments included.
    pub spelling: Atom<'image>,
}

/// One XML documentation provenance row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Doc<'image> {
    /// The declaring row the comment documents.
    pub declaration: u32,
    /// The source file path as the producer saw it.
    pub file: Atom<'image>,
    /// Inclusive UTF-8 byte start of the comment in the bound source.
    pub start: u32,
    /// Exclusive UTF-8 byte end of the comment in the bound source.
    pub end: u32,
    /// The raw documentation XML, wrapper element included.
    pub xml: Atom<'image>,
}

/// One compiler-resolved reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedReference<'image> {
    /// The owning declaration row.
    pub owner: u32,
    /// The resolved declaration row, when the target is inside the assembly.
    pub target: Option<u32>,
    /// The written target spelling for foreign targets.
    pub spelling: Atom<'image>,
    /// The source file path as the producer saw it.
    pub file: Atom<'image>,
    /// Inclusive UTF-8 byte start of the referenced expression.
    pub start: u32,
    /// Exclusive UTF-8 byte end of the referenced expression.
    pub end: u32,
    /// The closed reference class.
    pub kind: ReferenceTag,
}

/// One borrowed declaration row with closed attributes and typed coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Declaration<'image> {
    /// The declaration's closed Roslyn kind.
    pub kind: DeclarationKind,
    /// The closed effect flags.
    pub flags: DeclarationFlags,
    /// The partial-declaration role.
    pub partial: PartialRole,
    /// The closed reference convention (parameters, returns by reference).
    pub ref_kind: RefKind,
    /// The declared name bytes.
    pub name: Atom<'image>,
    /// The metadata fully-qualified name, when the declaration carries one.
    pub qualified: Option<Atom<'image>>,
    /// The declaring row's coordinate, when the declaration is a member.
    pub owner: Option<u32>,
    /// The declared type coordinate, when the declaration has one.
    pub declared_type: Option<TypeRef>,
    /// Inclusive source byte coordinate where the whole declaration starts.
    pub decl_start: u32,
    /// Inclusive source byte coordinate of the declared identifier.
    pub name_start: u32,
    /// Exclusive source byte coordinate of the declared identifier.
    pub name_end: u32,
    /// Ordered signature parameters.
    pub parameters: ParamSlice<'image>,
    /// Declaration-site generic parameters.
    pub type_parameters: GenericSlice<'image>,
    /// The documentation row coordinate, when documentation exists.
    pub doc: Option<u32>,
}

/// A validated half-open parameter range of one declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParamSlice<'image> {
    image: CSharpImage<'image>,
    start: usize,
    end: usize,
}

impl<'image> ParamSlice<'image> {
    /// Iterates the ordered parameters.
    #[must_use]
    pub fn iter(self) -> ParamIter<'image> {
        ParamIter {
            image: self.image,
            next: self.start,
            end: self.end,
        }
    }

    /// Number of parameters in the slice.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// True when the slice carries no parameters.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end == self.start
    }
}

/// An exact-size iterator over validated parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParamIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
    end: usize,
}

impl<'image> Iterator for ParamIter<'image> {
    type Item = Result<Parameter<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let parameter = Parameter::decode(self.image, self.next);
        self.next += 1;
        Some(parameter)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ParamIter<'_> {
    fn len(&self) -> usize {
        self.end - self.next
    }
}

/// A validated half-open generic-parameter range of one declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenericSlice<'image> {
    image: CSharpImage<'image>,
    start: usize,
    end: usize,
}

impl<'image> GenericSlice<'image> {
    /// Iterates the ordered generic parameters.
    #[must_use]
    pub fn iter(self) -> GenericIter<'image> {
        GenericIter {
            image: self.image,
            next: self.start,
            end: self.end,
        }
    }

    /// Number of generic parameters in the slice.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// True when the slice carries no generic parameters.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.end == self.start
    }
}

/// An exact-size iterator over validated generic parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenericIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
    end: usize,
}

impl<'image> Iterator for GenericIter<'image> {
    type Item = Result<GenericParameter<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let parameter = GenericParameter::decode(self.image, self.next);
        self.next += 1;
        Some(parameter)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for GenericIter<'_> {
    fn len(&self) -> usize {
        self.end - self.next
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SectionBounds {
    offset: usize,
    count: usize,
}

/// A validated immutable Roslyn image borrowing caller-owned image bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CSharpImage<'image> {
    bytes: &'image [u8],
    sections: [SectionBounds; SECTION_COUNT],
    source_digest: [u8; 32],
}

impl<'image> CSharpImage<'image> {
    /// Opens one complete image after checking its canonical envelope,
    /// domain-separated checksum, and every section's closed layout.
    pub fn open(bytes: &'image [u8]) -> Result<Self, ImageError> {
        if bytes.len() < HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let found_magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if found_magic != MAGIC {
            return Err(ImageError::Header(HeaderError::Magic {
                found: found_magic,
            }));
        }
        let version = u16_at(bytes, 4);
        if version != VERSION {
            return Err(ImageError::Header(HeaderError::Version { found: version }));
        }
        let header_bytes = usize::from(u16_at(bytes, 6));
        if header_bytes != HEADER_BYTES {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        let section_count = usize::from(u16_at(bytes, 44));
        if section_count != SECTION_COUNT {
            return Err(ImageError::Header(HeaderError::SectionCount {
                found: section_count,
            }));
        }
        if bytes[46..DIRECTORY_OFFSET] != [0; 2] {
            return Err(ImageError::Header(HeaderError::Reserved));
        }
        let body_bytes = usize::try_from(u32_at(bytes, 8)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len().saturating_sub(HEADER_BYTES),
            })
        })?;
        if HEADER_BYTES.checked_add(body_bytes) != Some(bytes.len()) {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len().saturating_sub(HEADER_BYTES),
            }));
        }
        let sections = read_directory(bytes)?;
        let mut source_digest = [0; 32];
        source_digest.copy_from_slice(&bytes[SOURCE_DIGEST_OFFSET..SOURCE_DIGEST_OFFSET + 32]);
        let image = Self {
            bytes,
            sections,
            source_digest,
        };
        image.validate_digest()?;
        image.validate_atoms()?;
        image.validate_declarations()?;
        image.validate_parameters()?;
        image.validate_type_parameters()?;
        image.validate_type_constraints()?;
        image.validate_types()?;
        image.validate_type_children()?;
        image.validate_attributes()?;
        image.validate_docs()?;
        image.validate_references()?;
        Ok(image)
    }

    /// Returns the SHA-256 digest of the source Roslyn bound before emission.
    #[must_use]
    pub const fn source_digest(self) -> [u8; 32] {
        self.source_digest
    }

    /// Iterates all declarations attributed to the source-binding file.
    #[must_use]
    pub const fn declarations(self) -> DeclarationIter<'image> {
        DeclarationIter {
            image: self,
            next: 0,
        }
    }

    /// Resolves one declaration coordinate into its borrowed row.
    pub fn declaration(self, index: u32) -> Result<Declaration<'image>, ImageError> {
        let index = self.coordinate(
            index,
            Section::Declarations,
            self.section(Section::Declarations).count,
        )?;
        self.declaration_at(index)
    }

    /// Resolves one type coordinate into its borrowed recursive node.
    pub fn type_node(self, reference: TypeRef) -> Result<TypeNode<'image>, ImageError> {
        let index = self.coordinate(
            reference.0,
            Section::Types,
            self.section(Section::Types).count,
        )?;
        let row = self.row(Section::Types, index);
        let child_start = self.range_start(u32_at(row, 8), Section::TypeChildren, index)?;
        let child_count = usize::try_from(u32_at(row, 12)).map_err(|_| ImageError::Span {
            index,
            start: u32::try_from(child_start).unwrap_or(u32::MAX),
            end: u32::try_from(self.section(Section::TypeChildren).count).unwrap_or(u32::MAX),
        })?;
        let child_end = self.range_end(child_start, child_count, Section::TypeChildren, index)?;
        let kind = TypeNodeKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
            plane: Section::Types,
        })?;
        let nullable = NullabilityCell::decode(row[1]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[1],
            plane: Section::Types,
        })?;
        Ok(TypeNode {
            kind,
            nullable,
            has_return: row[3] & 0x1 != 0,
            spelling: self.optional_atom(u32_at(row, 4)),
            children: TypeChildIter {
                image: self,
                next: child_start,
                end: child_end,
            },
        })
    }

    /// Iterates every applied-attribute row in canonical order.
    #[must_use]
    pub const fn attributes(self) -> AttributeIter<'image> {
        AttributeIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates every documentation row in canonical order.
    #[must_use]
    pub const fn docs(self) -> DocIter<'image> {
        DocIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates every compiler-resolved reference in canonical order.
    #[must_use]
    pub const fn references(self) -> ReferenceIter<'image> {
        ReferenceIter {
            image: self,
            next: 0,
        }
    }

    fn declaration_at(self, index: usize) -> Result<Declaration<'image>, ImageError> {
        let row = self.row(Section::Declarations, index);
        let kind = DeclarationKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[0],
            plane: Section::Declarations,
        })?;
        let flags = DeclarationFlags::decode(row[1]).ok_or(ImageError::DeclarationReserved {
            index,
            plane: Section::Declarations,
        })?;
        let partial = PartialRole::decode(row[2]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[2],
            plane: Section::Declarations,
        })?;
        let ref_kind = RefKind::decode(row[3]).ok_or(ImageError::DeclarationKind {
            index,
            found: row[3],
            plane: Section::Declarations,
        })?;
        let name = self.atom(u32_at(row, 4))?;
        self.check_optional_atom(u32_at(row, 8))?;
        let owner = self.optional_coordinate(u32_at(row, 12), Section::Declarations)?;
        let declared_type = self.optional_type(u32_at(row, 16))?;
        let decl_start = u32_at(row, 20);
        let name_start = u32_at(row, 24);
        let name_end = u32_at(row, 28);
        if decl_start > name_start || name_start > name_end {
            return Err(ImageError::Span {
                index,
                start: decl_start,
                end: name_end,
            });
        }
        let param_start = self.range_start(u32_at(row, 32), Section::Parameters, index)?;
        let param_count = usize::from(u16_at(row, 36));
        let param_end = self.range_end(param_start, param_count, Section::Parameters, index)?;
        let generic_start = self.range_start(u32_at(row, 38), Section::TypeParameters, index)?;
        let generic_count = usize::from(u16_at(row, 42));
        let generic_end =
            self.range_end(generic_start, generic_count, Section::TypeParameters, index)?;
        let doc = self.optional_coordinate(u32_at(row, 44), Section::Docs)?;
        Ok(Declaration {
            kind,
            flags,
            partial,
            ref_kind,
            name,
            qualified: self.optional_atom(u32_at(row, 8)),
            owner,
            declared_type,
            decl_start,
            name_start,
            name_end,
            parameters: ParamSlice {
                image: self,
                start: param_start,
                end: param_end,
            },
            type_parameters: GenericSlice {
                image: self,
                start: generic_start,
                end: generic_end,
            },
            doc,
        })
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(&self.bytes[..IMAGE_DIGEST_OFFSET]);
        digest.update(&self.bytes[HEADER_BYTES..]);
        if digest.finalize().as_slice()
            != &self.bytes[IMAGE_DIGEST_OFFSET..IMAGE_DIGEST_OFFSET + 32]
        {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_atoms(self) -> Result<(), ImageError> {
        let mut next_byte = 0usize;
        let count = self.section(Section::Atoms).count;
        for index in 0..count {
            let row = self.row(Section::Atoms, index);
            let offset = usize::try_from(u32_at(row, 0)).map_err(|_| ImageError::NameRange {
                index,
                offset: u32::MAX,
                length: 0,
                atom_bytes: self.section(Section::AtomBytes).count,
            })?;
            let length = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::NameRange {
                index,
                offset: u32::try_from(offset).unwrap_or(u32::MAX),
                length: u32::MAX,
                atom_bytes: self.section(Section::AtomBytes).count,
            })?;
            let end = offset
                .checked_add(length)
                .filter(|end| length > 0 && *end <= self.section(Section::AtomBytes).count)
                .ok_or(ImageError::NameRange {
                    index,
                    offset: u32::try_from(offset).unwrap_or(u32::MAX),
                    length: u32::try_from(length).unwrap_or(u32::MAX),
                    atom_bytes: self.section(Section::AtomBytes).count,
                })?;
            if str::from_utf8(self.atom_bytes(u32_at(row, 0))).is_err() {
                return Err(ImageError::NameUtf8 { index });
            }
            next_byte = end;
        }
        if next_byte != self.section(Section::AtomBytes).count {
            return Err(ImageError::NameRange {
                index: count,
                offset: u32::try_from(next_byte).unwrap_or(u32::MAX),
                length: 0,
                atom_bytes: self.section(Section::AtomBytes).count,
            });
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Declarations).count {
            self.declaration_at(index)?;
        }
        Ok(())
    }

    fn validate_parameters(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Parameters).count {
            let row = self.row(Section::Parameters, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Types,
                self.section(Section::Types).count,
            )?;
            self.atom(u32_at(row, 4))?;
            if RefKind::decode(row[8]).is_none() {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[8],
                    plane: Section::Parameters,
                });
            }
            if row[9] & !0x3 != 0 {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::Parameters,
                });
            }
            if row[10..12] != [0; 2] {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::Parameters,
                });
            }
            self.check_optional_atom(u32_at(row, 12))?;
            let start = u32_at(row, 16);
            let end = u32_at(row, 20);
            if start > end {
                return Err(ImageError::Span { index, start, end });
            }
        }
        Ok(())
    }

    fn validate_type_parameters(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::TypeParameters).count {
            let row = self.row(Section::TypeParameters, index);
            self.atom(u32_at(row, 0))?;
            let start = self.range_start(u32_at(row, 4), Section::TypeConstraints, index)?;
            let count = usize::from(u16_at(row, 8));
            let _ = self.range_end(start, count, Section::TypeConstraints, index)?;
            if VarianceTag::decode(row[10]).is_none() {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[10],
                    plane: Section::TypeParameters,
                });
            }
            if row[11] & !0x3f != 0 {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::TypeParameters,
                });
            }
        }
        Ok(())
    }

    fn validate_type_constraints(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::TypeConstraints).count {
            self.coordinate(
                u32_at(self.row(Section::TypeConstraints, index), 0),
                Section::Types,
                self.section(Section::Types).count,
            )?;
        }
        Ok(())
    }

    fn validate_types(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Types).count {
            let row = self.row(Section::Types, index);
            let kind = TypeNodeKind::decode(row[0]).ok_or(ImageError::DeclarationKind {
                index,
                found: row[0],
                plane: Section::Types,
            })?;
            if NullabilityCell::decode(row[1]).is_none() {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[1],
                    plane: Section::Types,
                });
            }
            if row[2] != 0 || row[3] & !0x1 != 0 {
                return Err(ImageError::DeclarationReserved {
                    index,
                    plane: Section::Types,
                });
            }
            self.check_optional_atom(u32_at(row, 4))?;
            let start = self.range_start(u32_at(row, 8), Section::TypeChildren, index)?;
            let count = usize::try_from(u32_at(row, 12)).map_err(|_| ImageError::Span {
                index,
                start: u32::try_from(start).unwrap_or(u32::MAX),
                end: u32::try_from(self.section(Section::TypeChildren).count).unwrap_or(u32::MAX),
            })?;
            self.range_end(start, count, Section::TypeChildren, index)?;
            let (min, max) = kind.child_law();
            if count < min || count > max {
                return Err(ImageError::TypeChildCount {
                    index,
                    kind,
                    min,
                    max,
                    actual: count,
                });
            }
        }
        Ok(())
    }

    fn validate_type_children(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::TypeChildren).count {
            let row = self.row(Section::TypeChildren, index);
            self.coordinate(
                u32_at(row, 4),
                Section::Types,
                self.section(Section::Types).count,
            )?;
            self.check_optional_atom(u32_at(row, 0))?;
        }
        Ok(())
    }

    fn validate_attributes(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Attributes).count {
            let row = self.row(Section::Attributes, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Declarations,
                self.section(Section::Declarations).count,
            )?;
            self.atom(u32_at(row, 4))?;
        }
        Ok(())
    }

    fn validate_docs(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::Docs).count {
            let row = self.row(Section::Docs, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Declarations,
                self.section(Section::Declarations).count,
            )?;
            self.atom(u32_at(row, 4))?;
            self.atom(u32_at(row, 16))?;
            let start = u32_at(row, 8);
            let end = u32_at(row, 12);
            if start > end {
                return Err(ImageError::Span { index, start, end });
            }
        }
        Ok(())
    }

    fn validate_references(self) -> Result<(), ImageError> {
        for index in 0..self.section(Section::References).count {
            let row = self.row(Section::References, index);
            self.coordinate(
                u32_at(row, 0),
                Section::Declarations,
                self.section(Section::Declarations).count,
            )?;
            self.optional_coordinate(u32_at(row, 4), Section::Declarations)?;
            self.atom(u32_at(row, 8))?;
            self.atom(u32_at(row, 12))?;
            if ReferenceTag::decode(row[24]).is_none() || row[25..28] != [0; 3] {
                return Err(ImageError::DeclarationKind {
                    index,
                    found: row[24],
                    plane: Section::References,
                });
            }
            let start = u32_at(row, 16);
            let end = u32_at(row, 20);
            if start > end {
                return Err(ImageError::Span { index, start, end });
            }
        }
        Ok(())
    }

    fn section(self, section: Section) -> SectionBounds {
        self.sections[section as usize - 1]
    }

    fn row(self, section: Section, index: usize) -> &'image [u8] {
        let bounds = self.section(section);
        let start = bounds.offset + index * section.row_bytes();
        &self.bytes[start..start + section.row_bytes()]
    }

    fn atom_bytes(self, raw: u32) -> &'image [u8] {
        let Some(index) = self.valid_atom_index(raw) else {
            return &[];
        };
        let row = self.row(Section::Atoms, index);
        let Ok(offset) = usize::try_from(u32_at(row, 0)) else {
            return &[];
        };
        let Ok(length) = usize::try_from(u32_at(row, 4)) else {
            return &[];
        };
        let base = self.section(Section::AtomBytes).offset;
        &self.bytes[base + offset..base + offset + length]
    }

    fn atom(self, raw: u32) -> Result<Atom<'image>, ImageError> {
        let index = raw;
        if self.valid_atom_index(index).is_none() {
            return Err(ImageError::NameRange {
                index: usize::try_from(raw).unwrap_or(usize::MAX),
                offset: raw,
                length: 0,
                atom_bytes: self.section(Section::AtomBytes).count,
            });
        }
        Ok(Atom {
            bytes: self.atom_bytes(raw),
        })
    }

    fn valid_atom_index(self, raw: u32) -> Option<usize> {
        if raw == ABSENT {
            return None;
        }
        let index = usize::try_from(raw).ok()?;
        (index < self.section(Section::Atoms).count).then_some(index)
    }

    fn check_optional_atom(self, raw: u32) -> Result<(), ImageError> {
        if raw == ABSENT {
            return Ok(());
        }
        self.atom(raw).map(|_| ())
    }

    fn optional_atom(self, raw: u32) -> Option<Atom<'image>> {
        self.valid_atom_index(raw).map(|_| Atom {
            bytes: self.atom_bytes(raw),
        })
    }

    fn optional_type(self, raw: u32) -> Result<Option<TypeRef>, ImageError> {
        if raw == ABSENT {
            return Ok(None);
        }
        let index = self.coordinate(raw, Section::Types, self.section(Section::Types).count)?;
        Ok(Some(TypeRef(u32::try_from(index).unwrap_or(u32::MAX))))
    }

    fn optional_coordinate(self, raw: u32, section: Section) -> Result<Option<u32>, ImageError> {
        if raw == ABSENT {
            return Ok(None);
        }
        let index = self.coordinate(raw, section, self.section(section).count)?;
        Ok(Some(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    fn coordinate(self, raw: u32, _section: Section, bound: usize) -> Result<usize, ImageError> {
        let index = usize::try_from(raw).map_err(|_| ImageError::Span {
            index: usize::MAX,
            start: raw,
            end: u32::try_from(bound).unwrap_or(u32::MAX),
        })?;
        if index >= bound {
            return Err(ImageError::Span {
                index,
                start: raw,
                end: u32::try_from(bound).unwrap_or(u32::MAX),
            });
        }
        Ok(index)
    }

    fn range_start(self, raw: u32, section: Section, row: usize) -> Result<usize, ImageError> {
        let bound = self.section(section).count;
        usize::try_from(raw).map_err(|_| ImageError::Span {
            index: row,
            start: raw,
            end: u32::try_from(bound).unwrap_or(u32::MAX),
        })
    }

    fn range_end(
        self,
        start: usize,
        count: usize,
        section: Section,
        row: usize,
    ) -> Result<usize, ImageError> {
        let bound = self.section(section).count;
        let end = start.checked_add(count).ok_or(ImageError::Span {
            index: row,
            start: u32::try_from(start).unwrap_or(u32::MAX),
            end: u32::try_from(bound).unwrap_or(u32::MAX),
        })?;
        if end > bound {
            return Err(ImageError::Span {
                index: row,
                start: u32::try_from(end).unwrap_or(u32::MAX),
                end: u32::try_from(bound).unwrap_or(u32::MAX),
            });
        }
        Ok(end)
    }
}

/// Exact-size iterator over borrowed C# declaration rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
}

impl<'image> Iterator for DeclarationIter<'image> {
    type Item = Result<Declaration<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(Section::Declarations).count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(self.image.declaration_at(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.image.section(Section::Declarations).count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DeclarationIter<'_> {
    fn len(&self) -> usize {
        self.image.section(Section::Declarations).count - self.next
    }
}

/// Exact-size iterator over applied-attribute rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttributeIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
}

impl<'image> Iterator for AttributeIter<'image> {
    type Item = Result<Attribute<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(Section::Attributes).count {
            return None;
        }
        let image = self.image;
        let row = image.row(Section::Attributes, self.next);
        self.next += 1;
        Some(
            image
                .coordinate(
                    u32_at(row, 0),
                    Section::Declarations,
                    image.section(Section::Declarations).count,
                )
                .and_then(|declaration| {
                    image.atom(u32_at(row, 4)).map(|spelling| Attribute {
                        declaration: u32::try_from(declaration).unwrap_or(u32::MAX),
                        spelling,
                    })
                }),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.image.section(Section::Attributes).count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AttributeIter<'_> {
    fn len(&self) -> usize {
        self.image.section(Section::Attributes).count - self.next
    }
}

/// Exact-size iterator over documentation rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
}

impl<'image> Iterator for DocIter<'image> {
    type Item = Result<Doc<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(Section::Docs).count {
            return None;
        }
        let image = self.image;
        let row = image.row(Section::Docs, self.next);
        self.next += 1;
        Some(
            image
                .coordinate(
                    u32_at(row, 0),
                    Section::Declarations,
                    image.section(Section::Declarations).count,
                )
                .and_then(|declaration| {
                    let file = image.atom(u32_at(row, 4))?;
                    let xml = image.atom(u32_at(row, 16))?;
                    Ok(Doc {
                        declaration: u32::try_from(declaration).unwrap_or(u32::MAX),
                        file,
                        start: u32_at(row, 8),
                        end: u32_at(row, 12),
                        xml,
                    })
                }),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.image.section(Section::Docs).count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for DocIter<'_> {
    fn len(&self) -> usize {
        self.image.section(Section::Docs).count - self.next
    }
}

/// Exact-size iterator over compiler-resolved references.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceIter<'image> {
    image: CSharpImage<'image>,
    next: usize,
}

impl<'image> Iterator for ReferenceIter<'image> {
    type Item = Result<ResolvedReference<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(Section::References).count {
            return None;
        }
        let image = self.image;
        let row = image.row(Section::References, self.next);
        self.next += 1;
        Some(
            image
                .coordinate(
                    u32_at(row, 0),
                    Section::Declarations,
                    image.section(Section::Declarations).count,
                )
                .and_then(|owner| {
                    let target =
                        image.optional_coordinate(u32_at(row, 4), Section::Declarations)?;
                    let spelling = image.atom(u32_at(row, 8))?;
                    let file = image.atom(u32_at(row, 12))?;
                    let kind =
                        ReferenceTag::decode(row[24]).ok_or(ImageError::DeclarationKind {
                            index: owner,
                            found: row[24],
                            plane: Section::References,
                        })?;
                    Ok(ResolvedReference {
                        owner: u32::try_from(owner).unwrap_or(u32::MAX),
                        target,
                        spelling,
                        file,
                        start: u32_at(row, 16),
                        end: u32_at(row, 20),
                        kind,
                    })
                }),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.image.section(Section::References).count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ReferenceIter<'_> {
    fn len(&self) -> usize {
        self.image.section(Section::References).count - self.next
    }
}

/// Reads one canonical directory: every entry must carry its section tag and
/// closed row width, an exact byte count, and a contiguous canonical offset.
fn read_directory(bytes: &[u8]) -> Result<[SectionBounds; SECTION_COUNT], ImageError> {
    let mut sections = [SectionBounds {
        offset: 0,
        count: 0,
    }; SECTION_COUNT];
    let mut expected_offset = HEADER_BYTES;
    for (index, section) in sections.iter_mut().enumerate() {
        let plane = Section::from_directory_index(index);
        let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        let tag = u16_at(bytes, entry);
        if tag != plane as u16 {
            return Err(ImageError::Header(HeaderError::DirectoryTag {
                expected: plane as u16,
                found: tag,
            }));
        }
        let row_bytes = usize::from(u16_at(bytes, entry + 2));
        if row_bytes != plane.row_bytes() {
            return Err(ImageError::Header(HeaderError::DirectoryRowBytes {
                expected: plane.row_bytes(),
                found: row_bytes,
            }));
        }
        let count = usize::try_from(u32_at(bytes, entry + 4)).map_err(|_| {
            ImageError::Header(HeaderError::DirectoryByteCount {
                count: usize::MAX,
                row_bytes,
                found: usize::MAX,
            })
        })?;
        let offset = usize::try_from(u32_at(bytes, entry + 8)).map_err(|_| {
            ImageError::Header(HeaderError::DirectoryRange {
                offset: usize::MAX,
                length: 0,
                image_bytes: bytes.len(),
            })
        })?;
        let byte_count = usize::try_from(u32_at(bytes, entry + 12)).map_err(|_| {
            ImageError::Header(HeaderError::DirectoryRange {
                offset,
                length: usize::MAX,
                image_bytes: bytes.len(),
            })
        })?;
        let expected_bytes = count.checked_mul(row_bytes).ok_or({
            ImageError::Header(HeaderError::DirectoryByteCount {
                count,
                row_bytes,
                found: byte_count,
            })
        })?;
        if byte_count != expected_bytes {
            return Err(ImageError::Header(HeaderError::DirectoryByteCount {
                count,
                row_bytes,
                found: byte_count,
            }));
        }
        if offset != expected_offset {
            return Err(ImageError::Header(HeaderError::DirectoryOffset {
                expected: expected_offset,
                found: offset,
            }));
        }
        let end = offset.checked_add(byte_count).ok_or({
            ImageError::Header(HeaderError::DirectoryRange {
                offset,
                length: byte_count,
                image_bytes: bytes.len(),
            })
        })?;
        if end > bytes.len() {
            return Err(ImageError::Header(HeaderError::DirectoryRange {
                offset,
                length: byte_count,
                image_bytes: bytes.len(),
            }));
        }
        *section = SectionBounds { offset, count };
        expected_offset = end;
    }
    if expected_offset != bytes.len() {
        return Err(ImageError::Header(HeaderError::DirectoryRange {
            offset: expected_offset,
            length: 0,
            image_bytes: bytes.len(),
        }));
    }
    Ok(sections)
}

/// Exact Roslyn image validation error retained through compiler terminals.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ImageError {
    /// Fixed envelope validation failed.
    #[error("invalid C# authority image header: {0}")]
    Header(#[from] HeaderError),
    /// The image digest does not cover its fixed header and body.
    #[error("C# authority image checksum does not match")]
    Digest,
    /// A closed tag is outside its section's closed vocabulary.
    #[error("C# authority image row {index} in {plane:?} has unknown tag {found}")]
    DeclarationKind {
        /// Zero-based row whose closed tag was rejected.
        index: usize,
        /// Unrecognized raw tag byte.
        found: u8,
        /// The section whose tag vocabulary rejected the value.
        plane: Section,
    },
    /// A row names non-zero reserved bytes or an out-of-vocabulary flag set.
    #[error("C# authority image row {index} in {plane:?} has reserved bytes set")]
    DeclarationReserved {
        /// Zero-based row that has reserved bits set.
        index: usize,
        /// The section holding the row.
        plane: Section,
    },
    /// An atom range lies outside the image atom plane.
    #[error("C# authority image row {index} atom range is invalid")]
    NameRange {
        /// Zero-based row naming the invalid atom range.
        index: usize,
        /// Atom-plane coordinate supplied by the row.
        offset: u32,
        /// Atom-plane byte length supplied by the row.
        length: u32,
        /// Complete atom-plane byte capacity.
        atom_bytes: usize,
    },
    /// A referenced atom is not valid UTF-8.
    #[error("C# authority image row {index} atom is not UTF-8")]
    NameUtf8 {
        /// Zero-based row whose atom cannot be decoded as UTF-8.
        index: usize,
    },
    /// A row's byte range is inverted or a coordinate exceeds its section.
    #[error("C# authority image row {index} range {start}..{end} is invalid")]
    Span {
        /// Zero-based row with the inverted range or out-of-plane coordinate.
        index: usize,
        /// Inclusive byte coordinate or observed coordinate.
        start: u32,
        /// Exclusive byte coordinate or the section's exclusive bound.
        end: u32,
    },
    /// A type-node kind's closed child arity was violated before lowering.
    #[error("C# authority type row {index} {kind:?} has {actual} children; expected {min}..={max}")]
    TypeChildCount {
        index: usize,
        kind: TypeNodeKind,
        min: usize,
        max: usize,
        actual: usize,
    },
}

/// Exact fixed-envelope error returned by [`CSharpImage::open`].
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HeaderError {
    /// The supplied image ends before the complete fixed header.
    #[error("truncated header: found {actual} bytes, need at least {HEADER_BYTES}")]
    Truncated {
        /// Number of image bytes made available to the decoder.
        actual: usize,
    },
    /// The image magic differs from `NCAI`.
    #[error("unexpected magic {found:?}")]
    Magic {
        /// Raw four-byte magic found in the image header.
        found: [u8; 4],
    },
    /// The image names an unsupported wire version.
    #[error("unsupported image version {found}")]
    Version {
        /// Unsupported raw image version.
        found: u16,
    },
    /// The header length differs from this version's fixed width.
    #[error("unexpected fixed header length {found}")]
    Length {
        /// Fixed-header length encoded by the image.
        found: usize,
    },
    /// The section count differs from this version's fixed directory.
    #[error("unexpected section count {found}")]
    SectionCount {
        /// Encoded section count.
        found: usize,
    },
    /// Declared body dimensions differ from the supplied bytes.
    #[error("declared body length {declared} differs from actual {actual}")]
    BodyLength {
        /// Body length declared by the fixed header.
        declared: usize,
        /// Body length available after the fixed header.
        actual: usize,
    },
    /// Reserved header bytes are non-zero.
    #[error("reserved header bytes are non-zero")]
    Reserved,
    /// A directory entry's section tag differs from its canonical position.
    #[error("directory entry expected tag {expected}, found {found}")]
    DirectoryTag {
        /// The required canonical tag.
        expected: u16,
        /// The encoded tag.
        found: u16,
    },
    /// A directory entry's row width differs from the section's closed layout.
    #[error("directory entry expected row width {expected}, found {found}")]
    DirectoryRowBytes {
        /// The required fixed row width.
        expected: usize,
        /// The encoded fixed row width.
        found: usize,
    },
    /// A section's byte count is not exactly count times row width.
    #[error("section count {count} and row width {row_bytes} do not produce {found} bytes")]
    DirectoryByteCount {
        /// The encoded row count.
        count: usize,
        /// The encoded fixed row width.
        row_bytes: usize,
        /// The encoded aggregate byte count.
        found: usize,
    },
    /// A section's byte range is not contiguous in canonical order.
    #[error("section expected offset {expected}, found {found}")]
    DirectoryOffset {
        /// The required canonical body offset.
        expected: usize,
        /// The offset encoded by the image.
        found: usize,
    },
    /// A section's byte range overflows or extends beyond the image.
    #[error("section offset {offset} and length {length} exceed image length {image_bytes}")]
    DirectoryRange {
        /// The encoded section start offset.
        offset: usize,
        /// The encoded aggregate section byte length.
        length: usize,
        /// The complete supplied image length.
        image_bytes: usize,
    },
}

const fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

const fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
