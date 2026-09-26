//! Closed C# authority projection and image vocabulary.

use super::projection::ProjectionAdmissionFault;

/// Closed C# authority projection terminal portable across the application
/// and interface boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpProjectionFault {
    /// A validated C# authority image row could not be reread.
    Image { cause: CSharpImageFault },
    /// The recursive authority type graph exceeded its projection budget.
    Depth { type_row: u32 },
    /// A declaration name span escaped the entered source.
    NameSpan { start: u32, end: u32 },
    /// A reference began before its owning declaration.
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    /// No admitted fact could own the authority's anonymous type row.
    Anchor { owner: u32 },
    /// A foreign occurrence key could not be built from authority facts.
    Foreign { reference: u32 },
    /// The bounded attribute lane exhausted its element capacity.
    AttributeCapacity { spellings: u32 },
    /// A projection index could not fit the compact lane.
    IndexCapacity {
        phase: CSharpProjectionIndexPhase,
        observed: u64,
    },
    /// A rectangular-array row repeated distinct element references.
    HeterogeneousArrayRank { first: u32, observed: u32 },
    /// A non-void result had no authority spelling for its carrier fact.
    ResultName { type_row: u32 },
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
    Truncated { actual: u32 },
    /// The fixed magic bytes differed.
    Magic { found: [u8; 4] },
    /// The image version was unsupported.
    Version { found: u16 },
    /// The fixed header length differed.
    Length { found: u32 },
    /// The directory count differed.
    SectionCount { found: u32 },
    /// Declared and supplied body lengths differed.
    BodyLength { declared: u32, actual: u32 },
    /// Reserved header bytes were nonzero.
    Reserved,
    /// A directory tag differed from canonical position.
    DirectoryTag { expected: u16, found: u16 },
    /// A directory row width differed from its fixed grammar.
    DirectoryRowBytes { expected: u32, found: u32 },
    /// Directory count, width, and aggregate byte count disagreed.
    DirectoryByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    /// A directory offset was not canonical.
    DirectoryOffset { expected: u32, found: u32 },
    /// A directory range escaped the image.
    DirectoryRange {
        offset: u32,
        length: u32,
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
    Header { cause: CSharpImageHeaderFault },
    /// The image checksum differed.
    Digest,
    /// A closed row tag was unknown in the named section.
    DeclarationKind {
        index: u32,
        found: u8,
        plane: CSharpImageSection,
    },
    /// A row carried reserved bytes or flags in the named section.
    DeclarationReserved {
        index: u32,
        plane: CSharpImageSection,
    },
    /// An atom range escaped the atom-byte plane.
    NameRange {
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    /// A referenced atom violated UTF-8.
    NameUtf8 { index: u32 },
    /// A source or section range was inverted or escaped its bound.
    Span { index: u32, start: u32, end: u32 },
    /// A type-node child run violated its closed cardinality law.
    TypeChildCount {
        index: u32,
        kind: CSharpImageTypeKind,
        min: u32,
        max: u32,
        actual: u32,
    },
}
impl core::fmt::Display for CSharpProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(core::mem::size_of::<CSharpProjectionFault>() <= 64);
