//! Closed Clang authority projection vocabulary.

use core::mem::size_of;

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
    Known {
        /// Exact 16-byte native declaration identity supplied by Clang; the bytes remain opaque and are not converted to a display name.
        identity: [u8; 16],
    },
    /// The authority supplied no declaration identity.
    Unavailable,
}

/// Closed Clang authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangProjectionFault {
    /// An authority source range escaped the entered source lease.
    Span {
        /// Inclusive source-byte offset of the rejected half-open span.
        start: u32,
        /// Exclusive source-byte offset of the rejected half-open span.
        end: u32,
    },
    /// A declaration requiring a name had no nonempty authority name span.
    Nameless {
        /// Zero-based Clang declaration row whose required name span was absent or empty.
        declaration: u32,
    },
    /// An anonymous authority type had no representable owning declaration.
    Anchor,
    /// A projection index could not fit the compact lane.
    IndexCapacity,
    /// A C++ override named a foreign native identity with no public key.
    ForeignOverride {
        /// Exact 16-byte key of the native declaration targeted by the C++ override; no public canonical key was available.
        identity: [u8; 16],
    },
    /// A reference named a foreign native identity with no public key.
    ForeignReference {
        /// Exact 16-byte key of the native declaration targeted by the reference; no public canonical key was available.
        identity: [u8; 16],
    },
    /// Qualifiers named a type form on which they are not semantically legal.
    IllegalQualifierTarget {
        /// Zero-based compact type coordinate to which the authority attached the illegal qualifier set.
        type_id: u32,
        /// Closed Clang type form at type_id; qualifier legality depends on this form.
        kind: ClangProjectionTypeKind,
        /// The exact const, volatile, and restrict bits Clang reported for type_id.
        qualifiers: ClangProjectionQualifiers,
    },
    /// A C++ member pointer named a non-class owner type.
    IllegalMemberPointerOwner {
        /// Zero-based compact type coordinate of the member-pointer type.
        pointer: u32,
        /// Zero-based compact type coordinate supplied as the member-pointer owner.
        owner: u32,
        /// Closed Clang type form found at owner; member pointers require a class type.
        kind: ClangProjectionTypeKind,
        /// Whether Clang supplied a compact identity for the declaration naming the owner.
        declaration: ClangProjectionDeclaration,
    },
}
impl core::fmt::Display for ClangProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(size_of::<ClangProjectionFault>() <= 32);
