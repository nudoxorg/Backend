//! Kind discriminants, in-memory kind bodies, and per-kind marker structs.
//!
//! # Hierarchy
//!
//! - [`KindDiscriminant`] — the frozen `u16` wire tag; serialized into change
//!   payloads and hashed into `IntroId` preimages.
//! - [`Kind`] — the rich in-memory kind body for arena entries.
//! - Marker structs (`ModuleMarker`, `RecordMarker`, …) — zero-sized types used
//!   as the phantom type parameter of [`crate::index::EntryIdx`]; each implements
//!   [`crate::index::EntryKind`].
//!
//! # Notes
//!
//! The `Type` / `TypeRef` / `Primitive` / `Width` tree is the canonical
//! in-memory representation of type expressions. Wire serialization is handled
//! separately by the `*Wire` variants in [`crate::wire`].

use serde::{Deserialize, Serialize};

use crate::index::{self, sealed};
use nudox_change::{IntroId, StableRef};

// ---------------------------------------------------------------------------
// KindDiscriminant
// ---------------------------------------------------------------------------

/// Frozen wire discriminant for each entry kind.
///
/// The numeric values are part of the stable wire format and **must never be
/// reused or renumbered**. New kinds get new numbers above the current maximum.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
#[repr(u16)]
pub enum KindDiscriminant {
    /// A namespace / module.
    Module = 1,
    /// A product / struct / class / record.
    Record = 2,
    /// A field or member of a record.
    Field = 3,
    /// A function, method, or callable.
    Function = 4,
    /// A type alias, typedef, or type declaration.
    Type = 5,
}

impl KindDiscriminant {
    /// Convert a raw `u16` to a discriminant, returning `None` for unknown values.
    #[inline]
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Module),
            2 => Some(Self::Record),
            3 => Some(Self::Field),
            4 => Some(Self::Function),
            5 => Some(Self::Type),
            _ => None,
        }
    }

    /// The frozen wire `u16` for this discriminant.
    #[inline]
    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

// ---------------------------------------------------------------------------
// Per-kind marker structs
// ---------------------------------------------------------------------------

/// Kind marker for module / namespace entries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ModuleMarker;

/// Kind marker for record / struct / class entries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RecordMarker;

/// Kind marker for field entries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FieldMarker;

/// Kind marker for function / method entries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FunctionMarker;

/// Kind marker for type alias / typedef entries.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeMarker;

// Implement the sealed EntryKind trait for each marker.
impl sealed::Sealed for ModuleMarker {}
impl index::EntryKind for ModuleMarker {}

impl sealed::Sealed for RecordMarker {}
impl index::EntryKind for RecordMarker {}

impl sealed::Sealed for FieldMarker {}
impl index::EntryKind for FieldMarker {}

impl sealed::Sealed for FunctionMarker {}
impl index::EntryKind for FunctionMarker {}

impl sealed::Sealed for TypeMarker {}
impl index::EntryKind for TypeMarker {}

// ---------------------------------------------------------------------------
// Width
// ---------------------------------------------------------------------------

/// Bit-width of a primitive numeric type.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Width {
    /// Pointer-sized (platform-dependent).
    Arch,
    /// A fixed number of bits (e.g. 8, 16, 32, 64, 128).
    Fixed(u32),
}

// ---------------------------------------------------------------------------
// TypeRef
// ---------------------------------------------------------------------------

/// A reference to a type: either a same-package intro or a cross-package
/// foreign reference.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TypeRef {
    /// Same-package type introduction.
    Same(IntroId),
    /// Cross-package type, identified by a stable ref.
    Foreign(StableRef),
}

// ---------------------------------------------------------------------------
// Primitive
// ---------------------------------------------------------------------------

/// Primitive (scalar or pointer) types.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Primitive {
    /// An integer with optional sign and width.
    Integer { signed: bool, width: Width },
    /// A floating-point number.
    Float(Width),
    /// Boolean.
    Bool,
    /// Unicode character.
    Char,
    /// String slice (language-specific semantics).
    Str,
    /// Mutable raw pointer.
    MutPointer(Box<TypeRef>),
    /// Immutable raw pointer.
    ConstPointer(Box<TypeRef>),
    /// A reference (borrow), optionally mutable, optionally with a lifetime.
    Reference {
        /// Optional named lifetime (e.g. `"'a"`).
        lifetime: Option<String>,
        mutable: bool,
        ty: Box<TypeRef>,
    },
    /// Language-specific builtin not covered by the above (e.g. `"never"`,
    /// `"void"`, `"dynamic"`).
    Builtin(String),
}

// ---------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------

/// An in-memory type expression attached to a [`Kind::Type`] entry.
///
/// Structural types (Tuple, Union, Intersection, …) nest `TypeRef`s which may
/// point into the same package (by `IntroId`) or across packages (by `StableRef`).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Type {
    /// A `self` / `Self` type parameter.
    SelfType,
    /// A primitive type.
    Primitive(Primitive),
    /// A product (tuple / anonymous struct).
    Tuple(Vec<TypeRef>),
    /// A variable-length slice.
    Slice(Box<TypeRef>),
    /// A fixed-length array.
    Array { ty: Box<TypeRef>, length: u64 },
    /// A coproduct / union.
    Union(Vec<TypeRef>),
    /// A structural intersection.
    Intersection(Vec<TypeRef>),
    /// The bottom / uninhabited type.
    Never,
    /// A top / unknown type (`any`, `object`, …).
    Any,
}

// ---------------------------------------------------------------------------
// Kind
// ---------------------------------------------------------------------------

/// The in-memory kind body of an arena [`crate::entry::Entry`].
///
/// Corresponds 1-to-1 with [`KindDiscriminant`], but carries rich data. Wire
/// serialization is handled by [`crate::wire::KindWire`].
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Kind {
    /// A module or namespace.
    Module,
    /// A record / struct / class.
    Record,
    /// A field or member.
    Field,
    /// A function or callable.
    Function,
    /// A type alias or declaration.
    Type(Type),
}

impl Kind {
    /// The frozen discriminant for this kind variant.
    #[inline]
    pub fn discriminant(&self) -> KindDiscriminant {
        match self {
            Kind::Module => KindDiscriminant::Module,
            Kind::Record => KindDiscriminant::Record,
            Kind::Field => KindDiscriminant::Field,
            Kind::Function => KindDiscriminant::Function,
            Kind::Type(_) => KindDiscriminant::Type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_discriminant_roundtrip() {
        for disc in [
            KindDiscriminant::Module,
            KindDiscriminant::Record,
            KindDiscriminant::Field,
            KindDiscriminant::Function,
            KindDiscriminant::Type,
        ] {
            assert_eq!(KindDiscriminant::from_u16(disc.as_u16()), Some(disc));
        }
        assert_eq!(KindDiscriminant::from_u16(0), None);
        assert_eq!(KindDiscriminant::from_u16(99), None);
    }

    #[test]
    fn kind_discriminant_values() {
        assert_eq!(KindDiscriminant::Module as u16, 1);
        assert_eq!(KindDiscriminant::Record as u16, 2);
        assert_eq!(KindDiscriminant::Field as u16, 3);
        assert_eq!(KindDiscriminant::Function as u16, 4);
        assert_eq!(KindDiscriminant::Type as u16, 5);
    }
}
