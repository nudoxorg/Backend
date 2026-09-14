//! Validated borrowed access to the Java compiler authority image.
//! The JDK writes fixed-width planes once; Rust validates their envelope once.
//! Consumers retain caller-owned bytes and traverse typed coordinates without JSON DTOs.

use core::{ops::Deref, str};

use sha2::{Digest, Sha256};
use thiserror::Error;

const MAGIC: [u8; 4] = *b"NJAI";
const V1: u16 = 1;
const V2: u16 = 2;
const DIRECTORY_OFFSET: usize = 48;
const DIRECTORY_ENTRY_BYTES: usize = 16;
const V1_SECTIONS: usize = 8;
const V2_SECTIONS: usize = 10;
const V1_HEADER_BYTES: usize = DIRECTORY_OFFSET + DIRECTORY_ENTRY_BYTES * V1_SECTIONS;
const ABSENT: u32 = u32::MAX;
const V1_DIGEST_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v1\0";
const V2_DIGEST_DOMAIN: &[u8] = b"nudox.java.authority.image.sha256.v2\0";
const MODIFIER_PUBLIC: u32 = 1 << 0;
const MODIFIER_PROTECTED: u32 = 1 << 1;
const MODIFIER_PRIVATE: u32 = 1 << 2;
const MODIFIER_ABSTRACT: u32 = 1 << 3;
const MODIFIER_STATIC: u32 = 1 << 4;
const MODIFIER_FINAL: u32 = 1 << 5;
const MODIFIER_TRANSIENT: u32 = 1 << 6;
const MODIFIER_VOLATILE: u32 = 1 << 7;
const MODIFIER_SYNCHRONIZED: u32 = 1 << 8;
const MODIFIER_NATIVE: u32 = 1 << 9;
const MODIFIER_STRICTFP: u32 = 1 << 10;
const MODIFIER_DEFAULT: u32 = 1 << 11;
const MODIFIER_SEALED: u32 = 1 << 12;
const MODIFIER_NON_SEALED: u32 = 1 << 13;
const KNOWN_MODIFIERS: u32 = MODIFIER_PUBLIC
    | MODIFIER_PROTECTED
    | MODIFIER_PRIVATE
    | MODIFIER_ABSTRACT
    | MODIFIER_STATIC
    | MODIFIER_FINAL
    | MODIFIER_TRANSIENT
    | MODIFIER_VOLATILE
    | MODIFIER_SYNCHRONIZED
    | MODIFIER_NATIVE
    | MODIFIER_STRICTFP
    | MODIFIER_DEFAULT
    | MODIFIER_SEALED
    | MODIFIER_NON_SEALED;

/// A Java release selected before the compiler task is created.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaRelease {
    /// Java 8 language and platform contract.
    Java8 = 8,
    /// Java 11 language and platform contract.
    Java11 = 11,
    /// Java 17 language and platform contract.
    Java17 = 17,
    /// Java 21 language and platform contract.
    Java21 = 21,
    /// Java 25 language and platform contract.
    Java25 = 25,
}

impl JavaRelease {
    const fn decode(raw: u16) -> Option<Self> {
        match raw {
            8 => Some(Self::Java8),
            11 => Some(Self::Java11),
            17 => Some(Self::Java17),
            21 => Some(Self::Java21),
            25 => Some(Self::Java25),
            _ => None,
        }
    }
}

/// A fixed image plane in canonical byte order.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImagePlane {
    /// Fixed-width atom offsets and lengths.
    Atoms = 1,
    /// Concatenated UTF-8 atom bytes.
    AtomBytes = 2,
    /// Fixed-width recursive type records.
    Types = 3,
    /// Type-child coordinates.
    TypeChildren = 4,
    /// Executable symbol records.
    Symbols = 5,
    /// Symbol parameter type coordinates.
    SymbolParameters = 6,
    /// Declaration records.
    Declarations = 7,
    /// Resolved call-reference records.
    References = 8,
    /// Per-declaration ranges into extension entries.
    DeclarationExtensions = 9,
    /// Throws, annotations, and record-component entries.
    ExtensionEntries = 10,
}

impl ImagePlane {
    const fn from_directory_index(index: usize) -> Self {
        match index {
            0 => Self::Atoms,
            1 => Self::AtomBytes,
            2 => Self::Types,
            3 => Self::TypeChildren,
            4 => Self::Symbols,
            5 => Self::SymbolParameters,
            6 => Self::Declarations,
            7 => Self::References,
            8 => Self::DeclarationExtensions,
            _ => Self::ExtensionEntries,
        }
    }

    const fn row_bytes(self) -> usize {
        match self {
            Self::Atoms => 8,
            Self::AtomBytes => 1,
            Self::Types | Self::Symbols => 16,
            Self::TypeChildren | Self::SymbolParameters => 4,
            Self::Declarations => 32,
            Self::References => 20,
            Self::DeclarationExtensions | Self::ExtensionEntries => 8,
        }
    }
}

/// A validated immutable Java authority image borrowing its caller's bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JavaImage<'image> {
    bytes: &'image [u8],
    sections: [Section; V2_SECTIONS],
    section_count: usize,
    version: u16,
    /// The release passed to the attributed `JavacTask`.
    pub release: JavaRelease,
}

impl<'image> JavaImage<'image> {
    /// Opens one complete image after checking its canonical envelope and all planes.
    pub fn open(bytes: &'image [u8]) -> Result<Self, ImageError> {
        if bytes.len() < V1_HEADER_BYTES {
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
        let section_count = match version {
            V1 => V1_SECTIONS,
            V2 => V2_SECTIONS,
            _ => return Err(ImageError::Header(HeaderError::Version { found: version })),
        };
        let header_size = DIRECTORY_OFFSET + DIRECTORY_ENTRY_BYTES * section_count;
        if bytes.len() < header_size {
            return Err(ImageError::Header(HeaderError::Truncated {
                actual: bytes.len(),
            }));
        }
        let header_bytes = u16_at(bytes, 6);
        if usize::from(header_bytes) != header_size {
            return Err(ImageError::Header(HeaderError::Length {
                found: header_bytes,
            }));
        }
        let release_raw = u16_at(bytes, 8);
        let release =
            JavaRelease::decode(release_raw).ok_or(ImageError::Header(HeaderError::Release {
                found: release_raw,
            }))?;
        let declared_section_count = u16_at(bytes, 10);
        if usize::from(declared_section_count) != section_count {
            return Err(ImageError::Header(HeaderError::SectionCount {
                found: declared_section_count,
            }));
        }

        let body_bytes = usize::try_from(u32_at(bytes, 12)).map_err(|_| {
            ImageError::Header(HeaderError::BodyLength {
                declared: usize::MAX,
                actual: bytes.len() - header_size,
            })
        })?;
        let image_bytes = header_size
            .checked_add(body_bytes)
            .ok_or(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len() - header_size,
            }))?;
        if image_bytes != bytes.len() {
            return Err(ImageError::Header(HeaderError::BodyLength {
                declared: body_bytes,
                actual: bytes.len() - header_size,
            }));
        }

        let sections = read_directory(bytes, section_count, header_size)?;
        let image = Self {
            bytes,
            sections,
            section_count,
            version,
            release,
        };
        image.validate_digest()?;
        image.validate_planes()?;
        Ok(image)
    }

    /// Borrows the complete canonical image bytes.
    #[must_use]
    pub const fn as_bytes(self) -> &'image [u8] {
        self.bytes
    }

    /// Iterates atom bytes in canonical directory order.
    #[must_use]
    pub const fn atoms(self) -> AtomIter<'image> {
        AtomIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates fixed-width type facts in canonical order.
    #[must_use]
    pub const fn types(self) -> TypeIter<'image> {
        TypeIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates executable symbols in canonical order.
    #[must_use]
    pub const fn symbols(self) -> SymbolIter<'image> {
        SymbolIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates declaration facts in canonical order.
    #[must_use]
    pub const fn declarations(self) -> DeclarationIter<'image> {
        DeclarationIter {
            image: self,
            next: 0,
        }
    }

    /// Iterates compiler-resolved call references in canonical order.
    #[must_use]
    pub const fn references(self) -> ReferenceIter<'image> {
        ReferenceIter {
            image: self,
            next: 0,
        }
    }

    /// Returns the borrowed extension cursor for one declaration ordinal.
    pub fn declaration_extensions(
        self,
        ordinal: usize,
    ) -> Result<ExtensionIter<'image>, ImageError> {
        if self.version == V1 {
            return Ok(ExtensionIter {
                image: self,
                next: 0,
                end: 0,
            });
        }
        let ordinal = row_index(
            ordinal,
            ImagePlane::Declarations,
            self.section(ImagePlane::Declarations).count,
        )? as usize;
        let row = self.row(ImagePlane::DeclarationExtensions, ordinal);
        self.extension_range(u32_at(row, 0) as usize, u32_at(row, 4) as usize)
    }

    /// Resolves one type coordinate into its borrowed type fact.
    pub fn type_fact(self, reference: TypeRef) -> Result<TypeFact<'image>, ImageError> {
        self.type_at(reference.ordinal)
    }

    /// Resolves one executable coordinate into its borrowed symbol fact.
    pub fn symbol(self, reference: SymbolRef) -> Result<Symbol<'image>, ImageError> {
        self.symbol_at(reference.ordinal)
    }

    fn validate_digest(self) -> Result<(), ImageError> {
        let mut digest = Sha256::new();
        digest.update(if self.version == V1 {
            V1_DIGEST_DOMAIN
        } else {
            V2_DIGEST_DOMAIN
        });
        digest.update(&self.bytes[..16]);
        let header_bytes = self.header_bytes();
        digest.update(&self.bytes[DIRECTORY_OFFSET..header_bytes]);
        digest.update(&self.bytes[header_bytes..]);
        if digest.finalize().as_slice() != &self.bytes[16..DIRECTORY_OFFSET] {
            return Err(ImageError::Digest);
        }
        Ok(())
    }

    fn validate_planes(self) -> Result<(), ImageError> {
        self.validate_atoms()?;
        self.validate_types()?;
        self.validate_symbols()?;
        self.validate_declarations()?;
        self.validate_references()?;
        if self.version == V2 {
            self.validate_extensions()?;
        }
        Ok(())
    }

    fn validate_atoms(self) -> Result<(), ImageError> {
        let mut next_byte = 0;
        for index in 0..self.section(ImagePlane::Atoms).count {
            let row = self.row(ImagePlane::Atoms, index);
            let offset = usize::try_from(u32_at(row, 0)).map_err(|_| ImageError::Atom {
                index,
                cause: AtomError::Range,
            })?;
            let length = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::Atom {
                index,
                cause: AtomError::Range,
            })?;
            if offset != next_byte {
                return Err(ImageError::Atom {
                    index,
                    cause: AtomError::NonCanonicalOffset { found: offset },
                });
            }
            let end = offset.checked_add(length).ok_or(ImageError::Atom {
                index,
                cause: AtomError::Range,
            })?;
            if end > self.section(ImagePlane::AtomBytes).count {
                return Err(ImageError::Atom {
                    index,
                    cause: AtomError::Range,
                });
            }
            if str::from_utf8(self.atom_bytes(offset, length)).is_err() {
                return Err(ImageError::Atom {
                    index,
                    cause: AtomError::Utf8,
                });
            }
            next_byte = end;
        }
        if next_byte != self.section(ImagePlane::AtomBytes).count {
            return Err(ImageError::Atom {
                index: self.section(ImagePlane::Atoms).count,
                cause: AtomError::TrailingBytes,
            });
        }
        Ok(())
    }

    fn validate_types(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::TypeChildren).count {
            self.type_at(u32_at(self.row(ImagePlane::TypeChildren, index), 0))?;
        }
        for index in 0..self.section(ImagePlane::Types).count {
            self.type_at(row_index(
                index,
                ImagePlane::Types,
                self.section(ImagePlane::Types).count,
            )?)?;
        }
        Ok(())
    }

    fn validate_symbols(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::SymbolParameters).count {
            self.type_at(u32_at(self.row(ImagePlane::SymbolParameters, index), 0))?;
        }
        for index in 0..self.section(ImagePlane::Symbols).count {
            self.symbol_at(row_index(
                index,
                ImagePlane::Symbols,
                self.section(ImagePlane::Symbols).count,
            )?)?;
        }
        Ok(())
    }

    fn validate_declarations(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::Declarations).count {
            Declaration::decode(self, self.row(ImagePlane::Declarations, index))?;
        }
        Ok(())
    }

    fn validate_references(self) -> Result<(), ImageError> {
        for index in 0..self.section(ImagePlane::References).count {
            Reference::decode(self, self.row(ImagePlane::References, index))?;
        }
        Ok(())
    }

    fn validate_extensions(self) -> Result<(), ImageError> {
        let mut previous_end = 0;
        for index in 0..self.section(ImagePlane::DeclarationExtensions).count {
            let row = self.row(ImagePlane::DeclarationExtensions, index);
            let start = u32_at(row, 0) as usize;
            let count = u32_at(row, 4) as usize;
            if (count == 0 && start != 0) || (count != 0 && start < previous_end) {
                return Err(ImageError::ChildRange {
                    plane: ImagePlane::ExtensionEntries,
                    start,
                    count,
                    upper_bound: self.section(ImagePlane::ExtensionEntries).count,
                });
            }
            for entry in self.extension_range(start, count)? {
                let entry = entry?;
                match entry {
                    DeclarationExtension::Throws(reference) => {
                        self.type_at(reference.ordinal)?;
                    }
                    DeclarationExtension::Annotation(_) => {}
                    DeclarationExtension::RecordComponent(index) => {
                        let declaration_index = coordinate(
                            index as u32,
                            ImagePlane::Declarations,
                            self.section(ImagePlane::Declarations).count,
                        )?;
                        let declaration = Declaration::decode(
                            self,
                            self.row(ImagePlane::Declarations, declaration_index),
                        )?;
                        if !matches!(
                            declaration.kind,
                            DeclarationKind::Field | DeclarationKind::EnumConstant
                        ) {
                            return Err(ImageError::RecordComponentKind { index });
                        }
                    }
                }
            }
            previous_end = start + count;
        }
        Ok(())
    }

    fn extension_range(
        self,
        start: usize,
        count: usize,
    ) -> Result<ExtensionIter<'image>, ImageError> {
        let upper_bound = self.section(ImagePlane::ExtensionEntries).count;
        let end = start.checked_add(count).ok_or(ImageError::ChildRange {
            plane: ImagePlane::ExtensionEntries,
            start,
            count,
            upper_bound,
        })?;
        if end > upper_bound {
            return Err(ImageError::ChildRange {
                plane: ImagePlane::ExtensionEntries,
                start,
                count,
                upper_bound,
            });
        }
        Ok(ExtensionIter {
            image: self,
            next: start,
            end,
        })
    }

    fn section(self, plane: ImagePlane) -> Section {
        self.sections[plane as usize - 1]
    }

    fn header_bytes(self) -> usize {
        DIRECTORY_OFFSET + DIRECTORY_ENTRY_BYTES * self.section_count
    }

    fn row(self, plane: ImagePlane, index: usize) -> &'image [u8] {
        let section = self.section(plane);
        let start = section.offset + index * plane.row_bytes();
        &self.bytes[start..start + plane.row_bytes()]
    }

    fn atom_bytes(self, offset: usize, length: usize) -> &'image [u8] {
        let start = self.section(ImagePlane::AtomBytes).offset + offset;
        &self.bytes[start..start + length]
    }

    fn atom(self, raw: u32) -> Result<Atom<'image>, ImageError> {
        if raw == ABSENT {
            return Err(ImageError::AbsentAtom);
        }
        let index = usize::try_from(raw).map_err(|_| ImageError::Coordinate {
            plane: ImagePlane::Atoms,
            index: usize::MAX,
            upper_bound: self.section(ImagePlane::Atoms).count,
        })?;
        if index >= self.section(ImagePlane::Atoms).count {
            return Err(ImageError::Coordinate {
                plane: ImagePlane::Atoms,
                index,
                upper_bound: self.section(ImagePlane::Atoms).count,
            });
        }
        let row = self.row(ImagePlane::Atoms, index);
        let offset = usize::try_from(u32_at(row, 0)).map_err(|_| ImageError::Atom {
            index,
            cause: AtomError::Range,
        })?;
        let length = usize::try_from(u32_at(row, 4)).map_err(|_| ImageError::Atom {
            index,
            cause: AtomError::Range,
        })?;
        Ok(Atom {
            bytes: self.atom_bytes(offset, length),
        })
    }

    fn optional_atom(self, raw: u32) -> Result<Option<Atom<'image>>, ImageError> {
        if raw == ABSENT {
            Ok(None)
        } else {
            self.atom(raw).map(Some)
        }
    }

    fn type_at(self, raw: u32) -> Result<TypeFact<'image>, ImageError> {
        let index = coordinate(
            raw,
            ImagePlane::Types,
            self.section(ImagePlane::Types).count,
        )?;
        TypeFact::decode(self, index)
    }

    fn symbol_at(self, raw: u32) -> Result<Symbol<'image>, ImageError> {
        let index = coordinate(
            raw,
            ImagePlane::Symbols,
            self.section(ImagePlane::Symbols).count,
        )?;
        Symbol::decode(self, index)
    }
}

/// One atom borrowed directly from the image's UTF-8 atom plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Atom<'image> {
    /// The canonical UTF-8 atom bytes.
    pub bytes: &'image [u8],
}

impl Deref for Atom<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes
    }
}

impl<'image> Atom<'image> {
    /// Borrows the atom as UTF-8 after the image's one-time UTF-8 validation.
    pub fn utf8(self) -> Result<&'image str, str::Utf8Error> {
        str::from_utf8(self.bytes)
    }
}

/// Immutable validated image-coordinate fact exposed by typed image handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JavaImageCoordinateView {
    /// Exact zero-based authority-image coordinate.
    pub ordinal: u32,
}

/// An opaque validated coordinate in the type plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeRef(JavaImageCoordinateView);

impl Deref for TypeRef {
    type Target = JavaImageCoordinateView;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// An opaque validated coordinate in the executable-symbol plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolRef(JavaImageCoordinateView);

impl Deref for SymbolRef {
    type Target = JavaImageCoordinateView;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A closed Java type form carried by one type-plane record.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeKind {
    /// One of Java's primitive types.
    Primitive = 1,
    /// The `void` return type.
    Void = 2,
    /// A declared class, interface, enum, annotation, or record type.
    Declared = 3,
    /// An array type.
    Array = 4,
    /// A declared type variable.
    Variable = 5,
    /// A wildcard with optional bounds.
    Wildcard = 6,
    /// An intersection type.
    Intersection = 7,
    /// A multi-catch union type.
    Union = 8,
    /// A compiler error type.
    Error = 9,
    /// Javac's no-type sentinel.
    None = 10,
    /// Java's null type.
    Null = 11,
}

impl TypeKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Primitive),
            2 => Some(Self::Void),
            3 => Some(Self::Declared),
            4 => Some(Self::Array),
            5 => Some(Self::Variable),
            6 => Some(Self::Wildcard),
            7 => Some(Self::Intersection),
            8 => Some(Self::Union),
            9 => Some(Self::Error),
            10 => Some(Self::None),
            11 => Some(Self::Null),
            _ => None,
        }
    }
}

/// A borrowed Java type fact with typed child coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeFact<'image> {
    /// The closed Java type form.
    pub kind: TypeKind,
    /// Optional atom-backed spelling for primitive, declared, variable, or error forms.
    pub spelling: Option<Atom<'image>>,
    /// Kind-specific flags, such as wildcard-bound or declared-owner presence.
    pub flags: u8,
    /// Child type coordinates in canonical order.
    pub children: TypeChildren<'image>,
}

/// An exact-size iterator over validated type coordinates in one child plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeChildren<'image> {
    image: JavaImage<'image>,
    plane: ImagePlane,
    next: usize,
    end: usize,
}

impl Iterator for TypeChildren<'_> {
    type Item = TypeRef;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let reference = TypeRef(JavaImageCoordinateView {
            ordinal: u32_at(self.image.row(self.plane, self.next), 0),
        });
        self.next += 1;
        Some(reference)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for TypeChildren<'_> {
    fn len(&self) -> usize {
        self.end - self.next
    }
}

/// A compiler-resolved executable symbol with parameter type coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Symbol<'image> {
    /// The canonical qualified type that declares the executable.
    pub owner: Atom<'image>,
    /// The constructor or method name.
    pub name: Atom<'image>,
    /// Parameter type coordinates in declared order.
    pub parameters: TypeChildren<'image>,
}

/// A closed declaration kind provided by javac's element model.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// A named Java module.
    Module = 1,
    /// A Java package.
    Package = 2,
    /// A concrete or abstract class.
    Class = 3,
    /// An interface.
    Interface = 4,
    /// A record.
    Record = 5,
    /// An enum type.
    Enum = 6,
    /// An annotation interface.
    Annotation = 7,
    /// An ordinary field.
    Field = 8,
    /// An enum constant.
    EnumConstant = 9,
    /// A constructor.
    Constructor = 10,
    /// A method.
    Method = 11,
}

impl DeclarationKind {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Module),
            2 => Some(Self::Package),
            3 => Some(Self::Class),
            4 => Some(Self::Interface),
            5 => Some(Self::Record),
            6 => Some(Self::Enum),
            7 => Some(Self::Annotation),
            8 => Some(Self::Field),
            9 => Some(Self::EnumConstant),
            10 => Some(Self::Constructor),
            11 => Some(Self::Method),
            _ => None,
        }
    }
}

/// A closed origin classification returned by javac's element API.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Explicitly written by the source author.
    Explicit = 1,
    /// Mandated by the language specification.
    Mandated = 2,
    /// Synthesized by the compiler.
    Synthetic = 3,
}

impl Origin {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Explicit),
            2 => Some(Self::Mandated),
            3 => Some(Self::Synthetic),
            _ => None,
        }
    }
}

/// A closed documentation form that preserves absence separately.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocFlavor {
    /// No documentation was attached to the element.
    Absent = 0,
    /// A traditional Javadoc comment.
    Traditional = 1,
    /// A Markdown documentation comment.
    Markdown = 2,
}

/// A validated bitset over Java's closed modifier vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Modifiers {
    /// The canonical modifier bits emitted by javac.
    pub bits: u32,
}

impl DocFlavor {
    const fn decode(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Absent),
            1 => Some(Self::Traditional),
            2 => Some(Self::Markdown),
            _ => None,
        }
    }
}

/// A borrowed declaration fact with closed attributes and typed coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Declaration<'image> {
    /// The declaration's closed Java kind.
    pub kind: DeclarationKind,
    /// Whether javac classifies the declaration as explicit, mandated, or synthetic.
    pub origin: Origin,
    /// The form of its optional documentation.
    pub documentation_flavor: DocFlavor,
    /// Bitset of Java modifiers using the image's fixed modifier vocabulary.
    pub modifiers: Modifiers,
    /// The declaration name or qualified module/package/type name.
    pub name: Atom<'image>,
    /// The qualified enclosing type, when the declaration is a member.
    pub owner: Option<Atom<'image>>,
    /// The exact Javadoc text, when present.
    pub documentation: Option<Atom<'image>>,
    /// The overload-distinguishing executable identity, when applicable.
    pub symbol: Option<SymbolRef>,
    /// The declaration's resolved type, when applicable.
    pub semantic_type: Option<TypeRef>,
}

/// One typed fact attached to a declaration in a v2 image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationExtension<'image> {
    /// A declared exception type coordinate.
    Throws(TypeRef),
    /// An annotation's exact javac spelling.
    Annotation(Atom<'image>),
    /// The declaration ordinal of a record component field.
    RecordComponent(usize),
}

/// A resolved method-invocation edge with Javac's UTF-16 source coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reference<'image> {
    /// The executable that lexically owns the invocation.
    pub owner: SymbolRef,
    /// The executable selected by javac overload resolution.
    pub target: SymbolRef,
    /// The compiler-reported source-file name.
    pub file: Atom<'image>,
    /// Inclusive UTF-16 offset of the selected member expression.
    pub start: u32,
    /// Exclusive UTF-16 offset of the selected member expression.
    pub end: u32,
}

/// An exact typed rejection from Java authority image validation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ImageError {
    /// The fixed image header violates a closed header rule.
    #[error("invalid Java authority image header: {0}")]
    Header(#[from] HeaderError),
    /// One canonical plane directory entry is invalid.
    #[error("invalid {plane:?} directory entry: {cause}")]
    Section {
        /// The plane whose directory entry is invalid.
        plane: ImagePlane,
        /// The exact directory violation.
        cause: SectionError,
    },
    /// The domain-separated SHA-256 checksum does not match the bytes.
    #[error("Java authority image checksum does not match")]
    Digest,
    /// One atom directory entry or UTF-8 byte range is invalid.
    #[error("invalid atom {index}: {cause}")]
    Atom {
        /// The atom-table index whose representation is invalid.
        index: usize,
        /// The exact atom-plane violation.
        cause: AtomError,
    },
    /// A required atom coordinate was encoded as the absent sentinel.
    #[error("a required Java authority atom is absent")]
    AbsentAtom,
    /// A coordinate points outside its target plane.
    #[error("invalid {plane:?} coordinate {index}; upper bound is {upper_bound}")]
    Coordinate {
        /// The target plane named by the coordinate.
        plane: ImagePlane,
        /// The offending decoded coordinate.
        index: usize,
        /// The exclusive target-plane bound.
        upper_bound: usize,
    },
    /// A fixed-width plane row carries an unknown closed tag.
    #[error("unknown {plane:?} tag {found}")]
    Tag {
        /// The plane whose closed tag vocabulary rejected the value.
        plane: ImagePlane,
        /// The unrecognized encoded tag.
        found: u8,
    },
    /// A child coordinate range overflows or exceeds its plane.
    #[error(
        "invalid {plane:?} child range starting at {start} with {count} entries; upper bound is {upper_bound}"
    )]
    ChildRange {
        /// The plane holding child-coordinate rows.
        plane: ImagePlane,
        /// The declared first child row.
        start: usize,
        /// The declared number of child rows.
        count: usize,
        /// The exclusive child-plane bound.
        upper_bound: usize,
    },
    /// A reference carries an inverted UTF-16 source range.
    #[error("invalid Java reference UTF-16 range {start}..{end}")]
    ReferenceRange {
        /// The reported source start coordinate.
        start: u32,
        /// The reported source end coordinate.
        end: u32,
    },
    /// Documentation presence disagrees with its closed documentation flavor.
    #[error("documentation flavor and atom presence disagree")]
    DocumentationPresence,
    /// A declaration modifier bitset names a modifier outside the closed vocabulary.
    #[error("unknown Java modifier bits {found:#x}")]
    ModifierBits {
        /// The encoded modifier bitset that contains an unrecognized bit.
        found: u32,
    },
    /// A record-component entry points at a non-field declaration.
    #[error("record component declaration {index} is not a field or enum constant")]
    RecordComponentKind { index: usize },
    /// Extension entry reserved bytes are non-zero.
    #[error("extension entry reserved bytes are non-zero")]
    ExtensionReserved,
}

/// A closed violation of the fixed Java authority image header.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum HeaderError {
    /// The complete fixed header is unavailable.
    #[error("truncated header: found {actual} bytes, need at least {V1_HEADER_BYTES}")]
    Truncated {
        /// The bytes that were available to parse.
        actual: usize,
    },
    /// The four-byte image magic differs from `NJAI`.
    #[error("unexpected magic {found:?}")]
    Magic {
        /// The four raw magic bytes found in the image.
        found: [u8; 4],
    },
    /// The image version is not supported by this reader.
    #[error("unsupported image version {found}")]
    Version {
        /// The encoded image version.
        found: u16,
    },
    /// The encoded fixed-header length differs from this format version.
    #[error("unexpected fixed header length {found}")]
    Length {
        /// The encoded fixed-header byte length.
        found: u16,
    },
    /// The image names a release outside the closed compiler profile vocabulary.
    #[error("unsupported Java release {found}")]
    Release {
        /// The encoded Java source release.
        found: u16,
    },
    /// The image names a directory cardinality outside this format version.
    #[error("unexpected directory section count {found}")]
    SectionCount {
        /// The encoded fixed-directory section count.
        found: u16,
    },
    /// The declared body length does not equal the supplied bytes after the header.
    #[error("declared body length {declared} differs from actual {actual}")]
    BodyLength {
        /// The body length declared by the image header.
        declared: usize,
        /// The available byte length following the fixed header.
        actual: usize,
    },
}

/// A closed violation of one fixed directory entry.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SectionError {
    /// The plane tag does not match its fixed canonical directory position.
    #[error("expected tag {expected}, found {found}")]
    Tag {
        /// The required tag for this directory position.
        expected: u16,
        /// The tag encoded by the image.
        found: u16,
    },
    /// The fixed row width differs from the plane's closed row layout.
    #[error("expected row width {expected}, found {found}")]
    RowBytes {
        /// The required fixed row width.
        expected: usize,
        /// The encoded fixed row width.
        found: usize,
    },
    /// The declared byte count is not exactly count times fixed row width.
    #[error("count {count} and row width {row_bytes} do not produce {found} bytes")]
    ByteCount {
        /// The encoded row count.
        count: usize,
        /// The encoded fixed row width.
        row_bytes: usize,
        /// The encoded aggregate byte count.
        found: usize,
    },
    /// The plane's byte range is not contiguous with the previous canonical plane.
    #[error("expected offset {expected}, found {found}")]
    Offset {
        /// The required canonical body offset.
        expected: usize,
        /// The offset encoded by the image.
        found: usize,
    },
    /// The plane range overflows or extends beyond the supplied image.
    #[error("offset {offset} and length {length} exceed image length {image_bytes}")]
    Range {
        /// The encoded plane start offset.
        offset: usize,
        /// The encoded aggregate plane byte length.
        length: usize,
        /// The complete supplied image length.
        image_bytes: usize,
    },
}

/// A closed violation of the atom table and atom-byte plane.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AtomError {
    /// The atom does not begin at the expected canonical atom-byte offset.
    #[error("noncanonical atom offset {found}")]
    NonCanonicalOffset {
        /// The unexpected atom-byte offset.
        found: usize,
    },
    /// The atom range overflows or exceeds the atom-byte plane.
    #[error("atom range is out of bounds")]
    Range,
    /// The atom bytes are not UTF-8.
    #[error("atom bytes are not UTF-8")]
    Utf8,
    /// Bytes remain in the atom-byte plane after the final atom.
    #[error("atom-byte plane has trailing unclaimed bytes")]
    TrailingBytes,
}

/// An exact-size iterator over borrowed atoms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomIter<'image> {
    image: JavaImage<'image>,
    next: usize,
}

impl<'image> Iterator for AtomIter<'image> {
    type Item = Result<Atom<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(ImagePlane::Atoms).count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(
            row_index(
                index,
                ImagePlane::Atoms,
                self.image.section(ImagePlane::Atoms).count,
            )
            .and_then(|raw| self.image.atom(raw)),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        exact_hint(self.image.section(ImagePlane::Atoms).count, self.next)
    }
}

impl ExactSizeIterator for AtomIter<'_> {
    fn len(&self) -> usize {
        self.image.section(ImagePlane::Atoms).count - self.next
    }
}

/// An exact-size iterator over borrowed type facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeIter<'image> {
    image: JavaImage<'image>,
    next: usize,
}

impl<'image> Iterator for TypeIter<'image> {
    type Item = Result<TypeFact<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(ImagePlane::Types).count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(
            row_index(
                index,
                ImagePlane::Types,
                self.image.section(ImagePlane::Types).count,
            )
            .and_then(|raw| self.image.type_at(raw)),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        exact_hint(self.image.section(ImagePlane::Types).count, self.next)
    }
}

impl ExactSizeIterator for TypeIter<'_> {
    fn len(&self) -> usize {
        self.image.section(ImagePlane::Types).count - self.next
    }
}

/// An exact-size iterator over borrowed executable symbols.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolIter<'image> {
    image: JavaImage<'image>,
    next: usize,
}

impl<'image> Iterator for SymbolIter<'image> {
    type Item = Result<Symbol<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(ImagePlane::Symbols).count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(
            row_index(
                index,
                ImagePlane::Symbols,
                self.image.section(ImagePlane::Symbols).count,
            )
            .and_then(|raw| self.image.symbol_at(raw)),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        exact_hint(self.image.section(ImagePlane::Symbols).count, self.next)
    }
}

impl ExactSizeIterator for SymbolIter<'_> {
    fn len(&self) -> usize {
        self.image.section(ImagePlane::Symbols).count - self.next
    }
}

/// An exact-size iterator over borrowed declaration facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationIter<'image> {
    image: JavaImage<'image>,
    next: usize,
}

/// An exact-size borrowed cursor over one declaration's v2 extension range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionIter<'image> {
    image: JavaImage<'image>,
    next: usize,
    end: usize,
}

impl<'image> Iterator for ExtensionIter<'image> {
    type Item = Result<DeclarationExtension<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }
        let row = self.image.row(ImagePlane::ExtensionEntries, self.next);
        self.next += 1;
        if row[1..4] != [0; 3] {
            return Some(Err(ImageError::ExtensionReserved));
        }
        Some(match row[0] {
            1 => Ok(DeclarationExtension::Throws(TypeRef(
                JavaImageCoordinateView {
                    ordinal: u32_at(row, 4),
                },
            ))),
            2 => self
                .image
                .atom(u32_at(row, 4))
                .map(DeclarationExtension::Annotation),
            3 => Ok(DeclarationExtension::RecordComponent(
                u32_at(row, 4) as usize
            )),
            found => Err(ImageError::Tag {
                plane: ImagePlane::ExtensionEntries,
                found,
            }),
        })
    }
}

impl ExactSizeIterator for ExtensionIter<'_> {
    fn len(&self) -> usize {
        self.end - self.next
    }
}

impl<'image> Iterator for DeclarationIter<'image> {
    type Item = Result<Declaration<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(ImagePlane::Declarations).count {
            return None;
        }
        let row = self.image.row(ImagePlane::Declarations, self.next);
        self.next += 1;
        Some(Declaration::decode(self.image, row))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        exact_hint(
            self.image.section(ImagePlane::Declarations).count,
            self.next,
        )
    }
}

impl ExactSizeIterator for DeclarationIter<'_> {
    fn len(&self) -> usize {
        self.image.section(ImagePlane::Declarations).count - self.next
    }
}

/// An exact-size iterator over borrowed resolved call references.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceIter<'image> {
    image: JavaImage<'image>,
    next: usize,
}

impl<'image> Iterator for ReferenceIter<'image> {
    type Item = Result<Reference<'image>, ImageError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.image.section(ImagePlane::References).count {
            return None;
        }
        let row = self.image.row(ImagePlane::References, self.next);
        self.next += 1;
        Some(Reference::decode(self.image, row))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        exact_hint(self.image.section(ImagePlane::References).count, self.next)
    }
}

impl ExactSizeIterator for ReferenceIter<'_> {
    fn len(&self) -> usize {
        self.image.section(ImagePlane::References).count - self.next
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Section {
    offset: usize,
    count: usize,
}

impl<'image> TypeFact<'image> {
    fn decode(image: JavaImage<'image>, index: usize) -> Result<Self, ImageError> {
        let row = image.row(ImagePlane::Types, index);
        let kind = TypeKind::decode(row[0]).ok_or(ImageError::Tag {
            plane: ImagePlane::Types,
            found: row[0],
        })?;
        Ok(Self {
            kind,
            spelling: image.optional_atom(u32_at(row, 4))?,
            flags: row[1],
            children: image.type_children(
                ImagePlane::TypeChildren,
                usize::try_from(u32_at(row, 8)).map_err(|_| ImageError::ChildRange {
                    plane: ImagePlane::TypeChildren,
                    start: usize::MAX,
                    count: usize::from(u16_at(row, 2)),
                    upper_bound: image.section(ImagePlane::TypeChildren).count,
                })?,
                usize::from(u16_at(row, 2)),
            )?,
        })
    }
}

impl<'image> Symbol<'image> {
    fn decode(image: JavaImage<'image>, index: usize) -> Result<Self, ImageError> {
        let row = image.row(ImagePlane::Symbols, index);
        Ok(Self {
            owner: image.atom(u32_at(row, 0))?,
            name: image.atom(u32_at(row, 4))?,
            parameters: image.type_children(
                ImagePlane::SymbolParameters,
                usize::try_from(u32_at(row, 8)).map_err(|_| ImageError::ChildRange {
                    plane: ImagePlane::SymbolParameters,
                    start: usize::MAX,
                    count: usize::from(u16_at(row, 12)),
                    upper_bound: image.section(ImagePlane::SymbolParameters).count,
                })?,
                usize::from(u16_at(row, 12)),
            )?,
        })
    }
}

impl<'image> Declaration<'image> {
    fn decode(image: JavaImage<'image>, row: &[u8]) -> Result<Self, ImageError> {
        let documentation_flavor = DocFlavor::decode(row[2]).ok_or(ImageError::Tag {
            plane: ImagePlane::Declarations,
            found: row[2],
        })?;
        let documentation = image.optional_atom(u32_at(row, 16))?;
        if matches!(documentation_flavor, DocFlavor::Absent) != documentation.is_none() {
            return Err(ImageError::DocumentationPresence);
        }
        Ok(Self {
            kind: DeclarationKind::decode(row[0]).ok_or(ImageError::Tag {
                plane: ImagePlane::Declarations,
                found: row[0],
            })?,
            origin: Origin::decode(row[1]).ok_or(ImageError::Tag {
                plane: ImagePlane::Declarations,
                found: row[1],
            })?,
            documentation_flavor,
            modifiers: modifiers(u32_at(row, 4))?,
            name: image.atom(u32_at(row, 8))?,
            owner: image.optional_atom(u32_at(row, 12))?,
            documentation,
            semantic_type: optional_type(u32_at(row, 24)),
            symbol: optional_symbol(u32_at(row, 28)),
        })
    }
}

impl<'image> Reference<'image> {
    fn decode(image: JavaImage<'image>, row: &[u8]) -> Result<Self, ImageError> {
        let start = u32_at(row, 12);
        let end = u32_at(row, 16);
        if start > end {
            return Err(ImageError::ReferenceRange { start, end });
        }
        let owner = SymbolRef(JavaImageCoordinateView {
            ordinal: u32_at(row, 0),
        });
        let target = SymbolRef(JavaImageCoordinateView {
            ordinal: u32_at(row, 4),
        });
        image.symbol(owner)?;
        image.symbol(target)?;
        Ok(Self {
            owner,
            target,
            file: image.atom(u32_at(row, 8))?,
            start,
            end,
        })
    }
}

impl<'image> JavaImage<'image> {
    fn type_children(
        self,
        plane: ImagePlane,
        start: usize,
        count: usize,
    ) -> Result<TypeChildren<'image>, ImageError> {
        let upper_bound = self.section(plane).count;
        let end = start.checked_add(count).ok_or(ImageError::ChildRange {
            plane,
            start,
            count,
            upper_bound,
        })?;
        if end > upper_bound {
            return Err(ImageError::ChildRange {
                plane,
                start,
                count,
                upper_bound,
            });
        }
        Ok(TypeChildren {
            image: self,
            plane,
            next: start,
            end,
        })
    }
}

fn read_directory(
    bytes: &[u8],
    section_count: usize,
    header_bytes: usize,
) -> Result<[Section; V2_SECTIONS], ImageError> {
    let mut sections = [Section {
        offset: 0,
        count: 0,
    }; V2_SECTIONS];
    let mut expected_offset = header_bytes;
    for (index, section) in sections.iter_mut().take(section_count).enumerate() {
        let plane = ImagePlane::from_directory_index(index);
        let entry = DIRECTORY_OFFSET + index * DIRECTORY_ENTRY_BYTES;
        let tag = u16_at(bytes, entry);
        if tag != plane as u16 {
            return Err(ImageError::Section {
                plane,
                cause: SectionError::Tag {
                    expected: plane as u16,
                    found: tag,
                },
            });
        }
        let row_bytes = usize::from(u16_at(bytes, entry + 2));
        if row_bytes != plane.row_bytes() {
            return Err(ImageError::Section {
                plane,
                cause: SectionError::RowBytes {
                    expected: plane.row_bytes(),
                    found: row_bytes,
                },
            });
        }
        let count = usize::try_from(u32_at(bytes, entry + 4)).map_err(|_| ImageError::Section {
            plane,
            cause: SectionError::ByteCount {
                count: usize::MAX,
                row_bytes,
                found: usize::MAX,
            },
        })?;
        let offset =
            usize::try_from(u32_at(bytes, entry + 8)).map_err(|_| ImageError::Section {
                plane,
                cause: SectionError::Range {
                    offset: usize::MAX,
                    length: 0,
                    image_bytes: bytes.len(),
                },
            })?;
        let byte_count =
            usize::try_from(u32_at(bytes, entry + 12)).map_err(|_| ImageError::Section {
                plane,
                cause: SectionError::Range {
                    offset,
                    length: usize::MAX,
                    image_bytes: bytes.len(),
                },
            })?;
        let expected_bytes = count.checked_mul(row_bytes).ok_or(ImageError::Section {
            plane,
            cause: SectionError::ByteCount {
                count,
                row_bytes,
                found: byte_count,
            },
        })?;
        if byte_count != expected_bytes {
            return Err(ImageError::Section {
                plane,
                cause: SectionError::ByteCount {
                    count,
                    row_bytes,
                    found: byte_count,
                },
            });
        }
        if offset != expected_offset {
            return Err(ImageError::Section {
                plane,
                cause: SectionError::Offset {
                    expected: expected_offset,
                    found: offset,
                },
            });
        }
        let end = offset.checked_add(byte_count).ok_or(ImageError::Section {
            plane,
            cause: SectionError::Range {
                offset,
                length: byte_count,
                image_bytes: bytes.len(),
            },
        })?;
        if end > bytes.len() {
            return Err(ImageError::Section {
                plane,
                cause: SectionError::Range {
                    offset,
                    length: byte_count,
                    image_bytes: bytes.len(),
                },
            });
        }
        *section = Section { offset, count };
        expected_offset = end;
    }
    if expected_offset != bytes.len() {
        return Err(ImageError::Section {
            plane: ImagePlane::from_directory_index(section_count.saturating_sub(1)),
            cause: SectionError::Range {
                offset: expected_offset,
                length: 0,
                image_bytes: bytes.len(),
            },
        });
    }
    Ok(sections)
}

fn coordinate(raw: u32, plane: ImagePlane, upper_bound: usize) -> Result<usize, ImageError> {
    let index = usize::try_from(raw).map_err(|_| ImageError::Coordinate {
        plane,
        index: usize::MAX,
        upper_bound,
    })?;
    if index >= upper_bound {
        return Err(ImageError::Coordinate {
            plane,
            index,
            upper_bound,
        });
    }
    Ok(index)
}

fn row_index(index: usize, plane: ImagePlane, upper_bound: usize) -> Result<u32, ImageError> {
    u32::try_from(index).map_err(|_| ImageError::Coordinate {
        plane,
        index,
        upper_bound,
    })
}

fn optional_type(raw: u32) -> Option<TypeRef> {
    (raw != ABSENT).then_some(TypeRef(JavaImageCoordinateView { ordinal: raw }))
}

fn optional_symbol(raw: u32) -> Option<SymbolRef> {
    (raw != ABSENT).then_some(SymbolRef(JavaImageCoordinateView { ordinal: raw }))
}

fn modifiers(bits: u32) -> Result<Modifiers, ImageError> {
    if bits & !KNOWN_MODIFIERS == 0 {
        Ok(Modifiers { bits })
    } else {
        Err(ImageError::ModifierBits { found: bits })
    }
}

fn exact_hint(total: usize, next: usize) -> (usize, Option<usize>) {
    let remaining = total - next;
    (remaining, Some(remaining))
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
