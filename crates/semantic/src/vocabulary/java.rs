//! Closed Java authority projection and image vocabulary.

/// One closed Java type kind retained when an image row violates its
/// type-specific grammar. This vocabulary mirrors the authority's public
/// closed tags without importing the Java image crate into this portable
/// terminal crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaProjectionTypeKind {
    /// Java primitive type row.
    Primitive,
    /// Java `void` type row.
    Void,
    /// Declared Java class, interface, enum, annotation, or record row.
    Declared,
    /// Java array type row.
    Array,
    /// Java declared type-variable row.
    Variable,
    /// Java wildcard row.
    Wildcard,
    /// Java intersection row.
    Intersection,
    /// Java multi-catch union row.
    Union,
    /// Javac error type row.
    Error,
    /// Javac no-type sentinel row.
    None,
    /// Java null type row.
    Null,
}

/// One exact foreign-key grammar rejection projected from a Java reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaForeignKeyFault {
    /// The authority's canonical foreign path was empty.
    EmptyPath,
    /// The authority's canonical foreign path contained a backslash.
    BackslashInPath,
}

/// One atom role in a resolved Java executable-symbol row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaSymbolAtom {
    /// The symbol's declaring owner atom.
    Owner,
    /// The symbol's member-name atom.
    Name,
}

/// Closed staging operation whose coordinate could not fit Java's compact
/// projection lane. This names the failed operation without inventing a raw
/// coordinate where the source operation did not expose one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaProjectionIndexPhase {
    /// A canonical fact ordinal could not fit the compact coordinate width.
    FactOrdinal,
    /// A recursive type-row coordinate could not fit.
    TypeRow,
    /// A recursive type-child coordinate could not fit.
    TypeChild,
    /// A qualified declaration-name index was full.
    NameIndex,
    /// An executable-symbol index was full.
    SymbolIndex,
    /// An overload-executable index was full.
    ExecutableIndex,
    /// A callable signature carrier coordinate could not fit.
    Signature,
    /// A documentation projection coordinate could not fit.
    Documentation,
    /// A UTF-16-to-byte conversion could not fit the source coordinate lane.
    Utf16,
}

/// One fixed Java authority-image plane in canonical directory order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImagePlane {
    /// Fixed-width atom offset and length rows.
    Atoms,
    /// Concatenated UTF-8 atom bytes.
    AtomBytes,
    /// Recursive type rows.
    Types,
    /// Type-child coordinate rows.
    TypeChildren,
    /// Executable symbol rows.
    Symbols,
    /// Symbol-parameter coordinate rows.
    SymbolParameters,
    /// Declaration rows.
    Declarations,
    /// Resolved call-reference rows.
    References,
    /// Per-declaration extension ranges.
    DeclarationExtensions,
    /// Extension payload rows.
    ExtensionEntries,
}

/// Exact Java authority-image header rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageHeaderFault {
    /// The fixed header was truncated.
    Truncated { actual: u32 },
    /// The fixed magic bytes differed.
    Magic { found: [u8; 4] },
    /// The image version was unsupported.
    Version { found: u16 },
    /// The encoded fixed-header length differed.
    Length { found: u16 },
    /// The encoded Java release was unsupported.
    Release { found: u16 },
    /// The directory count differed from the grammar.
    SectionCount { found: u16 },
    /// Declared and supplied body lengths differed.
    BodyLength { declared: u32, actual: u32 },
}

/// Exact Java authority-image directory rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageSectionFault {
    /// A directory tag differed at its canonical plane position.
    Tag { expected: u16, found: u16 },
    /// A fixed row width differed.
    RowBytes { expected: u32, found: u32 },
    /// Count, row width, and encoded aggregate bytes disagreed.
    ByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    /// A plane offset was not contiguous.
    Offset { expected: u32, found: u32 },
    /// A declared plane range escaped the source image.
    Range {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

/// Exact Java atom-table rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageAtomFault {
    /// An atom began at a noncanonical byte offset.
    NonCanonicalOffset { found: u32 },
    /// An atom range escaped the atom-byte plane.
    Range,
    /// Atom bytes violated their UTF-8 promise.
    Utf8,
    /// Atom-byte data remained after the final atom.
    TrailingBytes,
}

/// Exact portable Java authority-image rejection.
///
/// All source image offsets are checked into the explicit `u32` image
/// coordinate width by the projection boundary. A native `usize` outside
/// that width instead produces the projection's separate `IndexCapacity`
/// terminal; it is never truncated or replaced by a sentinel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageFault {
    /// Header grammar rejected one exact nested fact.
    Header { cause: JavaImageHeaderFault },
    /// A canonical directory plane rejected one exact nested fact.
    Section {
        plane: JavaImagePlane,
        cause: JavaImageSectionFault,
    },
    /// The image checksum differed.
    Digest,
    /// An atom row rejected one exact nested fact.
    Atom {
        index: u32,
        cause: JavaImageAtomFault,
    },
    /// A required atom coordinate used the absent grammar value.
    AbsentAtom,
    /// A coordinate escaped its exact target plane.
    Coordinate {
        plane: JavaImagePlane,
        index: u32,
        upper_bound: u32,
    },
    /// A fixed-width row used an unknown closed tag.
    Tag { plane: JavaImagePlane, found: u8 },
    /// A child range overflowed or escaped its target plane.
    ChildRange {
        plane: JavaImagePlane,
        start: u32,
        count: u32,
        upper_bound: u32,
    },
    /// A reference range was inverted in Java UTF-16 coordinates.
    ReferenceRange { start: u32, end: u32 },
    /// Documentation flavor and atom presence disagreed.
    DocumentationPresence,
    /// A declaration modifier bitset carried unrecognized bits.
    ModifierBits { found: u32 },
    /// A record-component extension targeted a non-field declaration row.
    RecordComponentKind { index: u32 },
    /// Extension reserved bytes were nonzero.
    ExtensionReserved,
}

/// Closed Java authority projection terminal.
///
/// Each variant carries precisely the coordinates the projection had at its
/// failure site. Absence is encoded by choosing a variant with no coordinate,
/// never with an optional-coordinate bag or a sentinel value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaProjectionFault {
    /// A previously validated Java image row could not be reread.
    Image { cause: JavaImageFault },
    /// The producer-bounded recursive type graph exceeded its depth limit at
    /// this exact authority type row.
    Depth { type_row: u32 },
    /// A row of the named closed Java type kind lacked a required child.
    MalformedType {
        type_row: u32,
        kind: JavaProjectionTypeKind,
    },
    /// A primitive spelling in this exact authority type row lay outside the
    /// shared primitive vocabulary.
    Primitive { type_row: u32 },
    /// One exact atom in a resolved executable-symbol row failed its UTF-8
    /// promise.
    AtomUtf8 { symbol: u32, atom: JavaSymbolAtom },
    /// The compile source itself was not UTF-8 for Javac coordinate mapping.
    SourceUtf8,
    /// One Javac UTF-16 unit offset could not map into the source.
    Utf16Offset {
        /// Requested UTF-16 unit coordinate.
        units: u32,
        /// Exact UTF-16 length of the bound source.
        source_utf16_len: u32,
    },
    /// A Javac UTF-16 range was not an ordered relative byte span.
    Utf16Range {
        /// Reported source start coordinate.
        start: u32,
        /// Reported source end coordinate.
        end: u32,
    },
    /// A reference owner named no pushed executable.
    OrphanOwner { owner: u32 },
    /// A resolved external reference failed canonical key validation.
    ForeignKey { cause: JavaForeignKeyFault },
    /// The authority's sibling list for this executable symbol exceeded its
    /// bounded pooled width.
    SiblingCapacity { symbol: u32 },
    /// A checked projection coordinate could not fit the named operation.
    IndexCapacity { phase: JavaProjectionIndexPhase },
}
impl core::fmt::Display for JavaProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Image { cause } => write!(formatter, "image {cause:?}"),
            Self::Depth { type_row } => write!(formatter, "depth at type row {type_row}"),
            Self::MalformedType { type_row, kind } => {
                write!(formatter, "malformed {kind:?} type row {type_row}")
            }
            Self::Primitive { type_row } => write!(formatter, "primitive type row {type_row}"),
            Self::AtomUtf8 { symbol, atom } => {
                write!(formatter, "symbol {symbol} {atom:?} atom UTF-8")
            }
            Self::SourceUtf8 => formatter.write_str("source UTF-8"),
            Self::Utf16Offset {
                units,
                source_utf16_len,
            } => write!(formatter, "UTF-16 offset {units} of {source_utf16_len}"),
            Self::Utf16Range { start, end } => write!(formatter, "UTF-16 range {start}..{end}"),
            Self::OrphanOwner { owner } => write!(formatter, "orphan owner {owner}"),
            Self::ForeignKey { cause } => write!(formatter, "foreign key {cause:?}"),
            Self::SiblingCapacity { symbol } => write!(formatter, "sibling capacity {symbol}"),
            Self::IndexCapacity { phase } => write!(formatter, "{phase:?} capacity"),
        }
    }
}
const _: () = assert!(core::mem::size_of::<JavaProjectionFault>() <= 32);
