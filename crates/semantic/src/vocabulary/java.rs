//! Closed Java authority projection and image vocabulary.

use core::mem::size_of;

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
    Truncated {
        /// Number of header bytes available when the fixed Java image header was read.
        actual: u32,
    },
    /// The fixed magic bytes differed.
    Magic {
        /// Four magic bytes read from the image header, retained exactly for diagnosis.
        found: [u8; 4],
    },
    /// The image version was unsupported.
    Version {
        /// Raw image-format version encoded in the header.
        found: u16,
    },
    /// The encoded fixed-header length differed.
    Length {
        /// Encoded fixed-header length in bytes.
        found: u16,
    },
    /// The encoded Java release was unsupported.
    Release {
        /// Java language release encoded in the image header.
        found: u16,
    },
    /// The directory count differed from the grammar.
    SectionCount {
        /// Number of directory planes encoded in the header.
        found: u16,
    },
    /// Declared and supplied body lengths differed.
    BodyLength {
        /// Body byte length encoded in the header.
        declared: u32,
        /// Body byte length actually supplied after the fixed header.
        actual: u32,
    },
}

/// Exact Java authority-image directory rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageSectionFault {
    /// A directory tag differed at its canonical plane position.
    Tag {
        /// Canonical plane tag required at this directory position.
        expected: u16,
        /// Plane tag encoded at this directory position.
        found: u16,
    },
    /// A fixed row width differed.
    RowBytes {
        /// Fixed byte width required for rows in this plane.
        expected: u32,
        /// Row byte width encoded in the directory entry.
        found: u32,
    },
    /// Count, row width, and encoded aggregate bytes disagreed.
    ByteCount {
        /// Number of rows declared for this plane.
        count: u32,
        /// Fixed byte width of one row in this plane.
        row_bytes: u32,
        /// Aggregate byte length encoded for this plane.
        found: u32,
    },
    /// A plane offset was not contiguous.
    Offset {
        /// First byte offset required by the canonical contiguous directory layout.
        expected: u32,
        /// Plane byte offset encoded in the directory.
        found: u32,
    },
    /// A declared plane range escaped the source image.
    Range {
        /// First image byte occupied by the plane.
        offset: u32,
        /// Byte length of the declared plane range.
        length: u32,
        /// Total byte length of the complete Java authority image.
        image_bytes: u32,
    },
}

/// Exact Java atom-table rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JavaImageAtomFault {
    /// An atom began at a noncanonical byte offset.
    NonCanonicalOffset {
        /// Atom byte offset encoded in the row; canonical atom storage must be contiguous.
        found: u32,
    },
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
    Header {
        /// Specific Java fixed-header rule that rejected the image.
        cause: JavaImageHeaderFault,
    },
    /// A canonical directory plane rejected one exact nested fact.
    Section {
        /// Java image plane whose directory entry failed validation.
        plane: JavaImagePlane,
        /// Specific closed directory rule rejected for that plane.
        cause: JavaImageSectionFault,
    },
    /// The image checksum differed.
    Digest,
    /// An atom row rejected one exact nested fact.
    Atom {
        /// Zero-based atom row whose encoding failed.
        index: u32,
        /// Specific atom-table rule rejected for that row.
        cause: JavaImageAtomFault,
    },
    /// A required atom coordinate used the absent grammar value.
    AbsentAtom,
    /// A coordinate escaped its exact target plane.
    Coordinate {
        /// Target Java image plane for the coordinate.
        plane: JavaImagePlane,
        /// Zero-based coordinate read from the row.
        index: u32,
        /// Number of rows in the target plane; valid coordinates are smaller.
        upper_bound: u32,
    },
    /// A fixed-width row used an unknown closed tag.
    Tag {
        /// Java image plane containing the tagged row.
        plane: JavaImagePlane,
        /// Raw one-byte tag that has no value in the closed vocabulary.
        found: u8,
    },
    /// A child range overflowed or escaped its target plane.
    ChildRange {
        /// Java plane containing the child coordinates.
        plane: JavaImagePlane,
        /// First child-row coordinate in the run.
        start: u32,
        /// Number of consecutive child coordinates claimed by the parent row.
        count: u32,
        /// Number of rows in the target plane; the run must end at or before this bound.
        upper_bound: u32,
    },
    /// A reference range was inverted in Java UTF-16 coordinates.
    ReferenceRange {
        /// Inclusive start of the call range in Java UTF-16 code units.
        start: u32,
        /// Exclusive end of the call range in Java UTF-16 code units.
        end: u32,
    },
    /// Documentation flavor and atom presence disagreed.
    DocumentationPresence,
    /// A declaration modifier bitset carried unrecognized bits.
    ModifierBits {
        /// Raw declaration modifier bitset; bits outside the Java image grammar are rejected.
        found: u32,
    },
    /// A record-component extension targeted a non-field declaration row.
    RecordComponentKind {
        /// Zero-based declaration row targeted by the record-component extension; it must be a field row.
        index: u32,
    },
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
    Image {
        /// Exact closed Java image rejection retained by the projection boundary.
        cause: JavaImageFault,
    },
    /// The producer-bounded recursive type graph exceeded its depth limit at
    /// this exact authority type row.
    Depth {
        /// Zero-based authority type row at which recursive Java projection exceeded its depth limit.
        type_row: u32,
    },
    /// A row of the named closed Java type kind lacked a required child.
    MalformedType {
        /// Zero-based Java authority type row missing a required child.
        type_row: u32,
        /// Closed Java type category whose row grammar requires that child.
        kind: JavaProjectionTypeKind,
    },
    /// A primitive spelling in this exact authority type row lay outside the
    /// shared primitive vocabulary.
    Primitive {
        /// Zero-based primitive type row whose spelling is outside the shared primitive vocabulary.
        type_row: u32,
    },
    /// One exact atom in a resolved executable-symbol row failed its UTF-8
    /// promise.
    AtomUtf8 {
        /// Zero-based resolved executable-symbol row carrying the atom.
        symbol: u32,
        /// Whether the invalid UTF-8 bytes belong to the owner name or member name.
        atom: JavaSymbolAtom,
    },
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
    OrphanOwner {
        /// Zero-based executable owner row for which no canonical fact was emitted.
        owner: u32,
    },
    /// A resolved external reference failed canonical key validation.
    ForeignKey {
        /// Exact closed foreign-key grammar rule violated by the external target.
        cause: JavaForeignKeyFault,
    },
    /// The authority's sibling list for this executable symbol exceeded its
    /// bounded pooled width.
    SiblingCapacity {
        /// Zero-based executable-symbol row whose bounded sibling list was full.
        symbol: u32,
    },
    /// A checked projection coordinate could not fit the named operation.
    IndexCapacity {
        /// Closed Java projection operation whose coordinate could not fit the compact lane.
        phase: JavaProjectionIndexPhase,
    },
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
const _: () = assert!(size_of::<JavaProjectionFault>() <= 32);
