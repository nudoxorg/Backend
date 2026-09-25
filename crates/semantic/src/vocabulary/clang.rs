//! Closed Clang authority projection vocabulary.

/// Closed native type kind retained by Clang projection terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionTypeKind {
    /// Unknown or unmodeled native type.
    Unknown,
    /// Native builtin type.
    Builtin,
    /// Declaration-named native type.
    Named,
    /// Native pointer.
    Pointer,
    /// Objective-C block pointer.
    BlockPointer,
    /// C++ member pointer.
    MemberPointer,
    /// C++ lvalue reference.
    LvalueReference,
    /// C++ rvalue reference.
    RvalueReference,
    /// Native array.
    Array,
    /// Native function type.
    Function,
}

/// Exact native qualifier fact retained by Clang projection terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClangProjectionQualifiers {
    /// Native `const` fact.
    pub is_const: bool,
    /// Native `volatile` fact.
    pub is_volatile: bool,
    /// Native `restrict` fact.
    pub is_restrict: bool,
}

/// Closed native declaration-identity availability in a Clang terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionDeclaration {
    /// The authority supplied a compact native declaration identity.
    Known { identity: [u8; 16] },
    /// The authority supplied no declaration identity.
    Unavailable,
}

/// Closed Clang authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionFault {
    /// An authority source range escaped the entered source lease.
    Span { start: u32, end: u32 },
    /// A declaration requiring a name had no nonempty authority name span.
    Nameless { declaration: u32 },
    /// An anonymous authority type had no representable owning declaration.
    Anchor,
    /// A projection index could not fit the compact lane.
    IndexCapacity,
    /// A C++ override named a foreign native identity with no public key.
    ForeignOverride { identity: [u8; 16] },
    /// A reference named a foreign native identity with no public key.
    ForeignReference { identity: [u8; 16] },
    /// Qualifiers named a type form on which they are not semantically legal.
    IllegalQualifierTarget {
        type_id: u32,
        kind: ClangProjectionTypeKind,
        qualifiers: ClangProjectionQualifiers,
    },
    /// A C++ member pointer named a non-class owner type.
    IllegalMemberPointerOwner {
        pointer: u32,
        owner: u32,
        kind: ClangProjectionTypeKind,
        declaration: ClangProjectionDeclaration,
    },
}
impl core::fmt::Display for ClangProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(core::mem::size_of::<ClangProjectionFault>() <= 32);
