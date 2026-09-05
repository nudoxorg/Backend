//! Typed JSON oracle for compiler lowering terminals.

use serde::Deserialize;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenLoweringCause {
    NoSupportedDeclaration,
    ExtensionAtomUnbound {
        row: u32,
        provisional: u32,
        atom_count: u32,
    },
    ExtensionTypeParametersUnbound {
        row: u32,
        start: u32,
        length: u32,
        element_count: u32,
    },
    FactRejected {
        fact: u32,
    },
    RustFunction,
    RustConstantType,
    RustGenericParameter,
    PythonAssignmentName,
    PythonAssignmentValue,
    ClangDeclarationForm,
    TypeScriptDeclarationForm,
    TypeScriptDeclarationType,
    CSharpDeclarationForm,
    CSharpDeclarationType,
    GoDeclarationForm,
    GoDeclarationType,
    JavaDeclarationForm,
    JavaProjection {
        fault: GoldenJavaProjectionFault,
    },
    GoProjection {
        fault: GoldenGoProjectionFault,
    },
    TypeScriptProjection {
        fault: GoldenTypeScriptProjectionFault,
    },
    PythonProjection {
        fault: GoldenPythonProjectionFault,
    },
    CSharpProjection {
        fault: GoldenCSharpProjectionFault,
    },
    ClangProjection {
        fault: GoldenClangProjectionFault,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaProjectionFault {
    Image {
        cause: GoldenJavaImageFault,
    },
    Depth {
        type_row: u32,
    },
    MalformedType {
        type_row: u32,
        kind: GoldenJavaTypeKind,
    },
    Primitive {
        type_row: u32,
    },
    AtomUtf8 {
        symbol: u32,
        atom: GoldenJavaSymbolAtom,
    },
    SourceUtf8,
    Utf16Offset {
        units: u32,
        source_utf16_len: u32,
    },
    Utf16Range {
        start: u32,
        end: u32,
    },
    OrphanOwner {
        owner: u32,
    },
    ForeignKey {
        cause: GoldenForeignKeyFault,
    },
    SiblingCapacity {
        symbol: u32,
    },
    IndexCapacity {
        phase: GoldenJavaIndexPhase,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaImageFault {
    Header {
        cause: GoldenJavaHeaderFault,
    },
    Section {
        plane: GoldenJavaImagePlane,
        cause: GoldenJavaSectionFault,
    },
    Digest,
    Atom {
        index: u32,
        cause: GoldenJavaAtomFault,
    },
    AbsentAtom,
    Coordinate {
        plane: GoldenJavaImagePlane,
        index: u32,
        upper_bound: u32,
    },
    Tag {
        plane: GoldenJavaImagePlane,
        found: u8,
    },
    ChildRange {
        plane: GoldenJavaImagePlane,
        start: u32,
        count: u32,
        upper_bound: u32,
    },
    ReferenceRange {
        start: u32,
        end: u32,
    },
    DocumentationPresence,
    ModifierBits {
        found: u32,
    },
    RecordComponentKind {
        index: u32,
    },
    ExtensionReserved,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaHeaderFault {
    Truncated { actual: u32 },
    Magic { found: [u8; 4] },
    Version { found: u16 },
    Length { found: u16 },
    Release { found: u16 },
    SectionCount { found: u16 },
    BodyLength { declared: u32, actual: u32 },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaSectionFault {
    Tag {
        expected: u16,
        found: u16,
    },
    RowBytes {
        expected: u32,
        found: u32,
    },
    ByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    Offset {
        expected: u32,
        found: u32,
    },
    Range {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaAtomFault {
    NonCanonicalOffset { found: u32 },
    Range,
    Utf8,
    TrailingBytes,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaTypeKind {
    Primitive,
    Void,
    Declared,
    Array,
    Variable,
    Wildcard,
    Intersection,
    Union,
    Error,
    None,
    Null,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaSymbolAtom {
    Owner,
    Name,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaIndexPhase {
    FactOrdinal,
    TypeRow,
    TypeChild,
    NameIndex,
    SymbolIndex,
    ExecutableIndex,
    Signature,
    Documentation,
    Utf16,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaImagePlane {
    Atoms,
    AtomBytes,
    Types,
    TypeChildren,
    Symbols,
    SymbolParameters,
    Declarations,
    References,
    DeclarationExtensions,
    ExtensionEntries,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenForeignKeyFault {
    EmptyPath,
    BackslashInPath,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackageLineageFault {
    EmptyEcosystem,
    EmptyPackage,
    SeparatorInEcosystem,
    SeparatorInPackage,
    Backslash { part: GoldenLineagePart },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenLineagePart {
    Ecosystem,
    Package,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoProjectionFault {
    Image,
    IndexCapacity,
    Depth,
    VariadicWithoutParameter { signature: u32 },
    Anchor,
    ListCapacity,
    OrphanTarget,
    ForeignKey { cause: GoldenForeignKeyFault },
    PackageLineage { cause: GoldenPackageLineageFault },
    AtomUtf8,
    RelativeSpan { start: u32, end: u32 },
    OrphanOwner { owner: u32 },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenTypeScriptProjectionFault {
    ForeignKey { cause: GoldenForeignKeyFault },
    PackageLineage { cause: GoldenPackageLineageFault },
    CoordinateOverflow { value: u64 },
    MissingImportBinding { fact: u32 },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPythonProjectionFault {
    ForeignSpellingUtf8,
    ForeignKey { cause: GoldenForeignKeyFault },
    PackageLineage { cause: GoldenPackageLineageFault },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpProjectionFault {
    Image {
        cause: GoldenCSharpImageFault,
    },
    Depth,
    NameSpan {
        start: u32,
        end: u32,
    },
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    Foreign,
    AttributeCapacity {
        spellings: u32,
    },
    IndexCapacity,
    HeterogeneousArrayRank,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageFault {
    Header {
        cause: GoldenCSharpImageHeaderFault,
    },
    Digest,
    DeclarationKind {
        index: u32,
        found: u8,
        plane: GoldenCSharpImageSection,
    },
    DeclarationReserved {
        index: u32,
        plane: GoldenCSharpImageSection,
    },
    NameRange {
        index: u32,
        offset: u32,
        length: u32,
        atom_bytes: u32,
    },
    NameUtf8 {
        index: u32,
    },
    Span {
        index: u32,
        start: u32,
        end: u32,
    },
    TypeChildCount {
        index: u32,
        kind: GoldenCSharpImageTypeKind,
        min: u32,
        max: u32,
        actual: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageHeaderFault {
    Truncated {
        actual: u32,
    },
    Magic {
        found: [u8; 4],
    },
    Version {
        found: u16,
    },
    Length {
        found: u32,
    },
    SectionCount {
        found: u32,
    },
    BodyLength {
        declared: u32,
        actual: u32,
    },
    Reserved,
    DirectoryTag {
        expected: u16,
        found: u16,
    },
    DirectoryRowBytes {
        expected: u32,
        found: u32,
    },
    DirectoryByteCount {
        count: u32,
        row_bytes: u32,
        found: u32,
    },
    DirectoryOffset {
        expected: u32,
        found: u32,
    },
    DirectoryRange {
        offset: u32,
        length: u32,
        image_bytes: u32,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageSection {
    Atoms,
    AtomBytes,
    Declarations,
    Parameters,
    TypeParameters,
    TypeConstraints,
    Types,
    TypeChildren,
    Attributes,
    Docs,
    References,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpImageTypeKind {
    Named,
    Array,
    Pointer,
    NullableValue,
    Tuple,
    FunctionPointer,
    TypeParameter,
    Dynamic,
    Error,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenClangProjectionFault {
    Span {
        start: u32,
        end: u32,
    },
    Nameless {
        declaration: u32,
    },
    Anchor,
    IndexCapacity,
    ForeignOverride {
        identity: [u8; 16],
    },
    ForeignReference {
        identity: [u8; 16],
    },
    IllegalQualifierTarget {
        type_id: u32,
        kind: GoldenClangTypeKind,
        qualifiers: GoldenClangQualifiers,
    },
    IllegalMemberPointerOwner {
        pointer: u32,
        owner: u32,
        kind: GoldenClangTypeKind,
        declaration: GoldenClangDeclaration,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenClangQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_restrict: bool,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenClangDeclaration {
    Known { identity: [u8; 16] },
    Unavailable,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenClangTypeKind {
    Unknown,
    Builtin,
    Named,
    Pointer,
    BlockPointer,
    MemberPointer,
    LvalueReference,
    RvalueReference,
    Array,
    Function,
}

macro_rules! impl_unit_conversion {
    ($source:ident => $target:ident [$($variant:ident),+ $(,)?]) => {
        impl From<compiler_vocabulary::$source> for $target {
            fn from(source: compiler_vocabulary::$source) -> Self {
                match source {
                    $(compiler_vocabulary::$source::$variant => Self::$variant),+
                }
            }
        }
    };
}

impl From<compiler_vocabulary::LoweringUnsupported> for GoldenLoweringCause {
    fn from(cause: compiler_vocabulary::LoweringUnsupported) -> Self {
        use compiler_vocabulary::LoweringUnsupported;
        match cause {
            LoweringUnsupported::NoSupportedDeclaration => Self::NoSupportedDeclaration,
            LoweringUnsupported::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            } => Self::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            },
            LoweringUnsupported::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            } => Self::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            },
            LoweringUnsupported::FactRejected { fact } => Self::FactRejected { fact },
            LoweringUnsupported::RustFunction => Self::RustFunction,
            LoweringUnsupported::RustConstantType => Self::RustConstantType,
            LoweringUnsupported::RustGenericParameter => Self::RustGenericParameter,
            LoweringUnsupported::PythonAssignmentName => Self::PythonAssignmentName,
            LoweringUnsupported::PythonAssignmentValue => Self::PythonAssignmentValue,
            LoweringUnsupported::ClangDeclarationForm => Self::ClangDeclarationForm,
            LoweringUnsupported::TypeScriptDeclarationForm => Self::TypeScriptDeclarationForm,
            LoweringUnsupported::TypeScriptDeclarationType => Self::TypeScriptDeclarationType,
            LoweringUnsupported::CSharpDeclarationForm => Self::CSharpDeclarationForm,
            LoweringUnsupported::CSharpDeclarationType => Self::CSharpDeclarationType,
            LoweringUnsupported::GoDeclarationForm => Self::GoDeclarationForm,
            LoweringUnsupported::GoDeclarationType => Self::GoDeclarationType,
            LoweringUnsupported::JavaDeclarationForm => Self::JavaDeclarationForm,
            LoweringUnsupported::JavaProjection { fault } => Self::JavaProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::GoProjection { fault } => Self::GoProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::TypeScriptProjection { fault } => Self::TypeScriptProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::PythonProjection { fault } => Self::PythonProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::CSharpProjection { fault } => Self::CSharpProjection {
                fault: fault.into(),
            },
            LoweringUnsupported::ClangProjection { fault } => Self::ClangProjection {
                fault: fault.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::JavaProjectionFault> for GoldenJavaProjectionFault {
    fn from(fault: compiler_vocabulary::JavaProjectionFault) -> Self {
        use compiler_vocabulary::JavaProjectionFault;
        match fault {
            JavaProjectionFault::Image { cause } => Self::Image {
                cause: cause.into(),
            },
            JavaProjectionFault::Depth { type_row } => Self::Depth { type_row },
            JavaProjectionFault::MalformedType { type_row, kind } => Self::MalformedType {
                type_row,
                kind: kind.into(),
            },
            JavaProjectionFault::Primitive { type_row } => Self::Primitive { type_row },
            JavaProjectionFault::AtomUtf8 { symbol, atom } => Self::AtomUtf8 {
                symbol,
                atom: atom.into(),
            },
            JavaProjectionFault::SourceUtf8 => Self::SourceUtf8,
            JavaProjectionFault::Utf16Offset {
                units,
                source_utf16_len,
            } => Self::Utf16Offset {
                units,
                source_utf16_len,
            },
            JavaProjectionFault::Utf16Range { start, end } => Self::Utf16Range { start, end },
            JavaProjectionFault::OrphanOwner { owner } => Self::OrphanOwner { owner },
            JavaProjectionFault::ForeignKey { cause } => Self::ForeignKey {
                cause: GoldenForeignKeyFault::from(cause),
            },
            JavaProjectionFault::SiblingCapacity { symbol } => Self::SiblingCapacity { symbol },
            JavaProjectionFault::IndexCapacity { phase } => Self::IndexCapacity {
                phase: phase.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::JavaImageFault> for GoldenJavaImageFault {
    fn from(fault: compiler_vocabulary::JavaImageFault) -> Self {
        use compiler_vocabulary::JavaImageFault;
        match fault {
            JavaImageFault::Header { cause } => Self::Header {
                cause: cause.into(),
            },
            JavaImageFault::Section { plane, cause } => Self::Section {
                plane: plane.into(),
                cause: cause.into(),
            },
            JavaImageFault::Digest => Self::Digest,
            JavaImageFault::Atom { index, cause } => Self::Atom {
                index,
                cause: cause.into(),
            },
            JavaImageFault::AbsentAtom => Self::AbsentAtom,
            JavaImageFault::Coordinate {
                plane,
                index,
                upper_bound,
            } => Self::Coordinate {
                plane: plane.into(),
                index,
                upper_bound,
            },
            JavaImageFault::Tag { plane, found } => Self::Tag {
                plane: plane.into(),
                found,
            },
            JavaImageFault::ChildRange {
                plane,
                start,
                count,
                upper_bound,
            } => Self::ChildRange {
                plane: plane.into(),
                start,
                count,
                upper_bound,
            },
            JavaImageFault::ReferenceRange { start, end } => Self::ReferenceRange { start, end },
            JavaImageFault::DocumentationPresence => Self::DocumentationPresence,
            JavaImageFault::ModifierBits { found } => Self::ModifierBits { found },
            JavaImageFault::RecordComponentKind { index } => Self::RecordComponentKind { index },
            JavaImageFault::ExtensionReserved => Self::ExtensionReserved,
        }
    }
}

impl From<compiler_vocabulary::JavaImageHeaderFault> for GoldenJavaHeaderFault {
    fn from(fault: compiler_vocabulary::JavaImageHeaderFault) -> Self {
        use compiler_vocabulary::JavaImageHeaderFault;
        match fault {
            JavaImageHeaderFault::Truncated { actual } => Self::Truncated { actual },
            JavaImageHeaderFault::Magic { found } => Self::Magic { found },
            JavaImageHeaderFault::Version { found } => Self::Version { found },
            JavaImageHeaderFault::Length { found } => Self::Length { found },
            JavaImageHeaderFault::Release { found } => Self::Release { found },
            JavaImageHeaderFault::SectionCount { found } => Self::SectionCount { found },
            JavaImageHeaderFault::BodyLength { declared, actual } => {
                Self::BodyLength { declared, actual }
            }
        }
    }
}

impl From<compiler_vocabulary::JavaImageSectionFault> for GoldenJavaSectionFault {
    fn from(fault: compiler_vocabulary::JavaImageSectionFault) -> Self {
        use compiler_vocabulary::JavaImageSectionFault;
        match fault {
            JavaImageSectionFault::Tag { expected, found } => Self::Tag { expected, found },
            JavaImageSectionFault::RowBytes { expected, found } => {
                Self::RowBytes { expected, found }
            }
            JavaImageSectionFault::ByteCount {
                count,
                row_bytes,
                found,
            } => Self::ByteCount {
                count,
                row_bytes,
                found,
            },
            JavaImageSectionFault::Offset { expected, found } => Self::Offset { expected, found },
            JavaImageSectionFault::Range {
                offset,
                length,
                image_bytes,
            } => Self::Range {
                offset,
                length,
                image_bytes,
            },
        }
    }
}

impl From<compiler_vocabulary::JavaImageAtomFault> for GoldenJavaAtomFault {
    fn from(fault: compiler_vocabulary::JavaImageAtomFault) -> Self {
        use compiler_vocabulary::JavaImageAtomFault;
        match fault {
            JavaImageAtomFault::NonCanonicalOffset { found } => Self::NonCanonicalOffset { found },
            JavaImageAtomFault::Range => Self::Range,
            JavaImageAtomFault::Utf8 => Self::Utf8,
            JavaImageAtomFault::TrailingBytes => Self::TrailingBytes,
        }
    }
}

impl_unit_conversion!(JavaProjectionTypeKind => GoldenJavaTypeKind [Primitive, Void, Declared, Array, Variable, Wildcard, Intersection, Union, Error, None, Null]);
impl_unit_conversion!(JavaSymbolAtom => GoldenJavaSymbolAtom [Owner, Name]);
impl_unit_conversion!(JavaForeignKeyFault => GoldenForeignKeyFault [EmptyPath, BackslashInPath]);
impl_unit_conversion!(JavaProjectionIndexPhase => GoldenJavaIndexPhase [FactOrdinal, TypeRow, TypeChild, NameIndex, SymbolIndex, ExecutableIndex, Signature, Documentation, Utf16]);
impl_unit_conversion!(JavaImagePlane => GoldenJavaImagePlane [Atoms, AtomBytes, Types, TypeChildren, Symbols, SymbolParameters, Declarations, References, DeclarationExtensions, ExtensionEntries]);
impl_unit_conversion!(ProjectionForeignKeyFault => GoldenForeignKeyFault [EmptyPath, BackslashInPath]);
impl_unit_conversion!(ProjectionLineagePart => GoldenLineagePart [Ecosystem, Package]);

impl From<compiler_vocabulary::ProjectionPackageLineageFault> for GoldenPackageLineageFault {
    fn from(fault: compiler_vocabulary::ProjectionPackageLineageFault) -> Self {
        use compiler_vocabulary::ProjectionPackageLineageFault;
        match fault {
            ProjectionPackageLineageFault::EmptyEcosystem => Self::EmptyEcosystem,
            ProjectionPackageLineageFault::EmptyPackage => Self::EmptyPackage,
            ProjectionPackageLineageFault::SeparatorInEcosystem => Self::SeparatorInEcosystem,
            ProjectionPackageLineageFault::SeparatorInPackage => Self::SeparatorInPackage,
            ProjectionPackageLineageFault::Backslash { part } => {
                Self::Backslash { part: part.into() }
            }
        }
    }
}

impl From<compiler_vocabulary::GoProjectionFault> for GoldenGoProjectionFault {
    fn from(fault: compiler_vocabulary::GoProjectionFault) -> Self {
        use compiler_vocabulary::GoProjectionFault;
        match fault {
            GoProjectionFault::Image => Self::Image,
            GoProjectionFault::IndexCapacity => Self::IndexCapacity,
            GoProjectionFault::Depth => Self::Depth,
            GoProjectionFault::VariadicWithoutParameter { signature } => {
                Self::VariadicWithoutParameter { signature }
            }
            GoProjectionFault::Anchor => Self::Anchor,
            GoProjectionFault::ListCapacity => Self::ListCapacity,
            GoProjectionFault::OrphanTarget => Self::OrphanTarget,
            GoProjectionFault::ForeignKey { cause } => Self::ForeignKey {
                cause: cause.into(),
            },
            GoProjectionFault::PackageLineage { cause } => Self::PackageLineage {
                cause: cause.into(),
            },
            GoProjectionFault::AtomUtf8 => Self::AtomUtf8,
            GoProjectionFault::RelativeSpan { start, end } => Self::RelativeSpan { start, end },
            GoProjectionFault::OrphanOwner { owner } => Self::OrphanOwner { owner },
        }
    }
}

impl From<compiler_vocabulary::TypeScriptProjectionFault> for GoldenTypeScriptProjectionFault {
    fn from(fault: compiler_vocabulary::TypeScriptProjectionFault) -> Self {
        use compiler_vocabulary::TypeScriptProjectionFault;
        match fault {
            TypeScriptProjectionFault::ForeignKey { cause } => Self::ForeignKey {
                cause: cause.into(),
            },
            TypeScriptProjectionFault::PackageLineage { cause } => Self::PackageLineage {
                cause: cause.into(),
            },
            TypeScriptProjectionFault::CoordinateOverflow { value } => {
                Self::CoordinateOverflow { value }
            }
            TypeScriptProjectionFault::MissingImportBinding { fact } => {
                Self::MissingImportBinding { fact }
            }
        }
    }
}

impl From<compiler_vocabulary::PythonProjectionFault> for GoldenPythonProjectionFault {
    fn from(fault: compiler_vocabulary::PythonProjectionFault) -> Self {
        use compiler_vocabulary::PythonProjectionFault;
        match fault {
            PythonProjectionFault::ForeignSpellingUtf8 => Self::ForeignSpellingUtf8,
            PythonProjectionFault::ForeignKey { cause } => Self::ForeignKey {
                cause: cause.into(),
            },
            PythonProjectionFault::PackageLineage { cause } => Self::PackageLineage {
                cause: cause.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::CSharpProjectionFault> for GoldenCSharpProjectionFault {
    fn from(fault: compiler_vocabulary::CSharpProjectionFault) -> Self {
        use compiler_vocabulary::CSharpProjectionFault;
        match fault {
            CSharpProjectionFault::Image { cause } => Self::Image {
                cause: cause.into(),
            },
            CSharpProjectionFault::Depth => Self::Depth,
            CSharpProjectionFault::NameSpan { start, end } => Self::NameSpan { start, end },
            CSharpProjectionFault::OwnerOrder {
                owner_start,
                reference_start,
            } => Self::OwnerOrder {
                owner_start,
                reference_start,
            },
            CSharpProjectionFault::Foreign => Self::Foreign,
            CSharpProjectionFault::AttributeCapacity { spellings } => {
                Self::AttributeCapacity { spellings }
            }
            CSharpProjectionFault::IndexCapacity => Self::IndexCapacity,
            CSharpProjectionFault::HeterogeneousArrayRank => Self::HeterogeneousArrayRank,
        }
    }
}

impl From<compiler_vocabulary::CSharpImageFault> for GoldenCSharpImageFault {
    fn from(fault: compiler_vocabulary::CSharpImageFault) -> Self {
        use compiler_vocabulary::CSharpImageFault;
        match fault {
            CSharpImageFault::Header { cause } => Self::Header {
                cause: cause.into(),
            },
            CSharpImageFault::Digest => Self::Digest,
            CSharpImageFault::DeclarationKind {
                index,
                found,
                plane,
            } => Self::DeclarationKind {
                index,
                found,
                plane: plane.into(),
            },
            CSharpImageFault::DeclarationReserved { index, plane } => Self::DeclarationReserved {
                index,
                plane: plane.into(),
            },
            CSharpImageFault::NameRange {
                index,
                offset,
                length,
                atom_bytes,
            } => Self::NameRange {
                index,
                offset,
                length,
                atom_bytes,
            },
            CSharpImageFault::NameUtf8 { index } => Self::NameUtf8 { index },
            CSharpImageFault::Span { index, start, end } => Self::Span { index, start, end },
            CSharpImageFault::TypeChildCount {
                index,
                kind,
                min,
                max,
                actual,
            } => Self::TypeChildCount {
                index,
                kind: kind.into(),
                min,
                max,
                actual,
            },
        }
    }
}

impl From<compiler_vocabulary::CSharpImageHeaderFault> for GoldenCSharpImageHeaderFault {
    fn from(fault: compiler_vocabulary::CSharpImageHeaderFault) -> Self {
        use compiler_vocabulary::CSharpImageHeaderFault;
        match fault {
            CSharpImageHeaderFault::Truncated { actual } => Self::Truncated { actual },
            CSharpImageHeaderFault::Magic { found } => Self::Magic { found },
            CSharpImageHeaderFault::Version { found } => Self::Version { found },
            CSharpImageHeaderFault::Length { found } => Self::Length { found },
            CSharpImageHeaderFault::SectionCount { found } => Self::SectionCount { found },
            CSharpImageHeaderFault::BodyLength { declared, actual } => {
                Self::BodyLength { declared, actual }
            }
            CSharpImageHeaderFault::Reserved => Self::Reserved,
            CSharpImageHeaderFault::DirectoryTag { expected, found } => {
                Self::DirectoryTag { expected, found }
            }
            CSharpImageHeaderFault::DirectoryRowBytes { expected, found } => {
                Self::DirectoryRowBytes { expected, found }
            }
            CSharpImageHeaderFault::DirectoryByteCount {
                count,
                row_bytes,
                found,
            } => Self::DirectoryByteCount {
                count,
                row_bytes,
                found,
            },
            CSharpImageHeaderFault::DirectoryOffset { expected, found } => {
                Self::DirectoryOffset { expected, found }
            }
            CSharpImageHeaderFault::DirectoryRange {
                offset,
                length,
                image_bytes,
            } => Self::DirectoryRange {
                offset,
                length,
                image_bytes,
            },
        }
    }
}

impl_unit_conversion!(CSharpImageSection => GoldenCSharpImageSection [Atoms, AtomBytes, Declarations, Parameters, TypeParameters, TypeConstraints, Types, TypeChildren, Attributes, Docs, References]);
impl_unit_conversion!(CSharpImageTypeKind => GoldenCSharpImageTypeKind [Named, Array, Pointer, NullableValue, Tuple, FunctionPointer, TypeParameter, Dynamic, Error]);
impl_unit_conversion!(ClangProjectionTypeKind => GoldenClangTypeKind [Unknown, Builtin, Named, Pointer, BlockPointer, MemberPointer, LvalueReference, RvalueReference, Array, Function]);

impl From<compiler_vocabulary::ClangProjectionFault> for GoldenClangProjectionFault {
    fn from(fault: compiler_vocabulary::ClangProjectionFault) -> Self {
        use compiler_vocabulary::ClangProjectionFault;
        match fault {
            ClangProjectionFault::Span { start, end } => Self::Span { start, end },
            ClangProjectionFault::Nameless { declaration } => Self::Nameless { declaration },
            ClangProjectionFault::Anchor => Self::Anchor,
            ClangProjectionFault::IndexCapacity => Self::IndexCapacity,
            ClangProjectionFault::ForeignOverride { identity } => {
                Self::ForeignOverride { identity }
            }
            ClangProjectionFault::ForeignReference { identity } => {
                Self::ForeignReference { identity }
            }
            ClangProjectionFault::IllegalQualifierTarget {
                type_id,
                kind,
                qualifiers,
            } => Self::IllegalQualifierTarget {
                type_id,
                kind: kind.into(),
                qualifiers: qualifiers.into(),
            },
            ClangProjectionFault::IllegalMemberPointerOwner {
                pointer,
                owner,
                kind,
                declaration,
            } => Self::IllegalMemberPointerOwner {
                pointer,
                owner,
                kind: kind.into(),
                declaration: declaration.into(),
            },
        }
    }
}

impl From<compiler_vocabulary::ClangProjectionQualifiers> for GoldenClangQualifiers {
    fn from(qualifiers: compiler_vocabulary::ClangProjectionQualifiers) -> Self {
        Self {
            is_const: qualifiers.is_const,
            is_volatile: qualifiers.is_volatile,
            is_restrict: qualifiers.is_restrict,
        }
    }
}

impl From<compiler_vocabulary::ClangProjectionDeclaration> for GoldenClangDeclaration {
    fn from(declaration: compiler_vocabulary::ClangProjectionDeclaration) -> Self {
        match declaration {
            compiler_vocabulary::ClangProjectionDeclaration::Known { identity } => {
                Self::Known { identity }
            }
            compiler_vocabulary::ClangProjectionDeclaration::Unavailable => Self::Unavailable,
        }
    }
}
