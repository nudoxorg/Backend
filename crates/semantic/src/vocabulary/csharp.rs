//! Closed C# authority projection and image vocabulary.

use core::mem::size_of;

use super::projection::ProjectionAdmissionFault;

/// Closed C# authority projection terminal portable across the application
/// and interface boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpProjectionFault {
    /// A validated C# authority image row could not be reread.
    Image {
        /// Exact closed rejection returned while rereading the validated C# image row.
        cause: CSharpImageFault,
    },
    /// The recursive authority type graph exceeded its projection budget.
    Depth {
        /// Zero-based C# authority type row at which recursive projection exceeded its depth budget.
        type_row: u32,
    },
    /// A declaration name span escaped the entered source.
    NameSpan {
        /// Inclusive byte offset of the declaration name in the entered C# source.
        start: u32,
        /// Exclusive byte offset of the declaration name in the entered C# source.
        end: u32,
    },
    /// A reference began before its owning declaration.
    OwnerOrder {
        /// Inclusive source-byte offset of the declaration that owns the reference.
        owner_start: u32,
        /// Inclusive source-byte offset of the reference occurrence.
        reference_start: u32,
    },
    /// No admitted fact could own the authority's anonymous type row.
    Anchor {
        /// Zero-based anonymous authority type row that no admitted fact could own.
        owner: u32,
    },
    /// A foreign occurrence key could not be built from authority facts.
    Foreign {
        /// Zero-based C# reference row for which authority facts could not form a canonical external key.
        reference: u32,
    },
    /// The bounded attribute lane exhausted its element capacity.
    AttributeCapacity {
        /// Number of distinct attribute-name spellings the projection needed to retain.
        spellings: u32,
    },
    /// A projection index could not fit the compact lane.
    IndexCapacity {
        /// Closed coordinate-conversion phase that exhausted the compact index width.
        phase: CSharpProjectionIndexPhase,
        /// Exact host-sized coordinate before conversion to the compact wire width.
        observed: u64,
    },
    /// A rectangular-array row repeated distinct element references.
    HeterogeneousArrayRank {
        /// First element-type reference in the rectangular-array row.
        first: u32,
        /// Another element-type reference in that row; a rectangular rank requires every element reference to agree.
        observed: u32,
    },
    /// A non-void result had no authority spelling for its carrier fact.
    ResultName {
        /// Zero-based C# result type row whose non-void carrier had no authority spelling.
        type_row: u32,
    },
    /// Canonical fact admission rejected one exact projected fact.
    Admission {
        /// Zero-based fact ordinal that would have been occupied.
        fact: u32,
        /// Exact byte length of the rejected fact name.
        name_len: u32,
        /// Full closed admission cause.
        cause: ProjectionAdmissionFault,
    },
}

/// Closed operation phase for a C# projection coordinate conversion.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpProjectionIndexPhase {
    /// Image-header coordinate.
    ImageHeader,
    /// Fact ordinal.
    FactOrdinal,
    /// Callable signature coordinate.
    Signature,
    /// Declaration row.
    Declaration,
    /// Type row.
    TypeRow,
    /// Type-child row.
    TypeChild,
    /// Attribute row.
    Attribute,
    /// Documentation row.
    Documentation,
    /// Reference row.
    Reference,
    /// Name/atom coordinate.
    Name,
    /// Source-span coordinate.
    SourceSpan,
}

/// One C# authority-image section in fixed directory order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageSection {
    /// Atom offsets and lengths.
    Atoms,
    /// Concatenated UTF-8 atom bytes.
    AtomBytes,
    /// Namespace, type, and member declarations.
    Declarations,
    /// Callable parameter rows.
    Parameters,
    /// Generic parameter rows.
    TypeParameters,
    /// Generic constraint rows.
    TypeConstraints,
    /// Recursive type rows.
    Types,
    /// Type-child rows.
    TypeChildren,
    /// Applied attribute rows.
    Attributes,
    /// XML documentation rows.
    Docs,
    /// Resolved reference rows.
    References,
}

/// Exact C# image header rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageHeaderFault {
    /// The fixed header was truncated.
    Truncated {
        /// Number of header bytes available when the fixed header was read.
        actual: u32,
    },
    /// The fixed magic bytes differed.
    Magic {
        /// Four magic bytes read from the image header, retained exactly for diagnosis.
        found: [u8; 4],
    },
    /// The image version was unsupported.
    Version {
        /// Raw image-format version from the fixed header.
        found: u16,
    },
    /// The fixed header length differed.
    Length {
        /// Encoded fixed-header length in bytes.
        found: u32,
    },
    /// The directory count differed.
    SectionCount {
        /// Encoded number of directory sections in the fixed header.
        found: u32,
    },
    /// Declared and supplied body lengths differed.
    BodyLength {
        /// Body byte length encoded in the fixed header.
        declared: u32,
        /// Body byte length actually supplied after the header.
        actual: u32,
    },
    /// Reserved header bytes were nonzero.
    Reserved,
    /// A directory tag differed from canonical position.
    DirectoryTag {
        /// Canonical section tag required at this directory position.
        expected: u16,
        /// Section tag encoded at this directory position.
        found: u16,
    },
    /// A directory row width differed from its fixed grammar.
    DirectoryRowBytes {
        /// Fixed byte width required for rows in this section.
        expected: u32,
        /// Row byte width encoded in this directory entry.
        found: u32,
    },
    /// Directory count, width, and aggregate byte count disagreed.
    DirectoryByteCount {
        /// Number of rows declared for this section.
        count: u32,
        /// Fixed byte width of one row in this section.
        row_bytes: u32,
        /// Aggregate byte length encoded for the section.
        found: u32,
    },
    /// A directory offset was not canonical.
    DirectoryOffset {
        /// First byte offset required by the canonical contiguous layout.
        expected: u32,
        /// Section byte offset encoded in the directory.
        found: u32,
    },
    /// A directory range escaped the image.
    DirectoryRange {
        /// First image byte occupied by the directory section.
        offset: u32,
        /// Byte length of the section range.
        length: u32,
        /// Total byte length of the complete C# authority image.
        image_bytes: u32,
    },
}

/// One closed C# authority type-node kind for an image child-law rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageTypeKind {
    /// Named type use.
    Named,
    /// Array type.
    Array,
    /// Pointer type.
    Pointer,
    /// Nullable value type.
    NullableValue,
    /// Tuple type.
    Tuple,
    /// Function-pointer type.
    FunctionPointer,
    /// Type-parameter use.
    TypeParameter,
    /// Dynamic type.
    Dynamic,
    /// Unbound Roslyn error type.
    Error,
}

/// Exact portable C# authority-image rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpImageFault {
    /// Fixed header grammar rejected one nested fact.
    Header {
        /// Specific fixed-header rule that rejected the image.
        cause: CSharpImageHeaderFault,
    },
    /// The image checksum differed.
    Digest,
    /// A closed row tag was unknown in the named section.
    DeclarationKind {
        /// Zero-based row in the named image section whose kind tag was unknown.
        index: u32,
        /// Raw one-byte declaration-kind tag from that row.
        found: u8,
        /// Closed C# image section containing the rejected row.
        plane: CSharpImageSection,
    },
    /// A row carried reserved bytes or flags in the named section.
    DeclarationReserved {
        /// Zero-based row containing a nonzero reserved byte or flag.
        index: u32,
        /// Closed C# image section containing the rejected row.
        plane: CSharpImageSection,
    },
    /// An atom range escaped the atom-byte plane.
    NameRange {
        /// Zero-based atom row whose byte range escaped atom storage.
        index: u32,
        /// First byte of the atom in the concatenated atom-byte plane.
        offset: u32,
        /// Encoded atom length in bytes.
        length: u32,
        /// Total byte length of concatenated atom storage.
        atom_bytes: u32,
    },
    /// A referenced atom violated UTF-8.
    NameUtf8 {
        /// Zero-based atom row whose referenced bytes were not valid UTF-8.
        index: u32,
    },
    /// A source or section range was inverted or escaped its bound.
    Span {
        /// Zero-based row carrying the rejected source or section range.
        index: u32,
        /// Inclusive source-byte or section-byte start encoded by the row.
        start: u32,
        /// Exclusive source-byte or section-byte end encoded by the row.
        end: u32,
    },
    /// A type-node child run violated its closed cardinality law.
    TypeChildCount {
        /// Zero-based recursive type row whose child run violated its kind-specific cardinality.
        index: u32,
        /// Closed type-node form that determines the permitted child count.
        kind: CSharpImageTypeKind,
        /// Minimum child-row count admitted for this type-node form.
        min: u32,
        /// Maximum child-row count admitted for this type-node form.
        max: u32,
        /// Number of child rows present in the type node.
        actual: u32,
    },
}
impl core::fmt::Display for CSharpProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(size_of::<CSharpProjectionFault>() <= 64);
